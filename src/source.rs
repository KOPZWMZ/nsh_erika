//! 帧源（`ErikaVideoSource`）：**显示句柄**。
//!
//! 它由 [`ErikaPlayer::new`](crate::ErikaPlayer::new) 与播放句柄一起交出：句柄留在外面
//! 控制（`open` / `play` / `pause` / …），帧源交给 `erika.video` 控件显示
//! （`erika::video(&source)`）。它**只有**与出帧有关的几个方法
//! （`request_frame` / `frame_snapshot` / `has_frame` / `id`），拿不到任何播放控制 ——
//! 和 media_kit 的 `VideoController` 一个角色。
//!
//! ## 为什么是「号」而不是「引用」
//!
//! NSH 的组件边界只能过 JSON（`native(...).prop(name, value)`），句柄过不去。所以帧源在
//! 构造时领一个进程内唯一的号（[`ErikaVideoSource::id`]），登记在进程级注册表里；控件
//! 拿号就能找回它（[`ErikaVideoSource::resolve`]）。注册表持**强引用**（不能是弱引用：
//! 页面函数返回后 `ErikaVideoSource` 这个局部值就 drop 了，而控件的 prop 要到挂载时才送到），
//! 由播放句柄负责摘除 —— 最后一个 [`ErikaPlayer`](crate::ErikaPlayer) drop 时自动清掉。
//!
//! **只有一条出帧路径**：erika 导出的线性 dma-buf。没有 CPU 回读兜底 —— 宿主
//! （GTK/GSK）导不进来就没画面（见 `dev_doc/nsh-erika-status.md` §8）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::cmd::{ErikaCmd, RenderCmd};

/// 帧邮箱：1 格、最后写入者胜。**不通知** —— 渲染层每帧主动拉。
///
/// 为什么不走 actor 的 State/Progress：那两条是 `watch`（通知所有观察者"变了"），
/// 而帧是**取货**——消费者每帧来拉，从不需要通知。
pub(crate) struct FrameSlot {
    texture: Mutex<Option<Arc<erika::DmabufFrame>>>,
    /// 每写一次 +1。渲染层靠它跳过"同一帧的重复建纹理"。
    generation: AtomicU64,
    /// 视频原始尺寸 `(w << 32) | h`；0 = 还不知道。引擎写、控件每帧读。
    ///
    /// 放这里而不是 `ErikaStatus`：它是**帧的元信息**，控件每帧都要，而状态通道的语义是
    /// "变了才发"。一次写 + 每帧原子读，比每帧填结构体再判等便宜得多。
    video_size: AtomicU64,
}

impl FrameSlot {
    pub(crate) fn new() -> Self {
        Self {
            texture: Mutex::new(None),
            generation: AtomicU64::new(0),
            video_size: AtomicU64::new(0),
        }
    }

    /// 引擎侧：记下视频原始尺寸（`None` = 没有视频轨 / 还不知道）。
    pub(crate) fn set_video_size(&self, size: Option<(u32, u32)>) {
        let packed = size.map_or(0, |(w, h)| (u64::from(w) << 32) | u64::from(h));
        self.video_size.store(packed, Ordering::Release);
    }

    /// 控件侧：视频原始尺寸，`None` = 还不知道。
    pub(crate) fn video_size(&self) -> Option<(u32, u32)> {
        let packed = self.video_size.load(Ordering::Acquire);
        (packed != 0).then(|| ((packed >> 32) as u32, packed as u32))
    }

    /// 引擎侧：写入新帧（替换旧的 → 旧的那个随引用计数归零回收，fd 随之关闭）
    pub(crate) fn publish(&self, frame: Arc<erika::DmabufFrame>) {
        let mut g = self.lock();
        *g = Some(frame);
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// 两侧都能调：open / stop 时作废当前帧
    pub(crate) fn clear(&self) {
        let mut g = self.lock();
        *g = None;
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// 现在槽里有没有帧（零成本，不 clone）。
    pub(crate) fn has_frame(&self) -> bool {
        self.lock().is_some()
    }

    /// 渲染层：一次拿到「代号 + 帧」
    pub(crate) fn current(&self) -> (u64, Option<Arc<erika::DmabufFrame>>) {
        let g = self.lock();
        (self.generation.load(Ordering::Acquire), g.clone())
    }

    fn lock(&self) -> MutexGuard<'_, Option<Arc<erika::DmabufFrame>>> {
        self.texture.lock().unwrap_or_else(|p| p.into_inner()) // 毒化了继续用
    }
}

/// 一帧的请求参数（渲染层 → 引擎）。
///
/// `export` 是引擎真正出帧的尺寸 —— 视频宽高比在画布里的**内接矩形**（见
/// `component.rs::export_size`）。有 letterbox 时两者不同：竖版控件里放 16:9 的片子，
/// 导出尺寸比画布小一半以上。`ContentFit::Contain` 负责把这张图摆回画布中间。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameRequest {
    /// 引擎出帧尺寸（画布内接视频比例后的尺寸）。
    pub export: (u32, u32),
    /// DPI 缩放：物理像素 / 逻辑像素。
    pub scale: f64,
}

impl Default for FrameRequest {
    fn default() -> Self {
        Self {
            export: (0, 0),
            scale: 1.0,
        }
    }
}

/// 渲染层 → 引擎 的请求。**不含 uri**——那是句柄自己的去重缓存。
pub(crate) struct Request {
    /// "有人要画面"的旗标。60Hz 只换位，用原子；引擎 `swap(false)` 读并复位。
    pub(crate) want_frame: AtomicBool,
    /// 画布 + 导出尺寸 + DPI。低频，用锁。
    pub(crate) frame: Mutex<FrameRequest>,
}

impl Request {
    pub(crate) fn new() -> Self {
        Self {
            want_frame: AtomicBool::new(false),
            frame: Mutex::new(FrameRequest::default()),
        }
    }

    pub(crate) fn frame(&self) -> MutexGuard<'_, FrameRequest> {
        self.frame.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// 进程级注册表：号 → 帧源。
///
/// **持强引用**，由播放句柄负责摘除（最后一个 [`ErikaPlayer`](crate::ErikaPlayer) drop 时）。
/// 为什么不能是弱引用：页面函数返回后 `ErikaVideoSource` 这个局部值就 drop 了，而控件的
/// prop 要到挂载时才送到 —— 那时弱引用早就死了（实测报 `source=1 找不到帧源`）。
static REGISTRY: OnceLock<Mutex<HashMap<u64, Arc<Slot>>>> = OnceLock::new();
/// 下一个要发的号。1 起，0 留作"没有"。
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 帧源的共享部分：一条引擎命令通道 + 一格帧槽 + 一个出帧请求。
pub(crate) struct Slot {
    id: u64,
    /// 还活着的播放句柄数（[`ErikaPlayer`](crate::ErikaPlayer) 的 clone / drop 维护）。
    players: AtomicUsize,
    /// 播放句柄全部 drop 之后置 `false`：控件下一帧自己停。
    alive: AtomicBool,
    pub(crate) engine: Arc<crate::player::EngineGuard>,
    frames: Arc<FrameSlot>,
    request: Arc<Request>,
}

impl Slot {
    /// 它的号（控件绑定显示目标用）。
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn new(
        engine: Arc<crate::player::EngineGuard>,
        frames: Arc<FrameSlot>,
        request: Arc<Request>,
    ) -> Arc<Self> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new(Self {
            id,
            players: AtomicUsize::new(1),
            alive: AtomicBool::new(true),
            engine,
            frames,
            request,
        });
        REGISTRY
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, slot.clone());
        log::debug!("erika.video: 帧源 #{id} 登记");
        slot
    }

    /// 又克隆出一个播放句柄。
    pub(crate) fn player_cloned(&self) {
        self.players.fetch_add(1, Ordering::Relaxed);
    }

    /// 少一个播放句柄；最后一个走人时摘掉注册表里的这一条。
    pub(crate) fn player_dropped(&self) {
        if self.players.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.alive.store(false, Ordering::Release);
            if let Some(map) = REGISTRY.get() {
                if let Ok(mut map) = map.lock() {
                    map.remove(&self.id);
                }
            }
            log::debug!("erika.video: 帧源 #{} 摘除（播放句柄已全部 drop）", self.id);
        }
    }
}
/// 显示句柄：交给 `erika.video` 控件的东西。
///
/// 由 [`ErikaPlayer::new`](crate::ErikaPlayer::new) 一起交出；`Clone` 只是 Arc 加一，
/// 可以随便被 UI 闭包捕获。**控制能力不在这里** —— 播放/暂停/选轨都在
/// [`ErikaPlayer`](crate::ErikaPlayer) 上。
#[derive(Clone)]
pub struct ErikaVideoSource {
    slot: Arc<Slot>,
}

impl ErikaVideoSource {
    pub(crate) fn new(slot: Arc<Slot>) -> Self {
        Self { slot }
    }

    /// 这个帧源的号：控件用它绑定显示目标（`erika::video(&source)` 内部就是这个）。
    pub fn id(&self) -> u64 {
        self.slot.id
    }

    /// 播放句柄还在不在（全部 drop 之后控件该收工了）。
    pub(crate) fn is_alive(&self) -> bool {
        self.slot.alive.load(Ordering::Acquire)
    }

    /// 按号找回帧源。控件专用：号无效（从没存在过 / 播放句柄已经 drop）时返回 `None`。
    pub(crate) fn resolve(id: u64) -> Option<Self> {
        let map = REGISTRY.get()?.lock().ok()?;
        let slot = map.get(&id)?.clone();
        Some(Self { slot })
    }

    /// 视频原始宽高比（没打开 / 没有视频轨时为 `None`）。控件用它算内接矩形。
    pub(crate) fn video_aspect(&self) -> Option<f32> {
        let (width, height) = self.slot.frames.video_size()?;
        (width > 0 && height > 0).then(|| width as f32 / height as f32)
    }

    /// **渲染层每帧调这个**，请求一帧（非阻塞，60Hz 也只会让引擎处理一次）。
    pub fn request_frame(&self, request: FrameRequest) {
        {
            let mut slot = self.slot.request.frame();
            *slot = request;
        }
        if !self.slot.request.want_frame.swap(true, Ordering::AcqRel) {
            let _ = self
                .slot
                .engine
                .handle
                .send_cmd(ErikaCmd::Render(RenderCmd::Wake));
        }
    }

    /// 控件侧导入 dma-buf 失败时调：告诉引擎本机出不了帧（引擎据此停止导出、写进状态）。
    ///
    /// 只在**第一次**调用有意义（引擎那边一次性置上），但重复调用无害。
    pub(crate) fn report_video_unavailable(&self, reason: String) {
        let _ = self
            .slot
            .engine
            .handle
            .send_cmd(ErikaCmd::VideoUnavailable(reason));
    }

    /// 现在有没有画面可画（**零成本、非阻塞**）。
    pub fn has_frame(&self) -> bool {
        self.slot.frames.has_frame()
    }

    /// 渲染层专用：一次拿到「代号 + 帧」。代号没变 = 没有新帧。
    pub(crate) fn frame_snapshot(&self) -> (u64, Option<Arc<erika::DmabufFrame>>) {
        self.slot.frames.current()
    }
}

impl PartialEq for ErikaVideoSource {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.slot, &other.slot) // 同一个帧源
    }
}
impl Eq for ErikaVideoSource {}

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
//! **每个平台一条出帧路径**：Linux 是 erika 导出的线性 dma-buf，没有 CPU 回读兜底 ——
//! 宿主（GTK/GSK）导不进来就没画面（见 `dev_doc/nsh-erika-status.md` §8）。
//!
//! ## 帧载荷：`FrameImage`
//!
//! 帧槽装的是 [`FrameImage`]，不是某个平台的原生句柄类型 —— 这样帧槽的读写口
//! （`publish` / `current` / `frame_snapshot` / `has_frame`）在所有平台是同一套代码。
//! 各平台的"这一帧长什么样"由各自的变体表示：
//!
//! - Linux：`Dmabuf(erika::DmabufFrame)`，即 `export_frame_dmabuf` 的产物（一个 fd +
//!   stride/offset/尺寸）；
//! - Windows：`SharedTexture`，erika 发布的 D3D11 共享纹理（`*mut c_void` 的 `IUnknown`，
//!   见 `dev_doc/nsh-erika-architecture.md` §5.7.1）。
//!
//! 控件侧按平台各取各的（Linux 在 `linux/frames.rs` 里 `as_dmabuf()` → `GdkDmabufTexture`）。
//!
//! 两边的变体各自只在对应平台编译，帧槽本身没有平台分支。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::core::cmd::{ErikaCmd, RenderCmd};
use crate::core::player::EngineGuard;

/// 一帧的画面载荷：各平台一份变体。
///
/// 帧槽的读写口（`publish` / `current` / `frame_snapshot` / `has_frame`）在任何平台都是
/// 同一件事，分平台的只有"怎么得到一帧"和"怎么把它画出来" —— 所以是一个枚举，而不是把
/// 整条帧槽按平台 cfg 掉。
///
/// 两边的产物都是**GPU 纹理**，只是交给宿主的方式不同：
///
/// - Linux：erika `export_frame_dmabuf` 导出的线性 dma-buf（一个 fd）；
/// - Windows：erika 的 D3D11 共享纹理（`IUnknown`，`GetSharedHandle` 后可被宿主 device 打开）。
///
/// 见 dev_doc `nsh-erika-architecture.md` §5.7.1（Windows 为什么走这条）。
pub enum FrameImage {
    /// erika 导出的线性 dma-buf（`export_frame_dmabuf` 的产物，四平面打包成一块，
    /// 按 `stride` 分行、`offset` 起头）。
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    Dmabuf(erika::DmabufFrame),
    /// erika 发布的 D3D11 共享纹理（`windows_flutter_texture_iunknown` 的产物）。
    #[cfg(target_os = "windows")]
    SharedTexture(crate::windows::frame::SharedTexture),
}

impl FrameImage {
    /// dma-buf 载荷；不是 dma-buf（别的平台的帧）时 `None`。
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    pub fn as_dmabuf(&self) -> Option<&erika::DmabufFrame> {
        match self {
            Self::Dmabuf(frame) => Some(frame),
        }
    }

    /// 共享纹理载荷；不是共享纹理时 `None`。
    #[cfg(target_os = "windows")]
    pub fn as_shared_texture(&self) -> Option<&crate::windows::frame::SharedTexture> {
        match self {
            Self::SharedTexture(frame) => Some(frame),
        }
    }
}

/// 把 `aspect` 的矩形**内接**到 `width × height` 的框里，返回内接尺寸（不居中）。
///
/// 全库唯一的"按比例内接"公式：引擎用它算导出尺寸，Windows 控件用它算 D2D 摆放矩形
/// （`fit_rect` 只在这之上多了"居中"一步）。**别再就地写第三份** —— 两份就够漂了。
///
/// `aspect` 未知/非法、或任一边 ≤ 0 时整框返回（不做除法）。
pub(crate) fn fit_size(aspect: Option<f32>, width: f32, height: f32) -> (f32, f32) {
    let Some(aspect) = aspect.filter(|aspect| aspect.is_finite() && *aspect > 0.0) else {
        return (width, height);
    };
    if width <= 0.0 || height <= 0.0 {
        return (width, height);
    }
    // 框比视频宽 → 高度顶满、左右留边；否则宽度顶满、上下留边。
    if width / height > aspect {
        (height * aspect, height)
    } else {
        (width, width / aspect)
    }
}

/// 引擎出帧尺寸（物理像素，取整）。
pub(crate) fn export_size(aspect: Option<f32>, width: u32, height: u32) -> (u32, u32) {
    let (width, height) = fit_size(aspect, width as f32, height as f32);
    (
        (width.round() as u32).max(1),
        (height.round() as u32).max(1),
    )
}

/// 帧邮箱：1 格、最后写入者胜。
///
/// 不走 actor 的 State/Progress：那两条是 `watch`（通知所有观察者"变了"），而帧是
/// **取货** —— 消费者每帧来拉，不需要"状态广播"。
///
/// 但"有货了"这**一件事**要通知：[`FrameSlot::waker`]。Linux 上没人注册（GTK frame clock
/// 每帧自己来取）;Windows 上控件注册 `ComponentContext::redraw()`,于是发布一帧就重绘一帧
/// —— 没有它,`draw` 是按需调用的,画面就只能等下一次交互才更新。
pub(crate) struct FrameSlot {
    // 载荷是平台无关的 `FrameImage`：Linux 装 dma-buf，其它平台的变体等各自接上。
    texture: Mutex<Option<Arc<FrameImage>>>,
    /// 每写一次 +1。渲染层靠它跳过"同一帧的重复建纹理"。
    generation: AtomicU64,
    /// 有新帧时叫醒宿主（Windows 的控件在这里注册 `ComponentContext::redraw`）。
    ///
    /// Linux 不需要：GTK frame clock 每帧自己敲门，控件按 tick 拉就行。
    /// Windows 的 `draw` 是**按需重绘**的，没有 frame clock —— 控件自己续帧只能二选一：
    /// 空转（实测把整窗重绘推到 4400/s）或者断链。所以"发布了新帧"必须由**发布方**推给宿主。
    waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
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
            waker: Mutex::new(None),
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
    pub(crate) fn publish(&self, frame: Arc<FrameImage>) {
        {
            let mut g = self.lock();
            *g = Some(frame);
            self.generation.fetch_add(1, Ordering::Release);
        }
        self.wake_host();
    }

    /// 两侧都能调：open / stop 时作废当前帧
    pub(crate) fn clear(&self) {
        {
            let mut g = self.lock();
            *g = None;
            self.generation.fetch_add(1, Ordering::Release);
        }
        // 画面被作废了（open / stop）：也认宿主重绘一次，把上一张撤掉。
        self.wake_host();
    }

    /// 注册"有新帧时叫醒宿主"的回调（只有 Windows 的控件注册；Linux 走 GTK frame clock）。
    #[cfg(target_os = "windows")]
    pub(crate) fn set_waker(&self, waker: Arc<dyn Fn() + Send + Sync>) {
        *self.waker.lock().unwrap_or_else(|p| p.into_inner()) = Some(waker);
    }

    /// 摘掉叫醒回调（控件卸载时）。不摘的话回调会一直攥着宿主的 `ComponentContext`，
    /// 而播放句柄可能比视图活得久。
    #[cfg(target_os = "windows")]
    pub(crate) fn clear_waker(&self) {
        *self.waker.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// 叫醒宿主。**不在持锁时调**：回调会进宿主的 UI 线程，别和数据锁绞在一起。
    ///
    /// 这个方法**两平台都编译**：`publish` / `clear` 都会调它，没插头时是个空操作
    /// （Linux 永远没插头 —— `set_waker` 那对方法是 Windows 专属的）。
    fn wake_host(&self) {
        let waker = self.waker.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(waker) = waker {
            waker();
        }
    }

    /// 现在槽里有没有帧（零成本，不 clone）。
    pub(crate) fn has_frame(&self) -> bool {
        self.lock().is_some()
    }

    /// 渲染层：一次拿到「代号 + 帧」
    pub(crate) fn current(&self) -> (u64, Option<Arc<FrameImage>>) {
        let g = self.lock();
        (self.generation.load(Ordering::Acquire), g.clone())
    }

    fn lock(&self) -> MutexGuard<'_, Option<Arc<FrameImage>>> {
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
    pub(crate) engine: Arc<EngineGuard>,
    frames: Arc<FrameSlot>,
    request: Arc<Request>,
}

impl Slot {
    /// 它的号（控件绑定显示目标用）。
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn new(
        engine: Arc<EngineGuard>,
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
    /// 只有逃生舱（`ErikaPlayer::video_source`）用它 —— 关掉 feature 时就没人构造了。
    #[cfg(feature = "escape-hatch")]
    pub(crate) fn new(slot: Arc<Slot>) -> Self {
        Self { slot }
    }

    /// 这个帧源的号：控件用它绑定显示目标（`ErikaVideo` 内部就是这个）。
    ///
    /// 只有**自己手写控件**时才用得上（离屏渲染 / 自建原生视图）：
    /// `native(COMPONENT_NAME).prop("source", source.id())`。
    #[cfg(feature = "escape-hatch")]
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

    /// 注册"有新帧时叫醒宿主"的回调。
    ///
    /// Windows 的控件在拿到 `source` 时注册（`ComponentContext` 是 `Send + Sync`，可以从
    /// 引擎线程调 `redraw()`）；Linux 的控件不注册 —— 它有 GTK frame clock。
    #[cfg(target_os = "windows")]
    pub(crate) fn set_host_waker(&self, waker: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.slot.frames.set_waker(waker);
    }

    /// 摘掉叫醒回调（控件卸载 / 换绑时）。
    #[cfg(target_os = "windows")]
    pub(crate) fn clear_host_waker(&self) {
        self.slot.frames.clear_waker();
    }

    /// 现在有没有画面可画（**零成本、非阻塞**）。
    pub fn has_frame(&self) -> bool {
        self.slot.frames.has_frame()
    }

    /// 一次拿到「代号 + 帧」。代号没变 = 没有新帧。
    ///
    /// 渲染层每帧调它（`erika.video` 控件的 `FrameBinder`），出帧测试也用它 ——
    /// 这是 `FrameSlot` 唯一的读口，控件以外没人能验证"像素真的出来了"。
    pub fn frame_snapshot(&self) -> (u64, Option<Arc<FrameImage>>) {
        self.slot.frames.current()
    }
}

impl PartialEq for ErikaVideoSource {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.slot, &other.slot) // 同一个帧源
    }
}
impl Eq for ErikaVideoSource {}

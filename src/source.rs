//! 帧源：只负责"把最新一帧交给渲染层"。
//!
//! 这是 [`crate::ErikaPlayer`] 的一个视图：同一个引擎、同一份帧槽，但**只能**拿到
//! 与渲染有关的几个方法（`request_frame` / `frame_snapshot` / `has_frame`），拿不到
//! 播放控制，也拿不到状态。和 media_kit 的 `VideoController` 一个角色。
//!
//! **只有一条出帧路径**：erika 导出的线性 dma-buf。没有 CPU 回读兜底 —— 宿主
//! （GTK/GSK）导不进来就没画面（见 `dev_doc/nsh-erika-status.md` §8）。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::cmd::{ErikaCmd, RenderCmd};

/// 帧邮箱：1 格、最后写入者胜。**不通知** —— 渲染层每帧主动拉。
///
/// 为什么不走 actor 的 State/Progress：那两条是 `watch`（通知所有观察者"变了"），
/// 而帧是**取货**——消费者每帧来拉，从不需要通知。
pub(crate) struct FrameSlot {
    texture: Mutex<Option<Arc<erika::DmabufFrame>>>,
    /// 每写一次 +1。渲染层靠它跳过"同一帧的重复建纹理"。
    generation: AtomicU64,
}

impl FrameSlot {
    pub(crate) fn new() -> Self {
        Self {
            texture: Mutex::new(None),
            generation: AtomicU64::new(0),
        }
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
/// `canvas` 是控件自己的物理尺寸（`gtk::Picture` 的宽高 × `scale_factor`）；`export`
/// 是引擎真正出帧的尺寸 —— 视频宽高比在画布里的**内接矩形**（见
/// `component.rs::export_size`）。有 letterbox 时两者不同：竖版控件里放 16:9 的片子，
/// 导出尺寸比画布小一半以上。`ContentFit::Contain` 负责把这张图摆回画布中间。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameRequest {
    /// 画布（控件）的物理像素尺寸。
    pub canvas: (u32, u32),
    /// 引擎出帧尺寸（画布内接视频比例后的尺寸）。
    pub export: (u32, u32),
    /// DPI 缩放：物理像素 / 逻辑像素。
    pub scale: f64,
}

impl Default for FrameRequest {
    fn default() -> Self {
        Self {
            canvas: (0, 0),
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

/// 帧源：交给 `erika.video` 控件的东西。
#[derive(Clone)]
pub struct ErikaVideoSource {
    /// 私有 —— 只用来发 Wake，不对外暴露任何控制能力。
    /// 和句柄共享同一个 [`EngineGuard`](crate::player::EngineGuard)：
    /// **最后一个 drop 的**负责等引擎线程退干净。
    engine: Arc<crate::player::EngineGuard>,
    frames: Arc<FrameSlot>,
    request: Arc<Request>,
}

impl ErikaVideoSource {
    pub(crate) fn new(
        engine: Arc<crate::player::EngineGuard>,
        frames: Arc<FrameSlot>,
        request: Arc<Request>,
    ) -> Self {
        Self {
            engine,
            frames,
            request,
        }
    }

    /// 视频原始宽高比（没打开 / 没有视频轨时为 `None`）。控件用它算内接矩形。
    pub(crate) fn video_aspect(&self) -> Option<f32> {
        let (width, height) = self.engine.handle.get_state().presenter.video_size?;
        (width > 0 && height > 0).then(|| width as f32 / height as f32)
    }

    /// **渲染层每帧调这个**，请求一帧（非阻塞，60Hz 也只会让引擎处理一次）。
    pub fn request_frame(&self, request: FrameRequest) {
        {
            let mut slot = self.request.frame();
            *slot = request;
        }
        if !self.request.want_frame.swap(true, Ordering::AcqRel) {
            let _ = self
                .engine
                .handle
                .send_cmd(ErikaCmd::Render(RenderCmd::Wake));
        }
    }

    /// 现在有没有画面可画（**零成本、非阻塞**）。
    pub fn has_frame(&self) -> bool {
        self.frames.has_frame()
    }

    /// 渲染层专用：一次拿到「代号 + 帧」。代号没变 = 没有新帧。
    pub(crate) fn frame_snapshot(&self) -> (u64, Option<Arc<erika::DmabufFrame>>) {
        self.frames.current()
    }
}

impl PartialEq for ErikaVideoSource {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.frames, &other.frames) // 同一个播放器的帧源 = 同一份帧槽
    }
}
impl Eq for ErikaVideoSource {}

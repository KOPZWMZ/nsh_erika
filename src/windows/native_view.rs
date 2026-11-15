//! `erika.video` 的画面控件（Windows）：把 erika 的 **D3D11 共享纹理**画进 NSH 的 D2D 上下文。
//!
//! 和 Linux 那份的区别是"接缝在哪"，不是"这个控件长什么样"：
//!
//! | | Linux（`linux/native_view.rs`） | Windows（本文件） |
//! |---|---|---|
//! | 宿主给什么 | 交一个 `gtk::Widget` 上去 | 每帧调 `draw(&ID2D1DeviceContext, size)` |
//! | 一帧从哪来 | erika 导出线性 dma-buf | erika 发布 `MISC_SHARED` 快照 |
//! | 怎么上屏 | `GdkDmabufTexture` 贴到 `gtk::Picture` | `OpenSharedResource` → `CreateBitmapFromDxgiSurface` → `DrawBitmap` |
//! | 谁驱动 | GTK frame clock 的 tick 回调 | **本文件的 `draw` 自己续帧**（`ctx.redraw()`）|
//!
//! ## 一帧的路径
//!
//! ```text
//! NSH 每帧调 draw(dc, size)
//!  ├ 从 dc 反查 NSH 的 D3D11 device（ID2D1Device2::GetDxgiDevice → GetParent）
//!  ├ 算物理像素尺寸 + 视频比例的内接矩形 → source.request_frame(..)   ← 要下一帧
//!  ├ frame_snapshot()：generation 变了才重建（QI → GetSharedHandle → OpenSharedResource
//!  │                                       → IDXGISurface → CreateBitmapFromDxgiSurface）
//!  ├ DrawBitmap 到控件中间（letterbox 由我们自己留边）
//!  └ ctx.redraw()  ← 有新画面才要下一帧，否则停在这里（暂停时不空转）
//! ```
//!
//! ## 为什么这么绕
//!
//! erika 的 d3d11 渲染器**没有** `export_frame_texture`（那是 Metal / Linux Vulkan 才有的
//! 通用导出接口），所以 Windows 只能借它给 Flutter 准备的那条出口。整条链是 GPU→GPU、
//! 不过 CPU；代价是上游为了交出一张"不可变 + GPU 完成态"的图，每帧多做一次
//! `CopyResource` + 一次 `wait_for_gpu`。细节与后续可选的上游改法见
//! dev_doc `nsh-erika-architecture.md` §5.7.1。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use nsh_core::JsonValue;
use nsh_platform_windows::{
    ComponentContext, ComponentFactory, NativeComponent,
    windows::{
        Win32::Graphics::{
            Direct2D::{
                Common::{D2D_RECT_F, D2D1_ALPHA_MODE_IGNORE, D2D1_PIXEL_FORMAT},
                D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_INTERPOLATION_MODE_LINEAR,
                ID2D1Bitmap1, ID2D1Device2, ID2D1DeviceContext,
            },
            Direct3D11::{ID3D11Device, ID3D11Texture2D},
            Dxgi::{
                Common::DXGI_FORMAT_B8G8R8A8_UNORM, IDXGIDevice, IDXGIDeviceSubObject,
                IDXGIResource, IDXGISurface,
            },
        },
        core::Interface,
    },
};

use crate::core::source::export_size;
use crate::{ErikaVideoSource, FrameImage, FrameRequest};

/// `request_frame` 的最小间隔：把"要帧"压在显示帧率的量级上。
///
/// 不加这道闸会**自激**：`draw → request_frame → Wake → 引擎出帧 → waker → draw → …`。
/// 视频 30fps 时这条链会被"没有新帧"自然打断；**一旦有弹幕在飘**，弹幕位置每 tick 都在动，
/// 于是每次 Wake 都产出一次发布 —— 实测 600 次/秒，等于把每帧的拷贝 + 同步放大 20 倍。
const REQUEST_EVERY: Duration = Duration::from_millis(16);

/// 多久没画上新画面，就允许越过 [`REQUEST_EVERY`] 再要一次。
///
/// 给"暂停时拖进度条"这类只有一次交互的场景兜底：那时不会再有 waker 回来叫我们。
const STARVE_AFTER: Duration = Duration::from_millis(200);

/// 注册用的工厂。
pub(crate) fn factory() -> ComponentFactory {
    Rc::new(|cx: ComponentContext| -> Box<dyn NativeComponent> { Box::new(ErikaView::new(cx)) })
}

/// erika 的画面控件（Windows）。
///
/// 只认一个 prop：`source`（要显示的帧源号）。控件不自建引擎、也不控制播放。
pub struct ErikaView {
    source: Option<ErikaVideoSource>,
    /// 从任意线程请求重绘用；每出一帧要一次。
    cx: ComponentContext,
    /// 当前贴着的纹理。设备/画面代号变了就整块重做（`draw` 拿的是 `&self`，所以要 RefCell）。
    cache: RefCell<Option<FrameCache>>,
    /// 已经报过"拿不到 NSH 的 D3D11 device"（只报一次，别每帧刷屏）。
    device_failed: Cell<bool>,
    /// 已经报过"打开共享纹理失败"。
    open_failed: Cell<bool>,
    /// "第一帧"日志只打一次。
    first_frame: Cell<bool>,
    /// `NSH_ERIKA_TRACE=1`：每秒一行 tick/paint/frames 统计（和 Linux 共用 `TickProbe`）。
    probe: RefCell<Option<crate::core::trace::TickProbe>>,
    /// 上一次 `request_frame` 的时刻（`None` = 还没要过，第一次总是要）。
    last_request: Cell<Option<Instant>>,
    /// 上一次真正画上新画面的时刻（判断"饿死"用）。
    last_paint: Cell<Option<Instant>>,
}

/// 一张已经能画的帧：打开的纹理 + 包在它上面的 D2D 位图。
struct FrameCache {
    /// 帧槽里的代号（`generation`），变了说明有新画面。
    generation: u64,
    /// 建这张位图用的 D2D 上下文。NSH 换 dc（设备重置）时它就会变，变了必须重建。
    dc: usize,
    /// 纹理本身要一起活着：位图是包在它的 DXGI surface 上的。
    _texture: ID3D11Texture2D,
    bitmap: ID2D1Bitmap1,
    /// 纹理尺寸（物理像素），用来算 letterbox。
    size: (u32, u32),
}

impl ErikaView {
    /// 建一个还没有帧源的控件（`set_prop("source", …)` 之后才开始出画面）。
    pub fn new(cx: ComponentContext) -> Self {
        Self {
            source: None,
            cx,
            cache: RefCell::new(None),
            device_failed: Cell::new(false),
            open_failed: Cell::new(false),
            first_frame: Cell::new(false),
            probe: RefCell::new(crate::core::trace::TickProbe::from_env()),
            last_request: Cell::new(None),
            last_paint: Cell::new(None),
        }
    }
}

impl Drop for ErikaView {
    fn drop(&mut self) {
        // 控件卸载：把"叫醒我"的回调摘掉，别让帧槽一直攥着宿主上下文。
        if let Some(source) = self.source.take() {
            source.clear_host_waker();
        }
    }
}

impl NativeComponent for ErikaView {
    /// 每帧被 NSH 调用：要下一帧、把已经到手的那帧画上去。
    fn draw(&self, dc: &ID2D1DeviceContext, size: (f32, f32)) {
        let Some(source) = self.source.as_ref() else {
            return;
        };
        // 播放句柄全部 drop 了：控件收工，别再要帧。
        if !source.is_alive() {
            return;
        }

        // NSH 的 dc 以逻辑点为单位；出帧要物理像素。
        let scale = dpi_scale(dc);
        let width = (size.0 as f64 * scale).round().max(1.0) as u32;
        let height = (size.1 as f64 * scale).round().max(1.0) as u32;

        // 和 Linux 同一套几何（`export_size`）：引擎只渲染视频比例的那块内接矩形。
        // Windows 上这个值还有第二个作用 —— 引擎线程拿它去 attach/resize 出图面
        // （见 `core/engine/render.rs::ensure_win_surface`），所以快照本身就是内接矩形大小，
        // 弹幕/字幕的排版视口也跟 Linux 的 `PlayRes` 一致。
        let aspect = source.video_aspect();
        let export = export_size(aspect, width, height);

        // 要帧节流：见 `REQUEST_EVERY` / `STARVE_AFTER` 的说明。
        let now = Instant::now();
        let starving = self
            .last_paint
            .get()
            .is_none_or(|painted| now.duration_since(painted) >= STARVE_AFTER);
        let due = self
            .last_request
            .get()
            .is_none_or(|asked| now.duration_since(asked) >= REQUEST_EVERY);
        if starving || due {
            self.last_request.set(Some(now));
            source.request_frame(FrameRequest { export, scale });
        }

        // 取帧：代号变了（或者换了 dc = 设备重置过）才重建位图 —— 建一次要 QI +
        // OpenSharedResource，不能每帧做。
        let (generation, image) = source.frame_snapshot();
        let mut cache = self.cache.borrow_mut();
        let stale = match cache.as_ref() {
            Some(current) => current.generation != generation || current.dc != dc.as_raw() as usize,
            None => image.is_some(),
        };
        let mut painted = false;
        if stale {
            *cache = None;
            if let Some(image) = image.as_deref() {
                *cache = self.rebuild(dc, image, generation);
                painted = cache.is_some();
                if painted {
                    self.last_paint.set(Some(Instant::now()));
                }
            }
        }

        // 摆位：视频比例内接控件、居中，多出来的留黑（和 Linux 的 ContentFit::Contain 一致）。
        // 窗口每次重绘都要重画这一帧（NSH 画的是整棵场景树），所以这里不是"新帧才画"。
        if let Some(current) = cache.as_ref() {
            let dest = fit_rect(size, current.size);
            // SAFETY: 调用 NSH 传进来的活上下文；位图与它建在同一个 device 上。
            unsafe {
                dc.DrawBitmap(
                    &current.bitmap,
                    Some(&dest),
                    1.0,
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    None,
                    None,
                );
            }
        }
        drop(cache);

        // 这里**不再自己续帧**。`draw` 是 NSH 按需调用的（Windows 上没有 frame clock），
        // 从里面 `redraw()` 就是"自己叫自己"，实测把整窗重绘推到 4400 次/秒、引擎空转
        // 4000+ tick/s；而不 `redraw()` 又会让链条断掉（暂停时连 seek 都刷不出来）。
        //
        // 正确分工：**谁产生新帧，谁叫醒宿主** —— 引擎发布时通过 `FrameSlot` 的 waker
        // 调一次 `ComponentContext::redraw()`（见 `source.rs::FrameSlot::wake_host`）。
        if let Some(probe) = self.probe.borrow_mut().as_mut()
            && probe.tick(Instant::now(), generation, painted)
        {
            probe.print(Instant::now());
        }
    }

    /// 唯一的 prop：`source`（要显示的帧源号）。
    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        if name != "source" {
            return;
        }
        let id = value.and_then(JsonValue::as_u64);
        // 换绑/解绑：旧位图立刻作废（它属于上一个帧源，甚至可能已经死了）。
        *self.cache.borrow_mut() = None;
        self.first_frame.set(false);
        self.open_failed.set(false);
        match id.and_then(ErikaVideoSource::resolve) {
            Some(source) => {
                // 有新帧就让引擎叫我们一声：`ComponentContext` 是 `Send + Sync` 的，
                // 引擎线程可以直接 `redraw()`（Windows 没有 frame clock，这是唯一不空转的驱动方式）。
                let cx = self.cx.clone();
                source.set_host_waker(std::sync::Arc::new(move || cx.redraw()));
                self.source = Some(source);
                // 立刻画一次 → `draw` 里 `request_frame` 发第一个 Wake，引擎才开始出帧。
                self.cx.redraw();
            }
            None => {
                self.source = None;
                if let Some(id) = id {
                    log::warn!("erika.video: source={id} 找不到帧源（句柄已经被 drop 了？）");
                }
            }
        }
    }
}

impl ErikaView {
    /// 把一帧共享纹理变成 D2D 位图。失败时返回 `None`（只报一次）。
    fn rebuild(
        &self,
        dc: &ID2D1DeviceContext,
        image: &FrameImage,
        generation: u64,
    ) -> Option<FrameCache> {
        let Some(shared) = image.as_shared_texture() else {
            self.fail_once(
                &self.open_failed,
                "帧载荷不是共享纹理（引擎侧还是旧路径？）",
            );
            return None;
        };
        let Some(device) = d3d_device(dc) else {
            self.fail_once(
                &self.device_failed,
                "从 D2D 上下文拿不到 D3D11 device —— 这个宿主的合成链路不支持外部纹理",
            );
            return None;
        };
        let resource: IDXGIResource = match shared.resource() {
            Some(resource) => resource,
            None => {
                self.fail_once(
                    &self.open_failed,
                    "共享纹理不支持 IDXGIResource（没有共享句柄）",
                );
                return None;
            }
        };
        // SAFETY: 句柄来自活着的快照纹理；`OpenSharedResource` 在同 adapter 的 device 上打开它。
        let texture = unsafe {
            let handle = match resource.GetSharedHandle() {
                Ok(handle) if !handle.is_invalid() => handle,
                Ok(_) => {
                    self.fail_once(&self.open_failed, "GetSharedHandle 返回空句柄");
                    return None;
                }
                Err(e) => {
                    self.fail_once(&self.open_failed, &format!("GetSharedHandle 失败: {e}"));
                    return None;
                }
            };
            let mut texture: Option<ID3D11Texture2D> = None;
            if let Err(e) = device.OpenSharedResource(handle, &mut texture) {
                self.fail_once(
                    &self.open_failed,
                    &format!("OpenSharedResource 失败（erika 与 NSH 是不是不在同一块 GPU？）: {e}"),
                );
                return None;
            }
            texture?
        };

        // SAFETY: 纹理是这张位图唯一的持有者（`_texture` 一起存着），D2D 只借用它的 surface。
        let bitmap = unsafe {
            let surface: IDXGISurface = match texture.cast() {
                Ok(surface) => surface,
                Err(e) => {
                    self.fail_once(&self.open_failed, &format!("纹理转 IDXGISurface 失败: {e}"));
                    return None;
                }
            };
            let properties = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            match dc.CreateBitmapFromDxgiSurface(&surface, Some(&properties)) {
                Ok(bitmap) => bitmap,
                Err(e) => {
                    self.fail_once(
                        &self.open_failed,
                        &format!("CreateBitmapFromDxgiSurface 失败: {e}"),
                    );
                    return None;
                }
            }
        };

        let size = unsafe { bitmap.GetPixelSize() };
        if !self.first_frame.replace(true) {
            log::info!(
                "erika.video: 第一帧共享纹理 {}x{}（{:#x}）",
                size.width,
                size.height,
                shared.raw() as usize
            );
        }
        Some(FrameCache {
            generation,
            dc: dc.as_raw() as usize,
            _texture: texture,
            bitmap,
            size: (size.width, size.height),
        })
    }

    /// 记一次失败：日志一次 + **上报引擎**。
    ///
    /// 上报很关键：引擎 latch 之后写进 `ErikaStatus::error`，调用者据此知道"这个控件不会出画了"。
    /// 不报的话（之前就是这样）调用者什么都看不到，而且我们会每帧默默重试 OpenSharedResource。
    fn fail_once(&self, flag: &Cell<bool>, message: &str) {
        if !flag.replace(true) {
            log::warn!("erika.video: {message}");
            if let Some(source) = self.source.as_ref() {
                source.report_video_unavailable(message.to_owned());
            }
        }
    }
}

/// 从 D2D 上下文反查 NSH 的 D3D11 device。
///
/// 两条路都试：`ID2D1Device2::GetDxgiDevice()`（Win10+ 的 D2D 设备都实现）优先，
/// 失败就退回"当前 target 的 DXGI surface → 它的 device"。都不通用就返回 `None`，
/// 控件只报一次、上报引擎，然后什么也不画（由调用者决定怎么办）。
fn d3d_device(dc: &ID2D1DeviceContext) -> Option<ID3D11Device> {
    // SAFETY: 都是对活的 COM 对象做查询，不转移所有权。
    unsafe {
        if let Ok(device) = dc.GetDevice()
            && let Ok(device2) = device.cast::<ID2D1Device2>()
            && let Ok(dxgi) = device2.GetDxgiDevice()
            && let Ok(d3d) = dxgi.GetParent::<ID3D11Device>()
        {
            log_adapter_once(&dxgi);
            return Some(d3d);
        }
        // 兜底：target 位图背后的 DXGI surface 也是同一个 device 的。
        if let Ok(target) = dc.GetTarget()
            && let Ok(bitmap) = target.cast::<ID2D1Bitmap1>()
            && let Ok(surface) = bitmap.GetSurface()
            && let Ok(sub_object) = surface.cast::<IDXGIDeviceSubObject>()
            && let Ok(d3d) = sub_object.GetDevice::<ID3D11Device>()
        {
            return Some(d3d);
        }
    }
    None
}

/// 打一次 NSH 那边的显卡信息 —— 真机排查"是不是同一块 GPU"时第一眼要看这个。
fn log_adapter_once(dxgi: &IDXGIDevice) {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: 查询活的 device 的 adapter，只读。
        unsafe {
            let Ok(adapter) = dxgi.GetAdapter() else {
                return;
            };
            let Ok(desc) = adapter.GetDesc() else {
                return;
            };
            let end = desc
                .Description
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(desc.Description.len());
            let name = String::from_utf16_lossy(&desc.Description[..end]);
            log::info!(
                "erika.video: NSH 的 D3D11 adapter = {name} (luid={:#x}:{:#x})",
                desc.AdapterLuid.HighPart as u32,
                desc.AdapterLuid.LowPart
            );
        }
    });
}

/// DPI 缩放：NSH 的 dc 是逻辑点，出帧要物理像素。
fn dpi_scale(dc: &ID2D1DeviceContext) -> f64 {
    let mut dpi_x = 96.0_f32;
    let mut dpi_y = 96.0_f32;
    // SAFETY: 只读查询。
    unsafe { dc.GetDpi(&mut dpi_x, &mut dpi_y) };
    if dpi_x.is_finite() && dpi_x > 0.0 {
        f64::from(dpi_x) / 96.0
    } else {
        1.0
    }
}

/// 视频在控件里的摆放矩形（D2D 矩形，单位**逻辑点**）：内接 + 居中，多出来的留黑。
fn fit_rect(size: (f32, f32), video: (u32, u32)) -> D2D_RECT_F {
    let aspect = (video.0 > 0 && video.1 > 0).then(|| video.0 as f32 / video.1 as f32);
    let (width, height) = crate::core::source::fit_size(aspect, size.0, size.1);
    let (left, top) = ((size.0 - width) / 2.0, (size.1 - height) / 2.0);
    D2D_RECT_F {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

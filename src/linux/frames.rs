//! 帧源 → `gtk::Picture`：tick 回调里请求帧、导入 dma-buf、贴上去。
//!
//! 这是唯一"每个 tick 都在跑"的地方（`ErikaView` 的贴图 + 诊断探针）。
//!
//! 出帧**只有一条路**：erika 导出线性 dma-buf，这里用 `GdkDmabufTexture` 导入。
//! 没有 CPU 回读兜底（见 dev_doc §8），导不进来就没有画面。

use std::cell::{Cell, RefCell};
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::time::Instant;

use gtk::prelude::*;
use nsh_platform_linux::gtk4 as gtk;

use crate::core::source::export_size;
use crate::{ErikaVideoSource, FrameImage, FrameRequest};

/// `DRM_FORMAT_ABGR8888`（内存里是 R,G,B,A，对应 erika 的 R8G8B8A8_UNORM）。
const FOURCC_ABGR8888: u32 = 0x3432_4241;

/// 把帧源绑在一张 `gtk::Picture` 上：绑上就开始每帧请帧、贴图；解绑就停。
pub(crate) struct FrameBinder {
    picture: gtk::Picture,
    /// 正在显示的帧源（`bind` 成功时才有）。
    source: Option<ErikaVideoSource>,
    /// 当前 tick 回调还该不该活着：换绑 / 解绑时先置 `false`，旧回调自己 `Break`。
    ticking: Rc<Cell<bool>>,
}

impl FrameBinder {
    pub(crate) fn new(picture: &gtk::Picture) -> Self {
        Self {
            picture: picture.clone(),
            source: None,
            ticking: Rc::new(Cell::new(false)),
        }
    }

    /// 绑一个帧源：装上 tick 回调，开始请帧。
    ///
    /// 换绑时旧回调靠 [`Self::ticking`] 自己 `Break`（`add_tick_callback` 没有撤销接口），
    /// 所以每次绑定都新建一套状态。
    pub(crate) fn bind(&mut self, source: ErikaVideoSource) {
        // 同一个帧源重复投递（比如 prop 被重新求值）时什么都不做：重建回调会白丢一帧状态。
        if self.ticking.get() && self.source.as_ref() == Some(&source) {
            return;
        }
        self.unbind();
        let ticking = Rc::new(Cell::new(true));
        self.ticking = ticking.clone();
        self.source = Some(source.clone());

        let ticks = Rc::new(Cell::new(0_u64));
        // 上一次贴上去的帧代号：没变就说明没有新帧，别重复建纹理。
        let painted = Rc::new(Cell::new(u64::MAX));
        let first = Rc::new(Cell::new(false));
        let trace = crate::core::trace::enabled();
        // 诊断：`NSH_ERIKA_TRACE=1` 时每秒一行 GTK tick 统计（实现在 `trace.rs`）。
        // `add_tick_callback` 收的是 `Fn`，所以要内部可变性。
        let tick_probe = Rc::new(RefCell::new(crate::core::trace::TickProbe::from_env()));
        // 基准用：`NSH_ERIKA_EXPORT=1280x720` 固定出帧尺寸，好和别的播放器同尺寸对比。
        let fixed_export = std::env::var("NSH_ERIKA_EXPORT").ok().and_then(|value| {
            let (w, h) = value.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        });
        // 导入失败之后**不再尝试**（没有兜底路径，重试只会每帧白建一次纹理）：
        // 置上就等于"这条路死了"，同时也就等于"只报一次"。
        let import_failed = Rc::new(Cell::new(false));

        self.picture.add_tick_callback(move |picture, _clock| {
            // 解绑了，或者播放句柄全被 drop 了：收工（回调在这里返回 `Break` 后，
            // GTK 会把它连同捕获的帧源一起释放）。
            if !ticking.get() || !source.is_alive() {
                return gtk::glib::ControlFlow::Break;
            }
            // 导入已经证明不可用：收工（引擎那边也已经知道，见 `report_video_unavailable`）。
            if import_failed.get() {
                return gtk::glib::ControlFlow::Break;
            }
            let scale = picture.scale_factor() as f64;
            let width = (picture.width() as f64 * scale).round().max(1.0) as u32;
            let height = (picture.height() as f64 * scale).round().max(1.0) as u32;
            // 按**视频宽高比**出帧：exports 是视频比例在控件里的内接矩形，不是整块控件。
            // 两个好处：
            //   1. 少合成/少搬运一大截像素（竖版控件里放 16:9 的片子能省一半以上）；
            //   2. erika 拿这个尺寸当 libass 的 `PlayRes`，所以字幕按它排版后不会再被
            //      aspect-fit 拉伸 —— 非全屏时字幕变扁那个 bug 顺带没了。
            let export =
                fixed_export.unwrap_or_else(|| export_size(source.video_aspect(), width, height));
            source.request_frame(FrameRequest {
                export,
                scale,
            });

            let (generation, image) = source.frame_snapshot();
            let count = ticks.get() + 1;
            ticks.set(count);
            // 诊断：`NSH_ERIKA_TRACE=1` 时每 2 秒打一行状态。
            if count % 120 == 1 && trace {
                log::info!(
                    "erika.video: tick {width}x{height} export={}x{} gen={generation} painted={} has_frame={} aspect={:?}",
                    export.0,
                    export.1,
                    painted.get(),
                    source.has_frame(),
                    source.video_aspect(),
                );
            }
            let mut swapped = false;
            if generation != painted.get() {
                match image {
                    Some(image) => match texture_for(&image) {
                        Some(texture) => {
                            swapped = true;
                            if !first.get() {
                                first.set(true);
                                if let Some(frame) = image.as_dmabuf(){
                                log::info!(
                                    "erika.video: 第一帧 dma-buf {}x{} stride={} offset={}",
                                    frame.width(),
                                    frame.height(),
                                    frame.stride(),
                                    frame.offset(),
                                );
                                }
                            }
                            picture.set_paintable(Some(&texture));
                        }
                        // 导入失败（GSK 后端接不了 dma-buf）：没有兜底路径。
                        // 记下来（下一帧直接 Break）并告诉引擎，由应用决定降级到内置 <video>。
                        None => {
                            if !import_failed.replace(true) {
                                log::warn!(
                                    "erika.video: dma-buf 导入失败，这个宿主没有可用的出帧路径"
                                );
                                source.report_video_unavailable(
                                    "GdkDmabufTexture 导入失败（GSK 后端接不了 dma-buf）".to_owned(),
                                );
                            }
                        }
                    },
                    // 代号前进但槽里没有帧 = 引擎 `FrameSlot::clear()` 过（`open` 换片 / `stop`）：
                    // 把画面撤掉。否则会停在上一帧，直到新片第一帧到达（可能几百 ms）。
                    //
                    // 暂停不会走到这里：那时引擎不 publish 也不 clear，代号压根不变。
                    None => {
                        picture.set_paintable(None::<&gtk::gdk::Texture>);
                        // 换片了，下一张就是新片的"第一帧"（日志再打一次，方便按片排查）。
                        first.set(false);
                    }
                }
                painted.set(generation);
            }
            // 诊断（`NSH_ERIKA_TRACE=1`，实现在 `trace.rs`）
            if let Some(probe) = tick_probe.borrow_mut().as_mut() {
                let now = Instant::now();
                if probe.tick(now, generation, swapped) {
                    probe.print(now);
                }
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    /// 解绑：旧回调下一帧自己退出，画面清空。
    pub(crate) fn unbind(&mut self) {
        self.ticking.set(false);
        self.ticking = Rc::new(Cell::new(false));
        self.source = None;
        self.picture.set_paintable(None::<&gtk::gdk::Texture>);
    }
}

/// 把引擎给的 dma-buf 帧变成 GTK 能贴的纹理。
fn texture_for(image: &FrameImage) -> Option<gtk::gdk::Texture> {
    // 交给 GDK 一份自己的 fd：它会在 release 回调里关掉，引擎那份不受影响。
    let frame = image.as_dmabuf()?;
    let owned = match frame.try_clone_fd() {
        Ok(fd) => fd,
        Err(error) => {
            log::warn!("erika.video: 复制 fd 失败: {error}");
            return None;
        }
    };
    let raw = owned.as_raw_fd();
    let built = unsafe {
        gtk::gdk::DmabufTextureBuilder::new()
            .set_width(frame.width())
            .set_height(frame.height())
            .set_fourcc(FOURCC_ABGR8888)
            .set_modifier(0)
            .set_n_planes(1)
            .set_stride(0, frame.stride())
            .set_offset(0, frame.offset())
            .set_fd(0, raw)
            .build_with_release_func(move || drop(owned))
    };
    match built {
        Ok(texture) => Some(texture),
        Err(error) => {
            log::trace!("erika.video: dmabuf 导入失败: {error}");
            None
        }
    }
}

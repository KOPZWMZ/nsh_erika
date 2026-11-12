//! 帧源 → `gtk::Picture`：tick 回调里请求帧、导入 dma-buf、贴上去。
//!
//! 这是唯一"每个 tick 都在跑"的地方（`ErikaView` 的贴图 + 诊断探针）。
//!
//! 出帧**只有一条路**：erika 导出线性 dma-buf，这里用 `GdkDmabufTexture` 导入。
//! 没有 CPU 回读兜底（见 dev_doc §8），导不进来就没有画面。

use std::cell::{Cell, RefCell};
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use nsh_platform_linux::gtk4 as gtk;

use crate::{ErikaVideoSource, FrameRequest};

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
        let trace = std::env::var_os("NSH_ERIKA_TRACE").is_some();
        // `NSH_ERIKA_TRACE=1` 时每秒一行 GTK tick 统计（见 `TickProbe`）。
        // `add_tick_callback` 收的是 `Fn`，所以要内部可变性。
        let tick_probe = Rc::new(RefCell::new(TickProbe::from_env()));
        // 基准用：`NSH_ERIKA_EXPORT=1280x720` 固定出帧尺寸，好和别的播放器同尺寸对比。
        let fixed_export = std::env::var("NSH_ERIKA_EXPORT").ok().and_then(|value| {
            let (w, h) = value.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        });
        // 导入失败只报一次（否则每帧一条）。
        let import_warned = Rc::new(Cell::new(false));

        self.picture.add_tick_callback(move |picture, _clock| {
            // 解绑了，或者播放句柄全被 drop 了：收工（回调在这里返回 `Break` 后，
            // GTK 会把它连同捕获的帧源一起释放）。
            if !ticking.get() || !source.is_alive() {
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
                canvas: (width, height),
                export,
                scale,
            });

            let (generation, frame) = source.frame_snapshot();
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
                if let Some(frame) = frame {
                    match texture_for(&frame) {
                        Some(texture) => {
                            swapped = true;
                            if !first.get() {
                                first.set(true);
                                log::info!(
                                    "erika.video: 第一帧 dma-buf {}x{} stride={} offset={}",
                                    frame.width(),
                                    frame.height(),
                                    frame.stride(),
                                    frame.offset(),
                                );
                            }
                            picture.set_paintable(Some(&texture));
                        }
                        // 导入失败（GSK 后端接不了 dma-buf）：只能不画，没有兜底路径。
                        None => {
                            if !import_warned.get() {
                                import_warned.set(true);
                                log::warn!("erika.video: dma-buf 导入失败，这个宿主没有可用的出帧路径");
                            }
                        }
                    }
                }
                painted.set(generation);
            }
            if let Some(probe) = tick_probe.borrow_mut().as_mut() {
                let now = Instant::now();
                if probe.tick(now, generation, swapped) {
                    // 每秒只读一次状态（`status()` 会克隆一份 ErikaStatus）：音频写入量、
                    // 设备环排队时长从这里取。
                    probe.print(now, &source.status());
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

/// 视频宽高比在控件里的内接矩形，单位是物理像素。
///
/// 宽高比还不知道时（轨道还没 probe 出来）先用整块控件，出第一帧之后再收窄。
fn export_size(aspect: Option<f32>, width: u32, height: u32) -> (u32, u32) {
    let Some(aspect) = aspect.filter(|aspect| aspect.is_finite() && *aspect > 0.0) else {
        return (width, height);
    };
    // 控件比视频宽 → 高度顶满、左右留边；否则宽度顶满、上下留边。
    if width as f32 / height as f32 > aspect {
        let width = ((height as f32) * aspect).round() as u32;
        (width.max(1), height)
    } else {
        let height = ((width as f32) / aspect).round() as u32;
        (width, height.max(1))
    }
}

/// 把引擎给的 dma-buf 帧变成 GTK 能贴的纹理。
fn texture_for(frame: &erika::DmabufFrame) -> Option<gtk::gdk::Texture> {
    // 交给 GDK 一份自己的 fd：它会在 release 回调里关掉，引擎那份不受影响。
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
/// `NSH_ERIKA_TRACE=1` 时的 GTK tick 统计：每秒一行，专门回答"GTK 到底掉没掉帧"。
///
/// ```text
/// [nsh-erika] gtk tick ≈60/s paint ≈60/s frames ≈60/s audio≈48.0k/s queued=118ms maxgap=18ms
/// [nsh-erika] gtk tick ≈1/s  paint ≈1/s  frames ≈1/s  audio≈0/s queued=?       maxgap=1000ms
/// ```
///
/// - `tick`：帧时钟回调次数/s —— 出帧频率的**唯一来源**；
/// - `paint`：真正换上新纹理的次数/s（每 tick 至多一次）；
/// - `audio`：写进音频设备的 PCM 帧数增量/s（`audio_written_frames`；44.1k/48k 正常，
///   `≈0/s` = 没声音）；
/// - `queued`：音频设备环里还排着多少毫秒（`audio_queued`；正常一百多毫秒，`?` = 设备
///   还没起来、`0ms` 或一直不涨 = 没人往环里写）；
/// - `frames`：引擎**发布到帧槽的帧数**增量/s（`FrameSlot::generation` 的增量，两条出帧
///   路径都算）。`frames > paint` = 引擎出了、控件没换上；`frames < tick` = 引擎跟不上。
///   三个数一起看就能定位断点：
///   - `frames < tick` → 引擎出帧没跟上（去看 `erika.video: 耗时 …`）；
///   - `frames ≈ tick > paint` → 引擎在出帧，但控件这边没换上（贴图/取帧间隙）；
///   - `tick` 自己掉了 → GTK/合成器限频，和上面两者无关。
/// - `maxgap`：这一秒里相邻两次 tick 的最大间隔，掉帧时明显大于 1/60s。
///
/// ⚠️ tick 彻底停了（最小化）时**每秒不会有任何输出** —— "这一行消失"本身就是结论。
/// ⚠️ 不要拿 `GdkFrameClock::fps()` 当基准：实测窗口被挡住、tick 掉到 1/s 时它照样报
/// 57.8 —— 那是 GTK 按 refresh interval 估的**显示器**刷新率，不是实际出帧率。
struct TickProbe {
    started: Instant,
    ticks: u32,
    paints: u32,
    last: Option<Instant>,
    max_gap: Duration,
    /// 上次打印时的帧槽 `generation`，用来算每秒发布帧数。
    last_generation: u64,
    /// 上次打印时的 `audio_written_frames`，用来算每秒写入的 PCM 帧数。
    last_audio: u64,
    /// 这一秒内帧槽 `generation` 前进了多少（= 引擎发布了几帧）。
    published: u64,
    /// 第一次打印还没有基线（否则会把"从开播到现在的总帧数"当成每秒帧数）。
    primed: bool,
}

impl TickProbe {
    fn from_env() -> Option<Self> {
        let on = std::env::var_os("NSH_ERIKA_TRACE")
            .is_some_and(|value| !value.is_empty() && value != "0");
        on.then(|| Self {
            started: Instant::now(),
            ticks: 0,
            paints: 0,
            last: None,
            max_gap: Duration::ZERO,
            last_generation: 0,
            last_audio: 0,
            published: 0,
            primed: false,
        })
    }

    /// 记一次 tick；返回 `true` 表示该打印了（由调用方补上状态再调 [`Self::print`]）。
    fn tick(&mut self, now: Instant, generation: u64, painted: bool) -> bool {
        self.ticks += 1;
        if !self.primed {
            self.last_generation = generation;
        } else {
            // 累加这一秒内的每一段增量（一次 tick 拿到的可能就是全部）。
            self.published += generation.saturating_sub(self.last_generation);
            self.last_generation = generation;
        }
        self.paints += u32::from(painted);
        if let Some(last) = self.last {
            self.max_gap = self.max_gap.max(now.saturating_duration_since(last));
        }
        self.last = Some(now);
        now.saturating_duration_since(self.started) >= Duration::from_secs(1)
    }

    /// 打印这一秒的统计并清零。
    fn print(&mut self, now: Instant, status: &crate::ErikaStatus) {
        let elapsed = now.saturating_duration_since(self.started);
        let per_second = |count: u64| count as f64 / elapsed.as_secs_f64();
        // 第一行只用来建基线（否则会把"开播以来的总数"当成每秒值）。
        let published = if self.primed { self.published } else { 0 };
        let audio = if self.primed {
            status
                .presenter
                .audio_written_frames
                .saturating_sub(self.last_audio)
        } else {
            0
        };
        let audio_rate = per_second(audio);
        let audio_text = if audio_rate >= 1000.0 {
            format!("{:.1}k/s", audio_rate / 1000.0)
        } else {
            format!("{:.0}/s", audio_rate)
        };
        let queued = status.presenter.audio_queued.map_or_else(
            || "?".to_string(),
            |value| format!("{:.0}ms", value.as_secs_f64() * 1000.0),
        );
        eprintln!(
            "[nsh-erika] gtk tick ≈{:.0}/s paint ≈{:.0}/s frames ≈{:.0}/s audio≈{audio_text} queued={queued} maxgap={:.0}ms",
            per_second(self.ticks.into()),
            per_second(self.paints.into()),
            per_second(published),
            self.max_gap.as_secs_f64() * 1000.0,
        );
        self.started = now;
        self.ticks = 0;
        self.paints = 0;
        self.max_gap = Duration::ZERO;
        self.published = 0;
        self.last_audio = status.presenter.audio_written_frames;
        self.primed = true;
    }
}

//! `erika.video`：erika 的画面在 NSH 里的原生控件。
//!
//! 它就是一个 `gtk::Picture`：帧由引擎线程导出成线性 dma-buf，tick 回调里导入成
//! `GdkDmabufTexture` 贴上去。**只有这一条路** —— 没有 CPU 回读兜底（见 dev_doc §8），
//! 宿主导不进来就没有画面。NSH 的其它节点照常叠在这个 Picture 上面（同一个 GSK 合成树）。
//!
//! ## prop
//!
//! | prop | 类型 | 含义 |
//! |---|---|---|
//! | `src` | string \| null | 媒体地址（本地路径 / `file://` / `http(s)://`）；空串或 null = 停播 |
//! | `paused` | bool | 暂停状态；默认 `false`（给 `src` 就播） |
//! | `danmaku` | bool | 弹幕开关；默认开 |

use std::cell::{Cell, RefCell};
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use nsh_core::JsonValue;
use nsh_platform_linux::{ComponentContext, ComponentFactory, NativeComponent, gtk4 as gtk};

use crate::{ErikaPlayer, ErikaVideoSource, FrameRequest};

/// `DRM_FORMAT_ABGR8888`（内存里是 R,G,B,A，对应 erika 的 R8G8B8A8_UNORM）。
const FOURCC_ABGR8888: u32 = 0x3432_4241;

/// 注册用的工厂。
pub(crate) fn factory() -> ComponentFactory {
    Rc::new(|cx: ComponentContext| -> Box<dyn NativeComponent> { Box::new(ErikaView::new(cx)) })
}

/// erika 的播放控件。
pub struct ErikaView {
    picture: gtk::Picture,
    /// 引擎起不来时（没有 Vulkan 等）为 `None`：控件照样挂上去，只是不出画面。
    engine: Option<(ErikaPlayer, ErikaVideoSource)>,
    paused: bool,
    danmaku: bool,
}

impl ErikaView {
    /// 建一个还没有媒体的控件。
    pub fn new(_cx: ComponentContext) -> Self {
        let picture = gtk::Picture::new();
        // 画面按控件尺寸等比缩放，多出来的留黑，不裁切。
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_hexpand(true);
        picture.set_vexpand(true);

        let engine = match ErikaPlayer::new() {
            Ok((player, video)) => {
                log::info!("erika.video: 引擎已启动");
                Some((player, video))
            }
            Err(error) => {
                log::error!("erika.video: 引擎起不来: {error}");
                None
            }
        };

        if let Some((player, video)) = engine.as_ref() {
            let video = video.clone();
            let player = player.clone();
            let ticks = Rc::new(Cell::new(0_u64));
            // 上一次贴上去的帧代号：没变就说明没有新帧，别重复建纹理。
            let painted = Rc::new(Cell::new(u64::MAX));
            let logged_first = Rc::new(Cell::new(false));
            let first = logged_first.clone();
            let trace = std::env::var_os("NSH_ERIKA_TRACE").is_some();
            // `NSH_ERIKA_TRACE=1` 时每秒一行 GTK tick 统计（见 `TickProbe`）。
            // `add_tick_callback` 收的是 `Fn`，所以要内部可变性。
            let tick_probe = Rc::new(RefCell::new(TickProbe::from_env()));
            // 基准用：`NSH_ERIKA_EXPORT=1280x720` 固定出帧尺寸，好和别的播放器同尺寸对比。
            let fixed_export = std::env::var("NSH_ERIKA_EXPORT")
                .ok()
                .and_then(|value| {
                    let (w, h) = value.split_once('x')?;
                    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
                });
            // 导入失败只报一次（否则每帧一条）。
            let import_warned = Rc::new(Cell::new(false));
            picture.add_tick_callback(move |picture, _clock| {
                let scale = picture.scale_factor() as f64;
                let width = (picture.width() as f64 * scale).round().max(1.0) as u32;
                let height = (picture.height() as f64 * scale).round().max(1.0) as u32;
                // 按**视频宽高比**出帧：exports 是视频比例在控件里的内接矩形，不是整块控件。
                // 两个好处：
                //   1. 少合成/少搬运一大截像素（竖版控件里放 16:9 的片子能省一半以上）；
                //   2. erika 拿这个尺寸当 libass 的 `PlayRes`，所以字幕按它排版后不会再被
                //      aspect-fit 拉伸 —— 非全屏时字幕变扁那个 bug 顺带没了。
                let export = fixed_export.unwrap_or_else(|| export_size(video.video_aspect(), width, height));
                video.request_frame(FrameRequest {
                    canvas: (width, height),
                    export,
                    scale,
                });

                let (generation, frame) = video.frame_snapshot();
                let count = ticks.get() + 1;
                ticks.set(count);
                // 诊断：`NSH_ERIKA_TRACE=1` 时每 2 秒打一行状态。
                if count % 120 == 1 && trace {
                    log::info!(
                        "erika.video: tick {width}x{height} export={}x{} gen={generation} painted={} has_frame={} state={:?} t={:.2}s playing={} err={:?}",
                        export.0,
                        export.1,
                        painted.get(),
                        video.has_frame(),
                        player.state(),
                        player.media_time().as_secs_f64(),
                        player.is_playing(),
                        player.last_error(),
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
                                    log::warn!(
                                        "erika.video: dma-buf 导入失败，这个宿主没有可用的出帧路径"
                                    );
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
                        probe.print(now, &player.status());
                    }
                }
                gtk::glib::ControlFlow::Continue
            });
        }

        Self {
            picture,
            engine,
            paused: false,
            danmaku: true,
        }
    }
}

impl NativeComponent for ErikaView {
    fn widget(&self) -> gtk::Widget {
        self.picture.clone().upcast()
    }

    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        match name {
            "src" => {
                let src = value.and_then(|v| v.as_str()).map(str::to_owned);
                let Some((player, _)) = self.engine.as_ref() else {
                    return;
                };
                match src.filter(|src| !src.is_empty()) {
                    Some(src) => {
                        player.open(erika::MediaRequest::new(src));
                        if !self.paused {
                            player.play();
                        }
                    }
                    // `null` / 空串：停播（媒体仍然开着，位置归 0）
                    None => player.stop(),
                }
            }
            "paused" => {
                self.paused = value.and_then(|v| v.as_bool()).unwrap_or(false);
                if let Some((player, _)) = self.engine.as_ref() {
                    // 没打开媒体时发 play/pause 会让 erika 报 "no media is open"，
                    // 那条会进 status.error，相当于自己制造噪声。
                    if player.has_media() {
                        if self.paused {
                            player.pause();
                        } else {
                            player.play();
                        }
                    }
                }
            }
            "danmaku" => {
                self.danmaku = value.and_then(|v| v.as_bool()).unwrap_or(true);
                if let Some((player, _)) = self.engine.as_ref() {
                    player.danmaku().set_enabled(self.danmaku);
                }
            }
            other => log::warn!("erika.video: 没有名为 `{other}` 的 prop"),
        }
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
        let queued = status
            .presenter
            .audio_queued
            .map_or_else(|| "?".to_string(), |value| {
                format!("{:.0}ms", value.as_secs_f64() * 1000.0)
            });
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

//! 内核:引擎线程 + `PresenterRuntime` 的**唯一 owner**。
//!
//! 按命令家族分文件(为什么要分族见 `src/cmd.rs`):
//!
//! | 文件 | 负责 |
//! |---|---|
//! | `engine.rs` | `ErikaEngine` 的字段 / `run` 循环 / `on_cmd` 分派 / 状态发布 |
//! | `playback.rs` | `open` / `stop` / `play` / `pause` / `seek` |
//! | `settings.rs` | 音量 / 倍速 / 音轨 / 字幕轨 |
//! | `danmaku.rs` | 弹幕开关 / 加轨 / 样式读改写 / 清空 |
//! | `render.rs` | 出帧握手(`Wake`)+ 出帧 / 没人要帧时的保活泵 |
//!
//! 同一个 `impl ErikaEngine` 拆在多个文件里(Rust 允许,只要模块路径一致):
//! **字段只有这一处声明**,子模块是 `engine` 的后代,按模块可见性可以直接读写。

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ErikaStatus;
use crate::core::cmd::ErikaCmd;
use crate::core::listener::ErikaEvent;
use crate::core::player::{
    DanmakuDensity, DanmakuFilter, DanmakuPaint, DanmakuPlacement, DanmakuText, SubtitleSnapshot,
};
use crate::core::source::{FrameSlot, Request};
use actor_lite::{ActorContext, ActorCore, CmdWait, ThreadActor};
use crossbeam_channel::{Receiver, TryRecvError};

use erika::presenter::{PresenterConfig, PresenterRuntime};
use erika::{Player, PlayerConfig, PlayerEvent, RendererBackendPreference};
mod danmaku;
mod playback;
mod render;
mod settings;
mod subtitle;

/// 没有命令时,引擎线程多久醒一次(排空 erika 事件 / 保活泵)。
///
/// 有命令进来会立刻唤醒,这个超时只负责"什么都不发生"时的那点轮询。
/// 保活泵([`KEEPALIVE_EVERY`])由 `run` 循环**每轮独立调用**,不依赖这个超时是否触发 ——
/// 但没人要帧时循环只能靠超时转起来,所以这里必须明显小于那个间隔。
const IDLE: Duration = Duration::from_millis(10);

/// 没人要帧时,引擎自己多久泵一次(音频 / 时钟 / 解码),**不发帧**。
///
/// `export_frame_dmabuf` / `render_tick` 同时就是音频泵 —— 完全没人泵,音频环就会饿。
/// 上游 `DEFAULT_AUDIO_LEAD_TIME` 是 120ms(音频领先时钟 120ms 就够),50ms 一次留了两倍余量。
const KEEPALIVE_EVERY: Duration = Duration::from_millis(50);

/// Windows 的泵间隔：**比 Linux 密得多**，因为 Windows 没有宿主 tick 来"要帧"。
///
/// Linux 上 GTK frame clock 每帧敲一次控件 tick → `request_frame` → 引擎被叫醒；
/// Windows 上控件是"按需重绘"的，引擎得自己按帧率泵：`render_tick` 在场景没变时
/// 几乎零成本（上游 `flutter_scene.matches` 直接返回），所以喷得密不亏。
#[cfg(target_os = "windows")]
const WINDOWS_PUMP_EVERY: Duration = Duration::from_millis(16);

/// Windows 的循环心跳。和泵同频 → 干净的 ~60Hz，而不是"10ms 醒来、16ms 才泵"的抖动。
#[cfg(target_os = "windows")]
const WINDOWS_IDLE: Duration = Duration::from_millis(16);

/// 本平台的心跳间隔（见 [`IDLE`] 与 [`WINDOWS_IDLE`]）。
fn idle_every() -> Duration {
    #[cfg(target_os = "windows")]
    {
        WINDOWS_IDLE
    }
    #[cfg(not(target_os = "windows"))]
    {
        IDLE
    }
}

/// 本平台"多久泵一次"（见上面两个常量的说明）。
fn pump_every() -> Duration {
    #[cfg(target_os = "windows")]
    {
        WINDOWS_PUMP_EVERY
    }
    #[cfg(not(target_os = "windows"))]
    {
        KEEPALIVE_EVERY
    }
}

pub(crate) struct ErikaEngine {
    /// 唯一 owner。!Send，所以整个 struct 只能活在 actor 线程里。
    presenter: PresenterRuntime,

    /// 帧邮箱（和句柄共享）：导出成功后写进去，渲染层每帧来拉
    frames: Arc<FrameSlot>,
    /// 渲染握手（和句柄共享）：要不要帧 / 画布尺寸 / DPI
    request: Arc<Request>,
    /// 最近一次错误：**只有引擎写**
    error: Option<String>,
    /// erika 的事件流。播放中 `PositionChanged` 每 2ms 一条,所以每 tick 排空并丢弃,
    /// 只把"离散事件"(状态/轨道/缓冲/错误/播完)当成"该通知 UI"的信号。
    player_events: Receiver<PlayerEvent>,
    /// 上一轮是否真的出了帧(只给 `NSH_ERIKA_TRACE` 探针看,不参与逻辑)
    last_rendered: bool,
    /// 上一次泵音频的时刻(出帧也算泵),节流 `KEEPALIVE_EVERY`
    last_keepalive: Instant,
    /// `NSH_ERIKA_TRACE=1` 时每秒打一行"引擎醒了多少次"(诊断用)
    tick_probe: Option<crate::core::trace::RateProbe>,
    /// 本机**没有可用的出帧路径**（导出失败或控件侧导入失败）。
    ///
    /// 一次置上、不再清除（换片也不清：这是机器能力，不是媒体状态，见 `playback.rs::open`）。
    /// 它同时**就是门卫**：置上之后 `pump_frame` 不再调导出，所以不会重复失败、
    /// 也就不需要额外的"已报告"标记。原因只进日志和 `error`，不留在字段里。
    video_unavailable: bool,
    /// 保活泵失败过。`render_tick` 会继续重试（可能只是一次瞬时错误），
    /// 所以这是**边沿**标记：失败记一次、成功就复位，日志不刷屏。
    keepalive_failed: bool,

    /// Windows：presenter 上当前挂着的出图面尺寸 `(宽, 高, DPI 缩放)`。
    ///
    /// Windows 没有"导出一帧"的接口，必须**先 attach 一个 `FlutterTexture` surface**，
    /// erika 才会渲染并发共享纹理快照。尺寸/DPI 变了走 `resize_surface`（只重建 targets），
    /// 不要重新 attach。
    #[cfg(target_os = "windows")]
    surface_metrics: Option<(u32, u32, f64)>,
    /// 诊断（`NSH_ERIKA_TRACE=1`）：每 60 次出帧一行各阶段耗时，实现在 `trace.rs`。
    timings_probe: Option<crate::core::trace::TimingsProbe>,
}
impl ErikaEngine {
    pub(crate) fn new(frames: Arc<FrameSlot>, request: Arc<Request>) -> Self {
        let presenter = new_presenter().unwrap_or_else(|e| panic!("{e}"));
        let player_events = presenter.player().subscribe();
        Self {
            presenter,
            frames,
            request,
            error: None,
            player_events,
            video_unavailable: false,
            keepalive_failed: false,
            timings_probe: crate::core::trace::TimingsProbe::from_env(),
            last_rendered: false,
            last_keepalive: Instant::now(),
            tick_probe: crate::core::trace::RateProbe::from_env("engine tick"),
            #[cfg(target_os = "windows")]
            surface_metrics: None,
        }
    }
    pub(crate) fn player_clone(&self) -> Player {
        self.presenter.player().clone()
    }

    /// 控制面:执行一条命令,返回"本轮要不要出帧"。
    ///
    /// 只有 [`RenderCmd::Wake`](crate::cmd::RenderCmd::Wake) 是 `true`。渲染家族**不在这里**
    /// 顺手发布状态 —— `Wake` 每帧都来,发布是白烧;其余各族处理完立刻发布一次。
    fn on_cmd(&mut self, cmd: ErikaCmd, ctx: &ActorContext<Self>) -> bool {
        match cmd {
            ErikaCmd::Render(c) => return self.on_render(c, ctx), // Wake → true
            ErikaCmd::Playback(c) => self.on_playback(c, ctx),
            ErikaCmd::Settings(c) => self.on_settings(c),
            ErikaCmd::Danmaku(c) => self.on_danmaku(c),
            ErikaCmd::Subtitle(c) => self.on_subtitle(c),
            ErikaCmd::Call(job) => job(&mut self.presenter), // 逃生舱:不代捕错误
            // 控件侧判断本机出不了帧：记下来（写进 `status.error`，由调用者决定），之后不再尝试出帧。
            // 错误写进 `self.error` 后由下面统一的 `publish` 带出去。
            ErikaCmd::VideoUnavailable(reason) => {
                if let Some(reason) = self.latch_video_unavailable(reason) {
                    self.error = Some(reason);
                }
            }
        }
        self.publish(ctx);

        false
    }
    /// 发一条控制命令；失败写进 `error`（没媒体时 erika 会自己报错，不用我们挡）。
    fn control(
        &mut self,
        what: &'static str,
        f: impl FnOnce(&mut PresenterRuntime) -> erika::Result<()>,
    ) {
        if let Err(e) = f(&mut self.presenter) {
            self.error = Some(format!("{what}: {e}"));
        }
    }

    /// 记下"本机出不了帧"。**第一次**返回要报出去的文案，之后返回 `None`（不重复报）。
    fn latch_video_unavailable(&mut self, reason: String) -> Option<String> {
        if self.video_unavailable {
            return None;
        }
        self.video_unavailable = true;
        let message = format!("出帧路径不可用，本机不再尝试: {reason}");
        log::warn!("nsh-erika: {message}");
        Some(message)
    }

    fn set_error(&mut self, ctx: &ActorContext<Self>, message: String) {
        self.error = Some(message);
        self.publish(ctx);
    }
    /// 组一份状态（只读 presenter + 自己的字段）
    fn current_status(&self) -> ErikaStatus {
        ErikaStatus {
            is_playing: self.presenter.is_playing(),
            duration: self.presenter.duration(),
            selection: self.presenter.track_selection(),
            // 字幕:样式整包 + 延迟(上游就这两块;font_scale / supersampling 没 getter,读不到)
            subtitle: SubtitleSnapshot::from_parts(
                self.presenter.subtitle_style(),
                self.presenter.subtitle_delay_micros(),
            ),
            volume: self.presenter.volume() as f32,
            muted: self.presenter.muted(),
            danmaku: {
                let config = self.presenter.danmaku_config().cloned().unwrap_or_default();
                // 样式分组:读侧与写侧共用同一批类型(`player/danmaku.rs` 的分组表)。
                crate::DanmakuSnapshot {
                    enabled: config.enabled,
                    tracks: self.presenter.danmaku_tracks(),
                    paint: DanmakuPaint::from(&config),
                    text: DanmakuText::from(&config),
                    placement: DanmakuPlacement::from(&config),
                    density: DanmakuDensity::from(&config),
                    filter: DanmakuFilter::from(&config),
                    config,
                }
            },
            error: self.error.clone(),
        }
    }
    /// 状态面:把状态发布到 actor 的 `watch`,并给 UI 发一条"变了"的通知。
    ///
    /// 只在**命令之后**和**erika 离散事件之后**被调用 —— 那两处都是真的发生了点什么，
    /// 所以不判等：`current_status()` 现在只是几个小 clone，比较一遍反而更贵。
    fn publish(&mut self, ctx: &ActorContext<Self>) {
        // 尺寸自愈：只在"还不知道"时才查一次 `tracks()`（一次 Vec 分配）。
        // `open` 时通常已经写上了；万一那时轨道还没 probe 完，这里补上。
        if self.frames.video_size().is_none() {
            self.frames.set_video_size(video_size_of(&self.presenter));
        }
        ctx.update_state(self.current_status());
        ctx.send_event(ErikaEvent::Changed);
    }

    /// 排空 erika 的事件流。返回"有没有值得通知 UI 的事"。
    ///
    /// `PositionChanged` 直接丢:进度用 `playback_snapshot()` 求值,不需要事件
    /// (播放中它每 2ms 一条,500 条/秒)。
    fn drain_player_events(&mut self) -> bool {
        let mut notify = false;
        loop {
            match self.player_events.try_recv() {
                Ok(PlayerEvent::PositionChanged(_)) => {}
                // erika 自己报的错(解码 / 音频 / 播放流程):文本必须留给 UI,
                // 否则用户只看到 `state() == Error`,看不到原因。
                Ok(PlayerEvent::Error(error)) => {
                    self.error = Some(format!("erika: {error}"));
                    notify = true;
                }
                Ok(_) => notify = true, // StateChanged / Ended / TracksChanged / …
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        notify
    }
}

impl ActorCore for ErikaEngine {
    type Cmd = ErikaCmd;
    type Event = ErikaEvent;
    type State = ErikaStatus;
    /// 不用 `Progress`:位置 / 进度这类高频量直读 `Player`(`playback_snapshot()`),
    /// `watch` 只留给"变了才值得通知"的状态。
    type Progress = ();

    fn init_state_and_progress(&self) -> (ErikaStatus, ()) {
        (self.current_status(), ())
    }
}

impl ThreadActor for ErikaEngine {
    fn run(mut self, mut ctx: ActorContext<Self>) {
        loop {
            // 命令 = 要不要出帧由它说;空闲超时也走一遍(没人要帧就什么都不做)
            let pump = match ctx.recv_cmd_timeout(idle_every()) {
                Ok(cmd) => self.on_cmd(cmd, &ctx),
                Err(CmdWait::Timeout) => true,
                Err(CmdWait::Closed) => break, // stop() 或所有句柄都没了
                _ => break,                    // CmdWait 是 non_exhaustive，留个兜底
            };

            if pump && let Some(reason) = self.pump_frame() {
                self.set_error(&ctx, reason);
            }
            // 保活：不管这一轮是被谁唤醒的，按自己的节流跑
            self.keepalive_pump();
            // 诊断（`NSH_ERIKA_TRACE=1`，实现在 `trace.rs`）
            if let Some(probe) = self.tick_probe.as_mut() {
                // "这一轮到底出帧了没有 / 有没有媒体" —— 卡顿时先看这两个。
                let (render, media) =
                    (self.last_rendered, self.presenter.player().has_open_media());
                probe.tick(|| format!("render={render} has_media={media}"));
            }
            // erika 的离散事件(状态/轨道/错误/播完):采样采不到,必须靠事件
            if self.drain_player_events() {
                self.publish(&ctx);
            }
        }
    }
}

/// 建一个 `PresenterRuntime`(引擎线程上调用)。
///
/// 它自带一个 wgpu 29 的 device(帧由它导出成 dma-buf,宿主一个 GPU 设备都不用建),
/// 建一次要几十毫秒 —— 不能在 UI 线程上做。
fn new_presenter() -> Result<PresenterRuntime, String> {
    #[cfg(target_os = "linux")]
    let renderer = RendererBackendPreference::WgpuFallback;
    #[cfg(target_os = "windows")]
    let renderer = RendererBackendPreference::PlatformNative;
    PresenterRuntime::new(PresenterConfig {
        player: PlayerConfig {
            renderer,
            ..PlayerConfig::default()
        },
        ..PresenterConfig::default()
    })
    .map_err(|error| format!("PresenterRuntime 创建失败: {error}"))
}
/// 视频轨的原始尺寸。没有视频轨 / 还没 probe 完就是 `None`。
fn video_size_of(presenter: &PresenterRuntime) -> Option<(u32, u32)> {
    presenter
        .player()
        .tracks()
        .into_iter()
        .find(|track| track.kind == erika::TrackKind::Video && track.width > 0)
        .map(|track| (track.width, track.height))
}

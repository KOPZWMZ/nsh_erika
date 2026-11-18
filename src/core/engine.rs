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

use crate::core::cmd::ErikaCmd;
use crate::core::listener::ErikaEvent;
use crate::core::player::{
    DanmakuDensity, DanmakuFilter, DanmakuPaint, DanmakuPlacement, DanmakuText, SubtitleSnapshot,
};
use crate::core::source::{FrameSlot, Request};
use crate::{ErikaProgress, ErikaStatus};
use actor_lite::{ActorContext, ActorCore, CmdWait, ThreadActor};
use crossbeam_channel::Receiver;

use erika::presenter::{PresenterConfig, PresenterRuntime};
use erika::{Player, PlayerConfig, PlayerEvent, RendererBackendPreference};
mod danmaku;
mod playback;
mod render;
mod settings;
mod subtitle;

/// 没有命令时,引擎线程多久醒一次(排空 erika 事件 / 保活泵 / 进度节拍)。
///
/// **这不是帧率参数。** 有命令进来会立刻唤醒(`send_cmd` 直接 `waker.wake()`):
/// Linux 上宿主帧时钟每帧调一次 `request_frame` → `Wake`,引擎当帧就醒;Windows 上出帧由
/// [`pump_every`] 自己驱动。这个超时只负责"什么都不发生"时的那点轮询。
///
/// 取 16ms(≈62.5Hz)的两个理由:
///
/// - **和 Windows 的泵同频** → 不会出现"16ms 才该泵、10ms 就醒"的抖动(Windows 没有宿主
///   tick 来要帧,全靠这条);
/// - **宁可过量采样**:场景没变时上游 `render_tick` 几乎零成本,而少采样会饿死显示
///   (16ms = 62.5Hz 略高于 60,17ms = 58.8Hz 就掉到 60 以下了)。
///
/// 空转一轮的代价是几个原子读 + 两次 `elapsed()`,微秒级 —— 别为它再调这个数。
const IDLE: Duration = Duration::from_millis(16);

/// 没人要帧时,引擎自己多久泵一次(音频 / 时钟 / 解码),**不发帧**。
///
/// `export_frame_dmabuf` / `render_tick` 同时就是音频泵 —— 完全没人泵,音频环就会饿。
/// 上游 `DEFAULT_AUDIO_LEAD_TIME` 是 120ms(音频领先时钟 120ms 就够),50ms 一次留了两倍余量。
const KEEPALIVE_EVERY: Duration = Duration::from_millis(50);

/// **Windows 的画面节奏**：引擎自泵、控件要帧闸（`windows/native_view.rs::REQUEST_EVERY`）、
/// 空闲心跳（[`IDLE`]）三处都取它，改一处等于同时改三处 —— 它们的值必须相等。
///
/// Linux 上**没有**这个概念：画面节奏由宿主帧时钟决定（GTK 每帧调 `request_frame`
/// → `Wake`），[`IDLE`] 只负责"什么都不发生时看一眼"。
///
/// 这个数取 16ms（≈62.5Hz）而不是 10ms 或 17ms：Windows 没有宿主 tick，全靠这条泵；
/// 略高于 60 好过略低于（17ms = 58.8Hz 会掉到 60 以下），而场景没变时上游
/// `render_tick` 几乎零成本（`flutter_scene.matches` 直接返回），过量采样不亏。
#[cfg(target_os = "windows")]
pub(crate) const FRAME_EVERY: Duration = Duration::from_millis(16);

/// 泵周期必须是心跳的**整数倍**，否则 [`IDLE`] 会把泵量化 ——
/// "心跳 10ms、该泵却在 16ms"的抖动就是这么来的。
///
/// 想改画面节奏只动了 [`FRAME_EVERY`] 时，这里会**编译期报错**，
/// 而不是等到 Windows 上实测发现"怎么只有 62.5Hz"。
#[cfg(target_os = "windows")]
const _: () = assert!(
    FRAME_EVERY.as_nanos() % IDLE.as_nanos() == 0,
    "Windows 画面节奏和 IDLE 不同步:改泵必须一起改心跳,否则泵会被量化"
);

/// 本平台"多久泵一次"：Windows 是**画面节奏**（[`FRAME_EVERY`]），其它平台只是音频保活
/// （[`KEEPALIVE_EVERY`]，不发帧）。
fn pump_every() -> Duration {
    #[cfg(target_os = "windows")]
    {
        FRAME_EVERY
    }
    #[cfg(not(target_os = "windows"))]
    {
        KEEPALIVE_EVERY
    }
}

/// 进度通道的节拍：**由引擎循环自己按墙钟数**，不依赖上游事件。
///
/// 循环本来就每隔 [`IDLE`]（16ms）醒一次，所以这个 100ms 只是
/// 一次 `elapsed()` 判断，不额外唤醒任何东西。和上游的 `PositionChanged` 解耦的好处是：
/// 进度通道的可用性不再取决于"erika 会不会发某个事件、多久发一条"。
const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// 一个**节拍**:记录"上一次是什么时候" + 自己的周期,只回答"到点没有"。
///
/// 它**不调度、不唤醒** —— 驱动者是引擎循环(`run`),循环本来就每隔
/// [`IDLE`](IDLE) 醒一次(16ms),到点判断只是一次 `elapsed()`
/// 的纯算术。所以:
///
/// - 节拍的**最小粒度 = [`IDLE`](IDLE)(16ms)**,想要比它更密的节奏(比如 200Hz 的某些仪器量),
///   这个类型帮不了你,那要换驱动方式;
/// - 每个不同频率各持**一个** `Tick`(现在是 `keepalive` 与 `progress`),它们共享同一次
///   循环唤醒,互不影响。
///
/// 为什么是 `due` + `mark` 两步,而不是"`due()` 返回 true 就自动重置":有的节拍必须在
/// **真干完活之后**才重置 —— `keepalive_pump` 在没媒体 / 没在播时会提前返回,那一次机会
/// 不能被吃掉,否则开播后要白等一个周期才泵第一下。反过来,"到期就 `mark` 再试发"也对
/// (进度通道就是:值没变由判等静默)。两种语义都留在调用方,这个类型只管记账。
#[derive(Clone, Copy)]
pub(super) struct Tick {
    every: Duration,
    last: Instant,
}

impl Tick {
    pub(super) fn new(every: Duration) -> Self {
        Self {
            every,
            last: Instant::now(),
        }
    }
    /// 到点没有。纯算术,**不重置**。
    pub(super) fn due(&self) -> bool {
        self.last.elapsed() >= self.every
    }
    /// 记一拍:这一轮已经把活干完了。
    pub(super) fn mark(&mut self) {
        self.last = Instant::now();
    }
}

pub(crate) struct ErikaEngine {
    /// 唯一 owner。!Send，所以整个 struct 只能活在 actor 线程里。
    presenter: PresenterRuntime,

    /// 帧邮箱（和句柄共享）：导出成功后写进去，渲染层每帧来拉
    frames: Arc<FrameSlot>,
    /// 渲染握手（和句柄共享）：要不要帧 / 画布尺寸 / DPI
    request: Arc<Request>,
    /// 最近一次错误：**只有 [`fail`](Self::fail) 写**
    error: Option<String>,
    /// erika 的事件流。播放中 `PositionChanged` 每 2ms 一条,所以每轮排空：
    /// 白名单里的离散事实（状态 / 缓冲 / 轨道 / 选轨 / 错误）翻译成事件转发，
    /// 其余（含 `PositionChanged`）直接丢。
    player_events: Receiver<PlayerEvent>,
    /// 上一轮是否真的出了帧(只给 `NSH_ERIKA_TRACE` 探针看,不参与逻辑)
    last_rendered: bool,
    /// 音频泵的节拍(出帧也算泵),`pump_every()`
    keepalive: Tick,
    /// 进度通道的节拍,`PROGRESS_EVERY`
    progress: Tick,
    /// 上一次真正发出去的进度快照。`update_progress` 和 `update_state` 一样是
    /// `send_replace`(**无条件通知**),所以这里得自己判等,否则每条命令都白唤醒一次。
    last_progress: ErikaProgress,
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
        // 初值 = 打开前的空进度(位置 0、时长未知)。它和 `init_state_and_progress` 给
        // watch 的那个一致,所以第一次 `publish` 不会白发一条。
        let last_progress = ErikaProgress::default();
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
            keepalive: Tick::new(pump_every()),
            progress: Tick::new(PROGRESS_EVERY),
            last_progress,
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
            ErikaCmd::Settings(c) => self.on_settings(c, ctx),
            ErikaCmd::Danmaku(c) => self.on_danmaku(c, ctx),
            ErikaCmd::Subtitle(c) => self.on_subtitle(c),
            ErikaCmd::Call(job) => job(&mut self.presenter), // 逃生舱:不代捕错误
            // 控件侧判断本机出不了帧：记下来（由 [`fail`](Self::fail) 同时写状态和事件），
            // 之后不再尝试出帧。
            ErikaCmd::VideoUnavailable(reason) => {
                if let Some(message) = self.latch_video_unavailable(reason) {
                    self.fail(ctx, message);
                }
            }
        }
        self.publish(ctx);

        false
    }
    /// 发一条控制命令；失败走 [`fail`](Self::fail)（没媒体时 erika 会自己报错，不用我们挡）。
    ///
    /// 要 `ctx` 不是为了出帧判定，而是因为失败要**同时**写状态和发事件 —— 见 [`fail`](Self::fail)。
    fn control(
        &mut self,
        ctx: &ActorContext<Self>,
        what: &'static str,
        f: impl FnOnce(&mut PresenterRuntime) -> erika::Result<()>,
    ) {
        if let Err(e) = f(&mut self.presenter) {
            self.fail(ctx, format!("{what}: {e}"));
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

    /// 失败的唯一出口：记忆 → 状态 → 事件，顺序固定、文案同一份。
    ///
    /// 三步的顺序不能换：`status.error` 必须早于事件，否则"事件说错了、状态还是旧的"。
    fn fail(&mut self, ctx: &ActorContext<Self>, message: String) {
        self.error = Some(message.clone());
        self.publish(ctx);
        ctx.send_event(ErikaEvent::Error(message));
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
    fn progress_snapshot(&self) -> ErikaProgress {
        ErikaProgress {
            position: self.presenter.media_time(),
            duration: self.presenter.duration(),
        }
    }

    /// 进度面:把**算好的**位置写进 actor 的 `watch`。
    ///
    /// 判等再写:`update_progress` 只是 `send_replace`,值没变也照样通知,
    /// 而这条路径会在**每条命令之后**跑一遍(音量、静音、弹幕样式都不动进度)。
    fn publish_progress(&mut self, ctx: &ActorContext<Self>) {
        let progress = self.progress_snapshot();
        if progress != self.last_progress {
            self.last_progress = progress.clone();
            ctx.update_progress(progress);
        }
    }
    /// 状态面:把状态发布到 actor 的 `watch`(不判等 —— `current_status()` 只是几个小 clone,
    /// 比较一遍反而更贵;代价是订阅者可能被"值没变"叫醒)。
    ///
    /// **只在命令处理路径上调用**(`on_cmd` 末尾、`open` 中途)，外加一个例外
    /// [`fail`](Self::fail) —— 上游报错时要把文案先写进状态，见那里的注释。
    /// erika 自己发来的其它离散事件**不经过这里** —— 那些事实走
    /// [`forward_player_events`] 的事件通道，要读值就直读 `player.*()`。
    fn publish(&mut self, ctx: &ActorContext<Self>) {
        // 尺寸自愈：只在"还不知道"时才查一次 `tracks()`（一次 Vec 分配）。
        // `open` 时通常已经写上了；万一那时轨道还没 probe 完，这里补上。
        if self.frames.video_size().is_none() {
            self.frames.set_video_size(video_size_of(&self.presenter));
        }
        ctx.update_state(self.current_status());
        // 进度必须在这里补一次:pause / seek / rate / open 都会换时钟基准,
        // 而这些变化之后 `PositionChanged` 不一定还会来(暂停就彻底不来了),
        // 少了这一句,进度条会一直停在旧位置。
        self.publish_progress(ctx);
    }

    /// 排空 erika 的事件流，把**白名单里的离散事实**翻译成 [`ErikaEvent`] 转发出去。
    ///
    /// 纯转发：这里不判断"该不该通知"，也不更新状态 —— 上游说什么就转什么，
    /// 只有 [`PlayerEvent::Error`] 是例外（错误文本是我们自己的字段，没有直读口，
    /// 所以要先写进 `status.error` 再发事件，走 [`fail`](Self::fail)）。
    ///
    /// **白名单外一律丢**，其中最重要的是 `PositionChanged`：播放中它每 2ms 一条
    /// （500 条/秒），是个**采样量**不是事实 —— 位置由进度通道
    /// （[`PROGRESS_EVERY`] 的节拍）去采。往这个分支里加任何逻辑都会把它带出去。
    fn forward_player_events(&mut self, ctx: &ActorContext<Self>) {
        loop {
            match self.player_events.try_recv() {
                Ok(PlayerEvent::StateChanged(state)) => {
                    ctx.send_event(ErikaEvent::StateChanged(state))
                }
                Ok(PlayerEvent::BufferingChanged(buffering)) => {
                    ctx.send_event(ErikaEvent::Buffering(buffering))
                }
                Ok(PlayerEvent::TracksChanged(_)) => ctx.send_event(ErikaEvent::TracksChanged), // 不带 Vec
                Ok(PlayerEvent::TrackSelectionChanged(selection)) => {
                    ctx.send_event(ErikaEvent::TrackSelectionChanged(selection))
                }
                // 唯一的例外：error 是我们自己的字段，没有直读口
                Ok(PlayerEvent::Error(error)) => self.fail(ctx, format!("erika: {error}")),

                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
}

impl ActorCore for ErikaEngine {
    type Cmd = ErikaCmd;
    type Event = ErikaEvent;
    type State = ErikaStatus;
    /// 进度走自己的 watch 通道:由 `PROGRESS_EVERY` 的节拍驱动(10Hz),
    /// 位置在引擎侧**算好**再发,读者拿到的是 `ErikaProgress { position, duration }`。
    type Progress = ErikaProgress;

    fn init_state_and_progress(&self) -> (ErikaStatus, ErikaProgress) {
        (self.current_status(), self.progress_snapshot())
    }
}

impl ThreadActor for ErikaEngine {
    fn run(mut self, mut ctx: ActorContext<Self>) {
        loop {
            // 命令 = 要不要出帧由它说;空闲超时也走一遍(没人要帧就什么都不做)
            let pump = match ctx.recv_cmd_timeout(IDLE) {
                Ok(cmd) => self.on_cmd(cmd, &ctx),
                Err(CmdWait::Timeout) => true,
                Err(CmdWait::Closed) => break, // stop() 或所有句柄都没了
                _ => break,                    // CmdWait 是 non_exhaustive，留个兜底
            };

            if pump && let Some(reason) = self.pump_frame() {
                self.fail(&ctx, reason);
            }
            // 保活：不管这一轮是被谁唤醒的，按自己的节流跑
            self.keepalive_pump();
            // 进度：同样按自己的节拍数墙钟，不依赖 erika 发什么事件。
            // 暂停 / 卡住时算出来的值不变，`publish_progress` 的判等会静默掉。
            if self.progress.due() {
                self.progress.mark();
                self.publish_progress(&ctx);
            }
            // 诊断（`NSH_ERIKA_TRACE=1`，实现在 `trace.rs`）
            if let Some(probe) = self.tick_probe.as_mut() {
                // "这一轮到底出帧了没有 / 有没有媒体" —— 卡顿时先看这两个。
                let (render, media) =
                    (self.last_rendered, self.presenter.player().has_open_media());
                probe.tick(|| format!("render={render} has_media={media}"));
            }
            self.forward_player_events(&ctx)
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

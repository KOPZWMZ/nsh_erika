//! 控制面:句柄 + 状态词汇表。
//!
//! 内核(`PresenterRuntime` 的唯一所有者、执行命令、按需出帧、发布状态)在
//! `engine/` 的 `ErikaEngine` 里,以 actor 的形式独占一条线程;命令词汇表在
//! `cmd.rs`;渲染握手件(帧邮箱 / 请求旗标)在 `source.rs`。本文件只放两样东西:
//!
//! 1. [`ErikaPlayer`]:句柄(`Clone + Send + Sync + Eq`),UI 唯一入口;
//! 2. [`ErikaStatus`]:presenter 侧的观测量(音量 / 统计 / 错误)。
//!
//! # 读 vs 写:**读直连 `Player`,写走队列**
//!
//! 句柄里同时握着 `erika::Player`(就是 presenter 内部那个的克隆)和一条命令通道。
//!
//! - **读**全部直连 `erika::Player`:零延迟、真推送、不用二次缓存 ——
//!   [`state`](ErikaPlayer::state) / [`intent_state`](ErikaPlayer::intent_state) /
//!   [`playback`](ErikaPlayer::playback) / [`tracks`](ErikaPlayer::tracks) 等;
//! - **写**几乎全部走队列,因为 `Player` 只管**解码侧**:暂停要 cork 音频设备、
//!   seek 要 flush 音频环、变速要让音频做 time-stretch —— 这三件事只有持有
//!   `audio_output` 的 presenter 做得出来,而它的协调方法全是 private。
//!   只有 `play` 和两个字幕文件方法在 presenter 那边也是纯转发,才允许直连。
//!
//! 为什么不用 `Deref` 把 `Player` 的方法全放出来:`Player` 上有 17 个写方法,
//! 漏遮一个就是"能编过、但音画不同步"。所以这里只逐个开只读方法,另外留一个
//! 显式的 [`raw`](ErikaPlayer::raw) 逃生口。
//!
//! # 错误语义(定稿规矩,新方法一律照抄)
//!
//! 1. **写命令 = 单向投递**,不阻塞、不返回 `Result`。失败写进
//!    [`status`](ErikaPlayer::status) 的 `error`,UI 靠状态订阅看到。
//!    理由:写命令要 presenter 参与(音频 cork / flush),同步等待会让 UI 线程跟着卡。
//! 2. **只有"读一个值"才允许回执**([`call_with`](ErikaPlayer::call_with)):典型是
//!    "在引擎线程上取一个 presenter 侧的数字",带 200ms 超时,不无限等、也不上 60fps 路径。
//! 3. **纯转发的直连写**(`add_external_subtitle` / `remove_subtitle_track`)直接返回
//!    上游的 `erika::Result` —— 它们本来就不需要 presenter 协调。
//! 4. **构造失败返回 `Err`**,不 panic;要不要 panic 由调用方决定。
//! 5. 任何失败都不许静默吞掉:要么进 `status().error`,要么进返回值。
//!
//! # 为什么必须有那条线程
//!
//! 1. `PresenterRuntime` 是 `!Send` —— 只能被**一个**线程拥有,不能搬走,
//!    也不能丢进 async task(那要求 future 是 `Send`);
//! 2. `open` / `seek` / `export_frame_dmabuf` 都是**同步阻塞**的:你那个 1.5 GB
//!    的 mkv 光 open 就要几百毫秒,`export` 里还有等 GPU 的 `poll`。
//!
//! 所以这条线程不是"播放需要线程"(解码 erika 自己在 `erika-playback` 线程里做),
//! 而是**"把阻塞调用关进笼子"**需要。UI 线程上只剩三件便宜事:
//! 发命令(`send_cmd`)、读状态(`get_state`)、取帧(`ErikaVideoSource::frame_snapshot`)。
//!
//! # 形状:media_kit
//!
//! ```ignore
//! let (player, video) = ErikaPlayer::new().expect("引擎起不来");  // 立刻返回,没有媒体
//! player.open(MediaRequest::new(url));      // 随时,可重复(换片)
//! player.play();                            // 播放要单独调,和 media_kit 一致
//! player.stop();                            // 停播:媒体还在,位置归 0
//! ```
//!
//! **没有**"幂等"包装:库不替你记 uri。想"只跑一次"就在控件/页面初始化时调一次,
//! 想"片源变了才打开"就把 `open` 挂在片源信号的变化上 ——
//! **别把 `open` 直接写在每次都会重跑的地方**(等于反复重开)。
//!
//! # 出帧:按需,不自驱
//!
//! 渲染层每帧调 [`ErikaVideoSource::request_frame`](crate::ErikaVideoSource::request_frame)。
//! **没有请求就不出帧** —— 不导出、不 capture、不建纹理,窗口被限频时不会空转烧 GPU,
//! 也不会和 UI 抢同一个队列到抖动。
//!
//! ⚠️ 但 `export_frame_dmabuf` / `render_tick` 同时是**音频消费泵**,所以"没人要帧"不能
//! 等于"完全不动":引擎会在没人要帧时按 `KEEPALIVE_EVERY`(50ms)调一次 `render_tick`
//! 保活(只泵音频/时钟,不发帧)。窗口被 GSK 限频到 1–2Hz 时就是靠它不断音 ——
//! 少了它,听感是"隔一会儿卡一下"。
//!
//! **别**把这个保活换成上游的 `audio_only_tick()`:它会挂起视频解码,恢复时是一次真
//! seek + flush 音频环,窗口可见性一抖就是连续爆音(dev_doc §7 有完整的日志链)。

use crate::cmd::{ErikaCmd, PlaybackCmd, SettingsCmd, SubtitleCmd};
use crate::engine::ErikaEngine;

use crate::source::{ErikaVideoSource, FrameSlot, Request};

use actor_lite::ActorHandle;

use erika::presenter::{PresenterRuntime, PresenterStats};
use erika::{ChapterInfo, MediaRequest, PlaybackSnapshot, PlayerState, TrackInfo, TrackSelection};

use std::sync::Arc;
use std::time::Duration;
mod danmaku;
mod subtitle;
mod tracks;

pub use danmaku::{
    BlockedModes, DanmakuDensity, DanmakuFilter, DanmakuPaint, DanmakuPlacement, DanmakuSnapshot,
    DanmakuText, ErikaDanmaku, FilterRule,
};
pub use subtitle::{ErikaSubtitle, SubtitleOverrides, SubtitlePlacement, SubtitleSnapshot, SubtitleTextStyle};
pub use tracks::ErikaTracks;
/// 等引擎线程退出的轮询间隔 / 上限。
///
/// 退出时要销毁的东西不少(Vulkan 设备、解码器、libass、字体),但都是"释放内存"级的工作,
/// 一秒够用;真要卡住就不等了(不能把宿主退出流程挂死)。
const SHUTDOWN_POLL: Duration = Duration::from_millis(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// 引擎线程的"最后一位守卫":**最后一个句柄 drop 时**,它负责停下来并等引擎退干净。
///
/// 没有它的话,`drop(player)` 只是关掉命令通道就立刻返回,引擎线程还在
/// `PresenterRuntime` 的析构里 free 一堆东西 —— 主线程这时候可能已经在跑
/// `main` 的收尾甚至进程退出,两者赛跑就是堆损坏。
pub(crate) struct EngineGuard {
    pub(crate) handle: ActorHandle<ErikaEngine>,
}

impl Drop for EngineGuard {
    fn drop(&mut self) {
        shutdown_engine(&self.handle);
    }
}

/// `stop()` 之后轮询到线程真的退出(或超时)。
fn shutdown_engine(handle: &ActorHandle<ErikaEngine>) {
    handle.stop();
    let deadline = std::time::Instant::now() + SHUTDOWN_TIMEOUT;
    while handle.is_alive() && std::time::Instant::now() < deadline {
        std::thread::sleep(SHUTDOWN_POLL);
    }
}

/// [`ErikaPlayer::call_with`] 等回执的上限。
///
/// 别改大:引擎可能正卡在 `open` / `seek` / `export` 里,等太久等于把 UI 一起拖住。
/// 拿不到就返回 `None`,由调用方决定要不要重试。
const CALL_WITH_TIMEOUT: Duration = Duration::from_millis(200);

/// presenter 侧的观测量 —— **`Player` 上读不到的那部分**。
///
/// 设备和渲染都在 presenter 手里,所以这些计数只能由引擎线程采样后发布过来
/// (actor 的 `watch`,非阻塞读)。只放真的有人在用的字段:原来整包搬
/// `PresenterRuntimeSnapshot`(40 个字段:弹幕 16、各阶段耗时 10、renderer 一堆),
/// 每次变化时走一次 `watch` + `PartialEq`,又胖又吵。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PresenterObservation {
    /// 解码/渲染计数。fps 探针用 `stats.rendered_video_frames`。
    pub stats: PresenterStats,
    /// 推给音频设备的帧数 —— "声音确实在走"的硬证据(headless 也能看,不依赖设备)。
    pub audio_written_frames: u64,
    /// 设备环里还排着的音频(真机稳态接近 0;headless 会顶在 cap 上)。
    pub audio_queued: Option<Duration>,
    /// 视频轨的原始尺寸 `(宽, 高)`;没有视频轨 / 还没打开就是 `None`。
    ///
    /// 控件用它算"视频比例在画布里的内接矩形"（`component.rs::export_size`），
    /// 出帧只覆盖那块矩形，UI 那边由 `ContentFit::Contain` 摆回画布中间。
    pub video_size: Option<(u32, u32)>,
    /// 渲染层最近一次请求的画布尺寸 `(物理宽, 物理高, DPI 缩放)`。
    ///
    /// 控件自己知道物理尺寸（`gtk::Picture` 宽高 × `scale_factor`），但**逻辑**尺寸和
    /// DPI 要一起看才知道：命中测试 / 把点击换算成"第几 %"时用得上。
    pub canvas: (u32, u32, f32),
}

/// 句柄对外暴露的**状态补充**:presenter 侧观测 + 音量 + 错误。
///
/// 生命周期 / 位置 / 时长 / 轨道 **不在这里** —— 那些直连 `Player`
/// ([`ErikaPlayer::state`] / [`ErikaPlayer::playback`] / [`ErikaPlayer::tracks`])。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ErikaStatus {
    /// 会话代次:**每次 `open`(换片)+1**,`stop` 不 +1。
    ///
    /// 做"增量/差值"的消费者(比如每秒算 fps 的探针)靠它判断基线要不要重置。
    pub session: u64,
    /// presenter 侧观测(Player 读不到的)。
    pub presenter: PresenterObservation,
    /// 当前音量(设备侧,`Player` 没有)。
    pub volume: f32,
    /// 是否静音。静音**不改** [`volume`](Self::volume):上游 `set_muted` 只把输出压到 0。
    pub muted: bool,
    /// 是否正在播。和 `Player::is_playing()` 同源,只是顺手发布出来,省得每次变化都
    /// 绕去读 `Player`。控件不读它(靠 tick 回调持续要帧),留给 UI 的播放/暂停图标用。
    pub playing: bool,
    /// 弹幕状态(开关 / 轨道 / 样式)。
    pub danmaku: danmaku::DanmakuSnapshot,
    /// 字幕状态(外观 / 版面 / 延迟)。
    pub subtitle: subtitle::SubtitleSnapshot,
    /// 最近一次错误(引擎写的:open 失败 / 协调命令失败 / 弹幕解析失败)。
    pub error: Option<String>,
}
/// 播放器句柄:**只管控制与状态**,碰不到帧(帧走 [`crate::ErikaVideoSource`])。
///
/// `Clone` 成本极低(两次引用计数 +1),可以随便传,也可以跨线程用。
/// 身份比较看 `engine` 的 `Arc`(同一个播放器 = 同一个 `Arc`),所以能直接拿来比 props
/// 变没变。
///
/// 放下最后一个句柄时命令通道关闭 → 引擎的 `recv_cmd_timeout` 返回 `Closed` →
/// `run` 退出(presenter 随之 Drop,音频设备、解码线程一起回收)。没有 `dispose`:
/// 想收掉引擎,就把所有句柄都 drop 掉。
pub struct ErikaPlayer {
    player: erika::Player,
    engine: Arc<EngineGuard>,
}

impl Clone for ErikaPlayer {
    fn clone(&self) -> Self {
        Self {
            player: self.player.clone(),
            engine: self.engine.clone(),
        }
    }
}
impl PartialEq for ErikaPlayer {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.engine, &other.engine)
    }
}
impl Eq for ErikaPlayer {}

impl ErikaPlayer {
    /// 建一个播放器:**立刻返回**,返回**控制句柄**和**帧源**两个视图(同一个引擎)。
    ///
    /// 引擎线程与 `PresenterRuntime` 的创建都在内部完成(几十毫秒,发生在引擎线程上)。
    /// 此时**没有媒体**。
    ///
    /// 失败(没有可用的 Vulkan 设备、线程起不来、启动卡住)返回 `Err(原因)`,
    /// **不 panic** —— 库不该带崩宿主 app,要不要 panic 由调用方决定
    /// (`erika.video` 控件只把原因打进日志,控件照样挂上去,只是不出画面)。
    pub fn new() -> Result<(Self, ErikaVideoSource), String> {
        let frames = Arc::new(FrameSlot::new());
        let request = Arc::new(Request::new());
        let (tx, rx) = std::sync::mpsc::channel();
        // `try_spawn_thread` 把"构造阶段 panic"变成可回传的错误
        // (没有 Vulkan 时 `ErikaEngine::new` 就是在这里炸的)。
        let handle = actor_lite::try_spawn_thread("erika-engine", {
            let frames = frames.clone();
            let request = request.clone();
            move || {
                let engine = ErikaEngine::new(frames, request);
                let player = engine.player_clone();
                let _ = tx.send(player);
                engine
            }
        })
        .map_err(|error| format!("引擎线程启动失败: {error}"))?;
        let player = rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| format!("引擎线程没能交出 Player: {error}"))?;
        let engine = Arc::new(EngineGuard { handle });
        let video = ErikaVideoSource::new(engine.clone(), frames, request);
        Ok((Self { player, engine }, video))
    }
    /// 立刻停掉引擎并**等它退完**(最多 `SHUTDOWN_TIMEOUT`)。
    ///
    /// 引擎线程上住着 `PresenterRuntime`(自带一套 wgpu 29 的 Vulkan 设备)、解码线程、
    /// libass 和字体。让它在进程收尾时还在 free 东西,就是和主线程赛跑 ——
    /// 实测会在退出时炸 `corrupted size vs. prev_size`。
    ///
    /// 所以宿主想在退出前先收干净就调它;不调也行,最后一个句柄 drop 时自动走同一套等待
    /// (控件的 `ErikaView` 就靠这条)。
    pub fn shutdown(&self) {
        shutdown_engine(&self.engine.handle);
    }
    /// 弹幕句柄：发弹幕命令 + 读弹幕快照。
    ///
    /// 每次调用返回一个新的句柄值（内部只做一次 Arc 加一），可以直接被 UI 闭包捕获。
    pub fn danmaku(&self) -> ErikaDanmaku {
        ErikaDanmaku::new(self.engine.clone())
    }

    /// 字幕句柄:样式(外观 / 版面)与延迟的读写。
    ///
    /// 每次调用返回一个新的句柄值(内部两次 Arc 加一),可以直接被 UI 闭包捕获。
    pub fn subtitle(&self) -> ErikaSubtitle {
        ErikaSubtitle::new(self.clone())
    }

    /// 发命令的唯一出口。
    ///
    /// 引擎已退出时这里静默失败——UI 层面由 [`status`](Self::status) 的存活判定
    /// 报告 `Error`(见那里的注释),不需要每个调用点自己处理。
    fn send(&self, cmd: ErikaCmd) {
        let _ = self.engine.handle.send_cmd(cmd);
    }

    // ── 三个薄 helper:抵消 `ErikaCmd::Playback(PlaybackCmd::Play)` 这种嵌套噪音,
    //    让下面所有调用点保持扁平的写法(拆类型 ≠ 拆通道,队列还是同一条)。

    /// 发一条生命周期 / 传输命令。
    fn send_playback(&self, cmd: PlaybackCmd) {
        self.send(ErikaCmd::Playback(cmd));
    }

    /// 发一条设置命令(音量 / 倍速)。
    fn send_settings(&self, cmd: SettingsCmd) {
        self.send(ErikaCmd::Settings(cmd));
    }

    /// 发一条字幕命令(样式 / 延迟)。`ErikaSubtitle` 用。
    pub(crate) fn send_subtitle(&self, cmd: SubtitleCmd) {
        self.send(ErikaCmd::Subtitle(cmd));
    }

    // ───────────── 生命周期 ─────────────

    /// 打开媒体。**立刻返回**,探测与解码器初始化在引擎线程上做。
    ///
    /// **总是发送命令**:同一个 uri 也会真的重新打开,失败之后也能重试
    /// (库里**没有**实现"幂等"的包装 —— 那需要句柄记一份 uri 缓存,得不偿失)。
    ///
    /// ⚠️ **别把它写在每次都会重跑的地方**:每次重开都是几百毫秒、进度归零。
    /// 只在片源真的变了时调一次(控件那边是按 `src` prop 变化才调)。
    ///
    /// 打开成功后状态是 [`PlayerState::Ready`],**不会自动播** ——
    /// 想要"打开即播"就跟一句 [`play`](Self::play)。
    ///
    /// 注意:`Opening` 是 erika 自己在 `PresenterRuntime::open` 里发的,而
    /// [`state`](Self::state) 直读 `Player` —— 所以命令发出后**立刻**就能看到 `Opening`。
    /// 但"上一部片的 `Ready`"和"这一部片的 `Ready`"长得一样,想确认"换完了"请看
    /// [`duration`](Self::duration) 或 [`generation`](Self::generation)。
    pub fn open(&self, request: MediaRequest) {
        self.send_playback(PlaybackCmd::Open(request));
    }

    /// 停播:位置归 0、画面清空,**媒体仍然开着**(还能 `play` / `seek` / 换片)。
    ///
    ///
    /// 和 erika 自己的 `close()` 的区别:那个是**终态**，我们不碰它。
    pub fn stop(&self) {
        self.send_playback(PlaybackCmd::Stop);
    }

    /// 播放(立即返回)。
    pub fn play(&self) {
        self.send_playback(PlaybackCmd::Play);
    }

    /// 暂停(立即返回)。
    pub fn pause(&self) {
        self.send_playback(PlaybackCmd::Pause);
    }

    /// 播放 / 暂停切换。看**意图**而不是看已提交状态(`play()` 是异步的)。
    pub fn toggle(&self) {
        if self.intent_state() == PlayerState::Playing {
            self.pause()
        } else {
            self.play()
        }
    }

    /// 跳转(立即返回,关键帧定位在引擎线程上完成)。
    pub fn seek(&self, pos: Duration) {
        self.send_playback(PlaybackCmd::Seek(pos));
    }

    // ───────────── 音频(音量 / 静音 / 倍速)─────────────
    //
    // 轨道与字幕的读写不在这个文件里:它们跨了直连读 / 队列写 / 阻塞直连三种通道,
    // 收在 `ErikaTracks` 句柄(见 `player/tracks.rs`)。

    /// 音量 0.0..=1.0。**立即返回**;`volume()` 在引擎处理完这条命令后就是新值。
    ///
    /// `Player` 上根本没有音量 —— 它住在 presenter 的音频设备上。
    pub fn set_volume(&self, volume: f64) {
        if !volume.is_finite() {
            return;
        }
        self.send_settings(SettingsCmd::Volume(volume.clamp(0.0, 1.0)));
    }

    /// 当前音量(0.0..=1.0)。读的是引擎发布的状态:`watch`,非阻塞,最多滞后一条命令。
    pub fn volume(&self) -> f64 {
        self.engine.handle.get_state().volume as f64
    }

    /// 静音开关。**不改变 [`volume`](Self::volume)**:上游 `set_muted` 只把输出压到 0,
    /// 记住的音量还在,取消静音就恢复原样。
    pub fn set_muted(&self, muted: bool) {
        self.send_settings(SettingsCmd::Muted(muted));
    }

    /// 当前是否静音(读引擎发布的状态,最多滞后一条命令)。
    pub fn is_muted(&self) -> bool {
        self.engine.handle.get_state().muted
    }

    /// 倍速。非法值直接丢弃(上游会把非有限 / `<= 0` normalize 成 1.0,放过去只会
    /// 让 UI 显示和实际不一致)。
    ///
    /// 变速是 **presenter 让音频环做 time-stretch**(变速不变调):直连 `Player` 的话
    /// 画面会变快而声音不变。
    pub fn set_rate(&self, rate: f64) {
        if !(rate.is_finite() && rate > 0.0) {
            return;
        }
        self.send_settings(SettingsCmd::Rate(rate));
    }

    /// 当前**生效**的倍速 —— 从 `PlaybackSnapshot::clock` 直读(播放中 2ms 更新、暂停 50ms)。
    /// 刚 `set_rate` 之后可能还是旧值:worker 提交后才变。
    pub fn rate(&self) -> f64 {
        self.playback().clock.rate()
    }

    /// 订阅"状态变了"的通知。
    ///
    /// 事件**不带数据**,只负责把 UI 叫醒 —— 醒来后调 [`status`](Self::status) 拉最新值。
    /// 这样只有一个真相(watch 里的状态),而 UI 不用轮询。
    ///
    /// 落后太多时会收到 `Lagged`:直接重拉一次状态即可(状态是 watch,不会丢)。
    pub fn subscribe(&self) -> crate::ErikaEvents {
        crate::ErikaEvents(self.engine.handle.get_events_rx())
    }

    /// 在引擎线程上对 `PresenterRuntime` 执行一段操作 —— **presenter 独有 API 的逃生口**。
    ///
    /// 常用能力已经有真方法了(播放控制 / 音量 / 倍速 / 轨道 / 字幕 / 弹幕),那些**优先用**
    /// —— 它们会做读改写、参数校验和音画协调。这里留给还没封的:超分、HDR headroom、
    /// 截图、调试 HUD……
    ///
    /// ```ignore
    /// player.call(|p| p.set_luma_upscaler(LumaUpscalerMode::Bilinear));
    /// player.call(|p| { let _ = p.capture_frame_rgba(1920, 1080); });
    /// ```
    ///
    /// ⚠️ 闭包跑在引擎线程上,**不能阻塞**(会把出帧一起卡住);返回值也拿不回来
    /// (命令是单向的)—— 需要返回值就借个 mpsc 自己捞。
    pub fn call(&self, job: impl FnOnce(&mut PresenterRuntime) + Send + 'static) {
        self.send(ErikaCmd::Call(Box::new(job)));
    }

    /// [`call`](Self::call) 的**带回执**版本:在引擎线程上执行,把返回值捞回来。
    ///
    /// - 200ms 内拿到 → `Some(值)`;
    /// - 超时 / 引擎已退出 → `None`(闭包可能随后才执行,值直接丢掉)。
    ///
    /// 只用于**低频**地"读一个 presenter 侧的值":统计、音量真值、某个上游 API 的成败。
    /// 有了它就不用再自己搭 `mpsc` + 超时(例子以前就是这么绕的)。
    ///
    /// ⚠️ **别放在 60fps 的路径上** —— 它是同步等待,会把 UI 线程拖住;
    /// 进度条那种高频读用 [`playback`](Self::playback)(直连 `Player`,不经过引擎线程)。
    ///
    /// ```ignore
    /// let frames = player.call_with(|p| p.runtime_snapshot().stats.rendered_video_frames);
    /// let ok = player.call_with(|p| p.set_playback_rate(2.0).is_ok());
    /// ```
    pub fn call_with<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut PresenterRuntime) -> T + Send + 'static,
    ) -> Option<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        // 复用单向逃生舱:把结果塞进 channel 就够了,不需要新的命令变体
        self.call(move |presenter| {
            let _ = tx.send(job(presenter));
        });
        rx.recv_timeout(CALL_WITH_TIMEOUT).ok()
    }
    /// 生命周期状态(erika 的 8 态:`Idle` / `Opening` / `Ready` / `Playing` /
    /// `Paused` / `Stopped` / `Closed` / `Error`)。
    pub fn state(&self) -> PlayerState {
        self.player.state()
    }
    /// **播放意图**:包含已排队、但 worker 还没提交的 `Play`。
    /// 按钮标签用它；用 [`state`](Self::state) 会滞后一次提交(点了播放还显示"已暂停")。
    pub fn intent_state(&self) -> PlayerState {
        self.player.playback_intent_state()
    }

    /// 一次锁拿到「时钟 + generation + state」。进度条每帧调这个,
    /// 再用 `snapshot.media_time()` 求值 —— 插值是上游算好的,不用自己写。
    pub fn playback(&self) -> PlaybackSnapshot {
        self.player.playback_snapshot()
    }

    /// 当前位置(采样值)。要 60fps 平滑请用 [`playback`](Self::playback)。
    pub fn media_time(&self) -> Duration {
        self.player.current_media_time()
    }

    /// 总时长。**`stop()` 之后不会变 `None`**(媒体还开着)——
    /// 判断"有没有视频"请用 [`state`](Self::state) 或 [`has_media`](Self::has_media)。
    pub fn duration(&self) -> Option<Duration> {
        self.player.duration()
    }

    /// 有没有媒体(打开中 / 暂停 / 停播都算有)。
    pub fn has_media(&self) -> bool {
        self.player.has_open_media()
    }

    /// 在不在播。
    pub fn is_playing(&self) -> bool {
        self.player.playback_snapshot().is_playing()
    }

    /// 流代次:`open`/`seek`/`stop` 都 +1。用来作废在途帧,**不是**会话代次。
    pub fn generation(&self) -> u64 {
        self.player.playback_generation()
    }

    /// 轨道句柄:媒体轨道(音轨 / 字幕轨)的读与选择 + 外部字幕的挂 / 摘。
    ///
    /// 每次调用返回一个新的句柄值(内部两次 Arc 加一),可以直接被 UI 闭包捕获。
    /// 列表是 [`ErikaTracks::list`],当前选择是 [`ErikaTracks::selection`] ——
    /// 视频轨不可选(上游没有 `select_video_track`)。
    pub fn tracks(&self) -> ErikaTracks {
        ErikaTracks::new(self.clone())
    }

    /// 轨道列表(直连 `Player`,零延迟)。给 [`ErikaTracks::list`] 用。
    pub(crate) fn media_tracks(&self) -> Vec<TrackInfo> {
        self.player.tracks()
    }

    /// 当前轨道选择(直连,零延迟)。给 [`ErikaTracks::selection`] 用。
    pub(crate) fn media_track_selection(&self) -> TrackSelection {
        self.player.track_selection()
    }

    /// 章节列表(打开时读出来的;没有就是空)。UI 做章节跳转用。
    pub fn chapters(&self) -> Vec<ChapterInfo> {
        self.player.chapters()
    }

    /// **裸访问逃生口**(opt-in)。
    ///
    /// ⚠️ 从这里拿到的**写方法全部绕过 presenter 的音频协调**,会音画不同步
    /// (实测:倍速 2x 声音不变快、seek(0) 声音不回去)。只用来读我们还没封装的东西。
    pub fn raw(&self) -> &erika::Player {
        &self.player
    }
    /// 读 presenter 侧的状态(音量 / 观测量 / 错误)。
    ///
    /// 来源是 actor 的 `State`(引擎在**状态真的变化时**才发布:每条命令之后、
    /// 收到 erika 的离散事件时、以及 1s 兜底采样),
    /// 非阻塞 `watch` 读。生命周期 / 位置 / 时长 / 轨道**不从这里拿**。
    ///
    /// 引擎线程已经退出时,这里会补一条 `error`(`"engine thread is gone"`),
    /// 免得 UI 以为一切正常。
    pub fn status(&self) -> ErikaStatus {
        let mut st = self.engine.handle.get_state();
        if !self.engine.handle.is_alive() {
            if st.error.is_none() {
                st.error = Some("engine thread is gone".into());
            }
        }
        st
    }

    /// 最近一次错误([`status`](Self::status) 里那个字段的便捷读法)。
    pub fn last_error(&self) -> Option<String> {
        self.status().error
    }
}

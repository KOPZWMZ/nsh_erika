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
//!   [`progress`](ErikaPlayer::progress) / [`tracks`](ErikaPlayer::tracks) 等;
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
//! 发命令(`send_cmd`)、读状态(`get_state`)、取帧(`ViewSource::frame_snapshot`)。
//!
//! # 形状:media_kit
//!
//! ```ignore
//! let player = ErikaPlayer::new().expect("引擎起不来");  // 立刻返回,没有媒体
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
//! 渲染层每帧调 [`ViewSource::request_frame`](crate::video::ViewSource::request_frame)。
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

use crate::core::cmd::{ErikaCmd, PlaybackCmd, SettingsCmd, SubtitleCmd};

use crate::core::engine::ErikaEngine;
use crate::core::session::ErikaSession;

#[cfg(feature = "escape-hatch")]
use crate::core::source::ViewSource;

use crate::events::{ErikaEventsListener, ErikaProgressListener, ErikaStatusListener};

use actor_lite::ActorHandle;
#[cfg(feature = "escape-hatch")]
use erika::presenter::PresenterRuntime;
use erika::{ChapterInfo, MediaRequest, Player, PlayerState, TrackInfo, TrackSelection};
use nsh_core::element::{JsHost, JsIn, JsOwned, JsValue, PropError, TsTypes};

use std::sync::{Arc, Mutex};
use std::time::Duration;
mod danmaku;
mod subtitle;
mod tracks;

pub use danmaku::{
    BlockedModes, DanmakuDensity, DanmakuFilter, DanmakuPaint, DanmakuPlacement, DanmakuSnapshot,
    DanmakuText, ErikaDanmaku, FilterRule,
};
pub use subtitle::{
    ErikaSubtitle, SubtitleOverrides, SubtitlePlacement, SubtitleSnapshot, SubtitleTextStyle,
};
pub use tracks::ErikaTracks;

/// [`ErikaPlayer::call_with`] 等回执的上限。
///
/// 别改大:引擎可能正卡在 `open` / `seek` / `export` 里,等太久等于把 UI 一起拖住。
/// 拿不到就返回 `None`,由调用方决定要不要重试。
#[cfg(feature = "escape-hatch")]
const CALL_WITH_TIMEOUT: Duration = Duration::from_millis(200);

/// 引擎视角的**状态快照**：presenter 侧那一半真相 + 几个方便字段。
///
/// 两条读法并存，分工是**粒度**，不是重复：
///
/// - **点查**（直连 `erika::Player`，零延迟、不克隆）：`state()` / `position()` /
///   `duration()` / `is_playing()` / `tracks()` —— 要"此刻一个值"就用它们；
/// - **组查**（就是本结构）：一次拿到一组**同一瞬间**的值，外加"变了"的通知
///   （[`subscribe_status`](ErikaPlayer::subscribe_status)）。其中**只有 presenter 侧的
///   那几项**（`volume` / `muted` / `danmaku` / `subtitle` / `error`）必须从这里拿。
///
/// 四条限制，写进签名也写不到，只能靠这段话：
///
/// 1. **只在命令路径上发布**：`is_playing` / `duration` / `selection` 在**上游自己**
///    改变状态时（自然播完、erika 侧报错）会停在旧值 —— 要准就直读
///    [`is_playing`](ErikaPlayer::is_playing) / [`duration`](ErikaPlayer::duration) /
///    [`tracks`](ErikaPlayer::tracks)。这一路只保证"命令处理完之后是新的"；
/// 2. **滞后 ≤ 一次泵周期**（引擎空闲 16ms）+ 那一轮泵的耗时；
/// 3. **过渡态不在里面**：`Opening` 只在 `open()` 阻塞期间存在，而那时引擎不会发布 ——
///    要"打开中"请用调用方的乐观更新，或直读 [`state`](ErikaPlayer::state)；
/// 4. **位置不在里面**：它随墙钟走，属于进度通道（10Hz 的 `ErikaProgress`）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ErikaStatus {
    /// 在不在播（**已提交**状态，不是意图）。按钮标签请配调用方自己的乐观 signal。
    ///
    /// ⚠️ 自然播完之后这里可能仍是 `true`（上游自己改状态时不发布）——
    /// 要准请直读 [`is_playing`](ErikaPlayer::is_playing)。
    pub is_playing: bool,
    /// 总时长。`stop()` 之后**不会**变 `None`（媒体还开着）；没有媒体才是 `None`。
    ///
    /// ⚠️ 上游迟到才 probe 出时长时（流式源）这里不会自己更新，要准请直读
    /// [`duration`](ErikaPlayer::duration)。
    pub duration: Option<Duration>,
    /// 当前轨道选择（`{ video, audio, subtitle }`，`None` = 关）。
    ///
    /// 选轨是**排队**的，所以刚调完 `select_*` 之后这里可能还是旧值（最多滞后一条命令）；
    /// 上游自己改选轨（例如内部重置）也不会发布 —— 要准用 `player.tracks().selection()`。
    pub selection: TrackSelection,
    /// 音量（在 presenter 的 `audio_output` 上，`Player` 没有）。
    ///
    /// 精度是 **f32**（上游 `normalize_volume` 之后的值）。比较请用容差，别用 `< 1e-6`。
    pub volume: f32,
    /// 静音。**不改** `volume`：上游 `set_muted` 只把输出压到 0。
    pub muted: bool,
    /// 弹幕面板的初始化值（应用猜不到"实际生效的是什么"）。
    pub danmaku: DanmakuSnapshot,
    /// 字幕面板的初始化值（同上）。
    pub subtitle: SubtitleSnapshot,
    /// 最近一次失败：控制面、出帧路径、上游解码 / 音频错误（三处都走
    /// `ErikaEngine::fail`）。`open` 会清它，其余情况下只被下一条错误覆盖。
    pub error: Option<String>,
}
/// 进度快照：位置 + 总时长，**引擎已经算好**，读者直接用（没有方法、没有 `Instant`）。
///
/// 最坏延迟 = 节流间隔（100ms）+ 一次泵耗时。要帧级精确请直读
/// [`ErikaPlayer::position`]（一次锁、永远新鲜），两条路各管一件事：
///
/// - **订阅 / 轮询进度** → 用它（10Hz，值已算好，`position.set(p.position)` 就完了）；
/// - **帧级精确**（波形游标、逐帧步进、A/B 点）→ [`position`](ErikaPlayer::position)。
///
/// 两个字段来自**同一次采样**，所以进度条可以放心用 `position / duration` 相除，
/// 不会出现 `position > duration` 的那一帧。
#[derive(Clone, Debug, Default, PartialEq)] // PartialEq 是给引擎侧判等用的，别省
pub struct ErikaProgress {
    /// 采样那一刻的位置。
    pub position: Duration,
    /// 总时长（`None` = 还没 probe 完 / 没有媒体）。
    pub duration: Option<Duration>,
}
/// 播放器句柄：**控制面**（命令通道 + 直读）+ 身份（号）+ 可能携带的所有权票据。
///
/// - `Clone` 很便宜：几个 `Arc` 各加一，可以随便传、可以跨线程用；
/// - `PartialEq` **比号**（号单调递增、永不复用），所以能直接放进 `signal()` 比 props 变没变；
/// - 平台控件拿到的只是它的**视图侧**（[`ViewSource`](crate::video::ViewSource)），不是它本身。
///
/// ## 谁负责关机
///
/// **不是这个类型**。`new()` 会把守卫（`ErikaSession`）装进 `session` 那张票据里；
/// 把句柄交给 [`ErikaVideo`](crate::ErikaVideo) 后，组件在挂载时 `adopt()` 认领它 ——
/// 从此卸载即销毁。没人认领时，票据随最后一根 clone 一起 drop，引擎跟着关。
///
/// 所以这里的字段（`player` / `engine`）**只是使用权**：引擎被停掉之后，命令会静默失败
/// （`send_cmd` 返回 `Err(Closed)`），读侧拿到冻结的最后一帧，`status()` 会补一句
/// `"engine thread is gone"`。
///
/// ⚠️ 这些字段只能由 `new()` / `ErikaSession::player()` 写：它们必须属于**同一台引擎**，
/// 这个不变式由构造函数保证（拆开的字段没有类型层面的绑定）。
#[derive(Clone)]
pub struct ErikaPlayer {
    pub(crate) id: u64,
    pub(crate) player: Player,
    pub(crate) engine: ActorHandle<ErikaEngine>,
    /// 所有权票据：**只有 `new()` 产出的句柄带**，组件内部自建的没有。
    /// 组件挂载时 `adopt()` 把它取走 —— 从此关机权归组件。
    pub(crate) session: Option<Arc<Mutex<Option<ErikaSession>>>>,
}

impl PartialEq for ErikaPlayer {
    fn eq(&self, o: &Self) -> bool {
        self.id == o.id
    }
}
impl Eq for ErikaPlayer {}
impl JsValue for ErikaPlayer {
    fn ts_type(_: &mut TsTypes) -> String {
        "never".into()
    }
    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        Err(value.expected("an ErikaPlayer（句柄只能从 Rust 侧传）"))
    }
    fn to_js<'a>(&self, _: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        Err(PropError("ErikaPlayer 不能跨到 JS".into()))
    }
}
impl ErikaPlayer {
    /// 这个会话的**视图侧**（帧邮箱 + 出帧请求）。**需要 `escape-hatch` feature。**
    ///
    /// 它住在注册表里，所以会话已经销毁 / 已经摘表时返回 `None`。
    ///
    /// 一般不用它：画面直接写 [`ErikaVideo`](crate::ErikaVideo)
    /// （`erika_video().src(uri).player(p)`）就行。要"同一个播放器在几处同时显示"（镜像）
    /// 或者自己管 tick 时才需要。
    #[cfg(feature = "escape-hatch")]
    pub fn view(&self) -> Option<ViewSource> {
        crate::core::registry::resolve(self.id)
    }

    /// 会话的号。控件绑定显示目标用（`native(COMPONENT_NAME).prop("source", id)`），
    /// 也是外部"只拿到号"时用来 `resolve` 回句柄的键。
    pub(crate) fn session_id(&self) -> u64 {
        self.id
    }

    /// 建一台引擎，并把**守卫装进句柄**。从这一行起所有方法都真的有效。
    ///
    /// 把返回的句柄交给 [`ErikaVideo`](crate::ErikaVideo)（`.player(p)`），组件会在挂载时
    /// `adopt()` 认领守卫 —— 从此**关机权归组件**：卸载即销毁。
    ///
    /// 如果这个句柄始终没被任何组件认领，它就随最后一根 clone 一起 drop，引擎跟着关。
    ///
    /// 失败（没有可用的 Vulkan 设备 / 线程起不来 / 启动卡住）返回 `Err(原因)`，不 panic ——
    /// 要不要 panic 由调用方决定。
    pub fn new() -> Result<Self, String> {
        let session = ErikaSession::create()?;
        Ok(Self {
            id: session.id,
            player: session.player.clone(),
            engine: session.engine.clone(),
            session: Some(Arc::new(Mutex::new(Some(session)))),
        })
    }
    /// 引擎线程还活着没有。组件用它区分"票据被别人认领（镜像）"和"票据已被上一次挂载消费"。
    pub(crate) fn is_engine_alive(&self) -> bool {
        self.engine.is_alive()
    }

    pub(crate) fn adopt(&self) -> Option<ErikaSession> {
        self.session
            .as_ref()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }

    /// 弹幕句柄：发弹幕命令 + 读弹幕快照。
    ///
    /// 每次调用返回一个新的句柄值（内部只做一次 Arc 加一），可以直接被 UI 闭包捕获。
    pub fn danmaku(&self) -> ErikaDanmaku {
        ErikaDanmaku::new(self.id, self.engine.clone())
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
        let _ = self.engine.send_cmd(cmd);
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

    /// 把播放状态设成**目标值**（幂等）：`true` = 播，`false` = 暂停。
    ///
    /// 用目标值而不是"翻转"，是因为翻转的奇偶性要求**每次按键都被精确处理一次**，而命令要跨
    /// 两条线程边界：① 我们的命令队列 → 引擎线程；② 引擎 → erika 的 worker。① 还**没有上界**
    /// （引擎可能正卡在 `open` 或 `export_frame_dmabuf` 里几百毫秒）。
    ///
    /// 所以 UI 侧请自己维护乐观 signal（页面里那个 `want`）：
    ///
    /// ```ignore
    /// let want = signal(player.is_playing());
    /// button().on_press(move || {
    ///     let next = !want.get();
    ///     want.set(next);          // 立刻反映，不等引擎
    ///     player.set_playing(next); // 目标值，连点两次也不会翻回去
    /// });
    /// ```
    pub fn set_playing(&self, playing: bool) {
        if playing {
            self.play();
        } else {
            self.pause();
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

    /// 音量 0.0..=1.0（**f32**）。**立即返回**;`volume()` 在引擎处理完这条命令后就是新值。
    ///
    /// `Player` 上根本没有音量 —— 它住在 presenter 的音频设备上。
    ///
    /// 精度说明:上游 presenter 对外收/发 `f64`(它同时是 Flutter 桥的边界类型),但
    /// **内部存的是 f32**(`set_volume` 落地时 `normalize_volume(volume as f32)`),
    /// 所以真实精度就是 f32 —— 这里跟着真实精度走,上行时 `as f64` 无损。
    pub fn set_volume(&self, volume: f32) {
        if let Some(volume) = normalize_volume(volume) {
            self.send_settings(SettingsCmd::Volume(volume as f64));
        }
    }

    /// 当前音量(0.0..=1.0,**f32**)。读的是引擎发布的状态:`watch`,非阻塞,最多滞后一条命令。
    ///
    /// 它和 [`ErikaStatus::volume`] 是同一个值。比较请用容差(示例里用 `±0.01`)——
    /// f32 在 0.3 附近的间隔约 `3e-8`,别写 `abs() < 1e-9` 那种永远不成立的判据。
    pub fn volume(&self) -> f32 {
        self.engine.get_state().volume
    }

    /// 静音开关。**不改变 [`volume`](Self::volume)**:上游 `set_muted` 只把输出压到 0,
    /// 记住的音量还在,取消静音就恢复原样。
    pub fn set_muted(&self, muted: bool) {
        self.send_settings(SettingsCmd::Muted(muted));
    }

    /// 当前是否静音(读引擎发布的状态,最多滞后一条命令)。
    pub fn is_muted(&self) -> bool {
        self.engine.get_state().muted
    }

    /// 倍速。非法值直接丢弃(上游会把非有限 / `<= 0` normalize 成 1.0,放过去只会
    /// 让 UI 显示和实际不一致)。
    ///
    /// 变速是 **presenter 让音频环做 time-stretch**(变速不变调):直连 `Player` 的话
    /// 画面会变快而声音不变。
    pub fn set_rate(&self, rate: f64) {
        if let Some(rate) = normalize_rate(rate) {
            self.send_settings(SettingsCmd::Rate(rate));
        }
    }

    /// 当前**生效**的倍速 —— 直读 `PlaybackSnapshot::clock`(播放中 2ms 更新、暂停 50ms)。
    ///
    /// 刚 `set_rate` 之后可能还是旧值:变速要经过"音频环 time-stretch + 时钟换斜率",
    /// worker 提交后才变。
    pub fn rate(&self) -> f64 {
        self.player.playback_snapshot().clock.rate()
    }

    /// 订阅**事件流**:erika 的离散事件(状态 / 缓冲 / 轨道 / 选轨 / 错误)逐条转发过来。
    ///
    /// 用途是"**在发生的那一刻**知道":播完切下一集([`ErikaEvent::Ended`](crate::events::ErikaEvent::Ended))、
    /// 弹缓冲指示器、出错弹提示、轨道变了重读一次。**不是**用来读状态的 ——
    /// 收到事件后要值就直读 `player.*()`
    /// (零延迟),别指望 [`status`](Self::status) 已经刷新。
    ///
    /// 高频量不会进来:`PositionChanged` 每 2ms 一条,位置走进度通道
    /// ([`subscribe_progress`](Self::subscribe_progress))。
    ///
    /// 落后太多(事件被丢)时会收到 [`ErikaEvent::Resync`](crate::events::ErikaEvent::Resync);
    /// 只有返回 `None` 才是引擎线程没了。
    pub fn subscribe_events(&self) -> ErikaEventsListener {
        ErikaEventsListener::new(self.engine.get_events_rx())
    }

    /// 订阅**状态**(低频):**只在命令路径上**发布(每条命令处理完 / 出错 / open 中途)。
    ///
    /// 拿的是 presenter 侧那几项(音量 / 静音 / 弹幕 / 字幕 / 错误)—— 那几项没有别的读法。
    /// 快照自带数据,所以订阅任务**不必持有句柄**:页面离开后引擎退出,`next()` 返回
    /// `None`,任务自己收摊。
    ///
    /// ⚠️ 两个坑:
    ///
    /// - 只报**订阅之后**的变化 —— 第一件事先调 [`status`](Self::status) 建基线;
    /// - **上游自己改状态时这条线是静的**(自然播完、erika 侧报错之后 `is_playing` 会停在
    ///   旧值)。要准就直读 [`is_playing`](Self::is_playing) / [`duration`](Self::duration)。
    ///
    /// 其余限制见 [`ErikaStatusListener`](crate::events::ErikaStatusListener)。
    pub fn subscribe_status(&self) -> ErikaStatusListener {
        ErikaStatusListener::new(self.engine.state_rx())
    }

    /// 订阅**进度**(10Hz):位置 + 总时长,引擎已经算好。
    ///
    /// 播放中由 erika 的 `PositionChanged` 当节拍;暂停/seek/变速之后由命令路径补发一次。
    /// 只想要"最准的位置"就别订阅 —— 直读 [`position`](Self::position) 一次锁、永远新鲜。
    ///
    /// ```ignore
    /// let mut progress = player.subscribe_progress();
    /// spawn(async move {
    ///     while let Some(p) = progress.next().await {
    ///         position.set(p.position);
    ///         duration.set(p.duration);
    ///     }
    /// });
    /// ```
    pub fn subscribe_progress(&self) -> ErikaProgressListener {
        ErikaProgressListener::new(self.engine.progress_rx())
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
    #[cfg(feature = "escape-hatch")]
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
    /// 进度条那种高频读用 [`position`](Self::position)(直连 `Player`,不经过引擎线程)。
    ///
    /// ```ignore
    /// let frames = player.call_with(|p| p.runtime_snapshot().stats.rendered_video_frames);
    /// let ok = player.call_with(|p| p.set_playback_rate(2.0).is_ok());
    /// ```
    #[cfg(feature = "escape-hatch")]
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

    /// 当前位置(一个数):**直读** `Player`，一次锁、永远新鲜。
    ///
    /// 上游叫 `current_media_time`，同一个东西 —— 我们这层统一用 `position`:
    /// **`position` 是一个数,`progress` 是一组数**。
    ///
    /// 和 [`progress`](Self::progress) 的分工:这个精确到此刻,但它和
    /// [`duration`](Self::duration) 是两次锁,同时读可能撕裂(上游 `playback_snapshot()`
    /// 就是为这个存在的)。所以**进度条 / 时间文字用 `progress()`**(同一次采样、10Hz),
    /// 帧级精确(波形游标、逐帧步进、A/B 点)才用这个。
    pub fn position(&self) -> Duration {
        self.player.current_media_time()
    }

    /// 当前进度(引擎算好的快照,10Hz 刷新)。订阅版见
    /// [`subscribe_progress`](Self::subscribe_progress)。
    pub fn progress(&self) -> ErikaProgress {
        self.engine.get_progress()
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
    /// 内部用：拿裸 `Player`（`raw()` 的 crate 内版本，不受 feature 影响）。
    pub(crate) fn player_inner(&self) -> &erika::Player {
        &self.player
    }

    /// **裸访问逃生口**(opt-in)。
    ///
    /// ⚠️ 从这里拿到的**写方法全部绕过 presenter 的音频协调**,会音画不同步
    /// (实测:倍速 2x 声音不变快、seek(0) 声音不回去)。只用来读我们还没封装的东西。
    pub fn raw(&self) -> &erika::Player {
        &self.player
    }
    /// 读 presenter 侧的状态(音量 / 静音 / 弹幕 / 字幕 / 错误)。
    ///
    /// 来源是 actor 的 `State`:引擎在**命令路径上**发布(每条命令处理完 / 出错 / open
    /// 中途;不判等,所以可能拿到与上次相同的值),这里是**命令侧的最新值**。
    ///
    /// ⚠️ 上游**自己**改变状态时不会发布 —— 自然播完之后 `is_playing` 在这里可能仍是
    /// `true`。要"此刻真值"请直读 [`is_playing`](Self::is_playing) /
    /// [`state`](Self::state) / [`duration`](Self::duration)。
    ///
    /// 生命周期 / 位置 / 时长 / 轨道**不从这里拿**(那些直连 `Player`,零延迟);
    /// 视频尺寸也不在这里(控件每帧要用,走 `FrameSlot` 的原子)。
    ///
    /// 引擎线程已经退出时,这里会补一条 `error`(`"engine thread is gone"`),
    /// 免得 UI 以为一切正常。
    pub fn status(&self) -> ErikaStatus {
        let mut st = self.engine.get_state();
        if !self.engine.is_alive() {
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

pub(crate) fn normalize_volume(volume: f32) -> Option<f32> {
    volume.is_finite().then(|| volume.clamp(0.0, 1.0))
}
pub(crate) fn normalize_rate(rate: f64) -> Option<f64> {
    (rate.is_finite() && rate > 0.0).then_some(rate)
}

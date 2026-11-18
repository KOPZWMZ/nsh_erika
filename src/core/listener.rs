//! 对外的**三条订阅通道**:状态 / 进度 / 事件。
//!
//! 每条线都有一对出口:一条是 `player.rs` 上的**轮询读**(点查或组查),一条是这里的**订阅**。
//! 两者**同源**(状态读的是同一个 `watch`),所以"轮询还是监听"可以随调用方挑。
//!
//! | 通道 | 语义 | 装什么 | 出口 |
//! |---|---|---|---|
//! | 状态 | 最新值,可覆盖,**低频** | [`ErikaStatus`](crate::state::ErikaStatus) | [`ErikaStatusListener`] |
//! | 进度 | 最新值,可覆盖,**10Hz** | `ErikaProgress` | [`ErikaProgressListener`] |
//! | 事件 | 每条尽量必达,**罕见** | [`ErikaEvent`] | [`ErikaEventsListener`] |
//!
//! 判据一句话:**要"最新值"走 watch(状态/进度),要"发生过"走 broadcast(事件)。**
//! 高频量绝不能进事件流 —— 播放中 erika 的 `PositionChanged` 是 2ms 一条,转发它等于
//! 每秒叫醒 UI 五百次。
//!
//! # 三条通道各管什么(改事件设计前先看这段)
//!
//! - **事件 = 上游离散事实的转发**,白名单(状态 / 缓冲 / 轨道 / 选轨 / 错误)。
//!   引擎在这里**不做推断**:上游没发的事实(典型是"自然播完"——上游只有
//!   `PlayerEvent::StateChanged(Stopped)`,和 `stop()` 是同一个值)不会由我们发明。
//! - **状态 = 命令侧快照**:只在命令路径(`on_cmd` / `fail` / `open` 中途)发布。
//!   erika 自己改变状态时(例如播完)**不发布** —— 那几个字段(`is_playing` /
//!   `duration` / `selection`)要准就直读 `player.is_playing()` / `duration()` / `tracks()`。
//! - **要"值"就直读**:`ErikaStatus` 存的是 presenter 侧的东西(音量 / 静音 / 弹幕 /
//!   字幕 / 错误),权威值永远在 `ErikaPlayer` 的直读方法上。
//!
//! # 为什么要包一层,而不是直接给 `watch::Receiver` / `broadcast::Receiver`
//!
//! 1. **`borrow()` 的 guard 持读锁** —— 跨 `.await` 持有会挡住写端(引擎线程),
//!    而那写法看起来完全正确。这里**不给** `borrow`,只给 [`get`](ErikaListener::get) /
//!    [`next`](ErikaListener::next)(克隆,立刻释放);
//! 2. **错误类型不进公开面**:`RecvError` / `TryRecvError` 统一成 `None` = 引擎没了,
//!    用户不用认识通道库的错误枚举;
//! 3. **换实现不是破坏性变更**:通道的实现(现在是 actor-lite / tokio)不出现在签名上。
//!
//! # 订阅任务**不许**持有 `ErikaPlayer`
//!
//! 监听器只持接收端,不持句柄 —— 这是故意的:页面离开后最后一个句柄 drop,引擎线程退出,
//! 写端消失,`next()` 返回 `None`,订阅任务自己结束。
//!
//! 反过来,如果订阅任务捕获了 `ErikaPlayer`,**它自己就是"最后一个句柄"** —— 引擎、
//! 解码线程、GPU、音频都不会退出,和"最后一个句柄 drop 就干净退出"直接打架。
//! 所以监听器要的东西(状态、进度、事件)全部**自带数据**,读者不必为了读一个值去持句柄。

use crate::core::player::{ErikaProgress, ErikaStatus};
use actor_lite::broadcast::error::{RecvError, TryRecvError};
use erika::{PlayerState, TrackSelection};

/// 引擎转发的**上游离散事件**。
///
/// 规则只有三条:
///
/// 1. **只转离散、低频的**:白名单是 `StateChanged` / `BufferingChanged` / `TracksChanged` /
///    `TrackSelectionChanged` / `Error`。`PositionChanged` 每 2ms 一条,是采样量不是事实,
///    永远不进这条流(位置走进度通道)。
/// 2. **payload 能不带就不带**:只有消费者猜不出来的才带(`PlayerState` / `bool` /
///    `TrackSelection`)。`TracksChanged` 故意不带 `Vec<TrackInfo>` —— 广播会把大包复制给
///    每个订阅者,而且那些类型一导出就跟着上游版本跑。要列表就调 `player.tracks()`。
/// 3. **不推断**:上游没发的事实这里也不会有。典型是**自然播完** —— 上游只有
///    `StateChanged(Stopped)`,和 `stop()` 是同一个值,本库不替你猜。要区分:
///    `state() == Stopped` 且 `generation` 没变(`open` / `seek` / `stop` 都会 +1)。
///
/// 事件**不承诺 `ErikaStatus` 已经刷新**(见模块文档):收到事件后要值就直读 `player.*()`。
/// 通道是 `broadcast`:容量有限,落后太多会收到 [`Resync`](Self::Resync) ——
/// 那不是"某样东西变了",是"你已经错过几条,重读一遍直读值"。
///
/// 变体不封口(`#[non_exhaustive]`):上游将来新增可转发的事件时,加变体不算破坏性变更。
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ErikaEvent {
    /// 播放状态变了(`Idle` / `Opening` / `Ready` / `Playing` / `Paused` / `Stopped` / …)。
    ///
    /// ⚠️ `Stopped` 有两种情况:自然播完,或者 `stop()`。本库不区分,见上一条规则 3。
    StateChanged(PlayerState),
    /// 缓冲状态变了。**上游没有 getter 读它**,所以这是拿到它的唯一途径。
    Buffering(bool),
    /// 轨道列表变了(probe 完成、外挂字幕挂上 / 摘掉)。**不带列表**,要列表调
    /// `player.tracks()`。
    TracksChanged,
    /// 当前选中的轨道变了(`select_audio` / `select_subtitle` 生效之后)。
    TrackSelectionChanged(TrackSelection),
    /// 出错了。全文在 [`ErikaStatus::error`](crate::state::ErikaStatus) 里也有 ——
    /// 事件是"必须被看见一次",状态是"现在坏着、为什么"。
    Error(String),
    /// 落后了:有 `missed` 条事件被丢掉(`broadcast` 容量有限)。
    ///
    /// 它不是"某样东西变了",而是"你可能漏了一条 `Buffering` 或 `Error`" ——
    /// 正确反应是重读一遍直读值,并按最坏情况处理。订阅任务**不该**因为它结束:
    /// 只有 `recv()` 返回 `None` 才是引擎线程没了。
    Resync {
        /// 被丢掉的条数(`broadcast` 的 `Lagged` 计数)。
        missed: u64,
    },
}

/// `watch` 通道的订阅出口。
///
/// **一般不用写这个名字** —— 用 [`ErikaStatusListener`] / [`ErikaProgressListener`]。
/// 它是两者的公共实现:语义为"最新值",引擎写多少次你不管,你只会看到最后一次,
/// 中间值可能被跳过(对进度条是优点)。
pub struct ErikaListener<T> {
    rx: actor_lite::watch::Receiver<T>,
}

impl<T> ErikaListener<T> {
    /// 引擎内部用:把一个接收端包起来。
    pub(crate) fn new(rx: actor_lite::watch::Receiver<T>) -> Self {
        Self { rx }
    }
}

impl<T: Clone> ErikaListener<T> {
    /// 等下一次变化,返回**此刻**的最新值。
    ///
    /// - `Some(v)`:`v` 是最新值,不是"变化发生那一瞬间"的值(中间的变化已被合并);
    /// - `None`:引擎线程没了,不会再有变化(订阅任务据此收摊)。
    ///
    /// ⚠️ 它只报**订阅之后**的变化。第一件事请先调 [`get`](Self::get) 建基线,
    /// 否则"订阅之前就一直是这个值"的那一版你永远收不到。
    pub async fn next(&mut self) -> Option<T> {
        // Err = 写端没了 = 引擎线程退出(或最后一个句柄被 drop)
        self.rx.changed().await.ok()?;
        Some(self.rx.borrow().to_owned())
    }

    /// 最新值(**克隆**),不等待、也不消费"已变化"标记 —— 什么时候读都是最新的。
    pub fn get(&self) -> T {
        self.rx.borrow().to_owned()
    }

    /// [`next`](Self::next) 的非阻塞版:有未读变化就返回最新值,否则 `None`。
    ///
    /// 给阻塞式 UI 循环 / 自检用(没有 async 上下文也能读)。引擎已退出同样返回 `None`。
    pub fn try_take(&mut self) -> Option<T> {
        match self.rx.has_changed() {
            Ok(true) => Some(self.rx.borrow_and_update().to_owned()),
            // Ok(false) = 没变化;Err = 写端没了。两种情况都没有新值可给。
            Ok(false) | Err(_) => None,
        }
    }
}

/// 引擎**状态**的订阅出口(低频):只在**命令路径**上发布。
///
/// 装的是 presenter 侧的那一半真相:音量、静音、弹幕 / 字幕配置、错误,外加三个方便字段
/// (`is_playing` / `duration` / `selection`)。**位置不在里面**(它随墙钟走,见
/// `ErikaProgress`)。
///
/// 三点必须知道:
///
/// - **上游自己改变状态时不会发布**:自然播完、或 erika 侧报错之后,`is_playing` /
///   `duration` / `selection` 可能停在旧值 —— 要准就直读 `player.is_playing()` /
///   `duration()` / `tracks()`。这一路只保证"命令处理完之后是新的"。
/// - **滞后 ≤ 一次泵周期**(引擎空闲时 16ms)+ 那一轮泵的耗时;
/// - **过渡态不在里面**(`Opening` 只在 `open()` 阻塞期间存在,而那期间引擎不会发布)。
///   要"打开中"这种态,由调用方乐观更新,或者直读 `player.state()`。
///
/// ```ignore
/// let mut status = player.subscribe_status();
/// spawn(async move {
///     while let Some(s) = status.next().await {
///         volume.set(s.volume);
///         muted.set(s.muted);
///         if let Some(text) = s.error.clone() { error_line.set(text); }
///     }
/// });
/// // 在不在播 / 有哪些轨道:直读。这一路对上游自己的状态变化是静默的。
/// ```
pub type ErikaStatusListener = ErikaListener<ErikaStatus>;

/// 引擎**进度**的订阅出口(10Hz):位置 + 总时长,引擎已经算好。
///
/// 和 [`ErikaStatusListener`] 的区别只有一条:**频率**。播放中由 erika 的
/// `PositionChanged` 当节拍节流到 10Hz;暂停时它不来,所以 pause / seek / 变速之后
/// 由命令路径补发一次(否则读者会一直看着旧位置)。
///
/// 只想要"最准的位置"就别订阅 —— 直读 [`ErikaPlayer::position`](crate::ErikaPlayer::position)
/// 是一次锁、永远新鲜。120ms 的延迟对进度条和时间文字无感,对帧级工具才有感。
///
/// ```ignore
/// let mut progress = player.subscribe_progress();
/// spawn(async move {
///     while let Some(p) = progress.next().await {
///         position.set(p.position);      // 已经算好，不用自己取时刻
///         duration.set(p.duration);
///     }
/// });
/// ```
pub type ErikaProgressListener = ErikaListener<ErikaProgress>;

/// 引擎**事件**流的订阅出口:[`ErikaPlayer::subscribe_events`](crate::ErikaPlayer::subscribe_events) 返回它。
///
/// 它是上游离散事实的转发(见 [`ErikaEvent`]),用途是"**在发生的那一刻**知道":
/// 弹缓冲指示器、播完切下一集、出错弹提示、轨道列表变了重读一次。
///
/// 两条硬约束:
///
/// - **不要拿它当状态用**:事件是一次性的,晚订阅就永远看不到;而且它不承诺
///   [`ErikaStatus`](crate::state::ErikaStatus) 已经刷新。要"现在是怎样"就直读 `player.*()`。
/// - **落后不等于结束**:容量有限,落后时会收到 [`ErikaEvent::Resync`];
///   只有 `recv()` / `try_recv()` 返回 `None`(写端关闭)才是引擎线程没了。
pub struct ErikaEventsListener {
    rx: actor_lite::broadcast::Receiver<ErikaEvent>,
}

impl ErikaEventsListener {
    /// 引擎内部用:把一个广播接收端包起来。
    pub(crate) fn new(rx: actor_lite::broadcast::Receiver<ErikaEvent>) -> Self {
        Self { rx }
    }

    /// 等一条事件。
    ///
    /// - `Some(_)`:去重拉一次状态(`player.status()` / 直读句柄);
    /// - `None`:引擎线程没了,不会再有事件。
    pub async fn recv(&mut self) -> Option<ErikaEvent> {
        match self.rx.recv().await {
            Ok(event) => Some(event),
            Err(RecvError::Lagged(missed)) => Some(ErikaEvent::Resync { missed }),
            Err(RecvError::Closed) => None,
        }
    }

    /// [`recv`](Self::recv) 的非阻塞版:有事件就取一条,没有或引擎已退出都是 `None`。
    ///
    /// 阻塞式 UI 循环 / 自检里用它(没有 async 上下文也能读)。
    pub fn try_recv(&mut self) -> Option<ErikaEvent> {
        match self.rx.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Lagged(missed)) => Some(ErikaEvent::Resync { missed }),
            Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => None,
        }
    }
}

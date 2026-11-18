//! 对外的**三条订阅通道**:状态 / 进度 / 事件。
//!
//! 每条线都有一对出口:一条是 `player.rs` 上的**轮询读**(点查或组查),一条是这里的**订阅**。
//! 两者**同源**(状态读的是同一个 `watch`),所以"轮询还是监听"可以随调用方挑。
//!
//! | 通道 | 语义 | 装什么 | 出口 |
//! |---|---|---|---|
//! | 状态 | 最新值,可覆盖,**低频** | [`ErikaStatus`](crate::ErikaStatus) | [`ErikaStatusListener`] |
//! | 进度 | 最新值,可覆盖,**10Hz** | `ErikaProgress` | [`ErikaProgressListener`] |
//! | 事件 | 每条尽量必达,**罕见** | [`ErikaEvent`] | [`ErikaEventsListener`] |
//!
//! 判据一句话:**要"最新值"走 watch(状态/进度),要"发生过"走 broadcast(事件)。**
//! 高频量绝不能进事件流 —— 播放中 erika 的 `PositionChanged` 是 2ms 一条,转发它等于
//! 每秒叫醒 UI 五百次。
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

/// 引擎发出的**语义事件**。
///
/// 只装**没有 level 可读**的边沿事实,以及**必须被看见一次**的提醒。有 getter 的状态变化
/// 一律走 `watch`(`ErikaStatus`),不在这里转发 —— 转发就是把上游的实现粒度搬给 UI。
#[derive(Clone, Debug)]
pub enum ErikaEvent {
    /// 状态可能变了:每条命令处理完、以及收到 erika 的离散事件时各发一次。
    ///
    /// 不判等 —— 命令是用户动作,重复一次也无所谓;判等要克隆 + 比较整包状态,更贵。
    /// 所以收到它之后**重拉一次状态**就对,但别假设"每次值都真的不同"。
    Changed,
    /// 自然播完(不是 `stop()`)。
    Ended,
    /// 出错了。全文在 `ErikaStatus::error` 里也有 —— 事件只是"必须被看见一次"的提醒。
    Error(String),
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

/// 引擎**状态**的订阅出口(低频):引擎每处理完一条命令、每收到一个 erika 离散事件发布一次。
///
/// 快照里装的是"一份对得上的完整真相":生命周期、时长、轨道清单的版本号、音量、
/// 静音、弹幕/字幕配置、错误。**位置不在里面**(它随墙钟走,见 `ErikaProgress`)。
///
/// 两点必须知道:
///
/// - **滞后 ≤ 一次泵周期**(引擎空闲时 16ms)+ 那一轮泵的耗时;
/// - **过渡态不在里面**(`Opening` 只在 `open()` 阻塞期间存在,而那期间引擎不会发布)。
///   要"打开中"这种态,由调用方乐观更新,或者直读 `player.state()`。
///
/// ```ignore
/// let mut status = player.subscribe_status();
/// let mut seen_tracks = player.status().tracks_epoch;   // 建基线
/// spawn(async move {
///     while let Some(s) = status.next().await {
///         playing.set(s.is_playing());
///         if s.tracks_epoch != seen_tracks { seen_tracks = s.tracks_epoch; /* 重读轨道 */ }
///     }
/// });
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
/// 事件只装**不能从状态读出来的边沿**:**播完了**必须靠它(200ms 轮询会晚一拍),
/// **出错了**靠它第一时间弹提示(全文同时在 `ErikaStatus::error` 里)。
///
/// 落后太多(事件被丢)时也返回 `Some` —— 事件不带数据,重拉一次状态就够了,
/// 用户不需要认识底层的 `Lagged`。
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
        use actor_lite::broadcast::error::RecvError;
        match self.rx.recv().await {
            Ok(event) => Some(event),
            Err(RecvError::Lagged(_)) => Some(ErikaEvent::Changed),
            Err(RecvError::Closed) => None,
        }
    }

    /// [`recv`](Self::recv) 的非阻塞版:有事件就取一条,没有或引擎已退出都是 `None`。
    ///
    /// 阻塞式 UI 循环 / 自检里用它(没有 async 上下文也能读)。
    pub fn try_recv(&mut self) -> Option<ErikaEvent> {
        use actor_lite::broadcast::error::TryRecvError;
        match self.rx.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Lagged(_)) => Some(ErikaEvent::Changed),
            Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => None,
        }
    }
}

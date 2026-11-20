//! 组件事件：把引擎的三条订阅通道翻译成 `ErikaVideo` 的 `on_*` 回调。
//!
//! # 为什么要有这一层
//!
//! 句柄路线的应用自己 `subscribe_progress()` / `subscribe_events()` 就行；**props 路线没有
//! 句柄**，事件是它唯一的出口。所以这一层不是"重复实现"，而是把订阅搬进组件，让
//! `erika_video().src(..)` 这种零句柄页面也能拿到状态、进度、播完和错误。
//!
//! # 为什么载荷都是我们自己的类型
//!
//! [`#[derive(Event)]`](nsh_core::element::Event) 会生成 `to_js` 和 TypeScript 声明，
//! 要求载荷实现 `JsValue`；而 `PlayerState` / `TrackInfo` 是上游类型 —— 孤儿规则不允许
//! 我们补 impl。所以每个载荷都是我们自己的投影（`VideoState` / `VideoTrack` …），
//! 由 [`From`] 从上游类型转过来。
//!
//! # 三条通道，一个事件流
//!
//! | 来源 | 出口 | 内容 |
//! |---|---|---|
//! | `subscribe_events()` | broadcast | 状态 / 播完 / 缓冲 / 轨道 / 选轨 / 错误 |
//! | `subscribe_progress()` | watch（10Hz） | 位置 + 时长 |
//!
//! 两条通道合流成一个事件流，**跨通道的先后不承诺**（比如最后一条 `Progress` 和 `Ended`
//! 谁先到）：各通道各自有序。另外两个 watch 的语义是"只报订阅之后的变化"，所以
//! [`wire`] 会先补发一次快照，否则句柄路线（引擎在挂载前就 open 过）会一直看不到当前值。
//!
//! # 开销
//!
//! 只要组件挂着，订阅就存在（不看有没有人挂监听 —— NSH 没有"有没有监听者"的查询）。
//! 代价是每条通道一个 `watch` 接收端 + 一个 `spawn` 任务；暂停时引擎不发布进度，
//! 任务就是在 `await` 上等着，不唤醒。

use nsh_core::prelude::*;

use crate::ErikaPlayer;
use crate::events::ErikaEvent;
use crate::state::ErikaProgress;
use erika::{PlayerState, TrackInfo, TrackKind, TrackSelection};

/// 播放状态（和上游 `PlayerState` 一一对应）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PropEnum)]
pub enum VideoState {
    /// 还没有媒体。
    #[default]
    Idle,
    /// 正在打开（阻塞的 probe / 解码器初始化）。
    Opening,
    /// 打开了，没在播。
    Ready,
    /// 播放中。
    Playing,
    /// 暂停中。
    Paused,
    /// 停播了，但媒体还开着。
    Stopped,
    /// 已关闭（终态）。
    Closed,
    /// 出错。
    Error,
}

impl From<PlayerState> for VideoState {
    fn from(state: PlayerState) -> Self {
        match state {
            PlayerState::Idle => Self::Idle,
            PlayerState::Opening => Self::Opening,
            PlayerState::Ready => Self::Ready,
            PlayerState::Playing => Self::Playing,
            PlayerState::Paused => Self::Paused,
            PlayerState::Stopped => Self::Stopped,
            PlayerState::Closed => Self::Closed,
            PlayerState::Error => Self::Error,
        }
    }
}

/// 轨道种类。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PropEnum)]
pub enum VideoTrackKind {
    /// 视频轨（不能选，只用来显示分辨率）。
    #[default]
    Video,
    /// 音轨。
    Audio,
    /// 字幕轨。
    Subtitle,
}

impl From<TrackKind> for VideoTrackKind {
    fn from(kind: TrackKind) -> Self {
        match kind {
            TrackKind::Video => Self::Video,
            TrackKind::Audio => Self::Audio,
            TrackKind::Subtitle => Self::Subtitle,
        }
    }
}

/// 一条轨道：只投影界面要用的字段（上游 `TrackInfo` 有 18 个）。
#[derive(Debug, Clone, PartialEq, JsObject)]
pub struct VideoTrack {
    /// 轨道号（`select_audio` / `select_subtitle` 用它）。
    pub id: i64,
    /// 种类。
    pub kind: VideoTrackKind,
    /// 标题（`None` 时界面自己兜底成"音轨 2"这种）。
    pub title: Option<SharedString>,
    /// 语言标签（`zh` / `eng` …）。
    pub language: Option<SharedString>,
    /// 编码（`aac` / `subrip` …）。
    pub codec: Option<SharedString>,
    /// 当前是否选中。
    pub selected: bool,
    /// 能不能摘掉（外挂字幕可以，内嵌的不行）。
    pub can_remove: bool,
}

impl From<TrackInfo> for VideoTrack {
    fn from(track: TrackInfo) -> Self {
        Self {
            id: track.id,
            kind: track.kind.into(),
            title: track.title.map(SharedString::from),
            language: track.language.map(SharedString::from),
            codec: track.codec.map(SharedString::from),
            selected: track.selected,
            can_remove: track.can_remove,
        }
    }
}

/// 进度：**毫秒**。两个字段来自同一次采样，所以可以直接相除。
#[derive(Debug, Clone, PartialEq, JsObject)]
pub struct VideoProgress {
    /// 当前位置。
    pub position_ms: u64,
    /// 总时长（还没 probe 完 / 没有媒体时是 `None`）。
    pub duration_ms: Option<u64>,
}

impl From<ErikaProgress> for VideoProgress {
    fn from(progress: ErikaProgress) -> Self {
        Self {
            position_ms: progress.position.as_millis() as u64,
            duration_ms: progress.duration.map(|duration| duration.as_millis() as u64),
        }
    }
}

/// 当前选中的轨道（`None` = 关闭）。
#[derive(Debug, Clone, Default, PartialEq, JsObject)]
pub struct SelectedTracks {
    /// 选中的视频轨（上游不可选，通常就是唯一的视频轨）。
    pub video: Option<i64>,
    /// 选中的音轨（`None` = 关掉音频）。
    pub audio: Option<i64>,
    /// 选中的字幕轨（`None` = 关字幕）。
    pub subtitle: Option<i64>,
}

impl From<TrackSelection> for SelectedTracks {
    fn from(selection: TrackSelection) -> Self {
        Self {
            video: selection.video,
            audio: selection.audio,
            subtitle: selection.subtitle,
        }
    }
}

/// `ErikaVideo` 的事件：每个变体一个监听方法（`on_state_change` / `on_progress` …）。
///
/// 全部可选：只挂你关心的那几个就行。
#[derive(Debug, Clone, PartialEq, Event)]
pub enum VideoEvent {
    /// 播放状态变了。
    ///
    /// `Stopped` 有两种来历（自然播完 / `stop()`），要区分就看紧随其后的 [`VideoEvent::Ended`](VideoEvent::Ended)。
    StateChange(VideoState),
    /// 进度（引擎节拍，10Hz）。
    ///
    /// 暂停 / 卡住时**不来新值**（上游那时不修改位置），外面保留最后一份即可 ——
    /// 别拿它判断"在不在播"。
    Progress(VideoProgress),
    /// 媒体**自己放完了**（不是 `stop()`）。
    ///
    /// 连播 / 结算 / "下一集"用它，别用 `position_ms ≈ duration_ms` 去猜。
    Ended,
    /// 缓冲状态变了（上游没有 getter，这是拿到它的唯一途径）。
    Buffering(bool),
    /// 轨道列表变了（probe 完成、外挂字幕挂上 / 摘掉）—— **带全量列表**：
    /// props 路线没有句柄可查，不带就永远读不到。
    TracksChange(Vec<VideoTrack>),
    /// 当前选中的轨道变了。
    SelectionChange(SelectedTracks),
    /// 出错了（全文；和 `status.error` 是同一份文案）。
    Error(SharedString),
}

/// 把三条订阅通道接到组件事件上，并补发一次快照。
///
/// 必须在组件 body 里调用：`spawn` 挂在当前 owner（= 组件）上，卸载时任务跟着取消；
/// `cx.emit` 也只在 NSH 引擎线程上安全。
pub(crate) fn wire(player: &ErikaPlayer, cx: Scope<VideoEvent>) {
    // ① 快照：两个 watch 都只报"订阅之后"的变化，而引擎可能早在挂载前就已经 open 过了
    //    （句柄路线）。放进 spawn：等挂载完、监听方法绑上之后第一轮再发。
    {
        let player = player.clone();
        spawn(async move {
            cx.emit(VideoEvent::StateChange(player.state().into()));
            cx.emit(VideoEvent::Progress(player.progress().into()));
            if player.has_media() {
                cx.emit(VideoEvent::TracksChange(tracks_of(&player)));
                cx.emit(VideoEvent::SelectionChange(
                    player.tracks().selection().into(),
                ));
            }
        });
    }

    // ② 离散事实：状态 / 播完 / 缓冲 / 轨道 / 选轨 / 错误。
    let mut events = player.subscribe_events();
    {
        let player = player.clone();
        spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    ErikaEvent::StateChanged(state) => {
                        cx.emit(VideoEvent::StateChange(state.into()))
                    }
                    ErikaEvent::Ended => cx.emit(VideoEvent::Ended),
                    ErikaEvent::Buffering(buffering) => cx.emit(VideoEvent::Buffering(buffering)),
                    ErikaEvent::TracksChanged => {
                        cx.emit(VideoEvent::TracksChange(tracks_of(&player)))
                    }
                    ErikaEvent::TrackSelectionChanged(selection) => {
                        cx.emit(VideoEvent::SelectionChange(selection.into()))
                    }
                    ErikaEvent::Error(message) => cx.emit(VideoEvent::Error(message.into())),
                    // 落后只意味着"可能漏了一条离散事实"，重读直读值即可；
                    // 它不是事件，不进事件面（订阅任务也不该因此结束）。
                    ErikaEvent::Resync { missed } => {
                        log::debug!("erika.video: 事件落后，丢了 {missed} 条")
                    }
                }
            }
        });
    }

    // ③ 进度：10Hz，值引擎已经算好。
    let mut progress = player.subscribe_progress();
    spawn(async move {
        while let Some(progress) = progress.next().await {
            cx.emit(VideoEvent::Progress(progress.into()));
        }
    });
}

/// 全量轨道列表（`TracksChange` 的载荷）。
fn tracks_of(player: &ErikaPlayer) -> Vec<VideoTrack> {
    player.tracks().list().into_iter().map(VideoTrack::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上游 8 态一个都不能漏（漏了会让 `match` 静默落到别的分支）。
    #[test]
    fn every_player_state_maps() {
        let pairs = [
            (PlayerState::Idle, VideoState::Idle),
            (PlayerState::Opening, VideoState::Opening),
            (PlayerState::Ready, VideoState::Ready),
            (PlayerState::Playing, VideoState::Playing),
            (PlayerState::Paused, VideoState::Paused),
            (PlayerState::Stopped, VideoState::Stopped),
            (PlayerState::Closed, VideoState::Closed),
            (PlayerState::Error, VideoState::Error),
        ];
        for (from, to) in pairs {
            assert_eq!(VideoState::from(from), to, "{from:?} 应映射成 {to:?}");
        }
    }

    #[test]
    fn selection_maps_three_tracks() {
        let selection = TrackSelection {
            video: Some(0),
            audio: Some(2),
            subtitle: None,
        };
        let mapped = SelectedTracks::from(selection);
        assert_eq!(mapped.video, Some(0));
        assert_eq!(mapped.audio, Some(2));
        assert_eq!(mapped.subtitle, None);
    }

    #[test]
    fn progress_maps_millis() {
        let progress = ErikaProgress {
            position: std::time::Duration::from_millis(1_500),
            duration: Some(std::time::Duration::from_millis(90_000)),
        };
        let mapped = VideoProgress::from(progress);
        assert_eq!(mapped.position_ms, 1_500);
        assert_eq!(mapped.duration_ms, Some(90_000));
    }
}

//! 轨道族：媒体轨道（音轨 / 字幕轨）的选择 + 外部字幕的挂 / 摘。
//!
//! # 为什么单独一个句柄
//!
//! 这一族同时跨了**三种通道**，而且还会长（后面还有一堆弹幕轨操作）：
//!
//! | 做什么 | 通道 |
//! |---|---|
//! | 读列表 / 读当前选择 | **直连** `erika::Player`（零延迟） |
//! | 选音轨 / 选字幕轨 | **队列**：presenter 要 quiesce + flush 音频环 + 清视觉 + bump 弹幕代次 |
//! | 挂 / 摘外部字幕 | **直连且阻塞**（内部有应答通道，等文件打开完） |
//!
//! 塞在 `ErikaPlayer` 上的话，这三种语义会混在同一个方法列表里，读的人分不清哪个会阻塞、
//! 哪个只是投一条命令。句柄名就是提示。
//!
//! # 弹幕轨**不在**这里
//!
//! 弹幕轨（`add_track` / 单轨开关 / 偏移 / 移除）是 [`ErikaDanmaku`](super::ErikaDanmaku)
//! 的事：它不带媒体、可以有多条、有自己的名字 / 来源 / 偏移，整块住在 presenter 上。

use erika::subtitle::SubtitleTrackConfig;
use erika::{TrackInfo, TrackSelection};

use super::ErikaPlayer;
use crate::cmd::SettingsCmd;

/// 轨道句柄：媒体轨道的读 / 选择，以及外部字幕的挂 / 摘。
///
/// 从 [`ErikaPlayer::tracks`](super::ErikaPlayer::tracks) 取；`Clone` 只是句柄克隆，
/// 同一个播放器派生出来的互相 `==`，可以直接被 UI 闭包捕获。
///
/// ⚠️ **视频轨不可选**：上游没有 `select_video_track`，[`selection`](Self::selection) 里的
/// `video` 只是信息。菜单里别给视频轨做下拉框。
pub struct ErikaTracks {
    player: ErikaPlayer,
}

impl Clone for ErikaTracks {
    fn clone(&self) -> Self {
        Self {
            player: self.player.clone(),
        }
    }
}
impl PartialEq for ErikaTracks {
    fn eq(&self, other: &Self) -> bool {
        self.player == other.player
    }
}
impl Eq for ErikaTracks {}

impl ErikaTracks {
    /// 建一个句柄（只给 `ErikaPlayer::tracks` 用）。
    pub(super) fn new(player: ErikaPlayer) -> Self {
        Self { player }
    }

    /// 当前媒体提供的**全部**轨道：容器里的 + 运行时挂进来的外部字幕。
    ///
    /// 两种情况看 `TrackInfo::source`（`Embedded` / `External`）区分，
    /// `can_remove` 为真的才是能摘掉的外部字幕。`open` 之后才有内容。
    ///
    /// 换轨时 erika 会发 `TracksChanged`，不用轮询。
    pub fn list(&self) -> Vec<TrackInfo> {
        self.player.media_tracks()
    }

    /// 当前选中的 track id（`TrackSelection { video, audio, subtitle }`，`None` = 关）。
    ///
    /// ⚠️ 选轨是**排队**的，所以刚调完 [`select_audio`](Self::select_audio) /
    /// [`select_subtitle`](Self::select_subtitle) 之后这里可能还是旧值 ——
    /// 最多滞后一条命令（和 `volume()` / `rate()` 一样）。
    pub fn selection(&self) -> TrackSelection {
        self.player.media_track_selection()
    }

    /// 选音轨（`None` = 关掉音频）。**排队**，立刻返回。
    ///
    /// presenter 会先 quiesce 帧输出、再 flush 音频环（否则旧音轨的 PCM 会继续响几百毫秒）、
    /// 清一次视觉、并 bump 弹幕代次 —— **一次换轨不便宜**，别让 UI 连续触发
    /// （做成"选完就收的下拉框"，别做成"逐个试听"）。
    pub fn select_audio(&self, track_id: Option<i64>) {
        self.player.send_settings(SettingsCmd::AudioTrack(track_id));
    }

    /// 选字幕轨（`None` = 关字幕）。同样排队、同样会 flush 音频环。
    ///
    /// `track_id` 既可以是容器里的字幕轨，也可以是
    /// [`add_external_subtitle`](Self::add_external_subtitle) 返回的外部字幕 id。
    pub fn select_subtitle(&self, track_id: Option<i64>) {
        self.player
            .send_settings(SettingsCmd::SubtitleTrack(track_id));
    }

    /// 挂一个外部字幕文件（srt / ass / …）。
    ///
    /// **同步阻塞**：内部有应答通道，会等字幕文件真正打开完才返回 ——
    /// 别放在 60fps 的路径上（也不要在渲染期直接调）。
    ///
    /// 返回的 [`SubtitleTrackConfig::id`] **直接拿去
    /// [`select_subtitle`](Self::select_subtitle)**，别去 [`list`](Self::list) 里找：
    /// 列表是 worker 同步过来的，挂完那一刻可能还没刷新（有短暂空窗）。
    ///
    /// （presenter 对这个也是**纯转发**，没有音频协调动作，所以走 `raw()` 不算绕过协调。）
    pub fn add_external_subtitle(&self, uri: impl Into<String>) -> erika::Result<SubtitleTrackConfig> {
        self.player.raw().add_external_subtitle(uri)
    }

    /// 摘掉一条外部字幕轨（只有 `TrackInfo::can_remove == true` 的能摘）。
    ///
    /// **直连**（纯转发，发完即返回）；给的不是外部字幕 id 时上游返回 `Err`。
    pub fn remove_subtitle(&self, track_id: i64) -> erika::Result<()> {
        self.player.raw().remove_subtitle_track(track_id)
    }
}

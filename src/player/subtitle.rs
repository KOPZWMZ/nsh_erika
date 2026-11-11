//! 字幕:[`ErikaSubtitle`] 句柄 —— 外观 / 版面 / 延迟。
//!
//! 从 [`ErikaPlayer::subtitle`](super::ErikaPlayer::subtitle) 取。上游把字幕样式放在
//! 一个整包里(`SubtitleStyleConfig`,20 个字段 + 一个 `override_mask`),
//! 所以这里和弹幕一样**按屏幕上的块分组**,组内整组覆盖、组间互不干扰:
//!
//! | 组 | 类型 | 管什么 | 读回 | 代价 |
//! |---|---|---|---|---|
//! | 外观 | [`SubtitleTextStyle`] | 字体 / 颜色 / 字形 / 描边 / 阴影 / 缩放 + 覆盖开关 | ✅ | libass 重排当前字幕 |
//! | 版面 | [`SubtitlePlacement`] | 对齐 + 三个边距 | ✅ | 同上 |
//! | 延迟 | 直接一个 `i64` | 字幕整体早/晚多少微秒 | ✅ | 只挪时间轴 |
//!
//! 三条要记住的:
//!
//! 1. **字幕没有"只重绘"的字段**:任何一次样式改动都会让 libass 重新排版当前显示的那几行
//!    (`refresh_current_overlay()`)。量级比弹幕小(只影响当前字幕),面板可以实时拖,
//!    但别按每个输入字符就发一条。
//! 2. **改名/改色可能"没生效"**:上游默认 `override_mask = 0`,意思是我们的样式只当
//!    **fallback**,容器里的 ASS 脚本保留自己的样式。要压过去就把
//!    [`SubtitleOverrides`] 设成 [`force_all()`](SubtitleOverrides::force_all)。
//! 3. **`font_scale` / `supersampling` 现在没封**:上游只有 setter 没有 getter
//!    (`subtitle_delay_micros` / `subtitle_style` 有),封了也没法给面板初值 ——
//!    真要调请走 [`ErikaPlayer::call`](super::ErikaPlayer::call),等上游补 getter 再封。

use erika::subtitle::SubtitleStyleConfig;

use super::ErikaPlayer;
use crate::cmd::SubtitleCmd;

mod placement;
mod style;

pub use placement::SubtitlePlacement;
pub use style::{SubtitleOverrides, SubtitleTextStyle};

/// 字幕句柄:样式(外观 / 版面)与延迟的读写。
///
/// 从 [`ErikaPlayer::subtitle`](super::ErikaPlayer::subtitle) 取;`Clone` 只是句柄克隆,
/// 同一个播放器派生出来的互相 `==`。
pub struct ErikaSubtitle {
    player: ErikaPlayer,
}

impl Clone for ErikaSubtitle {
    fn clone(&self) -> Self {
        Self {
            player: self.player.clone(),
        }
    }
}
impl PartialEq for ErikaSubtitle {
    fn eq(&self, other: &Self) -> bool {
        self.player == other.player
    }
}
impl Eq for ErikaSubtitle {}

impl ErikaSubtitle {
    /// 建一个句柄(只给 `ErikaPlayer::subtitle` 用)。
    pub(super) fn new(player: ErikaPlayer) -> Self {
        Self { player }
    }

    /// 当前字幕设置的快照(外观 / 版面 / 延迟)。读引擎发布的状态,非阻塞。
    ///
    /// 和 `status().subtitle` 是同一份东西;面板初始化一律读这里,
    /// **别**用 `SubtitleTextStyle::default()`(那是"恢复默认")。
    pub fn state(&self) -> SubtitleSnapshot {
        self.player.status().subtitle
    }

    /// 改字幕**外观**(字体 / 颜色 / 字形 / 描边 / 阴影 / 缩放 / 覆盖开关)。
    ///
    /// ⚠️ 会让 libass **重排当前字幕**;组外字段(对齐 / 边距 / 延迟)一个都不动。
    ///
    /// ```ignore
    /// let mut style = player.subtitle().state().style;
    /// style.font_size = 64.0;
    /// style.overrides = SubtitleOverrides::force_all();   // 否则容器脚本的样式会赢
    /// player.subtitle().set_style(style);
    /// ```
    pub fn set_style(&self, style: SubtitleTextStyle) {
        self.player.send_subtitle(SubtitleCmd::TextStyle(style));
    }

    /// 改字幕**版面**(对齐 + 左/右/竖直边距)。
    ///
    /// ⚠️ 同理会让 libass 重排;而且**只有 `overrides.alignment` / `overrides.margins`
    /// 打开时才会压过脚本** —— 默认(`none`)下容器 ASS 轨的对齐说了算。
    pub fn set_placement(&self, placement: SubtitlePlacement) {
        self.player.send_subtitle(SubtitleCmd::Placement(placement));
    }

    /// 字幕延迟(微秒):**正数 = 字幕晚出**,负数 = 早出。上游夹取 ±600 秒。
    ///
    /// "字幕和声音差半秒"这类修正用它;不重排字形,只挪时间轴 ——
    /// 但会连带改"字幕帧保留时长"(`subtitles.lead`),所以拖动时松手再提交。
    pub fn set_delay(&self, delay_micros: i64) {
        self.player.send_subtitle(SubtitleCmd::Delay(delay_micros));
    }
}

/// 字幕设置的快照。
///
/// 字幕整块住在 presenter 上(`erika::Player` 只带轨道,不带样式),所以只能由引擎
/// 采样后发布过来。低频:只在用户改样式/延迟时变。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubtitleSnapshot {
    /// 外观组当前值(= 上游 `SubtitleStyleConfig` 里那 17 个字段)。
    pub style: SubtitleTextStyle,
    /// 版面组当前值(对齐 + 三个边距)。
    pub placement: SubtitlePlacement,
    /// 字幕延迟(微秒,正 = 晚出)。
    pub delay_micros: i64,
}

impl SubtitleSnapshot {
    /// 从上游整包样式 + 延迟拼出快照(引擎发布时用)。
    pub(crate) fn from_parts(style: &SubtitleStyleConfig, delay_micros: i64) -> Self {
        Self {
            style: SubtitleTextStyle::from(style),
            placement: SubtitlePlacement::from(style),
            delay_micros,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 快照的两个组来自同一个整包,但落在各自的字段上 —— 别把对齐塞进外观组。
    #[test]
    fn snapshot_splits_one_config_into_two_groups() {
        let upstream = SubtitleStyleConfig {
            font_size: 72.0,
            alignment: 8,
            margin_left: 10,
            ..Default::default()
        };

        let snapshot = SubtitleSnapshot::from_parts(&upstream, 250_000);
        assert_eq!(snapshot.style.font_size, 72.0);
        assert_eq!(snapshot.placement.alignment, 8);
        assert_eq!(snapshot.placement.margin_left, 10);
        assert_eq!(snapshot.delay_micros, 250_000);
    }
}

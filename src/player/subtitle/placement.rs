//! 字幕**版面**组:对齐 + 三个边距。
//!
//! 和 [`style`](super::style) 一起写进上游同一个 `SubtitleStyleConfig`;
//! 本组的 `apply` **不碰** `override_mask`(那个开关归外观组,因为它同时管对齐全家)。
//!
//! ⚠️ 对齐和边距只有在 `overrides.alignment` / `overrides.margins` 打开时才压过脚本 ——
//! 默认(`none`)下容器 ASS 脚本的对齐会赢。

use erika::subtitle::SubtitleStyleConfig;

/// 字幕的**版面**:对齐方式 + 左 / 右 / 竖直边距。
///
/// 边距单位是 **ASS 脚本单位**,不是屏幕像素 —— 上游文档写得很清楚:
/// libass 不会拿脚本分辨率归一化边距,所以普通文本字幕(生成的脚本按帧尺寸写)里
/// 它们≈像素,而容器里的 ASS 轨则相对它自己声明的 `PlayResY`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubtitlePlacement {
    /// 对齐方式,ASS 小键盘 1..=9(默认 2 = 底部居中)。上游越界会归成 2。
    ///
    /// ```text
    /// 7 8 9     左上 中上 右上
    /// 4 5 6  =  左中 正中 右中
    /// 1 2 3     左下 中下 右下
    /// ```
    pub alignment: i32,
    /// 左边距(ASS 脚本单位)。上游夹取 `0..=10000`,默认 48。
    pub margin_left: i32,
    /// 右边距(ASS 脚本单位)。上游夹取 `0..=10000`,默认 48。
    pub margin_right: i32,
    /// 竖直边距(底对齐时是下边距,顶对齐时是上边距)。夹取 `0..=10000`,默认 54。
    pub margin_vertical: i32,
}

impl SubtitlePlacement {
    /// 只覆盖本组字段,其它组一律不动(`override_mask` 也不碰)。
    pub(crate) fn apply(&self, style: &mut SubtitleStyleConfig) {
        style.alignment = self.alignment;
        style.margin_left = self.margin_left;
        style.margin_right = self.margin_right;
        style.margin_vertical = self.margin_vertical;
    }
}

impl Default for SubtitlePlacement {
    fn default() -> Self {
        // 必须与上游 `SubtitleStyleConfig::default()` 的对应字段相等。
        Self {
            alignment: 2,
            margin_left: 48,
            margin_right: 48,
            margin_vertical: 54,
        }
    }
}

impl From<&SubtitleStyleConfig> for SubtitlePlacement {
    fn from(value: &SubtitleStyleConfig) -> Self {
        Self {
            alignment: value.alignment,
            margin_left: value.margin_left,
            margin_right: value.margin_right,
            margin_vertical: value.margin_vertical,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::style::SubtitleTextStyle;

    #[test]
    fn our_defaults_match_upstream() {
        let upstream = SubtitleStyleConfig::default();
        assert_eq!(SubtitlePlacement::from(&upstream), SubtitlePlacement::default());
    }

    #[test]
    fn placement_apply_leaves_the_style_group_alone() {
        let before = SubtitleStyleConfig::default();
        let mut style = before.clone();
        SubtitlePlacement {
            alignment: 8,
            ..Default::default()
        }
        .apply(&mut style);

        assert_eq!(
            style,
            SubtitleStyleConfig {
                alignment: 8,
                ..before
            }
        );
    }

    /// 外观组的 `apply` 会写 `override_mask`(它同时管对齐全家),
    /// 但版面组的 `apply` **必须**留它不动 —— 否则两组会互相冲掉掩码。
    #[test]
    fn placement_never_touches_the_override_mask() {
        let before = SubtitleStyleConfig {
            override_mask: 0x0000_07fc,
            ..Default::default()
        };
        let mut style = before.clone();
        SubtitlePlacement::default().apply(&mut style);

        assert_eq!(style.override_mask, before.override_mask);

        // 外观组确实会写它(对照,防止上面那条测的是"谁都不写")
        let mut style = before.clone();
        SubtitleTextStyle::default().apply(&mut style);
        assert_eq!(style.override_mask, 0);
    }
}

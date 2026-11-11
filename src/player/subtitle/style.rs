//! 字幕**外观**组:字体 / 颜色 / 字形 / 描边 / 阴影 / 缩放 + 覆盖总开关。
//!
//! 这一组和 [`placement`](super::placement) 一起写进上游的同一个
//! `SubtitleStyleConfig` —— 所以两者各自有 `apply`,引擎按"读真值 → 本组 apply → 写回"
//! 处理,组间不会互相覆盖(唯一的交集是 [`SubtitleOverrides`],见它的文档)。
//!
//! 与弹幕不同,**字幕没有"只重绘"的字段**:任何一次样式改动都会让 libass 重新排版
//! 当前显示的那几行(`refresh_current_overlay()`)。量级比弹幕小(只影响当前字幕),
//! 所以面板可以实时拖,但别按每个字符提交。

use erika::subtitle::{
    SUBTITLE_OVERRIDE_ALIGNMENT, SUBTITLE_OVERRIDE_ALL, SUBTITLE_OVERRIDE_ATTRIBUTES,
    SUBTITLE_OVERRIDE_BLUR, SUBTITLE_OVERRIDE_BORDER, SUBTITLE_OVERRIDE_COLORS,
    SUBTITLE_OVERRIDE_FONT_NAME, SUBTITLE_OVERRIDE_FONT_SIZE_FIELDS, SUBTITLE_OVERRIDE_MARGINS,
    SubtitleStyleConfig,
};

/// 哪些配置**压过**字幕脚本自带的样式。
///
/// 这是上游 `SubtitleStyleConfig::override_mask`(一个 `u32` 位掩码)的类型化版本。
/// 语义是:**默认(`none()`)时我们的字体/颜色只当 fallback** —— 容器里的 ASS 脚本保留
/// 自己的样式,我们只填脚本没指定的部分。所以"我改了字号没生效"通常不是 bug,是这个开关没开。
///
/// | 构造 | 位 | 说明 |
/// |---|---|---|
/// | [`none()`](Self::none) | 0 | 默认。脚本优先,配置只补空缺 |
/// | [`legacy_force()`](Self::legacy_force) | 字号/字体名/颜色/描边 | 对应上游的 `LEGACY_FORCE` |
/// | [`force_all()`](Self::force_all) | 全部 | 我们的样式完全压过脚本 |
///
/// ⚠️ 它在"外观"组里,但它**同时管版面组的对齐与边距** —— 这是唯一跨组的字段
/// (上游只有一个掩码,没法拆)。版面组的 `apply` 不碰它。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubtitleOverrides {
    /// 字号 / 描边宽度等度量。
    pub font_size: bool,
    /// 字体族。
    pub font_name: bool,
    /// 主色 / 描边色。
    pub colors: bool,
    /// 粗体 / 斜体 / 下划线 / 删除线 / 字距。
    pub attributes: bool,
    /// 描边 / 阴影 / 边框样式。
    pub border: bool,
    /// 对齐方式。
    pub alignment: bool,
    /// 三个边距。
    pub margins: bool,
    /// 模糊。
    pub blur: bool,
}

impl SubtitleOverrides {
    /// 一个位都不设:脚本优先,我们的样式只当 fallback(= 上游默认)。
    pub const fn none() -> Self {
        Self {
            font_size: false,
            font_name: false,
            colors: false,
            attributes: false,
            border: false,
            alignment: false,
            margins: false,
            blur: false,
        }
    }

    /// 上游的 `LEGACY_FORCE`:字号 / 字体名 / 颜色 / 描边 —— 最常见的"我要我的样式"档。
    pub const fn legacy_force() -> Self {
        Self {
            font_size: true,
            font_name: true,
            colors: true,
            border: true,
            ..Self::none()
        }
    }

    /// 全部打开:我们的样式完全压过脚本(连对齐和边距也是)。
    pub const fn force_all() -> Self {
        Self {
            font_size: true,
            font_name: true,
            colors: true,
            attributes: true,
            border: true,
            alignment: true,
            margins: true,
            blur: true,
        }
    }

    /// 转成上游的位掩码。
    fn to_bits(self) -> u32 {
        let mut bits = 0;
        bits |= if self.font_size { SUBTITLE_OVERRIDE_FONT_SIZE_FIELDS } else { 0 };
        bits |= if self.font_name { SUBTITLE_OVERRIDE_FONT_NAME } else { 0 };
        bits |= if self.colors { SUBTITLE_OVERRIDE_COLORS } else { 0 };
        bits |= if self.attributes { SUBTITLE_OVERRIDE_ATTRIBUTES } else { 0 };
        bits |= if self.border { SUBTITLE_OVERRIDE_BORDER } else { 0 };
        bits |= if self.alignment { SUBTITLE_OVERRIDE_ALIGNMENT } else { 0 };
        bits |= if self.margins { SUBTITLE_OVERRIDE_MARGINS } else { 0 };
        bits |= if self.blur { SUBTITLE_OVERRIDE_BLUR } else { 0 };
        bits & SUBTITLE_OVERRIDE_ALL
    }

    /// 从上游位掩码还原(未知位直接忽略 —— 上游本来也会 `&= ALL`)。
    fn from_bits(bits: u32) -> Self {
        let has = |bit: u32| bits & bit != 0;
        Self {
            font_size: has(SUBTITLE_OVERRIDE_FONT_SIZE_FIELDS),
            font_name: has(SUBTITLE_OVERRIDE_FONT_NAME),
            colors: has(SUBTITLE_OVERRIDE_COLORS),
            attributes: has(SUBTITLE_OVERRIDE_ATTRIBUTES),
            border: has(SUBTITLE_OVERRIDE_BORDER),
            alignment: has(SUBTITLE_OVERRIDE_ALIGNMENT),
            margins: has(SUBTITLE_OVERRIDE_MARGINS),
            blur: has(SUBTITLE_OVERRIDE_BLUR),
        }
    }
}

/// 字幕的**外观**:字体 / 颜色 / 字形 / 描边 / 阴影 / 缩放。
///
/// 读写共用同一类型:写 [`ErikaSubtitle::set_style`](super::ErikaSubtitle::set_style),
/// 读 [`SubtitleSnapshot::style`](super::SubtitleSnapshot::style)。
///
/// 颜色用 **RGBA 四字节**(和上游的 `0xRRGGBBAA` 一一对应,但不会让人把
/// `AA` 和 `RR` 写反)。所有度量值上游都会 `normalized()` 夹一遍,
/// 这里在字段文档里标了范围 —— 面板滑杆直接用这些范围,别制造越界值。
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleTextStyle {
    /// 字体族。空 = 用平台/libtass 默认。
    pub font_family: String,
    /// 字体文件路径(本地)。空 = 不额外加载字体。
    pub font_file_path: String,
    /// 基准字号(ASS 脚本单位)。上游夹取 `8.0..=400.0`,默认 48。
    pub font_size: f64,
    /// 描边宽度。上游夹取 `0.0..=32.0`,默认 2。
    pub outline_width: f64,
    /// 主色 RGBA。默认不透明白。
    pub primary_color: [u8; 4],
    /// 描边 / 阴影颜色 RGBA。默认半透明黑(`[0, 0, 0, 127]`)。
    pub outline_color: [u8; 4],
    /// 粗体。
    pub bold: bool,
    /// 斜体。
    pub italic: bool,
    /// 下划线。
    pub underline: bool,
    /// 删除线。
    pub strike_out: bool,
    /// 字距。上游夹取 `-100.0..=100.0`,默认 0。
    pub spacing: f64,
    /// 横向缩放百分比。上游夹取 `1.0..=1000.0`,默认 100。
    pub scale_x_percent: f64,
    /// 纵向缩放百分比。上游夹取 `1.0..=1000.0`,默认 100。
    ///
    /// ⚠️ 这两个是 ASS 的"把字幕故意拉宽/压扁"。**别拿它们去补偿比例问题** ——
    /// 画面/字幕的比例由控件负责：`erika.video` 按视频宽高比算内接矩形出帧
    /// （`component.rs::export_size`），erika 拿这个尺寸当 libass 的 `PlayRes`，
    /// 所以字幕按它排版之后不会再被 aspect-fit 拉伸。
    pub scale_y_percent: f64,
    /// 边框样式:`1` = 描边 + 阴影(默认),`3` = 不透明底框;其它值上游会归成 `1`。
    pub border_style: i32,
    /// 阴影深度。上游夹取 `0.0..=32.0`,默认 0。
    pub shadow_depth: f64,
    /// 模糊(高斯边缘)。上游夹取 `0.0..=100.0`,默认 0。
    pub blur: f64,
    /// 哪些字段压过脚本自带的样式(默认 [`SubtitleOverrides::none`] = 脚本优先)。
    pub overrides: SubtitleOverrides,
}

impl SubtitleTextStyle {
    /// 只覆盖本组字段(以及那个跨组的 `override_mask`),其它组一律不动。
    ///
    /// `pub(crate)` + 纯函数:单测喂一个 `SubtitleStyleConfig::default()` 就能验,
    /// 不需要 presenter / 引擎线程。
    pub(crate) fn apply(&self, style: &mut SubtitleStyleConfig) {
        style.font_family = self.font_family.clone();
        style.font_file_path = self.font_file_path.clone();
        style.font_size = self.font_size;
        style.outline_width = self.outline_width;
        style.primary_color_rgba = u32::from_be_bytes(self.primary_color);
        style.outline_color_rgba = u32::from_be_bytes(self.outline_color);
        style.bold = self.bold;
        style.italic = self.italic;
        style.underline = self.underline;
        style.strike_out = self.strike_out;
        style.spacing = self.spacing;
        style.scale_x_percent = self.scale_x_percent;
        style.scale_y_percent = self.scale_y_percent;
        style.border_style = self.border_style;
        style.shadow_depth = self.shadow_depth;
        style.blur = self.blur;
        style.override_mask = self.overrides.to_bits();
    }
}

impl Default for SubtitleTextStyle {
    fn default() -> Self {
        // 必须与上游 `SubtitleStyleConfig::default()` 逐字段相等
        // (颜色常量见 `subtitle.rs:761/763`,字号/描边见 2398-2402)。
        Self {
            font_family: String::new(),
            font_file_path: String::new(),
            font_size: 48.0,
            outline_width: 2.0,
            primary_color: [255, 255, 255, 255],
            outline_color: [0, 0, 0, 127],
            bold: false,
            italic: false,
            underline: false,
            strike_out: false,
            spacing: 0.0,
            scale_x_percent: 100.0,
            scale_y_percent: 100.0,
            border_style: 1,
            shadow_depth: 0.0,
            blur: 0.0,
            overrides: SubtitleOverrides::none(),
        }
    }
}

impl From<&SubtitleStyleConfig> for SubtitleTextStyle {
    fn from(value: &SubtitleStyleConfig) -> Self {
        Self {
            font_family: value.font_family.clone(),
            font_file_path: value.font_file_path.clone(),
            font_size: value.font_size,
            outline_width: value.outline_width,
            primary_color: value.primary_color_rgba.to_be_bytes(),
            outline_color: value.outline_color_rgba.to_be_bytes(),
            bold: value.bold,
            italic: value.italic,
            underline: value.underline,
            strike_out: value.strike_out,
            spacing: value.spacing,
            scale_x_percent: value.scale_x_percent,
            scale_y_percent: value.scale_y_percent,
            border_style: value.border_style,
            shadow_depth: value.shadow_depth,
            blur: value.blur,
            overrides: SubtitleOverrides::from_bits(value.override_mask),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_defaults_match_upstream() {
        let upstream = SubtitleStyleConfig::default();
        assert_eq!(SubtitleTextStyle::from(&upstream), SubtitleTextStyle::default());
    }

    #[test]
    fn style_apply_leaves_placement_alone() {
        let before = SubtitleStyleConfig::default();
        let mut style = before.clone();
        SubtitleTextStyle {
            font_size: 64.0,
            ..Default::default()
        }
        .apply(&mut style);

        assert_eq!(
            style,
            SubtitleStyleConfig {
                font_size: 64.0,
                ..before
            }
        );
    }

    /// 颜色是 `0xRRGGBBAA`:走 `[u8; 4]` 时不能把通道写反。
    #[test]
    fn colors_map_to_rrggbbaa() {
        let mut style = SubtitleStyleConfig::default();
        SubtitleTextStyle {
            primary_color: [10, 20, 30, 40],
            outline_color: [1, 2, 3, 4],
            ..Default::default()
        }
        .apply(&mut style);

        assert_eq!(style.primary_color_rgba, 0x0a14_1e28);
        assert_eq!(style.outline_color_rgba, 0x0102_0304);
        assert_eq!(style.primary_color_rgba.to_be_bytes(), [10, 20, 30, 40]);
    }

    /// 掩码:默认 0、`force_all` 覆盖全部已知位、往返一致。
    #[test]
    fn overrides_map_to_the_upstream_mask() {
        assert_eq!(SubtitleOverrides::none().to_bits(), 0);
        assert_eq!(
            SubtitleOverrides::from_bits(SUBTITLE_OVERRIDE_ALL),
            SubtitleOverrides::force_all()
        );
        assert_eq!(
            SubtitleOverrides::from_bits(SUBTITLE_OVERRIDE_ATTRIBUTES),
            SubtitleOverrides {
                attributes: true,
                ..SubtitleOverrides::none()
            }
        );
        assert_eq!(
            SubtitleOverrides::from_bits(SUBTITLE_OVERRIDE_BLUR),
            SubtitleOverrides {
                blur: true,
                ..SubtitleOverrides::none()
            }
        );
        // legacy_force 是 ALL 的子集
        assert_eq!(
            SubtitleOverrides::legacy_force().to_bits() & !SUBTITLE_OVERRIDE_ALL,
            0
        );
    }
}

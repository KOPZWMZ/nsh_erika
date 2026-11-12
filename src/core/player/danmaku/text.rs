//! 字体组：字号 / 超采样 / 自定义字体。
//!
//! 这一组**重排 + 重建光栅器**（上游 `apply_config` 里 `font_changed` 会重建
//! `DanmakuTextRasterizer`），所以面板必须**松手/选完文件才提交**。
//! 见 [`super`] 的分组表。

use erika::danmaku::DanmakuLayoutConfig;

use super::ErikaDanmaku;
use crate::core::cmd::DanmakuCmd;

/// 弹幕的**文字与字体**。
///
/// 四个字段在上游是**一起影响排版与光栅**的：`font_size` 参与排版，
/// `supersampling` / `custom_font_*` 还会重建字形光栅器（比单纯重排更贵）。
///
/// 字体两个字段（`custom_font_family` + `custom_font_file_path`）放在同一个结构里是
/// **故意的**：它们必须成对提交 —— 只改一半等于拿旧字体名配新字体文件。
///
/// ⚠️ 上游还有一个 `custom_font_face_index`（`#[doc(hidden)]`，TTC 集合里的 face 序号），
/// **不归任何组**：`apply` 不碰它（保持引擎当前值）。真要用请走
/// [`ErikaDanmaku::update_config`] 逃生舱。
///
/// 读写共用同一类型：写 [`ErikaDanmaku::set_text`]，读
/// [`DanmakuSnapshot::text`](super::DanmakuSnapshot::text)。
#[derive(Debug, PartialEq, Clone)]
pub struct DanmakuText {
    /// 基准字号（上游默认 30）。上游夹取 `≥ 1.0`，**没有上限** ——
    /// 面板自己要定一个（比如 ≤ 96），否则字号能大到只剩一两个字。
    pub font_size: f32,
    /// 光栅按 2× 分辨率出字形（默认开）。关掉省 CPU，但大字号会明显发虚。
    pub supersampling: bool,
    /// 自定义字体族名。空字符串 = 用默认字体。
    pub custom_font_family: String,
    /// 自定义字体文件路径（本地路径）。空字符串 = 用默认字体。
    pub custom_font_file_path: String,
}

impl DanmakuText {
    /// 只覆盖本组字段，其它组的字段一律不动（`custom_font_face_index` 也不碰）。
    pub(crate) fn apply(&self, cfg: &mut DanmakuLayoutConfig) {
        cfg.font_size = self.font_size;
        cfg.supersampling = self.supersampling;
        cfg.custom_font_family = self.custom_font_family.clone();
        cfg.custom_font_file_path = self.custom_font_file_path.clone();
    }
}

impl Default for DanmakuText {
    fn default() -> Self {
        // 必须与上游 `DanmakuLayoutConfig::default()` 逐字段相等：
        // 30.0 对 `typography.rs` 的 DEFAULT_CONFIG_FONT_SIZE，其余见上游 Default。
        Self {
            font_size: 30.0,
            supersampling: true,
            custom_font_family: String::new(),
            custom_font_file_path: String::new(),
        }
    }
}

impl From<&DanmakuLayoutConfig> for DanmakuText {
    fn from(value: &DanmakuLayoutConfig) -> Self {
        Self {
            font_size: value.font_size,
            supersampling: value.supersampling,
            custom_font_family: value.custom_font_family.clone(),
            custom_font_file_path: value.custom_font_file_path.clone(),
        }
    }
}

impl ErikaDanmaku {
    /// 改弹幕的**文字与字体**（字号 / 超采样 / 自定义字体）。
    ///
    /// ⚠️ **会重排、可能重建光栅器**：面板要"松手才提交"，别按每个 `pointermove` 发。
    /// 组外字段一个都不会动。
    ///
    /// ```ignore
    /// let mut text = player.danmaku().state().text;
    /// text.font_size = 36.0;
    /// player.danmaku().set_text(text);
    /// ```
    ///
    /// 整组覆盖的契约同 [`set_paint`](Self::set_paint)：这一组只应有一个 UI 拥有者。
    pub fn set_text(&self, text: DanmakuText) {
        self.send(DanmakuCmd::UpdateConfig(Box::new(move |cfg| {
            text.apply(cfg);
        })));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_defaults_match_upstream() {
        let upstream = DanmakuLayoutConfig::default();
        assert_eq!(DanmakuText::from(&upstream), DanmakuText::default());
    }

    #[test]
    fn text_apply_leaves_other_groups_alone() {
        let before = DanmakuLayoutConfig::default();
        let mut cfg = before.clone();
        DanmakuText {
            font_size: 36.0,
            ..Default::default()
        }
        .apply(&mut cfg);

        assert_eq!(
            cfg,
            DanmakuLayoutConfig {
                font_size: 36.0,
                ..before
            }
        );
    }

    /// 字体两字段必须一起进去（同一个结构体，物理上拆不开）。
    #[test]
    fn text_apply_writes_both_font_fields() {
        let mut cfg = DanmakuLayoutConfig::default();
        DanmakuText {
            custom_font_family: "Noto Sans CJK SC".into(),
            custom_font_file_path: "/tmp/noto.otf".into(),
            ..Default::default()
        }
        .apply(&mut cfg);
        assert_eq!(cfg.custom_font_family, "Noto Sans CJK SC");
        assert_eq!(cfg.custom_font_file_path, "/tmp/noto.otf");
    }
}

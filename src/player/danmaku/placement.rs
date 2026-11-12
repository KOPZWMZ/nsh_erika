//! 版面组：显示区域 / 滚动速度 / 轨道间距 / 描边宽度。
//!
//! 这一组**全部会重排**（含看起来像"绘制"的 `outline_width` —— 它参与排版测量），
//! 所以面板必须松手才提交。见 [`super`] 的分组表。

use erika::danmaku::DanmakuLayoutConfig;

use super::ErikaDanmaku;
use crate::cmd::DanmakuCmd;

/// 弹幕的**版面与节奏**。
///
/// 读写共用同一类型：写 [`ErikaDanmaku::set_placement`]，读
/// [`DanmakuSnapshot::placement`](super::DanmakuSnapshot::placement)。
///
/// # 滚动速度只有一个旋钮，上游有两个字段
///
/// 上游的滚动时长算出来是:
///
/// ```text
/// 每屏耗时 = scroll_duration_seconds × 宽度系数(按窗口宽缩放) ÷ scroll_speed_factor
/// ```
///
/// 这两个字段是**同一根轴**（前者是基准、后者是倍数），所以这里只暴露
/// `scroll_speed`（= 上游 `scroll_speed_factor`，1.0 为默认）。
/// `scroll_duration_seconds` **不归任何组**（保持引擎当前值，默认 10s）；
/// 真需要改它就走 [`ErikaDanmaku::update_config`] 逃生舱 —— 两个一起调只会互相稀释。
///
/// # `outline_width` 为什么在"版面"而不是"绘制"
///
/// 描边宽度会参与字形的排版测量（上游把它传进 `prepare_layout`），改了要重排。
/// 放进 [`DanmakuPaint`](super::DanmakuPaint) 会让那一组失去"可以实时拖"的前提。
#[derive(Debug, PartialEq, Clone)]
pub struct DanmakuPlacement {
    /// 弹幕可占的屏幕高度比例。上游夹取 `0.1..=1.0`（默认 1.0）——
    /// 面板滑杆的 min/max 直接用这个范围。
    pub display_area: f32,
    /// 滚动速度倍率。上游夹取 `0.2..=4.0`（默认 1.0，越大越快）。
    /// 映射到上游的 `scroll_speed_factor`，见上面的模块文档。
    pub scroll_speed: f32,
    /// 轨道间距（相对行高的比例）。上游夹取 `0.0..=2.0`（默认 0.15）。
    pub track_gap_ratio: f32,
    /// 描边宽度（像素）。上游夹取 `0.0..=4.0`（默认 1.0）。
    /// ⚠️ 它**参与排版测量**，改它会重排。
    pub outline_width: f32,
}

impl DanmakuPlacement {
    /// 只覆盖本组字段，其它组的字段一律不动（`scroll_duration_seconds` 也不碰）。
    pub(crate) fn apply(&self, cfg: &mut DanmakuLayoutConfig) {
        cfg.display_area = self.display_area;
        cfg.scroll_speed_factor = self.scroll_speed;
        cfg.track_gap_ratio = self.track_gap_ratio;
        cfg.outline_width = self.outline_width;
    }
}

impl Default for DanmakuPlacement {
    fn default() -> Self {
        // 必须与上游 `DanmakuLayoutConfig::default()` 逐字段相等
        // （`scroll_speed` 对应 `scroll_speed_factor` 的默认值 1.0）。
        Self {
            display_area: 1.0,
            scroll_speed: 1.0,
            track_gap_ratio: 0.15,
            outline_width: 1.0,
        }
    }
}

impl From<&DanmakuLayoutConfig> for DanmakuPlacement {
    fn from(value: &DanmakuLayoutConfig) -> Self {
        Self {
            display_area: value.display_area,
            scroll_speed: value.scroll_speed_factor,
            track_gap_ratio: value.track_gap_ratio,
            outline_width: value.outline_width,
        }
    }
}

impl ErikaDanmaku {
    /// 改弹幕的**版面与节奏**（显示区域 / 滚动速度 / 轨道间距 / 描边）。
    ///
    /// ⚠️ **会重排**：面板要"松手才提交"，别按每个 `pointermove` 发。
    /// 组外字段一个都不会动（包括 `scroll_duration_seconds`）。
    ///
    /// ```ignore
    /// let mut p = player.danmaku().state().placement;
    /// p.display_area = 0.5;   // 只用上半屏
    /// player.danmaku().set_placement(p);
    /// ```
    ///
    /// 整组覆盖的契约同 [`set_paint`](Self::set_paint)：这一组只应有一个 UI 拥有者。
    pub fn set_placement(&self, placement: DanmakuPlacement) {
        self.send(DanmakuCmd::UpdateConfig(Box::new(move |cfg| {
            placement.apply(cfg);
        })));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_defaults_match_upstream() {
        let upstream = DanmakuLayoutConfig::default();
        assert_eq!(
            DanmakuPlacement::from(&upstream),
            DanmakuPlacement::default()
        );
    }

    #[test]
    fn placement_apply_leaves_other_groups_alone() {
        let before = DanmakuLayoutConfig::default();
        let mut cfg = before.clone();
        DanmakuPlacement {
            display_area: 0.5,
            ..Default::default()
        }
        .apply(&mut cfg);

        assert_eq!(
            cfg,
            DanmakuLayoutConfig {
                display_area: 0.5,
                ..before
            }
        );
    }

    /// 滚动速度只有一个旋钮：写进 `scroll_speed_factor`，
    /// **不碰** `scroll_duration_seconds`（那是另一根同名轴，留给逃生舱）。
    #[test]
    fn scroll_speed_maps_to_factor_only() {
        let before = DanmakuLayoutConfig::default();
        let mut cfg = before.clone();
        DanmakuPlacement {
            scroll_speed: 2.0,
            ..Default::default()
        }
        .apply(&mut cfg);

        assert_eq!(cfg.scroll_speed_factor, 2.0);
        assert_eq!(
            cfg.scroll_duration_seconds, before.scroll_duration_seconds,
            "滚动时长不归这一组管"
        );
    }
}

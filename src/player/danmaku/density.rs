//! 密度组：同屏条数 / 每模式行数 / 去重 / 堆叠 / 追尾覆盖。
//!
//! 这一组**全部会重排**，而且是"离散开关"型设置（没有滑杆），
//! 点一下提交一次就行。见 [`super`] 的分组表。

use std::num::NonZeroU32;

use erika::danmaku::DanmakuLayoutConfig;

use super::ErikaDanmaku;
use crate::cmd::DanmakuCmd;

/// 弹幕的**密度与拥挤策略**。
///
/// 读写共用同一类型：写 [`ErikaDanmaku::set_density`]，读
/// [`DanmakuSnapshot::density`](super::DanmakuSnapshot::density)。
///
/// # 两个上限为什么是 `Option<NonZeroU32>`
///
/// 上游这两个字段是 `Option<u32>`，而且**`0` 的语义刚好相反**：
///
/// | 字段 | `None` | `Some(0)` |
/// |---|---|---|
/// | `max_quantity` | 不限 | **等于不限**（`0` 直接短路成"不过滤"） |
/// | `max_lines_per_mode` | 不限 | **全屏蔽该模式**（`0` 让每一行都超限） |
///
/// 把这种"同一个值两种意思"透传给面板，早晚有人踩。所以这里统一成
/// `Option<NonZeroU32>`（`None` = 不限），**`Some(0)` 在读侧映射成 `None`**。
///
/// ⚠️ 代价说明：如果你之前用逃生舱把 `max_lines_per_mode` 设成了 `Some(0)`（= 全屏蔽），
/// 面板读一次再写回就会变成"不限"。要"屏蔽某几类弹幕"请用
/// `DanmakuFilter` 的 `blocked_modes`，那才是它该用的地方。
#[derive(Debug, PartialEq, Clone, Default)]
pub struct DanmakuDensity {
    /// 同屏密度上限（条数）。`None` = 不限。
    ///
    /// 上游的算法是"间距 + 条数"双阈值节流（`QuantityDanmakuFilter` 的移植）：
    /// 与上一条的间隔小于 `滚动时长 / (n × 1.2)`，或同屏条数超过 `n × 1.2`，就丢掉。
    pub max_quantity: Option<NonZeroU32>,
    /// 每种模式（顶部/底部/滚动）最多占几行。`None` = 不限。
    pub max_lines_per_mode: Option<NonZeroU32>,
    /// 合并重复文本（"2333" × 20 条只留一条）。默认关。
    pub merge_duplicates: bool,
    /// 允许同轨堆叠（重叠显示）。默认关 —— 打开之后画面会很花。
    pub allow_stacking: bool,
    /// 允许滚动弹幕**追尾覆盖**前一条。默认关。
    pub allow_scroll_overwrite: bool,
}

impl DanmakuDensity {
    /// 只覆盖本组字段，其它组的字段一律不动。
    pub(crate) fn apply(&self, cfg: &mut DanmakuLayoutConfig) {
        cfg.max_quantity = self.max_quantity.map(NonZeroU32::get);
        cfg.max_lines_per_mode = self.max_lines_per_mode.map(NonZeroU32::get);
        cfg.merge_duplicates = self.merge_duplicates;
        cfg.allow_stacking = self.allow_stacking;
        cfg.allow_scroll_overwrite = self.allow_scroll_overwrite;
    }
}

impl From<&DanmakuLayoutConfig> for DanmakuDensity {
    fn from(value: &DanmakuLayoutConfig) -> Self {
        Self {
            max_quantity: to_limit(value.max_quantity),
            max_lines_per_mode: to_limit(value.max_lines_per_mode),
            merge_duplicates: value.merge_duplicates,
            allow_stacking: value.allow_stacking,
            allow_scroll_overwrite: value.allow_scroll_overwrite,
        }
    }
}

/// 上游的 `Option<u32>` → `Option<NonZeroU32>`；`0` 和 `None` 都当成"不限"。
fn to_limit(value: Option<u32>) -> Option<NonZeroU32> {
    value.and_then(NonZeroU32::new)
}

impl ErikaDanmaku {
    /// 改弹幕的**密度与拥挤策略**（同屏上限 / 每模式行数 / 去重 / 堆叠 / 追尾覆盖）。
    ///
    /// ⚠️ **会重排**（这一组影响每条弹幕落在哪一行）。离散控件，点了就提交。
    /// 组外字段一个都不会动。
    ///
    /// ```ignore
    /// let mut d = player.danmaku().state().density;
    /// d.max_quantity = std::num::NonZeroU32::new(40);
    /// d.merge_duplicates = true;
    /// player.danmaku().set_density(d);
    /// ```
    ///
    /// 整组覆盖的契约同 [`set_paint`](Self::set_paint)：这一组只应有一个 UI 拥有者。
    pub fn set_density(&self, density: DanmakuDensity) {
        self.send(DanmakuCmd::UpdateConfig(Box::new(move |cfg| {
            density.apply(cfg);
        })));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_defaults_match_upstream() {
        let upstream = DanmakuLayoutConfig::default();
        assert_eq!(DanmakuDensity::from(&upstream), DanmakuDensity::default());
    }

    #[test]
    fn density_apply_leaves_other_groups_alone() {
        let before = DanmakuLayoutConfig::default();
        let mut cfg = before.clone();
        DanmakuDensity {
            max_quantity: NonZeroU32::new(40),
            ..Default::default()
        }
        .apply(&mut cfg);

        assert_eq!(
            cfg,
            DanmakuLayoutConfig {
                max_quantity: Some(40),
                ..before
            }
        );
    }

    /// 上游 `0` 的两种坏语义都在读侧被归一成 `None`（不限）。
    #[test]
    fn zero_limits_are_normalized_to_unlimited() {
        let mut upstream = DanmakuLayoutConfig::default();
        upstream.max_quantity = Some(0);
        upstream.max_lines_per_mode = Some(0);

        let ours = DanmakuDensity::from(&upstream);
        assert_eq!(ours.max_quantity, None);
        assert_eq!(ours.max_lines_per_mode, None);

        // 写回去也不会再产生 `Some(0)`。
        let mut cfg = DanmakuLayoutConfig::default();
        ours.apply(&mut cfg);
        assert_eq!(cfg.max_quantity, None);
        assert_eq!(cfg.max_lines_per_mode, None);
    }

    #[test]
    fn positive_limits_survive_a_round_trip() {
        let mut cfg = DanmakuLayoutConfig::default();
        DanmakuDensity {
            max_quantity: NonZeroU32::new(40),
            max_lines_per_mode: NonZeroU32::new(3),
            ..Default::default()
        }
        .apply(&mut cfg);

        let read_back = DanmakuDensity::from(&cfg);
        assert_eq!(read_back.max_quantity, NonZeroU32::new(40));
        assert_eq!(read_back.max_lines_per_mode, NonZeroU32::new(3));
    }
}

//! 绘制组：透明度 / 阴影 / 随机配色。
//!
//! 这一组是**唯一"改了只重绘"**的组（上游 `without_paint_only_fields()` 的名单就是它），
//! 所以面板可以边拖边发。见 [`super`] 的分组表。

use erika::danmaku::{DanmakuLayoutConfig, DanmakuShadowStyle};

use super::ErikaDanmaku;
use crate::cmd::DanmakuCmd;

/// 弹幕的**绘制外观**:透明度 / 阴影 / 随机配色。
///
/// 与上游 `DanmakuLayoutConfig` 的 paint-only 字段**一一对应**
/// (`liberika/crates/erika/src/danmaku.rs:354` 的 `without_paint_only_fields()`
/// 就是那份名单)。**别往里加排版字段** —— 这一组的价值就是"改了只重绘":
/// 一旦掺进会触发重排的字段(比如 `outline_width`),面板就不能实时拖了。
///
/// 读写两侧**共用这一个类型**:
/// - 写:[`ErikaDanmaku::set_paint`],组内整组覆盖;
/// - 读:[`DanmakuSnapshot::paint`](super::DanmakuSnapshot::paint),引擎当前值(面板的初始值)。
///
/// [`Default`] 与上游默认值**逐字段相等**(由 `tests::our_defaults_match_upstream` 钉住),
/// 所以它的语义是"这一组的默认设置",也就是**恢复默认** ——
/// 而不是"我什么都不改"(那是 `Option` 方案才有的语义,这里没有)。
#[derive(Debug, PartialEq, Clone)]
pub struct DanmakuPaint {
    /// 整体不透明度。上游夹取 `0.0..=1.0` —— 面板滑杆的 min/max 直接用这个范围,
    /// 别让滑杆产生越界值(否则显示值和实际生效值会分叉)。
    pub opacity: f32,
    /// 阴影档位:`None` / `Soft` / `Medium` / `Strong`(上游默认)。
    pub shadow_style: DanmakuShadowStyle,
    /// 阴影偏移(像素,`[x, y]`)。NaN / ∞ 会被上游换成 `1.0`。
    pub shadow_offset: [f32; 2],
    /// 按"时间 + 模式 + 文本"哈希从 11 色表取色:同一条弹幕每次颜色都一样。
    pub random_colors: bool,
}

impl DanmakuPaint {
    /// 只覆盖本组字段,其它组的字段一律不动。
    ///
    /// `pub(crate)` + 纯函数:单测直接喂一个 `DanmakuLayoutConfig::default()` 就能验,
    /// 不需要 presenter/引擎线程。
    pub(crate) fn apply(&self, cfg: &mut DanmakuLayoutConfig) {
        cfg.opacity = self.opacity;
        cfg.shadow_offset = self.shadow_offset;
        cfg.shadow_style = self.shadow_style;
        cfg.random_colors = self.random_colors;
    }
}

impl Default for DanmakuPaint {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            shadow_offset: [1.0, 1.0],
            shadow_style: DanmakuShadowStyle::Strong,
            random_colors: false,
        }
    }
}

impl From<&DanmakuLayoutConfig> for DanmakuPaint {
    fn from(value: &DanmakuLayoutConfig) -> Self {
        Self {
            opacity: value.opacity,
            shadow_style: value.shadow_style,
            shadow_offset: value.shadow_offset,
            random_colors: value.random_colors,
        }
    }
}

impl ErikaDanmaku {
    /// 改弹幕的**绘制外观**(透明度 / 阴影 / 随机配色) —— 只覆盖这一组的字段。
    ///
    /// **只重绘、不重排**:上游对这几个字段不做重新排版(它们不在
    /// `without_paint_only_fields()` 的比较范围里),所以面板可以边拖边发,
    /// 不会 bump 弹幕代次、不会让已排好的弹幕重新算位置。
    ///
    /// 组外的字段(字号 / 版面 / 密度 / 屏蔽 / 开关)**一个都不会动**:
    /// 引擎拿到当前 config、只把这一组的 4 个字段写进去,其余原样写回。
    ///
    /// ```ignore
    /// // 面板:初始值读快照(别用 Default —— 那是"恢复默认")
    /// let mut paint = player.danmaku().state().paint;
    /// paint.opacity = 0.6;
    /// player.danmaku().set_paint(paint);
    ///
    /// // 只改一项时:
    /// player.danmaku().set_paint(DanmakuPaint { opacity: 0.6, ..Default::default() });
    /// ```
    ///
    /// ⚠️ 组内是**整组覆盖**:4 个字段都会被写成本结构里的值。所以这一组应当只有
    /// **一个 UI 拥有者**(一个设置面板页签),别让两处各持一份 [`DanmakuPaint`] 去写 ——
    /// 那样它们在组内会互相覆盖。跨组没有这个问题(字段不重叠)。
    pub fn set_paint(&self, paint: DanmakuPaint) {
        self.send(DanmakuCmd::UpdateConfig(Box::new(move |cfg| {
            paint.apply(cfg);
        })));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_defaults_match_upstream() {
        let upstream = DanmakuLayoutConfig::default();
        assert_eq!(DanmakuPaint::from(&upstream), DanmakuPaint::default());
    }

    #[test]
    fn paint_apply_leaves_other_groups_alone() {
        let before = DanmakuLayoutConfig::default();
        let mut cfg = before.clone();
        DanmakuPaint {
            opacity: 0.5,
            ..Default::default()
        }
        .apply(&mut cfg);

        assert_eq!(
            cfg,
            DanmakuLayoutConfig {
                opacity: 0.5,
                ..before
            }
        );
    }
}

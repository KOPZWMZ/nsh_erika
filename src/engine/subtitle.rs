//! `SubtitleCmd`:字幕样式(外观 / 版面)与延迟。
//!
//! 上游的字幕样式是**一个整包**(`SubtitleStyleConfig`)+ 一个整包 setter,
//! 没有"只改一个字段"的 API —— 所以和弹幕一样在引擎侧做读改写:
//! 读引擎真值 → 本组 `apply` → 写回。组间字段不重叠(唯一交集是 `override_mask`,
//! 它归外观组),因此两个组可以各自独立地被不同 UI 处修改。
//!
//! 延迟是个独立方法(`set_subtitle_delay`),不在整包里,所以单独一个变体。

use erika::subtitle::SubtitleStyleConfig;

use super::ErikaEngine;
use crate::cmd::SubtitleCmd;

impl ErikaEngine {
    /// 字幕命令的总入口。
    ///
    /// 没有 `ctx`:这一族不参与出帧判定,错误由 `run` 后面的发布流程带出去。
    pub(super) fn on_subtitle(&mut self, cmd: SubtitleCmd) {
        match cmd {
            SubtitleCmd::TextStyle(style) => {
                self.write_subtitle_style(|current| style.apply(current))
            }
            SubtitleCmd::Placement(placement) => {
                self.write_subtitle_style(|current| placement.apply(current))
            }
            SubtitleCmd::Delay(delay_micros) => self.presenter.set_subtitle_delay(delay_micros),
        }
    }

    /// 字幕样式的唯一写法:读引擎真值 → 改 → 写回(两个组共用)。
    ///
    /// 上游 `set_subtitle_style` 自己会 `normalized()` 夹一遍、并在相等时短路,
    /// 所以这里不用重复做校验。
    fn write_subtitle_style(&mut self, change: impl FnOnce(&mut SubtitleStyleConfig)) {
        let mut style = self.presenter.subtitle_style().clone();
        change(&mut style);
        self.presenter.set_subtitle_style(style);
    }
}

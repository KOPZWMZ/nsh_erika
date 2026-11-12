//! 屏蔽组：按模式屏蔽 / 按内容屏蔽。
//!
//! 这一组全部会重排（改的是"哪些弹幕能进排版"），加上屏蔽词是文本输入，
//! 所以面板要**回车/失焦才提交**，别边打字边发。见 [`super`] 的分组表。

use erika::danmaku::DanmakuLayoutConfig;

use super::ErikaDanmaku;
use crate::cmd::DanmakuCmd;

/// 按"弹幕模式"屏蔽哪几类。
///
/// 就是上游的 `block_top` / `block_bottom` / `block_scroll` 三个 bool。
/// 单独包一层而不是塞进 [`DanmakuFilter`] 放三个 bool:面板上是**一组勾选框**,
/// 读写时一起进出更省事。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockedModes {
    /// 屏蔽顶部弹幕(固定在上方的那一类)。
    pub top: bool,
    /// 屏蔽底部弹幕(字幕位)。
    pub bottom: bool,
    /// 屏蔽滚动弹幕(平时满屏飘的那种)。
    pub scroll: bool,
}

impl From<&DanmakuLayoutConfig> for BlockedModes {
    fn from(value: &DanmakuLayoutConfig) -> Self {
        Self {
            top: value.block_top,
            bottom: value.block_bottom,
            scroll: value.block_scroll,
        }
    }
}

/// 一条屏蔽规则:普通关键词,或正则。
///
/// 上游这两种东西**混在同一个 `Vec<String>` 里**,靠一条隐式语法区分:
/// `名字/表达式/`(首尾都有 `/`、且以 `/` 结尾)才按正则处理,而且**名字部分会被丢弃**。
/// 那个语法是上游的实现细节,不该让每个调用方自己拼字符串 —— 所以这里给它一个类型。
///
/// ```ignore
/// use nsh_erika::FilterRule;
/// let filter = DanmakuFilter {
///     block_words: vec![
///         FilterRule::Word("剧透".into()),
///         FilterRule::Regex(r"^广告\d+$".into()),
///     ],
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterRule {
    /// 关键词:子串匹配(上游用 Aho-Corasick,大小写敏感)。
    Word(String),
    /// 正则:整个文本里匹配即屏蔽(上游用 `regex` crate 的默认语义)。
    Regex(String),
}

impl FilterRule {
    /// 翻译成上游那条隐式语法:`Word` 原样,`Regex` 包成 `名字/表达式/`。
    ///
    /// 名字位上游会丢弃,这里固定填 `过滤` 只是让日志/调试时能看出这是正则。
    fn to_upstream(&self) -> String {
        match self {
            FilterRule::Word(word) => word.clone(),
            FilterRule::Regex(pattern) => format!("{REGEX_RULE_PREFIX}{pattern}/"),
        }
    }

    /// 反过来解析上游的字符串 —— 规则与上游 `parse_regex_rule` **逐条对齐**:
    /// 必须含 `/`、首尾 `/` 不是同一个、以 `/` 结尾、中间非空,才算正则。
    fn from_upstream(rule: &str) -> Self {
        let Some(first) = rule.find('/') else {
            return FilterRule::Word(rule.to_string());
        };
        let Some(last) = rule.rfind('/') else {
            return FilterRule::Word(rule.to_string());
        };
        if first == last || last != rule.len() - 1 {
            return FilterRule::Word(rule.to_string());
        }
        let pattern = &rule[first + 1..last];
        if pattern.is_empty() {
            return FilterRule::Word(rule.to_string());
        }
        FilterRule::Regex(pattern.to_string())
    }
}

/// 正则规则的前缀(上游会丢弃名字部分,只取两个 `/` 中间的内容)。
const REGEX_RULE_PREFIX: &str = "过滤/";

/// 弹幕的**屏蔽与过滤**。
///
/// 读写共用同一类型：写 [`ErikaDanmaku::set_filter`]，读
/// [`DanmakuSnapshot::filter`](super::DanmakuSnapshot::filter)。
///
/// ⚠️ **会重排**:改屏蔽等于改"哪些弹幕参与排版"。文本输入要回车/失焦才提交,
/// 别按 `oninput` 每个字符发一次。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DanmakuFilter {
    /// 按模式屏蔽(顶部 / 底部 / 滚动)。
    pub blocked_modes: BlockedModes,
    /// 按内容屏蔽:关键词 + 正则。空 = 不按内容屏蔽。
    pub block_words: Vec<FilterRule>,
}

impl DanmakuFilter {
    /// 只覆盖本组字段，其它组的字段一律不动。
    pub(crate) fn apply(&self, cfg: &mut DanmakuLayoutConfig) {
        cfg.block_top = self.blocked_modes.top;
        cfg.block_bottom = self.blocked_modes.bottom;
        cfg.block_scroll = self.blocked_modes.scroll;
        cfg.block_words = self
            .block_words
            .iter()
            .map(FilterRule::to_upstream)
            .collect();
    }
}

impl From<&DanmakuLayoutConfig> for DanmakuFilter {
    fn from(value: &DanmakuLayoutConfig) -> Self {
        Self {
            blocked_modes: BlockedModes::from(value),
            block_words: value
                .block_words
                .iter()
                .map(|rule| FilterRule::from_upstream(rule))
                .collect(),
        }
    }
}

impl ErikaDanmaku {
    /// 改弹幕的**屏蔽与过滤**(按模式屏蔽 / 关键词 / 正则)。
    ///
    /// ⚠️ **会重排**;屏蔽词是文本输入,面板要回车/失焦才提交。
    /// 组外字段一个都不会动。
    ///
    /// ```ignore
    /// let mut f = player.danmaku().state().filter;
    /// f.blocked_modes.top = true;                      // 不要顶部弹幕
    /// f.block_words.push(FilterRule::Word("剧透".into()));
    /// player.danmaku().set_filter(f);
    /// ```
    ///
    /// 整组覆盖的契约同 [`set_paint`](Self::set_paint)：这一组只应有一个 UI 拥有者。
    pub fn set_filter(&self, filter: DanmakuFilter) {
        self.send(DanmakuCmd::UpdateConfig(Box::new(move |cfg| {
            filter.apply(cfg);
        })));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_defaults_match_upstream() {
        let upstream = DanmakuLayoutConfig::default();
        assert_eq!(DanmakuFilter::from(&upstream), DanmakuFilter::default());
    }

    #[test]
    fn filter_apply_leaves_other_groups_alone() {
        let before = DanmakuLayoutConfig::default();
        let mut cfg = before.clone();
        DanmakuFilter {
            blocked_modes: BlockedModes {
                top: true,
                ..Default::default()
            },
            ..Default::default()
        }
        .apply(&mut cfg);

        assert_eq!(
            cfg,
            DanmakuLayoutConfig {
                block_top: true,
                ..before
            }
        );
    }

    /// 正则要被翻成上游那条 `名字/表达式/` 语法,关键词原样。
    #[test]
    fn rules_map_to_the_upstream_syntax() {
        let mut cfg = DanmakuLayoutConfig::default();
        DanmakuFilter {
            block_words: vec![
                FilterRule::Word("剧透".into()),
                FilterRule::Regex("^广告".into()),
            ],
            ..Default::default()
        }
        .apply(&mut cfg);

        assert_eq!(
            cfg.block_words,
            vec!["剧透".to_string(), "过滤/^广告/".to_string()]
        );
    }

    /// 读侧要能把上游语法解析回来(并且和 `apply` 是同一个规则)。
    #[test]
    fn upstream_syntax_round_trips() {
        let mut cfg = DanmakuLayoutConfig::default();
        let ours = DanmakuFilter {
            blocked_modes: BlockedModes {
                top: true,
                bottom: false,
                scroll: true,
            },
            block_words: vec![
                FilterRule::Word("剧透".into()),
                FilterRule::Regex("^广告\\d+$".into()),
            ],
        };
        ours.apply(&mut cfg);

        assert_eq!(DanmakuFilter::from(&cfg), ours);
    }

    /// 没有 `/` 的、`/` 不在末尾的、`//` 空表达式的,都按普通关键词处理(和上游一致)。
    #[test]
    fn non_regex_shapes_stay_keywords() {
        assert_eq!(
            FilterRule::from_upstream("剧透"),
            FilterRule::Word("剧透".into())
        );
        assert_eq!(
            FilterRule::from_upstream("a/b"),
            FilterRule::Word("a/b".into())
        );
        assert_eq!(
            FilterRule::from_upstream("过滤//"),
            FilterRule::Word("过滤//".into())
        );
    }
}

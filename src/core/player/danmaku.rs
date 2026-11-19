//! 弹幕句柄（[`ErikaDanmaku`]）+ 状态快照（[`DanmakuSnapshot`]）。
//!
//! # 样式为什么按"屏幕上的块"分组
//!
//! 上游 `DanmakuLayoutConfig` 有 24 个字段、只有一个整包 setter。整包推给 UI 会有两个问题:
//! ① 改一个字段会把其余 23 个冲回 UI 手里的旧值;② 24 个字段一次暴露,谁也不知道
//! 哪个改动代价大。所以拆成 **5 个组**,一组一个结构体、一次提交:
//!
//! | 组 | 类型 | 上游字段 | 引擎代价 | 面板能不能实时拖 | 状态 |
//! |---|---|---|---|---|---|
//! | 绘制 | [`DanmakuPaint`] | `opacity` / `shadow_style` / `shadow_offset` / `random_colors` | **只重绘** | ✅ 可以 | 已落地 |
//! | 字体 | [`DanmakuText`] | `font_size` / `supersampling` / `custom_font_*` | 重排 + 重建光栅器 | ❌ 必须松手 | 已落地 |
//! | 版面 | [`DanmakuPlacement`] | `display_area` / `scroll_speed_factor` / `track_gap_ratio` / `outline_width` | 重排 | ❌ 必须松手 | 已落地 |
//! | 密度 | [`DanmakuDensity`] | `max_quantity` / `max_lines_per_mode` / `merge_duplicates` / `allow_stacking` / `allow_scroll_overwrite` | 重排 | ❌ 离散控件 | 已落地 |
//! | 屏蔽 | [`DanmakuFilter`] | `block_top` / `block_bottom` / `block_scroll` / `block_words` | 重排 | ❌ 回车才提交 | 已落地 |
//!
//! 两条契约:
//!
//! 1. **组内是整组覆盖** —— 所以每组只应有一个 UI 拥有者（一个面板页签一份）。
//!    两处各持一份同组结构体去写，会互相覆盖；**跨组没有这个问题**（字段不重叠）。
//! 2. **`enabled` 不属于任何组** —— 它是开关不是样式，走
//!    [`ErikaDanmaku::set_enabled`]（上游对关闭/重开的代价也不对称：关可复用几何，重开强制重排）。
//!
//! # 加一组要改哪几处
//!
//! 因为样式命令现在还复用 [`ErikaDanmaku::update_config`] 这条读改写通道，加一组**不用动**
//! `cmd.rs` / `engine/danmaku.rs`:
//!
//! 1. 新建 `player/danmaku/<组>.rs`：结构体 + `apply`（`pub(crate)` 纯函数）+ `Default`
//!    （必须与上游默认逐字段相等）+ `From<&DanmakuLayoutConfig>` + `impl ErikaDanmaku { set_xxx }`
//!    + 两条单测（默认值对齐 / 只碰本组）；
//! 2. 本文件：`mod <组>;` + `pub use <组>::DanmakuXxx;`；
//! 3. [`DanmakuSnapshot`]：加一个同名字段；
//! 4. `engine.rs` 的 `publish`：加一行 `xxx: DanmakuXxx::from(&config),`（放在 `config` 前面）；
//! 5. `lib.rs` / `player.rs` 的 re-export。
//!
//! 未成组的字段（`scroll_duration_seconds` / `custom_font_face_index` 等）继续走
//! [`ErikaDanmaku::update_config`] 逃生舱。

use actor_lite::ActorHandle;
#[cfg(feature = "escape-hatch")]
use erika::danmaku::DanmakuLayoutConfig;
use erika::danmaku::DanmakuTrackInfo;

use crate::core::{
    cmd::{DanmakuCmd, ErikaCmd},
    engine::ErikaEngine,
};

use std::sync::Arc;

mod density;
mod filter;
mod paint;
mod placement;
mod text;

pub use density::DanmakuDensity;
pub use filter::{BlockedModes, DanmakuFilter, FilterRule};
pub use paint::DanmakuPaint;
pub use placement::DanmakuPlacement;
pub use text::DanmakuText;

/// 弹幕库的控制句柄：发弹幕命令 + 读弹幕状态。
///
/// 从 [`ErikaPlayer::danmaku`](crate::ErikaPlayer::danmaku) 取；`Clone` 只是 Arc 加一，
/// 可以直接被 UI 闭包捕获（同一个播放器派生出来的句柄互相 `==`）。
///
/// **读只能读快照**：`erika::Player` 上没有任何弹幕方法（弹幕整块住在 presenter 上），
/// 所以这里没有"直连读"这条路 —— 写走命令队列，读走引擎发布的状态。
pub struct ErikaDanmaku {
    engine: Arc<ActorHandle<ErikaEngine>>,
}
impl Clone for ErikaDanmaku {
    fn clone(&self) -> Self {
        Self {
            engine: self.engine.clone(),
        }
    }
}
impl PartialEq for ErikaDanmaku {
    fn eq(&self, o: &Self) -> bool {
        Arc::ptr_eq(&self.engine, &o.engine)
    }
}
impl Eq for ErikaDanmaku {}
impl ErikaDanmaku {
    /// 建一个句柄（只给 `ErikaPlayer` 用，外部请走 `player.danmaku()`）。
    pub(super) fn new(engine: ActorHandle<ErikaEngine>) -> Self {
        Self {
            engine: Arc::new(engine),
        }
    }
    /// 发一条弹幕命令。
    fn send(&self, cmd: DanmakuCmd) {
        let _ = self.engine.send_cmd(ErikaCmd::Danmaku(cmd));
    }

    /// 弹幕总开关。引擎里做"读改写"(和 erika 自己的 `set_danmaku_enabled` 一致),
    /// 所以不会把别的样式字段冲掉。
    ///
    /// ⚠️ 开关**不属于**任何样式组(分组见模块文档「样式为什么按屏幕上的块分组」):
    /// 关闭可以直接复用已有几何并立刻压制,
    /// 而**重新开启会强制重排** —— 代价不对称,别把它塞进样式结构体里。
    pub fn set_enabled(&self, enabled: bool) {
        self.send(DanmakuCmd::Enabled(enabled));
    }

    /// 加一条弹幕轨(**追加**,不替换已有的)。`uri` 可以是本地路径,也可以是 URL。
    ///
    /// 解析在**引擎线程**上做(`read_uri_to_end` + `parse_auto`):十万条的 B 站 XML
    /// 要几百毫秒,放 UI 线程就是掉帧。失败会写进 [`ErikaPlayer::status`](crate::ErikaPlayer::status)
    /// 的 `error`。
    ///
    /// ⚠️ 弹幕轨道和媒体**不绑定**(只是一串 `pts → 文本`),所以 **`open()` 换片时会自动
    /// 清空所有轨道**(2026-11-07 起;不清就会串到新片上)。
    ///
    /// 两条边界写清楚:
    /// - **首次打开不清**:`add_track` 在第一次 `open` **之前**调用是合法的
    ///   (命令队列保序,先挂弹幕再开片是自然写法);
    /// - **换片才清**:已经有媒体时再 `open` = 换片,那一刻所有轨道清空,要接着用就重新
    ///   `add_track`。
    ///
    /// 全局偏移**不会被清**(那是用户偏好),单轨偏移随轨道一起没。
    ///
    /// **拿轨道 id**:这个方法**不返回** id —— 解析是异步的(十万条 B 站 XML 要几百毫秒),
    /// 做成同步回执就是"要么阻塞 UI 线程、要么给个短超时然后经常拿不到"。
    /// 正确的用法是**从快照里认领**:调完等一次
    /// [`state`](Self::state) 刷新,`state().tracks` 里**新出现**的那条就是它
    /// (`DanmakuTrackInfo { id, name, source, enabled, offset_micros, item_count }`)。
    /// 之后 `id` 就能喂给 `set_track_enabled` / `set_track_offset` / `remove_track`。
    pub fn add_track(&self, uri: impl Into<String>, name: impl Into<String>) {
        self.send(DanmakuCmd::AddTrack {
            uri: uri.into(),
            name: name.into(),
        });
    }
    // ───────────── 弹幕 ─────────────
    //
    // `Player` 完全不知道弹幕:设置 / 轨道 / 加载全在 presenter 上,而且都是 `&mut self`。
    // 所以这里**没有"直连"这个选项**,只能走队列。
    //
    // 解析不用我们写:`DanmakuTimeline::parse_auto` 已经能吃 B 站 XML / JSON / JSONL,
    // 并且自带整份容错契约(缺正文跳过、非法字段回默认、id 顺序分配、u64 精确解析)。

    /// 在引擎线程上改弹幕样式 —— **逃生舱**,未成组的字段用它。
    ///
    /// 成组之后优先用 `set_paint` / `set_text` / … 那一批:它们是数据命令
    /// （可 `Debug`、可单测、不会把消费者的代码放进引擎线程）。
    ///
    /// **别让 UI 传整份 config** —— 那样改一个字段会把其余 23 个字段冲回 UI 手里的旧值。
    /// 用这个 mutator,引擎读当前值、改、写回。
    ///
    /// ```ignore
    /// let danmaku = player.danmaku();
    /// danmaku.update_config(|c| c.scroll_duration_seconds = 8.0);   // 还没成组的字段
    /// danmaku.update_config(|c| c.custom_font_face_index = 1);
    /// ```
    ///
    /// **需要 `escape-hatch` feature**：它把上游的整包 config 露给调用方，是逃生舱
    /// （日常改样式请用 `set_paint` / `set_text` / … 那一批）。注意分组 setter 内部**也走
    /// 这条命令**，所以引擎侧的 `DanmakuCmd::UpdateConfig` 永远在，只是不再对外公开。
    #[cfg(feature = "escape-hatch")]
    pub fn update_config(&self, f: impl FnOnce(&mut DanmakuLayoutConfig) + Send + 'static) {
        self.send(DanmakuCmd::UpdateConfig(Box::new(f)));
    }

    /// 移除一条弹幕轨(按 [`state`](Self::state) 里拿到的 `id`)。
    ///
    /// id 不存在时**不 panic**:引擎会把 `"移除弹幕轨失败:没有 id = N 的轨道"`
    /// 写进 [`ErikaPlayer::status`](crate::ErikaPlayer::status) 的 `error`。
    pub fn remove_track(&self, track_id: u64) {
        self.send(DanmakuCmd::RemoveTrack(track_id));
    }

    /// 单条轨道的开关(不影响总开关 [`set_enabled`](Self::set_enabled))。
    ///
    /// 典型用法:用户在"弹幕源列表"里勾掉某一条,但保留别的轨道继续跑。
    /// id 无效时同上,失败写进 `status().error`。
    pub fn set_track_enabled(&self, track_id: u64, enabled: bool) {
        self.send(DanmakuCmd::TrackEnabled { track_id, enabled });
    }

    /// 单条轨道的偏移(微秒,**正 = 这条弹幕整体晚出**)。
    ///
    /// 用来治"某一条源的字幕/弹幕时间轴对不上"——比如外挂 XML 比画面早半秒,
    /// 给 `-500_000` 就能对齐。只动这一条,其它轨道不受影响。
    pub fn set_track_offset(&self, track_id: u64, offset_micros: i64) {
        self.send(DanmakuCmd::TrackOffset {
            track_id,
            offset_micros,
        });
    }

    /// **全局**偏移(微秒,**正 = 所有弹幕晚出**),等价于"字幕延迟"的弹幕版。
    ///
    /// 和[单轨偏移](Self::set_track_offset)是**叠加**的:最终时间 = 媒体时间 +
    /// 全局偏移 + 该轨偏移。视频整体音画对不上时用这个,单条源对不上用那个。
    pub fn set_global_offset(&self, offset_micros: i64) {
        self.send(DanmakuCmd::GlobalOffset(offset_micros));
    }

    /// 清空所有弹幕轨。
    pub fn clear(&self) {
        self.send(DanmakuCmd::Clear);
    }

    /// 弹幕状态快照(开关 / 轨道列表 / 当前样式)。读的是引擎发布的状态,非阻塞。
    pub fn state(&self) -> DanmakuSnapshot {
        self.engine.get_state().danmaku
    }
}
/// 弹幕状态快照。
///
/// 弹幕整块都住在 presenter 上(`Player` 完全不知道它的存在),
/// 所以只能由引擎线程读取后发布过来。
/// 低频:只在用户操作(开关/加轨/改样式/清空)时变。
///
/// **面板初始化请用下面那几个组视图** —— 它们的类型和 `set_*` 的入参是同一批,读出来改两下
/// 就能写回去。
///
/// 这里**不放**上游的整包配置(24 字段):有那需求的人走逃生舱自己取 ——
/// `player.call_with(|p| p.danmaku_config().cloned())`。快照里再存一份等于把同一份数据
/// 在每条命令上多克隆一遍,还会引诱调用方整份写回去(那会冲掉别人改过的字段)。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DanmakuSnapshot {
    /// 总开关(对应 `DanmakuLayoutConfig::enabled`)。
    pub enabled: bool,
    /// 当前所有轨道(名字/来源/开关/偏移/条数)。
    pub tracks: Vec<DanmakuTrackInfo>,
    /// 绘制组的类型化视图(= `config` 里那 4 个字段),**面板拿它当初始值**。
    ///
    /// 和 [`DanmakuPaint::default()`] 的区别:这个是**引擎当前值**,`default()` 是
    /// "恢复默认"。初始化一律读这里,否则用户上次设的透明度会被默认值冲掉。
    pub paint: DanmakuPaint,
    /// 字体组的类型化视图(= `config` 里那 4 个字段),面板拿它当初始值。
    pub text: DanmakuText,
    /// 版面组的类型化视图(`scroll_speed` 对应上游的 `scroll_speed_factor`)。
    pub placement: DanmakuPlacement,
    /// 密度组的类型化视图(两个上限用 `Option<NonZeroU32>`,`None` = 不限)。
    pub density: DanmakuDensity,
    /// 屏蔽组的类型化视图(模式屏蔽 + 关键词/正则)。
    pub filter: DanmakuFilter,
}

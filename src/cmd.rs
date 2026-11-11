//! 命令词汇表:UI 线程 → 引擎线程的**唯一**通道内容。
//!
//! 顶层只分五族([`ErikaCmd`])+ 一个逃生舱,一族一条"为什么"注释;每个家族的 handler
//! 收窄成 `fn on_xxx(&mut self, cmd: XxxCmd, …)`,各自的引擎实现在 `engine/<家族>.rs`。
//! 新增一族命令**不用动**顶层的 `on_cmd`。
//!
//! # 为什么要分族
//!
//! 以前是扁平的 19 个变体 + 一个巨型 `match`,而且还在长(字幕样式 / 超分 / HDR /
//! 截图都要往里塞)。分族之后:
//!
//! - 顶层 `match` 只剩 6 个 arm(五族 + [`ErikaCmd::Call`]);
//! - handler 的参数被收窄(`fn on_settings(&mut self, cmd: SettingsCmd)`),编译器照样查漏;
//! - 每族可以单独写文档 / 默认值 / 参数校验。
//!
//! # 分族**不是**分通道
//!
//! [`ErikaCmd`] 只被**一条**队列承载:`open→play`、`stop→play` 的用户顺序靠那条队列
//! 保证,不靠这里的类型结构。别顺手拆成多条通道 —— 拆了就丢顺序。
//!
//! # 分族的界线
//!
//! 判据是"这条命令要动谁":
//!
//! | 家族 | 动什么 | 谁必须参与 |
//! |---|---|---|
//! | [`PlaybackCmd`] | 在不在播 / 播哪一段 | presenter(音频 cork/flush、清视觉、bump 代号) |
//! | [`SettingsCmd`] | 在播**但换个参数** | presenter(音量在 `audio_output` 上;变速要 time-stretch) |
//! | [`DanmakuCmd`] | 弹幕轨与样式 | presenter(弹幕整块只在 presenter 上) |
//! | [`RenderCmd`] | 出不出帧 / 走哪条出帧路 | 引擎自己(`presenter` + 帧槽) |
//! | [`ErikaCmd::Call`] | 以上都不是 | 见下面的警告 |

use std::time::Duration;

use erika::MediaRequest;

use crate::player::{SubtitlePlacement, SubtitleTextStyle};
use erika::danmaku::DanmakuLayoutConfig;
use erika::presenter::PresenterRuntime;

/// 逃生舱:交给引擎线程执行的任意 presenter 操作。
///
/// 只给**还没有真方法**的能力留口子(有了真方法就优先用,那些才会做读改写 / 参数校验)。
///
/// ```ignore
/// player.call(|p| p.set_luma_upscaler(LumaUpscalerMode::Bilinear)); // 超分
/// player.call(|p| { let _ = p.capture_frame_rgba(1920, 1080); });    // 截一张
/// ```
pub(crate) type Job = Box<dyn FnOnce(&mut PresenterRuntime) + Send>;

/// 发给引擎线程的命令。
///
/// **顺序保证来自"同一条队列"**,不来自这里的类型结构。
pub(crate) enum ErikaCmd {
    /// 生命周期 + 传输:每条都要 presenter 协调。
    Playback(PlaybackCmd),
    /// 参数类:都不改变"在播/不在播",但都动 presenter 的内部状态。
    Settings(SettingsCmd),
    /// 弹幕:整块只在 presenter 上,而且都是 `&mut self`。
    Danmaku(DanmakuCmd),
    /// 字幕样式(外观 / 版面)与延迟:同样是整包只在 presenter 上。
    Subtitle(SubtitleCmd),
    /// 渲染握手:动的是引擎自己的出帧路径和帧槽,和媒体无关。
    Render(RenderCmd),
    /// presenter 独有、还没封的能力(超分、HDR headroom、HUD、截图、轨道章节…)。
    ///
    /// **故意留在顶层**:它能跑任意代码,塞进任何家族都会破坏"该家族语义闭合"。
    Call(Job),
}

/// 生命周期 + 传输。
///
/// 每一条都要 **presenter** 参与才正确:`play`/`pause` 要 cork 音频设备、
/// `seek`/`stop` 要 flush 音频环、`open` 要重建解码器并清视觉 ——
/// 这些协调方法在 presenter 上是 private,直连 `erika::Player` 会音画不同步
/// (实测:直连 `set_playback_rate(2.0)` 画面变快、声音不变)。
pub(crate) enum PlaybackCmd {
    /// 打开媒体(换片)。`session` 会 +1。
    Open(MediaRequest),
    /// 停播:位置归 0、画面清空,**媒体仍然开着**。
    Stop,
    /// 起播。
    Play,
    /// 暂停(要 cork 音频设备)。
    Pause,
    /// 跳转(要 flush 音频环)。
    Seek(Duration),
}

/// 设置类:都不改变"在播/不在播",但都会动 presenter 的内部状态。
pub(crate) enum SettingsCmd {
    /// 倍速:要让音频环做 time-stretch(变速不变调)。
    Rate(f64),
    /// 音量:住在 presenter 的 `audio_output` 上,`erika::Player` 根本没有。
    Volume(f64),
    /// 静音:只把**输出**压到 0,**不改变**记住的音量值(取消静音即恢复)。
    Muted(bool),
    /// 选音轨(`None` = 关掉音频):presenter 会先 quiesce 再 flush 音频环。
    AudioTrack(Option<i64>),
    /// 选字幕轨(`None` = 关字幕):同上,会 flush 音频环。
    SubtitleTrack(Option<i64>),
}

/// 弹幕:开关 / 加轨 / 样式 / 清空。
///
/// `erika::Player` 完全不知道弹幕的存在,所以这一族
/// **没有"直连"这个选项**,只能走队列。
pub(crate) enum DanmakuCmd {
    /// 总开关(引擎里做读改写,不会冲掉别的样式字段)。
    Enabled(bool),
    /// 追加一条弹幕轨(本地路径或 URL,解析在引擎线程上做)。
    AddTrack { uri: String, name: String },
    /// 读改写:引擎拿当前 config、改、写回;UI 不用持有整包(否则会互相冲掉)。
    UpdateConfig(Box<dyn FnOnce(&mut DanmakuLayoutConfig) + Send>),
    /// 移除一条轨道。上游返回 `bool`(找不到就是 false),失败会写进 `status().error`。
    RemoveTrack(u64),
    /// 单轨开关(不影响总开关 `Enabled`)。
    TrackEnabled { track_id: u64, enabled: bool },
    /// 单轨偏移(微秒,正 = 这条弹幕整体晚出)。
    TrackOffset { track_id: u64, offset_micros: i64 },
    /// 全局偏移(微秒):所有轨一起挪,等价于"字幕延迟"的弹幕版。
    GlobalOffset(i64),
    /// 清空所有轨道。
    Clear,
}

/// 字幕:外观 / 版面 / 延迟。
///
/// 上游的字幕样式是**一个整包**(`SubtitleStyleConfig`)+ 一个整包 setter,
/// 没有"只改一个字段"的 API —— 所以两个样式变体都交给引擎做读改写
/// (读真值 → 本组 `apply` → 写回),组间字段不重叠。
///
/// **延迟不在整包里**(上游是独立的 `set_subtitle_delay`),所以单独一个变体。
pub(crate) enum SubtitleCmd {
    /// 外观组:字体 / 颜色 / 字形 / 描边 / 阴影 / 缩放 + 覆盖掩码。
    TextStyle(SubtitleTextStyle),
    /// 版面组:对齐 + 左/右/竖直边距。
    Placement(SubtitlePlacement),
    /// 字幕延迟(微秒,正 = 晚出)。
    Delay(i64),
}

/// 渲染握手。只有 `Wake` 一条。
///
/// 宿主(GTK 控件)不提供别的信号 —— 它不持有 GPU 设备，也没有"窗口可见性"事件可转。
/// 没人要帧时引擎自己按 50ms 节奏保活泵音频（见 `engine/render.rs`），所以没有
/// "遮挡 / 不可见 / 暂停出帧"之类的命令，也不需要。出帧只有 dma-buf 一条路，
/// 也没有"换设备 / 降级"之类的命令。
pub(crate) enum RenderCmd {
    /// 要一帧(渲染层每帧至多投一次,引擎侧 `swap` 合并)。
    Wake,
}

//! 邮箱（[`ErikaSlot`]）：**外部拿控制句柄的唯一入口**。
//!
//! 引擎由组件创建（`erika.video` 的组件体），创建成功后把「号 + 句柄」投递进页面
//! 事先建好的这个槽；外部随时用 [`player`](ErikaSlot::player) 借用，组件卸载时清空 ——
//! 之后一律 `None`。
//!
//! ## 为什么需要它
//!
//! NSH 的组件体只在挂载时跑一次，而且**父级先跑、子组件后跑**：页面想控制播放，
//! 就必须有一个"页面先建好、组件后来填"的落点。所以句柄不是"传进去"，而是"填出来"。
//!
//! ## 槽里为什么只放「号」
//!
//! 会话本身在进程级注册表里（`id → PlayerInner`），槽只记住"我的号是几"：
//!
//! - 引擎的生死只由 [`ErikaSession`](crate::ErikaSession) 决定，槽拿着号**不会**续命；
//! - 会话一关，注册表那条就被摘掉，[`player`](ErikaSlot::player) 自然变成 `None`；
//! - 槽仍然是 `Signal`，所以"引擎起来了"这件事能驱动 UI（按钮从灰变亮）。
//!
//! ## 语义
//!
//! [`player`](ErikaSlot::player) 返回 `None` 只有三种情况，一律按"不可用"处理：
//! 还没投递（组件还没挂载）、已经卸载（组件清了槽）、引擎起来了但已经停了。
//! **`None` 不是错误**，不要当异常处理。

use nsh_core::element::{JsHost, JsIn, JsOwned, JsValue, PropError, TsTypes};
use nsh_core::prelude::*;

use crate::ErikaPlayer;

/// 槽的状态。**只有数据，没有任何引用。**
#[derive(Clone, PartialEq)]
enum SlotState {
    /// 空：组件还没挂载，或者已经卸载。
    Empty,
    /// 有引擎：号既是平台视图要的 `source`，也是外部 `resolve` 成句柄的键。
    Ready(u64),
    /// 组件那次创建失败了。文案和它渲染的错误界面是同一份。
    Failed(String),
}

/// 页面建、组件填的邮箱。`Copy`，可以随便塞进闭包、`signal()`、子组件 props。
///
/// ```ignore
/// let slot = ErikaSlot::new();                       // 页面体里（需要 owner）
/// view().child(erika_video().src(uri).slot(slot))
///       .child(button().on_press(move || {
///           if let Some(player) = slot.player() { player.pause() }
///       }))
/// ```
///
/// ⚠️ 读 [`player`](Self::player) / [`id`](Self::id) / [`error`](Self::error) 会**订阅**这个
/// 信号（在 binding 里读就是响应式的）；只想要一次性快照就用 `untracked(...)` 包一层。
#[derive(Clone, Copy, PartialEq)]
pub struct ErikaSlot(Signal<SlotState>);

// `#[derive(Props)]` 要求每个 prop 类型实现 `JsValue`（它同时也是 TS 声明生成器）。
// 邮箱是**纯 Rust 侧**的句柄：JS 传不进来、也不该往 JS 传。于是这里双向都拒绝，
// 效果是"类型系统里有它、JS 侧看不到它" —— 这正是我们要的。
impl JsValue for ErikaSlot {
    fn ts_type(_types: &mut TsTypes) -> String {
        "never".into()
    }

    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        Err(value.expected("an ErikaSlot（邮箱只能从 Rust 侧传）"))
    }

    fn to_js<'a>(&self, _host: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        Err(PropError("ErikaSlot 不能跨到 JS".into()))
    }
}

impl ErikaSlot {
    /// 建一个空槽。**必须在页面体（或任何有 owner 的地方）里调**。
    pub fn new() -> Self {
        Self(signal(SlotState::Empty))
    }

    /// 此刻能不能控制：`None` = 还没挂载 / 已经卸载。
    ///
    /// 每次调用是一次 `Signal::get` + 一次注册表查表，**别放在 60fps 的路径上**；
    /// 按钮点击、seek 提交这类低频动作用它正合适。
    pub fn player(&self) -> Option<ErikaPlayer> {
        match self.0.get() {
            // 号在、会话没了（组件已卸载）时 `resolve` 返回 `None`。
            SlotState::Ready(id) => ErikaPlayer::resolve(id),
            _ => None,
        }
    }

    /// 平台视图要的号（组件内部把它写进 `source` prop）。`None` = 现在没有可显示的会话。
    pub fn id(&self) -> Option<u64> {
        match self.0.get() {
            SlotState::Ready(id) => Some(id),
            _ => None,
        }
    }

    /// 组件那次创建失败的原因（有的话）。
    pub fn error(&self) -> Option<String> {
        match self.0.get() {
            SlotState::Failed(why) => Some(why),
            _ => None,
        }
    }

    // ── 组件侧 ────────────────────────────────────────────────────────────
    //
    // 三处写值**全部用 `try_set`**：页面的 owner 可能比组件先 dispose，
    // 那时 `Signal::set` 会 panic（它 `expect("a signal")`），而 `try_set` 返回 false。

    /// 投递：组件创建成功后调一次（只写号）。
    pub(crate) fn fill(&self, player: &ErikaPlayer) {
        let _ = self.0.try_set(SlotState::Ready(player.session_id()));
    }

    /// 组件创建失败。
    pub(crate) fn fail(&self, why: String) {
        let _ = self.0.try_set(SlotState::Failed(why));
    }

    /// 清空：组件卸载时调。之后 [`player`](Self::player) 一律 `None`。
    pub(crate) fn clear(&self) {
        let _ = self.0.try_set(SlotState::Empty);
    }
}

//! 号空间：进程内唯一的 `u64` → [`ViewSource`]，**跨 JSON 边界的唯一桥**。
//!
//! NSH 的原生组件只能收 JSON（`native(...).prop("source", …)`），句柄过不去。所以：
//!
//! - 一个会话在创建时领一个进程内唯一的号，把它的**视图侧**登记进这张表；
//! - 平台控件拿到 `source` 号，就能换回 [`ViewSource`]（每帧要帧、贴图）；
//! - 会话销毁时摘表 —— 之后同一个号再也查不到东西。
//!
//! ## 表里为什么没有守卫
//!
//! [`ViewSource`] 只有「帧槽 + 请求 + 命令通道」，**不含 `ErikaSession`**（唯一所有者）。
//! 所以"还有人在显示"**不会**给引擎续命；反过来说，引擎的生死与这张表无关，
//! 由会话自己（`ErikaSession::drop`）决定。
//!
//! ## 谁能动这张表
//!
//! | 谁 | 做什么 |
//! |---|---|
//! | [`ErikaSession::create`](crate::ErikaSession::create) | [`next_id`] + [`register`] |
//! | [`ErikaSession::drop`](crate::ErikaSession) | [`unregister`] |
//! | 平台控件（Linux `FrameBinder` / Windows `draw`） | [`resolve`]（按 `source` 号） |
//! | [`ErikaPlayer::view`](crate::ErikaPlayer::view)（逃生舱） | [`resolve`] |
//!
//! 外面看不到这张表，只能通过上面四个入口碰它。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::core::source::ViewSource;

/// 号 → 视图侧。**键是 `source` 号**，值是平台层画帧要的全部东西。
static REGISTRY: OnceLock<Mutex<HashMap<u64, ViewSource>>> = OnceLock::new();

/// 下一个要发的号。1 起，0 留作"没有"（`source = null` = 没有东西可显示）。
///
/// 单调递增、**永不复用** —— 所以"号的相等"就等于"会话的相等"，也永远不会有一个
/// 新会话撞上旧号、让还绑着旧会话的控件拿到别的画面。
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn table() -> &'static Mutex<HashMap<u64, ViewSource>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 领一个新号。**只由 [`ErikaSession::create`](crate::ErikaSession::create) 调。**
pub(crate) fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// 把一个会话的视图侧登记到 `id` 上。
pub(crate) fn register(id: u64, source: ViewSource) {
    table()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id, source);
}

/// 摘掉 `id` 那一条（会话销毁时）。
///
/// **被移除的值在锁外析构**：万一将来 [`ViewSource`] 长出 `Drop`（比如要回收 GPU 资源），
/// 它在持锁时析构就可能再进这张表 → 自锁。现在它没有 `Drop`，这是防御性写法。
pub(crate) fn unregister(id: u64) {
    let removed = REGISTRY
        .get()
        .and_then(|table| table.lock().ok().and_then(|mut table| table.remove(&id)));
    drop(removed);
}

/// 按号取回视图侧。`None` = 从没注册过 / 已经摘表（会话销毁了）。
pub(crate) fn resolve(id: u64) -> Option<ViewSource> {
    let table = table().lock().ok()?;
    table.get(&id).cloned()
}

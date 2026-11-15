//! # nsh-erika
//!
//! 把 [erika](https://github.com/AimesSoft/liberika) 的视频 / 字幕 / 弹幕画面接进
//! NSH（Rust UI）的 **native component** `erika.video`。
//!
//! 形状和 media_kit 一样:**播放器是独立的句柄,画面是接收帧源的控件**,两者分开。
//!
//! ## 用法
//!
//! ```ignore
//! // 应用 crate 的根（nsh 生成的宿主按平台去这里找入口）
//! #[native_components(linux)]
//! fn components(components: &nsh_platform_linux::Components) {
//!     nsh_erika::register(components);
//! }
//!
//! // 页面：句柄和帧源都在外面创建
//! let player = ErikaPlayer::new()?;
//! player.open(MediaRequest::new(uri));
//! player.play();
//! view().child(nsh_erika::ErikaVideo(&player));    // 控件只负责显示
//! ```
//!
//! ## 读直连,写走队列
//!
//! [`ErikaPlayer`] 同时握着 `erika::Player`(presenter 内部那个的克隆)和一条命令通道:
//!
//! - **读**直连 `Player`:`state()` / `intent_state()` / `playback()` / `media_time()` /
//!   `duration()` / `tracks()` / `chapters()` ……
//! - **写**几乎全走队列,因为 `Player` 只管解码侧:暂停要 cork 音频设备、seek 要
//!   flush 音频环、变速要让音频做 time-stretch —— 只有持有 `audio_output` 的
//!   presenter 做得出来。
//!
//! ## 分层
//!
//! ```text
//! ┌ 内核层 core/ —— 不认识任何 UI 框架（但还没完全平台无关，见「平台」） ─┐
//! │ engine/     ErikaEngine(actor):PresenterRuntime 的唯一 owner          │
//! │   engine.rs   结构体 + run 循环 + on_cmd 分派 + 状态发布               │
//! │   render.rs   on_render + pump_frame：Wake / 出帧 / 保活泵            │
//! │ cmd.rs      命令词汇表:ErikaCmd 五族                                  │
//! │ player.rs   ErikaPlayer + ErikaStatus                                │
//! │ source.rs   ErikaVideoSource + FrameSlot（帧槽 + 视频尺寸）           │
//! └──────────────────────────────────────────────────────────────────────┘
//! ┌ Linux 适配层 —— 唯一允许出现 gtk 的地方 ─────────────────────────────┐
//! │ component.rs  ErikaView：`gtk::Picture` + tick 回调 + `source` prop   │
//! │ linux/frames.rs  FrameBinder：帧源 → `GdkDmabufTexture`                │
//! └──────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## 画面怎么到屏幕上
//!
//! erika 的渲染器把合成结果导出成**线性 dma-buf**（`export_frame_dmabuf`），控件用
//! `GdkDmabufTexture` 直接导入交给 `gtk::Picture`。我们这边不建 Vulkan 设备，也不需要
//! 导入外部纹理。
//!
//! **只有这一条路**：没有 CPU 回读兜底（`capture_frame_rgba` 是截图 API，按设计不启动
//! 音频、也不计入渲染帧，见 dev_doc §8）。宿主（GTK/GSK）导不进来就没有画面。
//!
//! 导出或导入失败时，原因会写进 [`ErikaStatus::error`]（`player.status().error` /
//! [`ErikaPlayer::last_error`]）并且**之后不再尝试导出**；应用据此整条降级到 NSH 内置
//! `<video>`（GStreamer）。`ErikaPlayer::new()` 本身就失败（没有可用的 Vulkan 设备）时
//! 是同步 `Err`，那时压根别建控件。
//!
//! ## 用法：句柄在外面，控件只显示
//!
//! ```no_run
//! # use nsh_core::prelude::*;
//! # fn page() -> impl IntoElement {
//! // 1) 句柄在外面创建
//! let player = nsh_erika::ErikaPlayer::new().expect("引擎起不来");
//! player.open(nsh_erika::MediaRequest::new("/path/to/video.mkv"));
//! player.play();
//!
//! // 2) 页面里只放"显示"；控制条用应用自己的 NSH 元素画，直接调句柄
//! view()
//!     .flex_col()
//!     .flex_1()
//!     .child(nsh_erika::ErikaVideo(&player).flex_1())
//!     .child(text("控制条写在这：button().on_press(move || player.pause())"))
//! # }
//! ```
//!
//! 控件**不自建引擎**、也没有任何控制 prop：它的 prop 表只有 `source`（帧源的号）。
//! 为什么是号而不是句柄 —— NSH 的组件边界只能过 JSON，见 [`video`] 的说明。
//!
//! ## 平台
//!
//! 目前只有 Linux。macOS / Windows 要各自新写一份导出适配（erika 那边是 Metal / D3D 句柄）。
//!
//! 目录已经按「`core/` = 平台无关 / `linux/` = Linux」分过一轮，但**还没干净**。
//! 还耦合在 core 里的点都打了 `TODO(platform)`，用 `rg 'TODO\(platform\)' src` 一次列全：
//!
//! - `core/source.rs`：帧槽装的是 `erika::DmabufFrame`（Linux 专用类型）；
//! - `core/engine/render.rs`：`pump_frame` 调 `export_frame_dmabuf`（只有 Linux 的 erika 有）；
//! - `core/trace.rs`：`GtkTickProbe` 是 GTK 专用；
//! - `component.rs`：实际是 Linux/GTK 适配层，建议和 `linux/frames.rs` 一起挪进 `linux/`。
//!
//! 跨平台从这张清单开始拆：先把「决定出不出帧」和「按平台导出」分开，
//! 再抽象帧载荷（dma-buf / Metal 纹理 / D3D 共享纹理）。

#![warn(missing_docs)]

// 这个 crate 只有 Linux + Vulkan 的实测路径。换平台要新写一份导出适配,
// 与其让用户对着一堆上游报错猜,不如在编译期直接说清楚。
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
compile_error!(
    "nsh-erika 目前只有 Linux + Windows。\
     跨平台移植还没做:每个平台要新写一份导出适配（erika 侧是 Metal / D3D 句柄）。"
);

mod core;
#[cfg(target_os = "linux")]
pub(crate) use linux as backend;
#[cfg(target_os = "linux")]
pub use nsh_platform_linux as plaform;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;
pub use backend::native_view::ErikaView;
pub use core::engine::{ErikaEvent, ErikaEvents};
pub use core::player::{
    BlockedModes, DanmakuDensity, DanmakuFilter, DanmakuPaint, DanmakuPlacement, DanmakuSnapshot,
    DanmakuText, ErikaDanmaku, ErikaPlayer, ErikaStatus, ErikaSubtitle, ErikaTracks, FilterRule,
    SubtitleOverrides, SubtitlePlacement, SubtitleSnapshot, SubtitleTextStyle,
};
pub use core::source::{ErikaVideoSource, FrameImage, FrameRequest};
pub use erika::PlayerState;
#[cfg(target_os = "windows")]
pub use nsh_platform_windows as plaform;
pub use plaform::Components as PlaformComponents;
#[cfg(target_os = "windows")]
pub(crate) use windows as backend;
// 句柄的 `call` / `status` 直接暴露 erika 自己的类型:不另造一层。
pub use erika::MediaRequest;
/// 只导出**出现在我们公开签名 / 公开字段里**的上游类型 —— 消费者不依赖 erika。
pub use erika::danmaku::{DanmakuLayoutConfig, DanmakuShadowStyle, DanmakuTrackInfo};
pub use erika::presenter::{PresenterRuntime, PresenterStats};
pub use erika::subtitle::SubtitleTrackConfig;
pub use erika::{ChapterInfo, TrackInfo, TrackKind, TrackSelection};

/// 原生控件名。
///
/// 一般不用直接写：用 [`video`]（`nsh_erika::video(&source)`）就会带上 `source` prop。
/// 手写时是 `native(nsh_erika::COMPONENT_NAME).prop("source", source.id())`。
pub const COMPONENT_NAME: &str = "erika.video";

/// 把 `erika.video` 注册进 `components`。
///
/// 调用点是**应用 crate 的根**（`nsh.json` `"rust"` 那个 crate）：nsh 生成的宿主按平台
/// 去那里找固定名字的入口，不能写在这个库里面。
pub fn register(component: &PlaformComponents) {
    component.register(COMPONENT_NAME, backend::native_view::factory());
}

impl ErikaPlayer {
    /// 这个播放器的画面：`view().child(player.video().flex_1())`。
    ///
    /// 和 [`video`] 是同一个东西，只是从播放句柄出发——一个播放器只能有一个画面源，
    /// 所以不需要另外拿 [`ErikaVideoSource`]。返回的是普通的 NSH 元素，
    /// `.flex_1()` / `.rounded(…)` 这些照常链式调用。
    pub fn video(&self) -> nsh_core::prelude::NativeView {
        view_for(self.source_id())
    }
}

/// 一个播放器的画面：`view().child(ErikaVideo(&player).flex_1())`。
///
/// 这是对外的**唯一**画面入口 —— 里面做了三件事，使用者都看不到：把句柄换成一个号、
/// 生成 `native("erika.video")` 元素、把号作为 `source` prop 放上去。返回的是普通
/// NSH 元素，`.flex_1()` / `.rounded(…)` 这些照常链式调用。
///
/// 传 `&player` 或 `player.clone()` 都行（`Borrow`）。
///
/// 名字是 PascalCase 只是为了写起来像元素；NSH 自带的元素函数是 snake_case
/// （`video()`、`text()`），两者风格上并不冲突。
#[allow(non_snake_case)]
pub fn ErikaVideo(player: impl std::borrow::Borrow<ErikaPlayer>) -> nsh_core::prelude::NativeView {
    view_for(player.borrow().source_id())
}

/// 帧源号 → `erika.video` 元素。
fn view_for(id: u64) -> nsh_core::prelude::NativeView {
    nsh_core::prelude::native(COMPONENT_NAME).prop("source", nsh_core::JsonValue::from(id))
}

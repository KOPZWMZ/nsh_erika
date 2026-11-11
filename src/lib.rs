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
//! // 页面
//! let player = ErikaPlayer::new()?.0;             // 句柄，Clone + Send + Sync
//! player.open(MediaRequest::new(uri));
//! player.play();
//! view().child(native("erika.video").prop("src", ...).prop("paused", ...))
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
//! ┌ 内核层 —— 不认识任何 UI 框架 ────────────────────────────────────────┐
//! │ engine/     ErikaEngine(actor):PresenterRuntime 的唯一 owner          │
//! │   mod.rs      结构体 + run 循环 + on_cmd 分派 + 状态发布               │
//! │   render.rs   on_render + pump_frame：Wake / 出帧 / 保活泵            │
//! │ cmd.rs      命令词汇表:ErikaCmd 五族                                  │
//! │ player.rs   ErikaPlayer + ErikaStatus                                │
//! │ source.rs   ErikaVideoSource + FrameSlot（一格 dma-buf 帧槽）        │
//! └──────────────────────────────────────────────────────────────────────┘
//! ┌ 适配层 —— 唯一允许出现 gtk 的地方 ───────────────────────────────────┐
//! │ component.rs  ErikaView：`gtk::Picture` + tick 回调 + prop 分发       │
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
//! 音频、也不计入渲染帧，见 dev_doc §8）。宿主（GTK/GSK）导不进来就没有画面；这种机器上
//! 产品策略是整条降级到 NSH 内置 `<video>`（GStreamer）。
//!
//! ## 平台
//!
//! 目前只有 Linux。macOS / Windows 要各自新写一份导出适配（erika 那边是 Metal / D3D 句柄）。

#![warn(missing_docs)]

// 这个 crate 只有 Linux + Vulkan 的实测路径。换平台要新写一份导出适配,
// 与其让用户对着一堆上游报错猜,不如在编译期直接说清楚。
#[cfg(not(target_os = "linux"))]
compile_error!(
    "nsh-erika 目前只有 Linux + Vulkan（dma-buf）的实测路径。\
     跨平台移植还没做:每个平台要新写一份导出适配（erika 侧是 Metal / D3D 句柄）。"
);

mod cmd;
mod engine;
mod player;
mod source;
mod trace;

#[cfg(target_os = "linux")]
mod component;

#[cfg(target_os = "linux")]
pub use component::ErikaView;

pub use engine::{ErikaEvent, ErikaEvents};
pub use erika::PlayerState;

pub use player::{
    BlockedModes, DanmakuDensity, DanmakuFilter, DanmakuPaint, DanmakuPlacement, DanmakuSnapshot,
    DanmakuText, ErikaDanmaku, ErikaPlayer, ErikaStatus, ErikaSubtitle, ErikaTracks, FilterRule,
    PresenterObservation, SubtitleOverrides, SubtitlePlacement, SubtitleSnapshot,
    SubtitleTextStyle,
};
pub use source::{ErikaVideoSource, FrameRequest};
// 句柄的 `call` / `status` 直接暴露 erika 自己的类型:不另造一层。
pub use erika::presenter::{PresenterRuntime, PresenterStats};
/// 只导出**出现在我们公开签名 / 公开字段里**的上游类型 —— 消费者不依赖 erika。
pub use erika::danmaku::{DanmakuLayoutConfig, DanmakuShadowStyle, DanmakuTrackInfo};
pub use erika::{ChapterInfo, TrackInfo, TrackKind, TrackSelection};
pub use erika::subtitle::SubtitleTrackConfig;
pub use erika::MediaRequest;

/// 原生控件名：页面里写 `native("erika.video")`。
pub const COMPONENT_NAME: &str = "erika.video";

/// 把 `erika.video` 注册进 `components`。
///
/// 调用点是**应用 crate 的根**（`nsh.json` `"rust"` 那个 crate）：nsh 生成的宿主按平台
/// 去那里找固定名字的入口，不能写在这个库里面。
#[cfg(target_os = "linux")]
pub fn register(components: &nsh_platform_linux::Components) {
    components.register(COMPONENT_NAME, component::factory());
}

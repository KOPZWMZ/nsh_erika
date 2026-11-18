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
//! // 应用 crate 的根：一个平台一个函数。`#[native_components(…)]` 自己会加 `#[cfg]`，
//! // 所以这里不用写 cfg，也不用提平台 crate 的名字（类型走 `PlatformComponents`）。
//! #[native_components(linux)]
//! fn register_components_linux(components: &nsh_erika::PlatformComponents) {
//!     nsh_erika::register(components);
//! }
//! #[native_components(windows)]
//! fn register_components_windows(components: &nsh_erika::PlatformComponents) {
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
//! - **读**直连 `Player`:`state()` / `position()` / `duration()` / `is_playing()` /
//!   `tracks()` / `chapters()` ……(进度那一组用 `progress()`)
//! - **写**几乎全走队列,因为 `Player` 只管解码侧:暂停要 cork 音频设备、seek 要
//!   flush 音频环、变速要让音频做 time-stretch —— 只有持有 `audio_output` 的
//!   presenter 做得出来。
//!
//! ## 分层
//!
//! ```text
//! ┌ 内核层 core/ —— 不认识任何 UI 框架，也不认识 gtk / D2D ──────────────┐
//! │ engine/     ErikaEngine(actor):PresenterRuntime 的唯一 owner          │
//! │   engine.rs   结构体 + run 循环 + on_cmd 分派 + 状态发布               │
//! │   render.rs   on_render + pump_frame：Wake / 出帧 / 保活泵            │
//! │ cmd.rs      命令词汇表:ErikaCmd 五族                                  │
//! │ player.rs   ErikaPlayer + ErikaStatus                                │
//! │ listener.rs 三条订阅通道的出口:ErikaStatusListener / …Listener + ErikaEvent │
//! │ source.rs   ErikaVideoSource + FrameSlot + FrameImage（帧载荷）       │
//! └──────────────────────────────────────────────────────────────────────┘
//! ┌ 平台适配层 —— 唯一允许出现 gtk / D2D 的地方 ─────────────────────────┐
//! │ linux/native_view.rs   ErikaView：`gtk::Picture` + tick 回调          │
//! │ linux/frames.rs        FrameBinder：FrameImage → `GdkDmabufTexture`   │
//! │ windows/native_view.rs ErikaView：`draw(&ID2D1DeviceContext, size)`   │
//! └──────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## 画面怎么到屏幕上
//!
//! **Linux**：erika 的渲染器把合成结果导出成**线性 dma-buf**（`export_frame_dmabuf`），控件用
//! `GdkDmabufTexture` 直接导入交给 `gtk::Picture`。我们这边不建 Vulkan 设备，也不需要
//! 导入外部纹理。
//!
//! **Windows**：erika 没有 dma-buf 那种导出接口，走它现成的 Flutter texture 出口 ——
//! 引擎线程 `attach_surface(FlutterTexture{尺寸})` 一次，之后每轮 `render_tick` 让 erika
//! 渲染进离屏面并发布一张 `MISC_SHARED` 快照；控件在 `draw(&ID2D1DeviceContext, size)` 里
//! `OpenSharedResource` 打开同一块显存 → `CreateBitmapFromDxgiSurface` → `DrawBitmap`。
//! 整条链不过 CPU（代价是上游每帧一次 `CopyResource` + 一次 GPU 同步等待，见 dev_doc §5.7.1）。
//!
//! **每个平台只有一条路**：没有 CPU 回读兜底（`capture_frame_rgba` 是截图 API，按设计不启动
//! 音频、也不计入渲染帧，见 dev_doc §8）。宿主导不进来就没有画面。
//!
//! 导出 / 导入 / 拿不到共享纹理时，原因会写进 [`ErikaStatus::error`](crate::state::ErikaStatus)（`player.status().error` /
//! [`ErikaPlayer::last_error`]）并且**之后不再尝试**。这个库只负责**上报** —— 要不要换实现、
//! 换哪个、怎么提示用户，都由调用者决定（它不知道宿主还有没有别的播放器可用）。
//! `ErikaPlayer::new()` 本身就失败时是同步 `Err`，那时压根别建控件。
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
//! 为什么是号而不是句柄 —— NSH 的组件边界只能过 JSON，见 [`ErikaVideo`] 的说明。
//!
//! ## 平台
//!
//! | 平台 | 出帧 | 控件 | 状态 |
//! |---|---|---|---|
//! | Linux | erika 导出线性 dma-buf → `GdkDmabufTexture` → `gtk::Picture` | `linux/native_view.rs` | 跑通（含 VA-API 直通） |
//! | Windows | D3D11 共享纹理 → `OpenSharedResource` → D2D `DrawBitmap` | `windows/native_view.rs` | 跑通（硬解 + 弹幕） |
//! | macOS / Android | 未做 | — | `compile_error!` 在编译期挡住 |
//!
//! 帧载荷抽象成 `FrameImage`（各平台一份变体：Linux 的 `Dmabuf`、Windows 的 `SharedTexture`）；
//! 帧槽与 `pump_frame` 里"要不要出帧 / 保活泵"的策略是平台无关的，分平台的只有两处：
//! **怎么导出这一帧**、**怎么把它画到宿主上**。
//!
//! 分平台的点就这几处（`TODO(platform)` 已经清干净了）：
//!
//! - `core/source.rs`：`as_dmabuf()` / `as_shared_texture()` 各自只在对应平台编译；
//! - `core/engine/render.rs`：Windows 那一支必须先挂出图面才出帧（`ensure_win_surface`）；
//! - `core/trace.rs`：`TickProbe` 两平台共用（tick 来源不同，统计口径一样）。

#![warn(missing_docs)]

// Linux 和 Windows 各有一条跑通的出帧路径；其它平台还没有适配,
// 与其让用户对着一堆上游报错猜,不如在编译期直接说清楚。
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
compile_error!(
    "nsh-erika 目前只有 Linux（dma-buf）和 Windows（D3D11 共享纹理）两条出帧路径。\
     macOS（Metal 纹理）和 Android（ANativeWindow）还没做。"
);

mod core;
#[cfg(target_os = "linux")]
pub(crate) use linux as backend;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub(crate) use windows as backend;

// ──────────────────────────── 公开面（改之前先读）────────────────────────────
//
// 规则三条:
//
//   1. **根上只留"每个 app 都要手写"的**:建播放器 / 挂画面 / 注册控件 / 打开媒体 /
//      判断状态。就这几个,别再加。
//   2. **我们自己的类型按"屏幕上的关注点"分模块**:`danmaku` / `subtitle` / `tracks` /
//      `state` / `events` / `video`。内部 `core::` 永远私有,这些只是出口(纯 `pub use`,
//      不改内部结构)。
//   3. **上游类型一律走 `nsh_erika::erika::…`**;只有"出现在我们公开字段 / 参数里"的
//      才在对应模块就地再放一份 —— 让写面板的人不用在两个命名空间之间来回跳。
//   4. **逃生舱挂到 `escape-hatch` feature 下,默认不导出**(见 `video` 模块与
//      `ErikaPlayer::video_source`)。判据:它出现在首页就是"错误的邀请"。
//
// 想加一个导出之前先问"它属于上面哪一条"。一条都不属于 → 它大概是内部实现,不该导。

/// erika 上游的**全量** API —— 兜底路径。
///
/// 这里没有的、以及将来上游新增的，都去 `nsh_erika::erika::…` 找。
///
/// ⚠️ **不要**在自己的 `Cargo.toml` 里再直接依赖 liberika：只要 git rev / tag 和我们对不齐，
/// 就是**两个 crate、两套类型**，报错会变成
/// `expected erika::MediaRequest, found erika::MediaRequest`，几乎看不出原因。
pub use ::erika;

/// 弹幕：控制句柄 + 状态快照 + 五组样式。
pub mod danmaku {
    pub use crate::core::player::{
        BlockedModes, DanmakuDensity, DanmakuFilter, DanmakuPaint, DanmakuPlacement,
        DanmakuSnapshot, DanmakuText, ErikaDanmaku, FilterRule,
    };
    /// 整包样式（24 字段）。**需要 `escape-hatch`**：它和 `ErikaDanmaku::update_config`、
    /// `DanmakuSnapshot::config` 是一组，日常请用五组类型化视图。
    #[cfg(feature = "escape-hatch")]
    pub use ::erika::danmaku::DanmakuLayoutConfig;
    pub use ::erika::danmaku::{DanmakuShadowStyle, DanmakuTrackInfo};
}
/// 字幕：控制句柄 + 状态快照 + 外观 / 版面 / 延迟。
pub mod subtitle {
    pub use crate::core::player::{
        ErikaSubtitle, SubtitleOverrides, SubtitlePlacement, SubtitleSnapshot, SubtitleTextStyle,
    };
    pub use ::erika::subtitle::SubtitleTrackConfig;
}

/// 轨道：列表、选择、外挂字幕。
pub mod tracks {
    pub use crate::core::player::ErikaTracks;
    pub use ::erika::subtitle::SubtitleTrackConfig;
    pub use ::erika::{ChapterInfo, TrackInfo, TrackKind, TrackSelection};
}

/// 状态的两个快照：`ErikaStatus`（命令侧）与 `ErikaProgress`（10Hz 进度）。
pub mod state {
    pub use crate::core::player::{ErikaProgress, ErikaStatus};
    pub use ::erika::TrackSelection;
}

/// 事件流：上游离散事件的转发 + 三条订阅出口。
pub mod events {
    pub use crate::core::listener::{
        ErikaEvent, ErikaEventsListener, ErikaListener, ErikaProgressListener, ErikaStatusListener,
    };
    pub use ::erika::{PlayerState, TrackSelection};
}

/// 帧源与帧载荷：**自写控件 / 出帧自检**才用，普通应用用 [`ErikaVideo`](crate::ErikaVideo)。
#[cfg(feature = "escape-hatch")]
pub mod video {
    pub use crate::core::source::{ErikaVideoSource, FrameImage, FrameRequest};
    #[cfg(target_os = "windows")]
    pub use crate::windows::frame::SharedTexture;
    #[cfg(target_os = "linux")]
    pub use ::erika::DmabufFrame;
}
#[cfg(target_os = "linux")]
pub use nsh_platform_linux::Components as PlatformComponents;
#[cfg(target_os = "windows")]
pub use nsh_platform_windows::Components as PlatformComponents;

// ─── 根：只有这几个 ───────────────────────────────────────────────────────────

/// 打开媒体的请求（路径 / URL / HTTP 头 / 起始位置）。上游类型，根上留一份高频的。
pub use ::erika::MediaRequest;
/// 播放状态机的 8 个态。上游类型，根上留一份高频的（`match` 事件时天天写）。
pub use ::erika::PlayerState;
pub use core::player::ErikaPlayer;

/// 原生控件名。
///
/// 一般不用直接写：用 [`ErikaVideo`]（`nsh_erika::ErikaVideo(&player)`）就会带上 `source` prop。
/// 手写时是 `native(nsh_erika::COMPONENT_NAME).prop("source", source.id())`。
pub const COMPONENT_NAME: &str = "erika.video";

/// 把 `erika.video` 注册进 `components`。
///
/// 调用点是**应用 crate 的根**（`nsh.json` `"rust"` 那个 crate）：nsh 生成的宿主按平台
/// 去那里找固定名字的入口，不能写在这个库里面。
pub fn register(component: &PlatformComponents) {
    component.register(COMPONENT_NAME, backend::native_view::factory());
}

impl ErikaPlayer {
    /// 这个播放器的画面：`view().child(player.video().flex_1())`。
    ///
    /// 和 [`ErikaVideo`] 是同一个东西，只是从播放句柄出发——一个播放器只能有一个画面源，
    /// 所以不需要另外拿 `video::ErikaVideoSource`（那是逃生舱，要 `escape-hatch`）。返回的是普通的 NSH 元素，
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

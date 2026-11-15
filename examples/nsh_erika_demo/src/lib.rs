//! The app's Rust UI: its root component, which `nsh run`, `nsh package`
//! and `cargo run` mount.
//!
//! 这是 `nsh-erika` 的示例应用（`nsh_erika_demo`）：只有一个铺满窗口的 `erika.video`。
//! 片源从环境变量读，方便不改代码就换文件：
//!
//! ```sh
//! NSH_ERIKA_SRC=/path/to/video.mkv nsh run
//! ```

use nsh_base::{button, theme};
use nsh_core::prelude::*;
use nsh_erika::PlatformComponents;

/// The app's UI set (`nsh.json` `"rust"`): what a TSX entry may import from
/// `nsh_erika_demo/native`.
#[native_module("nsh_erika_demo/native")]
fn register_ui(registry: &mut Registry) {
    registry.register::<App>();
}

/// `nsh-erika` 的原生控件。入口必须写在应用 crate 的根：nsh 生成的宿主按平台
/// 来这里找固定名字的函数。

#[native_components(linux)]
fn register_components_linux(components: &PlatformComponents) {
    nsh_erika::register(components);
}

#[native_components(windows)]
fn register_components_window(components: &PlatformComponents) {
    nsh_erika::register(components);
}
/// The application's root component。
///
/// **句柄在外面**：`ErikaPlayer::new()` 在这里（页面只跑一次）创建，`player` 留下控制、
/// `source` 交给 `erika.video` 显示。控件本身不自建引擎、也没有控制 prop。
///
/// `NSH_PLAYER_BUILTIN=1` 时换成 NSH 自带的 `<video>`（GTK 的 `MediaFile`，底层是
/// GStreamer），用来做性能对照组。
#[component]
pub fn App() -> impl IntoElement {
    let builtin = std::env::var_os("NSH_PLAYER_BUILTIN").is_some();
    let src = std::env::var("NSH_ERIKA_SRC").unwrap_or_else(|_| "../../media/tiny.mkv".to_string());
    let danmaku_uri =
        std::env::var("DMK").unwrap_or_else(|_| "../../media/danmaku.json".to_string());
    let paused = signal(false);

    // 1) 外部创建句柄 + 打开媒体
    let mut handles = None;
    if !builtin {
        match nsh_erika::ErikaPlayer::new() {
            Ok(player) => {
                if !src.is_empty() {
                    player.open(nsh_erika::MediaRequest::new(src.clone()));
                    player.play();
                    player.danmaku().add_track(danmaku_uri, "bilibili")
                }
                handles = Some(player);
            }
            Err(error) => eprintln!("erika.video: 引擎起不来: {error}"),
        }
    }

    let root = view().flex_col().flex_1().bg(move || theme().background);
    if builtin {
        return root.child(
            video(move || src.clone())
                .flex_1()
                .paused(move || paused.get()),
        );
    }
    let Some(player) = handles else {
        return root.child(text("引擎起不来：看日志").flex_1());
    };

    // 2) 页面只放"显示"；控制条是普通的 NSH 元素，直接调句柄
    let control = player.clone();
    root.child(nsh_erika::ErikaVideo(&player).flex_1()).child(
        button()
            .px(px(16.0))
            .py(px(8.0))
            .rounded(move || theme().radius)
            .bg(move || theme().primary)
            .active(|s| s.bg(move || theme().primary.opacity(0.9)))
            .on_press(move || {
                if paused.get() {
                    control.play();
                    paused.set(false);
                } else {
                    control.pause();
                    paused.set(true);
                }
            })
            .child(
                text(move || if paused.get() { "播放" } else { "暂停" })
                    .text_color(move || theme().primary_foreground),
            ),
    )
}

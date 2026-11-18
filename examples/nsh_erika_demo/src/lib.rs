//! The app's Rust UI: its root component, which `nsh run`, `nsh package`
//! and `cargo run` mount.
//!
//! 这是 `nsh-erika` 的示例应用（`nsh_erika_demo`）：一块铺满的 `erika.video` +
//! 一条**手动控制条**（播放/暂停、±10s、音量、静音、停止）和一个状态行。
//!
//! 控制条是这套示例的主要用途：出帧、seek、音量这些路径光看日志分不清"引擎没动"
//! 还是"控件没贴图"，手动按几下最快。片源从环境变量读，方便不改代码就换文件：
//!
//! ```sh
//! NSH_ERIKA_SRC=/path/to/video.mkv DMK=/path/to/danmaku.json nsh run
//! DMK=off NSH_ERIKA_SRC=/path/to/video.mkv nsh run   # 不挂弹幕（量性能的对照组）
//! ```
//!
//! `NSH_PLAYER_BUILTIN=1` 时换成 NSH 自带的 `<video>`（底层 GStreamer），**用来做
//! 性能对照组**（比 CPU/GPU 占用），不是这个库的降级路径 —— 这个库失败只上报
//! `status.error`，换不换实现由调用者决定。

use std::time::Duration;

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
#[component]
pub fn App() -> impl IntoElement {
    let builtin = std::env::var_os("NSH_PLAYER_BUILTIN").is_some();
    let src = std::env::var("NSH_ERIKA_SRC").unwrap_or_else(|_| "../../media/tiny.mkv".to_string());
    let danmaku_uri =
        std::env::var("DMK").unwrap_or_else(|_| "../../media/danmaku.json".to_string());
    let paused = signal(false);

    let root = view().flex_col().flex_1().bg(move || theme().background);
    if builtin {
        return root.child(
            video(move || src.clone())
                .flex_1()
                .controls(true)
                .paused(move || paused.get()),
        );
    }

    // 1) 外部创建句柄 + 打开媒体
    let mut handles = None;
    if !src.is_empty() {
        match nsh_erika::ErikaPlayer::new() {
            Ok(player) => {
                player.open(nsh_erika::MediaRequest::new(src.clone()));
                player.play();
                // `DMK=off` 时不挂弹幕：量 CPU/GPU 时要一个"没有弹幕"的对照组。
                if danmaku_uri != "off" && !danmaku_uri.is_empty() {
                    player.danmaku().add_track(danmaku_uri, "bilibili");
                }
                handles = Some(player);
            }
            Err(error) => eprintln!("erika.video: 引擎起不来: {error}"),
        }
    }
    let Some(player) = handles else {
        return root.child(text("引擎起不来：看日志").flex_1());
    };

    // 2) 控制条的状态：引擎是异步 actor，UI **不去猜**状态，每 200ms 拉一次。
    //    按钮按下时也会立刻写一遍（否则要等下一次轮询才看到反馈）。
    let playing = signal(player.is_playing());
    let position = signal(player.position().as_secs_f64());
    let duration = signal(player.duration().map_or(0.0, |d| d.as_secs_f64()));
    let volume = signal(player.volume());
    let muted = signal(player.is_muted());
    let error = signal(player.last_error());

    let ticker = player.clone();
    // `set_interval` 归当前 Owner 所有：页面卸载时这个任务自动取消，
    // 它攥着的那个 `ticker` clone 也跟着释放（最后一个句柄走了引擎就退出）。
    set_interval(Duration::from_millis(200), move || {
        playing.set(ticker.is_playing());
        position.set(ticker.position().as_secs_f64());
        duration.set(ticker.duration().map_or(0.0, |d| d.as_secs_f64()));
        volume.set(ticker.volume());
        muted.set(ticker.is_muted());
        error.set(ticker.last_error());
    });

    // 3) 按钮的按下动作（每个按钮拿一份句柄 clone —— 它只是两个引用计数）
    let p_toggle = player.clone();
    let toggle = move || {
        if playing.get() {
            p_toggle.pause();
            playing.set(false);
        } else {
            p_toggle.play();
            playing.set(true);
        }
    };

    let p_back = player.clone();
    let back = move || {
        let now = p_back.position();
        p_back.seek(now.saturating_sub(Duration::from_secs(10)));
    };

    let p_forward = player.clone();
    let forward = move || {
        let now = p_forward.position();
        p_forward.seek(now + Duration::from_secs(10));
    };

    let p_stop = player.clone();
    let stop = move || {
        p_stop.stop();
        playing.set(false);
    };

    // 音量按 10% 一档：信号当"下一档"的基准，避免读引擎状态时的滞后。
    let p_quieter = player.clone();
    let quieter = move || {
        let next = (volume.get() - 0.1).max(0.0);
        p_quieter.set_volume(next);
        volume.set(next);
    };

    let p_louder = player.clone();
    let louder = move || {
        let next = (volume.get() + 0.1).min(1.0);
        p_louder.set_volume(next);
        volume.set(next);
    };

    let p_mute = player.clone();
    let mute = move || {
        let next = !muted.get();
        p_mute.set_muted(next);
        muted.set(next);
    };

    // 4) 页面只放"显示"；控制条是普通的 NSH 元素，直接调句柄
    root.child(nsh_erika::ErikaVideo(&player).flex_1()).child(
        view()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_4()
            .px(px(12.0))
            .py(px(8.0))
            .bg(move || theme().card)
            .child(tap(
                move || if playing.get() { "暂停" } else { "播放" }.to_string(),
                toggle,
            ))
            .child(tap(|| "-10s".to_string(), back))
            .child(tap(|| "+10s".to_string(), forward))
            .child(tap(|| "音量 -".to_string(), quieter))
            .child(tap(|| "音量 +".to_string(), louder))
            .child(tap(
                move || {
                    if muted.get() {
                        "取消静音"
                    } else {
                        "静音"
                    }
                    .to_string()
                },
                mute,
            ))
            .child(tap(|| "停止".to_string(), stop))
            .child(
                text(move || {
                    status_line(
                        position.get(),
                        duration.get(),
                        volume.get(),
                        muted.get(),
                        error.get(),
                    )
                })
                .text_color(move || theme().muted_foreground),
            ),
    )
}

/// 控制条上的一个按钮（示例用的最小样式：不做 `ui` 层，只取主题里的两三个 token）。
fn tap(label: impl Fn() -> String + 'static, on_press: impl Fn() + 'static) -> impl IntoElement {
    button()
        .px(px(12.0))
        .py(px(6.0))
        .rounded(move || theme().radius)
        .bg(move || theme().secondary)
        .hover(|style| style.bg(move || theme().accent))
        .active(|style| style.bg(move || theme().accent))
        .on_press(on_press)
        .child(text(label).text_color(move || theme().secondary_foreground))
}

/// 状态行：`1:23 / 4:56 · 音量 80%`，出错时把 `status.error` 接在后面。
///
/// 出错是这套适配层最要紧的信号（出帧路径没了就永远没有画面），所以直接显示在看得到的地方。
fn status_line(
    position: f64,
    duration: f64,
    volume: f64,
    muted: bool,
    error: Option<String>,
) -> String {
    let time = if duration > 0.0 {
        format!("{} / {}", clock(position), clock(duration))
    } else {
        clock(position)
    };
    let volume = if muted {
        "静音".to_string()
    } else {
        format!("音量 {:.0}%", volume * 100.0)
    };
    match error {
        Some(error) => format!("{time} · {volume} · ⚠ {error}"),
        None => format!("{time} · {volume}"),
    }
}

/// 秒 → `m:ss`（超过一小时补 `h:mm:ss`）。
fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

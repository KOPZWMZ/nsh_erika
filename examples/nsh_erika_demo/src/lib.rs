//! The app's Rust UI: its root component, which `nsh run`, `nsh package`
//! and `cargo run` mount.
//!
//! 这是 `nsh-erika` 的示例应用（`nsh_erika_demo`）。它现在同时是**生命周期验证台**：
//!
//! 1. **首页**：两个入口按钮 + 一个实时读数（本进程里 `erika-engine` 线程数、进入次数）。
//! 2. **播放页**：一块铺满的 `erika.video` + 一条**手动控制条**（播放/暂停、±10s、音量、
//!    静音、停止）和一个状态行，外加「← 返回」。
//!
//! 播放页是**独立组件**，所以「进入 = 建实例」「返回 = 实例销毁」—— 引擎跟着走。
//! 反复进出，首页那个线程数应该**回到 0**；回不去就是引擎没被回收。
//!
//! 播放页有两条路，用来分别验证两种所有权：
//!
//! | 入口 | 谁建引擎 | 控制从哪来 |
//! |---|---|---|
//! | 外部句柄 | 页面自己 `ErikaPlayer::new()` | 句柄方法（`player.seek()` ……） |
//! | 组件自建 | `erika_video().src(uri)` | props（`paused` / `muted` / `volume` / `rate`） |
//!
//! 片源从环境变量读，方便不改代码就换文件：
//!
//! ```sh
//! TEST_VIDEO=/path/to/video.mkv TEST_DANMAKU=/path/to/danmaku.json nsh run
//! TEST_DANMAKU=off TEST_VIDEO=/path/to/video.mkv nsh run   # 不挂弹幕（量性能的对照组）
//! ```
//!
//! `NSH_PLAYER_BUILTIN=1` 时换成 NSH 自带的 `<video>`（底层 GStreamer），**用来做
//! 性能对照组**（比 CPU/GPU 占用），不是这个库的降级路径 —— 这个库失败只上报
//! `status.error`，换不换实现由调用者决定。

use std::time::Duration;

use nsh_base::{button, theme};

use nsh_core::prelude::*;
use nsh_erika::{PlatformComponents, PlayerState, events::ErikaEvent};
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

/// 两个页面共用的跳转状态。`Signal` 是 `Copy`，所以整包也能当 context 值随便传。
#[derive(Clone, Copy)]
struct Pages {
    /// 在不在播放页。
    open: Signal<bool>,
    /// 播放页里由**谁**建引擎：`false` = 页面用句柄（`ErikaPlayer::new()`），
    /// `true` = 组件自建（`erika_video().src(uri)`，没有句柄）。
    own: Signal<bool>,
    /// 进过几次播放页 —— 用来确认"反复进出"真的发生了。
    visits: Signal<u32>,
}

/// 应用根：**只决定"在哪个页面"，不持有任何引擎**。
///
/// 播放页放在 [`when`] 的一个分支里，所以它随条件**建/销**：返回首页 = 实例销毁 =
/// 它的 `stored` / 订阅任务 / 元素闭包一起释放，引擎的最后一个引用就在这里归零。
#[component]
pub fn App() -> impl IntoElement {
    let pages = Pages {
        open: signal(false),
        own: signal(false),
        visits: signal(0),
    };
    provide_context(pages);

    view()
        .flex_col()
        .flex_1()
        .bg(move || theme().background)
        .child(when(pages.open, || player_page()).otherwise(|| home_page()))
}

/// 首页：入口按钮 + 生命周期读数。
#[component]
pub fn HomePage() -> impl IntoElement {
    let pages = use_context::<Pages>().expect("App 应该 provide 了 Pages");

    // 实时线程数：每 300ms 采一次。
    // `set_interval` 返回的 `Spawned` 一被 drop 就会取消任务，而组件体**返回即结束** ——
    // 所以必须把它存进 owner（`stored`），不能只用 `let _ = …`。
    let threads = signal(engine_threads());
    let _ticker = stored(set_interval(Duration::from_millis(300), move || {
        threads.set(engine_threads())
    }));

    view()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_4()
        .px(px(24.0))
        .child(
            text("nsh-erika 生命周期验证")
                .text_color(move || theme().foreground),
        )
        .child(tap(
            || "进入播放页（外部句柄）".to_string(),
            {
                let pages = pages;
                move || {
                    pages.own.set(false);
                    pages.visits.update(|n| *n += 1);
                    pages.open.set(true);
                }
            },
        ))
        .child(tap(
            || "进入播放页（组件自建引擎）".to_string(),
            {
                let pages = pages;
                move || {
                    pages.own.set(true);
                    pages.visits.update(|n| *n += 1);
                    pages.open.set(true);
                }
            },
        ))
        .child(
            text(move || format!("erika 线程数：{}（0 = 引擎已全部回收）", threads.get()))
                .text_color(move || theme().foreground),
        )
        .child(
            text(move || format!("进入次数：{}", pages.visits.get()))
                .text_color(move || theme().muted_foreground),
        )
        .child(
            text("进入 → 返回，重复几次：数字应该回到 0。一个活着的引擎约 30~40 个线程（解码/渲染的子线程继承了线程名），所以别拿 1 当基准。")
                .text_color(move || theme().muted_foreground),
        )
}

/// 播放页：**页面的寿命 = 引擎的寿命**。
///
/// 三条路都从同一个 `root` 长出来（同一具体类型，可以直接 `return`）：
/// 内置 `<video>` 对照组 / 组件自建引擎 / 外部句柄。
#[component]
pub fn PlayerPage() -> impl IntoElement {
    let pages = use_context::<Pages>().expect("App 应该 provide 了 Pages");
    let builtin = std::env::var_os("NSH_PLAYER_BUILTIN").is_some();
    let src = std::env::var("TEST_VIDEO").unwrap_or_else(|_| "../../media/tiny.mkv".to_string());
    let danmaku_uri =
        std::env::var("TEST_DANMAKU").unwrap_or_else(|_| "../../media/danmaku.json".to_string());
    let paused = signal(false);

    // 挂载/卸载各打一行：控制台里能直接对上"进入 → 建引擎"和"返回 → 销毁"。
    eprintln!("[demo] 播放页挂载（组件自建 = {}）", pages.own.get());
    cleanup(move || eprintln!("[demo] 播放页卸载：引擎应该开始销毁"));

    let root = view().flex_col().flex_1().bg(move || theme().background);

    // 返回按钮：所有分支共用。它只碰 context 里的信号 —— 不持有引擎。
    let leave = {
        let pages = pages;
        move || pages.open.set(false)
    };

    // ── 对照组：NSH 自带 <video>（底层 GStreamer）────────────────────────────
    if builtin {
        return root
            .child(
                video(move || src.clone())
                    .flex_1()
                    .controls(true)
                    .paused(move || paused.get()),
            )
            .child(bar().child(tap(|| "← 返回".to_string(), leave)));
    }

    // ── 组件自建引擎：没有句柄，控制全走 props ────────────────────────────────
    if pages.own.get() {
        let toggle_paused = {
            let paused = paused;
            move || paused.update(|p| *p = !*p)
        };
        return root
            .child(
                nsh_erika::erika_video()
                    .src(src)
                    .paused(move || paused.get())
                    .flex_1(),
            )
            .child(
                bar()
                    .child(tap(
                        move || {
                            if paused.get() {
                                "播放".to_string()
                            } else {
                                "暂停".to_string()
                            }
                        },
                        toggle_paused,
                    ))
                    .child(tap(|| "← 返回（销毁引擎）".to_string(), leave))
                    .child(
                        text("引擎归组件：返回即销毁，没有句柄可用")
                            .text_color(move || theme().muted_foreground),
                    ),
            );
    }

    // ── 外部句柄：从这个页面的**邮箱**里拿，页面销毁时组件把槽清空 ───────────
    if src.is_empty() {
        return root
            .child(text("没有片源：设 VIDEO").flex_1())
            .child(bar().child(tap(|| "← 返回".to_string(), leave)));
    }

    // 邮箱：页面建、组件填。它不是句柄，只是一个"填/取"的落点。
    let slot = nsh_erika::ErikaSlot::new();
    let playing = signal(false);
    let position = signal(Duration::ZERO);
    let duration = signal(None::<Duration>);
    let volume = signal(1.0_f32);
    let muted = signal(false);
    let error = signal(None::<String>);

    // 邮箱是**异步填的**（父级先跑、组件后跑）：等它第一次有值，再把三条订阅接上。
    // `armed` 是 `Stored`（非响应式），用来保证只订阅一次 —— effect 会随邮箱变化再跑。
    let armed = stored(false);
    effect(move || {
        let Some(player) = slot.player() else { return };
        if armed.get() {
            return;
        }
        armed.with(|a| *a = true);

        // 初值直读（订阅只报订阅之后的变化）
        playing.set(player.is_playing());
        volume.set(player.volume());
        muted.set(player.is_muted());
        error.set(player.last_error());

        // ① 进度：10Hz，位置 + 时长引擎已经算好
        let mut progress = player.subscribe_progress();
        spawn(async move {
            while let Some(p) = progress.next().await {
                position.set(p.position);
                duration.set(p.duration);
            }
        });

        // ② 事件：只在"发生的那一刻"用
        let mut events = player.subscribe_events();
        spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    ErikaEvent::StateChanged(state) => {
                        playing.set(matches!(state, PlayerState::Playing));
                        println!("状态改变了: {state:?}");
                    }
                    ErikaEvent::Ended => {
                        playing.set(false);
                        println!("播完了");
                    }
                    ErikaEvent::Error(text) => {
                        error.set(Some(text.clone()));
                        println!("Erika 引擎出错了: {text}");
                    }
                    ErikaEvent::TracksChanged => println!("轨道改变了"),
                    ErikaEvent::TrackSelectionChanged(_) => println!("选择了轨道"),
                    ErikaEvent::Buffering(buffering) => println!("缓冲状态: {buffering}"),
                    ErikaEvent::Resync { missed } => println!("事件落后，丢了 {missed} 条"),
                    _ => {}
                }
            }
        });

        // ③ presenter 侧的观测量（音量 / 静音 / 错误）
        let mut status = player.subscribe_status();
        spawn(async move {
            while let Some(snapshot) = status.next().await {
                volume.set(snapshot.volume);
                muted.set(snapshot.muted);
                error.set(snapshot.error);
            }
        });

        // 弹幕（`DANMAKU=off` 时不挂，做性能对照）
        if danmaku_uri != "off" && !danmaku_uri.is_empty() {
            player.danmaku().add_track(danmaku_uri.clone(), "bilibili");
        }
    });

    // 控制条的每个动作都现取句柄：`None` = 还没挂载 / 已卸载，一律静默丢弃。
    let toggle = move || {
        let Some(p) = slot.player() else { return };
        if playing.get() {
            p.pause();
            playing.set(false);
        } else {
            p.play();
            playing.set(true);
        }
    };
    let seek_back = move || {
        if let Some(p) = slot.player() {
            let now = p.position();
            p.seek(now.saturating_sub(Duration::from_secs(10)));
        }
    };
    let seek_forward = move || {
        if let Some(p) = slot.player() {
            p.seek(p.position() + Duration::from_secs(10));
        }
    };
    let stop = move || {
        if let Some(p) = slot.player() {
            p.stop();
            playing.set(false);
        }
    };
    // 音量按 10% 一档：信号当"下一档"的基准，避免读引擎状态时的滞后。
    let quieter = move || {
        let next = (volume.get() - 0.1).max(0.0);
        if let Some(p) = slot.player() {
            p.set_volume(next);
        }
        volume.set(next);
    };
    let louder = move || {
        let next = (volume.get() + 0.1).min(1.0);
        if let Some(p) = slot.player() {
            p.set_volume(next);
        }
        volume.set(next);
    };
    let mute = move || {
        let next = !muted.get();
        if let Some(p) = slot.player() {
            p.set_muted(next);
        }
        muted.set(next);
    };

    root.child(nsh_erika::erika_video().src(src.clone()).slot(slot).flex_1())
        .child(
            bar()
                .child(tap(
                    move || if playing.get() { "暂停" } else { "播放" }.to_string(),
                    toggle,
                ))
                .child(tap(|| "-10s".to_string(), seek_back))
                .child(tap(|| "+10s".to_string(), seek_forward))
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
                .child(tap(|| "← 返回（销毁引擎）".to_string(), leave))
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

/// 本进程里叫 `erika-engine` 的线程数。
///
/// Linux 读 `/proc/self/task/*/comm`（线程名就是 `try_spawn_thread("erika-engine", …)`
/// 传的那个）；别的平台没有等价物，返回 `0` 并在界面上显示成 0（**不代表没有泄漏**）。
fn engine_threads() -> usize {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir("/proc/self/task")
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| {
                        std::fs::read_to_string(entry.path().join("comm"))
                            .is_ok_and(|name| name.trim() == "erika-engine")
                    })
                    .count()
            })
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// 控制条的外壳（两行按钮 + 状态行）。
fn bar() -> nsh_core::element::View {
    view()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_4()
        .px(px(12.0))
        .py(px(8.0))
        .bg(move || theme().card)
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
    position: Duration,
    duration: Option<Duration>,
    volume: f32,
    muted: bool,
    error: Option<String>,
) -> String {
    // 时间显示只到秒，两个字段都来自同一次进度采样（不会出现 position > duration）。
    let position = position.as_secs_f64();
    let duration = duration.map_or(0.0, |d| d.as_secs_f64());
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

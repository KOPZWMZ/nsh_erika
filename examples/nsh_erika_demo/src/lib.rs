//! The app's Rust UI: its root component, which `nsh run`, `nsh package`
//! and `cargo run` mount.
//!
//! 这是 `nsh-erika` 的示例应用，同时是**手动验收台**。它有三个页面，分别验证一件事：
//!
//! | 页面 | 建引擎的是 | 有控制条吗 | 验证什么 |
//! |---|---|---|---|
//! | ① 生命周期 | `ErikaPlayer::new()`（页面建） | ❌ 只有「返回」 | **进出页面 = 建/销引擎**（首页有线程数读数） |
//! | ② 外部句柄 | 同上 | ✅ 句柄方法 | 全能力控制：播放/seek/音量/静音/停止 + 进度与事件订阅 |
//! | ③ 内部自建 | `erika_video().src(uri)`（组件建） | ✅ 只走 props | 纯声明式：`playing` / `volume` / `muted` / `rate` 控制 + `on_progress` / `on_ended` 读回 |
//!
//! 三个页面都是**独立组件**，所以「进入 = 建实例」「返回 = 实例销毁」。引擎的生死：
//!
//! - ①② 的引擎由页面 `ErikaPlayer::new()` 建，句柄里带一张**所有权票据**；组件挂载时
//!   `adopt()` 认领它，从此关机权归组件 —— 返回即停、摘表、画面解绑。
//! - ③ 的引擎由组件自己建，守卫直接放在组件 owner 里。
//!
//! 片源从环境变量读，方便不改代码就换文件：
//!
//! ```sh
//! TEST_VIDEO=/path/to/video.mkv TEST_DANMAKU=/path/to/danmaku.json nsh run
//! TEST_DANMAKU=off TEST_VIDEO=/path/to/video.mkv nsh run   # 不挂弹幕
//! ```

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

/// 当前在哪个页面。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    /// ① 只验证创建/销毁：没有控制条。
    Lifecycle,
    /// ② 外部句柄 + 控制条。
    External,
    /// ③ 组件自建引擎 + props 控制。
    Internal,
}

/// 三个页面共用的跳转状态。`Signal` 是 `Copy`，整包塞进 context。
#[derive(Clone, Copy)]
struct Pages {
    page: Signal<Page>,
    /// 进过几次播放页 —— 用来确认"反复进出"真的发生了。
    visits: Signal<u32>,
}

impl Pages {
    fn enter(self, page: Page) {
        self.visits.update(|n| *n += 1);
        self.page.set(page);
    }
    fn back(self) {
        self.page.set(Page::Home);
    }
}

/// 应用根：**只决定"在哪个页面"，不持有任何引擎**。
///
/// 每个页面放在 [`when`] 的一个分支里，所以它随条件建/销：返回首页 = 实例销毁 =
/// 它建的东西（引擎、订阅任务、元素闭包）一起释放。
#[component]
pub fn App() -> impl IntoElement {
    let pages = Pages {
        page: signal(Page::Home),
        visits: signal(0),
    };
    provide_context(pages);

    let on = move |page: Page| computed(move || pages.page.get() == page);

    view()
        .flex_col()
        .flex_1()
        .bg(move || theme().background)
        .child(when(on(Page::Lifecycle), || lifecycle_page()))
        .child(when(on(Page::External), || external_page()))
        .child(when(on(Page::Internal), || internal_page()))
        .child(when(on(Page::Home), || home_page()))
}

/// 首页：三个入口 + 生命周期读数。
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
        .child(text("nsh-erika 示例").text_color(move || theme().foreground))
        .child(tap(
            || "① 生命周期：进入即建、返回即销（无控制条）".to_string(),
            {
                let pages = pages;
                move || pages.enter(Page::Lifecycle)
            },
        ))
        .child(tap(
            || "② 外部句柄 + 控制条（ErikaPlayer::new + slot 控制）".to_string(),
            {
                let pages = pages;
                move || pages.enter(Page::External)
            },
        ))
        .child(tap(
            || "③ 组件自建引擎 + props 控制（纯声明式）".to_string(),
            {
                let pages = pages;
                move || pages.enter(Page::Internal)
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
            text("反复进出：数字应该回到 0。一个活着的引擎约 30~40 个线程（子线程继承了线程名），别拿 1 当基准。")
                .text_color(move || theme().muted_foreground),
        )
}

/// ① 只验证创建/销毁：**没有任何控制方法**，进去出画面，返回就销毁。
#[component]
pub fn LifecyclePage() -> impl IntoElement {
    let pages = use_context::<Pages>().expect("App 应该 provide 了 Pages");
    let src = test_video();
    let root = view().flex_col().flex_1().bg(move || theme().background);
    let back = {
        let pages = pages;
        move || pages.back()
    };

    let player = match nsh_erika::ErikaPlayer::new() {
        Ok(player) => player,
        Err(error) => {
            eprintln!("[demo] 引擎起不来: {error}");
            return root
                .child(text("引擎起不来：看日志").flex_1())
                .child(bar().child(tap(|| "← 返回".to_string(), back)));
        }
    };
    eprintln!(
        "[demo] LifecyclePage 挂载：erika 线程数 = {}",
        engine_threads()
    );
    cleanup(move || eprintln!("[demo] LifecyclePage 卸载：引擎应该开始销毁"));

    // 句柄交给组件即完成使命（组件在挂载时认领票据）；这一页不调任何控制方法。
    // `playing(true)` 是**固定值**：等于"初始配置"—— 不写它组件不会替你做决定（不自动播）。
    root.child(
        nsh_erika::erika_video()
            .src(src)
            .playing(true)
            .bind_player(player)
            .flex_1(),
    )
    .child(
        bar()
            .child(tap(|| "← 返回（销毁引擎）".to_string(), back))
            .child(
                text("这一页没有控制条：只看画面，返回即销毁")
                    .text_color(move || theme().muted_foreground),
            ),
    )
}

/// ② 外部句柄 + 控制条：句柄在页面里建，控制条直接调它的方法。
#[component]
pub fn ExternalPage() -> impl IntoElement {
    let pages = use_context::<Pages>().expect("App 应该 provide 了 Pages");
    let src = test_video();
    let danmaku_uri = test_danmaku();
    let root = view().flex_col().flex_1().bg(move || theme().background);
    let back = {
        let pages = pages;
        move || pages.back()
    };

    if src.is_empty() {
        return root
            .child(text("没有片源：设 TEST_VIDEO").flex_1())
            .child(bar().child(tap(|| "← 返回".to_string(), back)));
    }

    // 句柄：**这一行就把引擎建好了**（同步），所以三条订阅可以立刻接 —— 不用等挂载。
    // 它带一张"所有权票据"：交给组件后，组件在挂载时认领，卸载即销毁。
    let player = match nsh_erika::ErikaPlayer::new() {
        Ok(player) => player,
        Err(error) => {
            eprintln!("[demo] 引擎起不来: {error}");
            return root
                .child(text("引擎起不来：看日志").flex_1())
                .child(bar().child(tap(|| "← 返回".to_string(), back)));
        }
    };
    eprintln!(
        "[demo] ExternalPage 挂载：erika 线程数 = {}",
        engine_threads()
    );
    cleanup(move || eprintln!("[demo] ExternalPage 卸载：引擎应该开始销毁"));

    let initial = player.progress();
    let playing = signal(player.is_playing());
    let position = signal(initial.position);
    let duration = signal(initial.duration);
    let volume = signal(player.volume());
    let muted = signal(player.is_muted());
    let error = signal(player.last_error());

    // ① 进度：10Hz，位置 + 时长引擎已经算好
    let mut progress = player.subscribe_progress();
    spawn(async move {
        while let Some(p) = progress.next().await {
            position.set(p.position);
            duration.set(p.duration);
        }
    });

    // ② 事件：只在"发生的那一刻"用（`StateChanged` 自带新状态，所以这条任务不用持句柄）
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

    // 打开 + 起播（命令进队列，立刻返回）
    player.open(nsh_erika::MediaRequest::new(src.clone()));
    player.play();
    if danmaku_uri != "off" && !danmaku_uri.is_empty() {
        player.danmaku().add_track(danmaku_uri, "弹幕");
    }

    // 控制条：句柄 clone 只是引用计数 +1，动作直接调
    let toggle = {
        let player = player.clone();
        move || {
            if playing.get() {
                player.pause();
                playing.set(false);
            } else {
                player.play();
                playing.set(true);
            }
        }
    };
    let seek_back = {
        let player = player.clone();
        move || {
            let now = player.position();
            player.seek(now.saturating_sub(Duration::from_secs(10)));
        }
    };
    let seek_forward = {
        let player = player.clone();
        move || player.seek(player.position() + Duration::from_secs(10))
    };
    let stop = {
        let player = player.clone();
        move || {
            player.stop();
            playing.set(false);
        }
    };
    // 音量按 10% 一档：信号当"下一档"的基准，避免读引擎状态时的滞后。
    let quieter = {
        let player = player.clone();
        move || {
            let next = (volume.get() - 0.1).max(0.0);
            player.set_volume(next);
            volume.set(next);
        }
    };
    let louder = {
        let player = player.clone();
        move || {
            let next = (volume.get() + 0.1).min(1.0);
            player.set_volume(next);
            volume.set(next);
        }
    };
    let mute = {
        let player = player.clone();
        move || {
            let next = !muted.get();
            player.set_muted(next);
            muted.set(next);
        }
    };

    // 页面自己 open + play + 挂弹幕：所以**不传 `src`**（组件不该再开一遍 —— 换片那条
    // 路径会清掉弹幕轨），也不写 `playing`（不干预）。画面只负责显示。
    root.child(
        nsh_erika::erika_video_with(player).flex_1(),
    )
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
            .child(tap(|| "← 返回（销毁引擎）".to_string(), back))
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

/// ③ 组件自建引擎：**没有句柄**，控制全走 props。
#[component]
pub fn InternalPage() -> impl IntoElement {
    let pages = use_context::<Pages>().expect("App 应该 provide 了 Pages");
    let src = test_video();
    let root = view().flex_col().flex_1().bg(move || theme().background);
    let back = {
        let pages = pages;
        move || pages.back()
    };

    // 打开就播：固定值之外的另一种用法 —— 绑定到信号（= 控制端）。
    let playing = signal(true);
    // 读侧：**没有句柄**，全靠组件事件（`on_progress` / `on_ended`）。
    let position = signal(Duration::ZERO);
    let duration = signal(Option::<Duration>::None);
    let ended = signal(false);
    let muted = signal(false);
    let volume = signal(1.0_f32);
    // 倍速在 0.5 / 1 / 1.5 / 2 之间循环
    let rate = signal(1.0_f64);

    let toggle_playing = {
        let playing = playing;
        move || playing.update(|p| *p = !*p)
    };
    let toggle_muted = {
        let muted = muted;
        move || muted.update(|m| *m = !*m)
    };
    let quieter = {
        let volume = volume;
        move || volume.update(|v| *v = (*v - 0.1).max(0.0))
    };
    let louder = {
        let volume = volume;
        move || volume.update(|v| *v = (*v + 0.1).min(1.0))
    };
    let cycle_rate = {
        let rate = rate;
        move || {
            rate.update(|r| {
                *r = match *r {
                    x if x < 0.75 => 1.0,
                    x if x < 1.25 => 1.5,
                    x if x < 1.75 => 2.0,
                    _ => 0.5,
                }
            })
        }
    };

    root.child(
        nsh_erika::erika_video()
            .src(src)
            .playing(move || playing.get())
            .muted(move || muted.get())
            .volume(move || volume.get())
            .rate(move || rate.get())
            .on_progress(move |progress: &nsh_erika::events::VideoProgress| {
                position.set(Duration::from_millis(progress.position_ms));
                duration.set(progress.duration_ms.map(Duration::from_millis));
            })
            .on_ended(move || ended.set(true))
            .flex_1(),
    )
    .child(
        bar()
            .child(tap(
                move || {
                    if playing.get() {
                        "暂停".to_string()
                    } else {
                        "播放".to_string()
                    }
                },
                toggle_playing,
            ))
            .child(tap(|| "音量 -".to_string(), quieter))
            .child(tap(|| "音量 +".to_string(), louder))
            .child(tap(
                move || {
                    if muted.get() {
                        "取消静音".to_string()
                    } else {
                        "静音".to_string()
                    }
                },
                toggle_muted,
            ))
            .child(tap(move || format!("倍速 {:.1}x", rate.get()), cycle_rate))
            .child(tap(|| "← 返回（销毁引擎）".to_string(), back))
            .child(
                text(move || {
                    let total = duration
                        .get()
                        .map(|duration| clock(duration.as_secs_f64()))
                        .unwrap_or_else(|| "--:--".to_string());
                    let line = format!("{} / {}", clock(position.get().as_secs_f64()), total);
                    if ended.get() {
                        format!("{line}   播完了")
                    } else {
                        line
                    }
                })
                .text_color(move || theme().muted_foreground),
            ),
    )
}

/// 片源：`TEST_VIDEO`，默认仓库里的 `media/tiny.mkv`。
fn test_video() -> String {
    std::env::var("TEST_VIDEO").unwrap_or_else(|_| "../../media/tiny.mkv".to_string())
}

/// 弹幕：`TEST_DANMAKU`，`off` = 不挂。
fn test_danmaku() -> String {
    std::env::var("TEST_DANMAKU").unwrap_or_else(|_| "../../media/danmaku.json".to_string())
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

/// 控制条的外壳（一行按钮 + 状态行）。
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

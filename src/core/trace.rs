//! **所有诊断集中在这里**（开关：`NSH_ERIKA_TRACE=1`，任何非空、非 `0` 的值）。
//!
//! ⚠️ **这个模块不参与任何逻辑**。关掉时每个调用点只剩一次 `Option::is_some/as_mut`
//! 判断：没有分配、没有时间查询、没有日志格式化。往里加东西时守住这条 ——
//! 需要"每帧都算"的观测量不算诊断，不该放这里。
//!
//! 三个探针，各回答一个问题：
//!
//! | 探针 | 在哪用 | 回答 |
//! |---|---|---|
//! | [`RateProbe`] | `engine.rs` 的 `run` 循环 | 引擎线程每秒醒几次、这一轮出帧了没有 |
//! | [`TimingsProbe`] | `engine/render.rs::pump_frame` | erika 内部各阶段耗时（pump/sub/video/audio/danmaku/render） |
//! | [`TickProbe`] | 控件的 tick：Linux 的 `linux/frames.rs`、Windows 的 `windows/native_view.rs` | 宿主每秒 tick/paint 几次、引擎发了几帧、最大间隔 |
//!
//! 两行一起看就能分清"是谁在降频"：
//!
//! ```text
//! NSH_ERIKA_TRACE=1 nsh run
//! [nsh-erika] engine tick ≈100/s render=true has_media=true
//! [nsh-erika] tick ≈60/s paint ≈60/s frames ≈60/s maxgap=18ms
//! ```
//!
//! - `engine tick` 在、`tick` 不出现 → 宿主那份 tick 停了（Linux：GTK frame clock；Windows：NSH 不再重绘）；
//! - `render=false has_media=true` → 控件没在要帧（最小化 / 被盖住 / GSK 限频），
//!   引擎只跑保活泵（每 50ms 一次 `render_tick`）：画面停、声音继续，这是预期行为；
//! - 两个都在但 `frames < tick` → 引擎出帧跟不上，看 `耗时 …` 那行（[`TimingsProbe`]）。
//!
//! 另外还有几条**只为确认"有没有出画"**的一次性日志，不是探针（见各自的注释）：
//! `第一帧 dma-buf …`（`frames.rs`）、`erika.video: tick …`（每 120 tick 一行）、
//! 保活泵失败的一次性 WARN。导入 / 导出失败则会写进 `status().error`，不只打日志。

use std::time::{Duration, Instant};

/// 统一的开关判断：`NSH_ERIKA_TRACE=1`（任何非空、非 `0` 的值）。
pub(crate) fn enabled() -> bool {
    std::env::var_os("NSH_ERIKA_TRACE")
        .map(|value| !value.is_empty() && value != "0")
        .unwrap_or(false)
}

/// 一个"每秒打一行"的计数器。
pub(crate) struct RateProbe {
    label: &'static str,
    started: Instant,
    count: u64,
}

impl RateProbe {
    /// 开关打开时返回 `Some`，否则 `None`。
    ///
    /// 关掉时调用点只剩一次 `Option::as_mut()` 判断，没有分配、没有时间查询。
    pub(crate) fn from_env(label: &'static str) -> Option<Self> {
        enabled().then(|| Self {
            label,
            started: Instant::now(),
            count: 0,
        })
    }

    /// 计一次。跨过 1 秒就打印 `≈N/s <detail>`；`detail` 只在真要打印时才求值。
    pub(crate) fn tick(&mut self, detail: impl FnOnce() -> String) {
        self.count += 1;
        let elapsed = self.started.elapsed();
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let rate = self.count as f64 / elapsed.as_secs_f64();
        eprintln!("[nsh-erika] {} ≈{rate:.0}/s {}", self.label, detail());
        self.started = Instant::now();
        self.count = 0;
    }
}

/// 每 60 次出帧打一行 erika 自己记的各阶段耗时（毫秒）。
///
/// 引擎线程吃满 CPU 时，这一行直接指出是字幕光栅、视频上传、弹幕排版还是合成。
pub(crate) struct TimingsProbe {
    count: u64,
}

impl TimingsProbe {
    pub(crate) fn from_env() -> Option<Self> {
        enabled().then_some(Self { count: 0 })
    }

    /// 记一次出帧；够 60 次就取一次快照打一行。
    ///
    /// 开销：仅在打印那一帧克隆一次 `runtime_snapshot()`。
    pub(crate) fn tick(&mut self, presenter: &erika::presenter::PresenterRuntime) {
        self.count += 1;
        if self.count % 60 != 0 {
            return;
        }
        let snapshot = presenter.runtime_snapshot();
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
        log::info!(
            "erika.video: 耗时 pump={:.2} sub={:.2} video={:.2} audio={:.2} danmaku={:.2} render={:.2} total={:.2} (ms)",
            ms(snapshot.last_pump_duration),
            ms(snapshot.last_subtitle_pump_duration),
            ms(snapshot.last_video_pump_duration),
            ms(snapshot.last_audio_pump_duration),
            ms(snapshot.last_danmaku_plan_duration),
            ms(snapshot.last_render_duration),
            ms(snapshot.last_tick_duration),
        );
    }
}

/// `NSH_ERIKA_TRACE=1` 时控件侧的 tick 统计：每秒一行，专门回答"宿主到底掉没掉帧"。
///
/// 两个平台共用（tick 的来源不同：Linux 是 GTK frame clock 的回调，Windows 是 NSH 每帧调
/// 的 `draw`），所以统计口径一样：`tick` 是宿主敲了几次门、`paint` 是我们换上画面几次、
/// `frames` 是引擎发布了几帧。
///
/// ```text
/// [nsh-erika] tick ≈60/s paint ≈60/s frames ≈60/s maxgap=18ms
/// [nsh-erika] tick ≈1/s  paint ≈1/s  frames ≈1/s  maxgap=1000ms
/// ```
///
/// - `tick`：帧时钟回调次数/s —— 出帧频率的**唯一来源**；
/// - `paint`：真正换上新纹理的次数/s（每 tick 至多一次）；
/// - `frames`：引擎**发布到帧槽的帧数**增量/s（`FrameSlot::generation` 的增量，引擎发布到帧槽的
///   次数都算）。`frames > paint` = 引擎出了、控件没换上；`frames < tick` = 引擎跟不上。
///   三个数一起看就能定位断点：
///   - `frames < tick` → 引擎出帧没跟上（去看 `erika.video: 耗时 …`）；
///   - `frames ≈ tick > paint` → 引擎在出帧，但控件这边没换上（贴图/取帧间隙）；
///   - `tick` 自己掉了 → GTK/合成器限频，和上面两者无关。
/// - `maxgap`：这一秒里相邻两次 tick 的最大间隔，掉帧时明显大于 1/60s。
///
/// ⚠️ tick 彻底停了（最小化）时**每秒不会有任何输出** —— "这一行消失"本身就是结论。
/// ⚠️ 不要拿 `GdkFrameClock::fps()` 当基准：实测窗口被挡住、tick 掉到 1/s 时它照样报
/// 57.8 —— 那是 GTK 按 refresh interval 估的**显示器**刷新率，不是实际出帧率。
///
/// macOS 的 tick 会来自 `CVDisplayLink`/`CAMetalLayer`，那时可以复用这套计数。
pub(crate) struct TickProbe {
    started: Instant,
    ticks: u32,
    paints: u32,
    last: Option<Instant>,
    max_gap: Duration,
    /// 上次打印时的帧槽 `generation`，用来算每秒发布帧数。
    last_generation: u64,
    /// 这一秒内帧槽 `generation` 前进了多少（= 引擎发布了几帧）。
    published: u64,
    /// 第一次打印还没有基线（否则会把"从开播到现在的总帧数"当成每秒帧数）。
    primed: bool,
}

impl TickProbe {
    pub(crate) fn from_env() -> Option<Self> {
        let on = std::env::var_os("NSH_ERIKA_TRACE")
            .is_some_and(|value| !value.is_empty() && value != "0");
        on.then(|| Self {
            started: Instant::now(),
            ticks: 0,
            paints: 0,
            last: None,
            max_gap: Duration::ZERO,
            last_generation: 0,
            published: 0,
            primed: false,
        })
    }

    /// 记一次 tick；返回 `true` 表示该打印了（由调用方补上状态再调 [`Self::print`]）。
    pub(crate) fn tick(&mut self, now: Instant, generation: u64, painted: bool) -> bool {
        self.ticks += 1;
        if !self.primed {
            self.last_generation = generation;
        } else {
            // 累加这一秒内的每一段增量（一次 tick 拿到的可能就是全部）。
            self.published += generation.saturating_sub(self.last_generation);
            self.last_generation = generation;
        }
        self.paints += u32::from(painted);
        if let Some(last) = self.last {
            self.max_gap = self.max_gap.max(now.saturating_duration_since(last));
        }
        self.last = Some(now);
        now.saturating_duration_since(self.started) >= Duration::from_secs(1)
    }

    /// 打印这一秒的统计并清零。
    pub(crate) fn print(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.started);
        let per_second = |count: u64| count as f64 / elapsed.as_secs_f64();
        // 第一行只用来建基线（否则会把"开播以来的总数"当成每秒值）。
        let published = if self.primed { self.published } else { 0 };
        eprintln!(
            "[nsh-erika] tick ≈{:.0}/s paint ≈{:.0}/s frames ≈{:.0}/s maxgap={:.0}ms",
            per_second(self.ticks.into()),
            per_second(self.paints.into()),
            per_second(published),
            self.max_gap.as_secs_f64() * 1000.0,
        );
        self.started = now;
        self.ticks = 0;
        self.paints = 0;
        self.max_gap = Duration::ZERO;
        self.published = 0;
        self.primed = true;
    }
}

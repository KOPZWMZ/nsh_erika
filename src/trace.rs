//! 引擎侧的按秒速率探针（`NSH_ERIKA_TRACE=1`）—— **只用来诊断，不参与逻辑**。
//!
//! 用途：卡顿时分清"是谁在降频"。两行一起看就够：
//!
//! ```text
//! NSH_ERIKA_TRACE=1 nsh run
//! [nsh-erika] engine tick ≈100/s render=true has_media=true
//! erika.video: tick 1672x941 export=1672x941 gen=42 painted=42 has_frame=true state=Playing …
//! ```
//!
//! 引擎线程每 10ms（`IDLE`）醒一次做轮询，`render` 才是"这一轮真的出帧了没有"：
//!
//! - `render=false has_media=true`：控件没在要帧（最小化 / 被盖住 / GSK 把 tick 限频），
//!   引擎只跑保活泵（每 50ms 一次 `render_tick`，喂音频不发帧）—— 画面停、声音继续，
//!   这是预期行为，不是卡死；
//! - 第一行一直在、第二行（`erika.video: tick …`）不出现：tick 回调本身停了，
//!   问题在 GTK 那一侧。
//!
//! 同一个变量还会让引擎每 60 帧打一行各阶段耗时（见 `engine/render.rs::probe_timings`）。

use std::time::{Duration, Instant};

/// 一个"每秒打一行"的计数器。
pub(crate) struct RateProbe {
    label: &'static str,
    started: Instant,
    count: u64,
}

impl RateProbe {
    /// `NSH_ERIKA_TRACE=1`（任何非空、非 `0` 的值）时返回 `Some`，否则 `None`。
    ///
    /// 关掉时调用点只剩一次 `Option::as_mut()` 判断，没有分配、没有时间查询。
    pub(crate) fn from_env(label: &'static str) -> Option<Self> {
        let on = std::env::var_os("NSH_ERIKA_TRACE")
            .map(|value| !value.is_empty() && value != "0")
            .unwrap_or(false);
        on.then(|| Self {
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

//! `RenderCmd` + 出帧:出帧握手与"按需出帧"。
//!
//! `Wake` 是**唯一**的命令 —— 它也就是 `on_cmd` 的返回值那一半(`true` = 出帧)。
//!
//! 没人要画面时**不出帧,但仍然要泵**:`export_frame_dmabuf` / `render_tick` 本身就是
//! 音频泵,控件停止要帧(= 窗口最小化 / 被盖住 / GSK 把 tick 限频到 1–2Hz)之后如果完全
//! 不泵,音频环就会饿 —— 听感是"隔一会儿卡一下"。所以这条路径分两档:
//!
//! | 控件要帧 | 做什么 |
//! |---|---|
//! | 是 | [`export_frame_dmabuf`](erika::presenter::PresenterRuntime::export_frame_dmabuf)= 泵 + 出帧 |
//! | 否 | 每 [`KEEPALIVE_EVERY`](super::KEEPALIVE_EVERY) 调一次 `render_tick` = 只泵,不发帧 |
//!
//! ⚠️ 保活泵**不能**换成上游的 `PresenterRuntime::audio_only_tick()`:它会
//! `set_video_decode_suspended(true)`,而恢复时 `set_video_decode_suspended(false)` 是
//! **一次真 seek**(退关键帧 + preroll + flush 音频环,实测 200–300ms、丢几十帧)。
//! 窗口可见性抖动时它和"要不要出帧"互相触发,就是连续爆音(见 dev_doc §7)。
//!
//! 出帧**只有一条路**:erika 导出线性 dma-buf,控件用 `GdkDmabufTexture` 导入 ——
//! 我们这边一个 Vulkan 设备都不用建。没有 CPU 回读兜底(见 dev_doc §8),导出失败就
//! 只报一次 WARN、不出新帧(上一张继续显示)。

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use actor_lite::ActorContext;

use super::{ErikaEngine, KEEPALIVE_EVERY};
use crate::cmd::RenderCmd;

impl ErikaEngine {
    /// 出帧握手命令。返回值 = 本轮要不要出帧（`Wake` 恒为 `true`）。
    pub(super) fn on_render(&mut self, cmd: RenderCmd, _ctx: &ActorContext<Self>) -> bool {
        match cmd {
            RenderCmd::Wake => true,
        }
    }

    /// 出帧面：控件要帧就导出,不要就只泵音频(保活)。
    ///
    /// `want_frame` 是"这一轮之前有人要过帧"的旗标(`swap(false)` = 读并复位),所以一次
    /// 请求只会兑现一次出帧。`IDLE` 超时也会走到这里,那时旗标是 `false`,只走保活泵。
    pub(super) fn pump_frame(&mut self) {
        // 取一次请求快照：swap(false) = 读并复位，否则引擎会一直以为"被需要"
        let want = self.request.want_frame.swap(false, Ordering::AcqRel);
        let (width, height, scale) = {
            let request = *self.request.frame();
            (request.export.0, request.export.1, request.scale)
        };

        let has_media = self.presenter.player().has_open_media();

        // 尺寸不合法(父容器还没给确定尺寸 → 画布 0 面积)不算"有人要画面":导出本来就做不了。
        let want = want && width > 0 && height > 0;
        self.last_rendered = want && has_media;

        if !self.last_rendered {
            // 没人要帧:只泵音频 / 时钟 / 解码,不发帧。窗口被限频时靠它不断音。
            self.keepalive_pump(has_media);
            return;
        }

        // 出帧本身就泵过音频了,重置保活计时,免得紧接着再白泵一次。
        self.last_keepalive = Instant::now();

        let result = self.presenter.export_frame_dmabuf(width, height, scale);
        // 诊断：各阶段耗时，实现在 `trace.rs`（`NSH_ERIKA_TRACE=1` 才动）。
        if let Some(probe) = self.timings_probe.as_mut() {
            probe.tick(&self.presenter);
        }
        match result {
            Ok(Some(frame)) => self.frames.publish(Arc::new(frame)),
            Ok(None) => {} // 暂停/无新帧：保留上一张
            Err(e) => {
                // 没有兜底路径：驱动不支持 / 这张图导不出来就只出一次 WARN，
                // 上一张继续显示（产品策略是整条降级到 NSH 内置 <video>，见 dev_doc §8）。
                if !self.export_error_reported {
                    self.export_error_reported = true;
                    log::warn!("nsh-erika: dma-buf 导出失败（不再降级到 CPU 回读）: {e}");
                }
            }
        }
    }

    /// 保活泵：**只**喂音频 / 推进时钟 / 推进解码,不取帧、不建纹理。
    ///
    /// 只在播放中做 —— 暂停时 `render_tick` 泵不出任何音频,白花合成的时间。
    fn keepalive_pump(&mut self, has_media: bool) {
        // 先看节流(纯计算),再问 presenter —— `is_playing()` 要拿一次锁。
        if self.last_keepalive.elapsed() < KEEPALIVE_EVERY || !has_media {
            return;
        }
        if !self.presenter.is_playing() {
            return;
        }
        self.last_keepalive = Instant::now();
        let time = self.presenter.media_time().as_secs_f64();
        if let Err(e) = self.presenter.render_tick(time) {
            if !self.keepalive_error_reported {
                self.keepalive_error_reported = true;
                log::warn!("nsh-erika: keepalive pump: {e}");
            }
        }
    }

}

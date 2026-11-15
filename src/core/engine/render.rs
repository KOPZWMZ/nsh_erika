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
//! 我们这边一个 Vulkan 设备都不用建。没有 CPU 回读兜底(见 dev_doc §8):导出失败就把
//! 文案写进状态(`ErikaStatus::error`,应用据此降级到内置 `<video>`)、之后**不再尝试导出**,
//! 上一张继续显示;音频改由保活泵继续喂。
//!
//! TODO(platform): 上面写的是 **Linux 的形状**。「要不要出帧 / 保活泵」这套策略是跨平台的，
//! 平台相关的只有 `export_frame_dmabuf` 那一行：macOS 走 Metal 句柄、Windows 走 D3D 共享纹理。
//! 拆的时候把 `pump_frame` 分成「决定出不出帧」（留在 core）+「按平台导出」（各平台实现）。

use std::sync::atomic::Ordering;
use std::time::Instant;

use actor_lite::ActorContext;

use super::{ErikaEngine, KEEPALIVE_EVERY};
use crate::core::cmd::RenderCmd;

impl ErikaEngine {
    /// 出帧握手命令。返回值 = 本轮要不要出帧（`Wake` 恒为 `true`）。
    pub(super) fn on_render(&mut self, cmd: RenderCmd, _ctx: &ActorContext<Self>) -> bool {
        match cmd {
            RenderCmd::Wake => true,
        }
    }

    /// 出帧面：控件要帧就导出,不要就立刻返回。
    ///
    /// `want_frame` 是"这一轮之前有人要过帧"的旗标(`swap(false)` = 读并复位),所以一次
    /// 请求只会兑现一次出帧。`IDLE` 超时也会走到这里,那时旗标是 `false`,直接返回 ——
    /// **保活泵不在这里**,它由 `run` 循环每轮独立调用(`keepalive_pump`)。
    ///
    /// 返回 `Some(文案)` = 这一帧发现**本机出不了帧**，由 `run` 写进状态让应用看到。
    pub(super) fn pump_frame(&mut self) -> Option<String> {
        self.last_rendered = false;
        // 取一次请求快照：swap(false) = 读并复位，否则引擎会一直以为"被需要"
        if !self.request.want_frame.swap(false, Ordering::AcqRel) {
            return None;
        }
        // 已知本机出不了帧：别再调导出（它只会再失败一次）。音频由 `keepalive_pump` 管。
        if self.video_unavailable {
            return None;
        }
        let (width, height, scale) = {
            let request = *self.request.frame();
            (request.export.0, request.export.1, request.scale)
        };

        // TODO(platform): 只有 Linux 的 erika 有 `export_frame_dmabuf`；
        // macOS / Windows 换成各自的句柄导出（Metal / D3D），其余逻辑不动。
        #[cfg(target_os = "linux")]
        let result = self.presenter.export_frame_dmabuf(width, height, scale);
        // 诊断：各阶段耗时，实现在 `trace.rs`（`NSH_ERIKA_TRACE=1` 才动）。
        if let Some(probe) = self.timings_probe.as_mut() {
            probe.tick(&self.presenter);
        };
        #[cfg(target_os = "linux")]
        match result {
            Ok(Some(frame)) => {
                use crate::core::source::FrameImage;
                use std::sync::Arc;
                self.frames.publish(Arc::new(FrameImage::Dmabuf(frame)));
                self.last_rendered = true;
                //重置音频泵的计时
                self.last_keepalive = Instant::now();
                None
            }
            Ok(None) => None, // 暂停/无新帧：保留上一张
            // 没有兜底路径（见 dev_doc §8）：记下来，之后不再尝试导出，
            // 由应用决定整条降级到 NSH 内置 <video>。
            Err(e) => self.latch_video_unavailable(format!("dma-buf 导出失败: {e}")),
        }
        #[cfg(target_os = "windows")]
        {
            let _ = (width, height, scale);
            let time = self.presenter.media_time().as_secs_f64();
            let _ = self.presenter.render_tick(time);
            self.last_keepalive = Instant::now();
            None
        }
    }

    /// 保活泵：**只**喂音频 / 推进时钟 / 推进解码,不取帧、不建纹理。
    ///
    /// 只在播放中做 —— 暂停时 `render_tick` 泵不出任何音频,白花合成的时间。
    pub(super) fn keepalive_pump(&mut self) {
        if self.last_keepalive.elapsed() < KEEPALIVE_EVERY {
            return;
        } // 纯算术
        if !self.presenter.player().has_open_media() {
            return;
        } // 到期才加锁
        if !self.presenter.is_playing() {
            return;
        }
        self.last_keepalive = Instant::now();
        let time = self.presenter.media_time().as_secs_f64();
        match self.presenter.render_tick(time) {
            // 边沿标记：失败记一次，成功后复位，下次再坏才会再报。
            Ok(_) => self.keepalive_failed = false,
            Err(e) => {
                if !self.keepalive_failed {
                    self.keepalive_failed = true;
                    log::warn!("nsh-erika: keepalive pump: {e}");
                }
            }
        }
    }
}

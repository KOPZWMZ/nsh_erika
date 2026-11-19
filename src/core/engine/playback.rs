//! `PlaybackCmd`:生命周期 + 传输(`open` / `stop` / `play` / `pause` / `seek`)。
//!
//! 这一族的每一条**都要 presenter 参与**才正确:暂停要 cork 音频设备、seek/stop 要
//! flush 音频环、open 要重建解码器并清视觉 —— 那些协调方法在 presenter 上是 private,
//! 所以这一族**没有一条**能直连 `erika::Player`(实测直连就会音画不同步)。

use std::sync::atomic::Ordering;

use actor_lite::ActorContext;
use erika::MediaRequest;
use erika::presenter::PresenterRuntime;

use super::ErikaEngine;
use crate::core::cmd::PlaybackCmd;
use crate::core::engine::video_size_of;

impl ErikaEngine {
    /// 生命周期 / 传输命令的总入口。
    pub(super) fn on_playback(&mut self, cmd: PlaybackCmd, ctx: &ActorContext<Self>) {
        match cmd {
            PlaybackCmd::Open(req) => self.open(req, ctx),
            PlaybackCmd::Stop => self.stop(ctx),
            PlaybackCmd::Play => self.play_or_pause(ctx, true),
            PlaybackCmd::Pause => self.play_or_pause(ctx, false),
            PlaybackCmd::Seek(pos) => self.control(ctx, "seek", move |p| p.seek(pos)),
        }
    }

    fn open(&mut self, req: MediaRequest, ctx: &ActorContext<Self>) {
        // 这一次到底是"首次打开"还是"换片"？只有换片才清弹幕轨（见下面）。
        // 必须在 `presenter.open` 之前取：它一执行媒体就换了。
        let had_media = self.presenter.player().has_open_media();

        // 作废当前帧（`FrameSlot::clear` 会把代号 +1，控件据此知道"没有画面了"）。
        // 先让 UI 看到"打开中"，再去做阻塞的活。
        self.frames.clear();
        // 清控制面错误（换片了，上一次 open / seek 的失败已经没有意义）。
        // **不动 `video_unavailable`**：那是机器能力（驱动缺扩展 / GSK 接不了 dma-buf），
        // 换片不会让它变好；清了只会让它再失败一次。
        self.error = None;
        self.publish(ctx);

        match self.presenter.open(req) {
            Ok(()) => {
                // **换片**自动清空弹幕轨（2026-11-07 拍板）：轨道只是一串 `pts → 文本`，
                // 和媒体不绑定 —— 换片不清就会串到新片上（旧片的弹幕飘在新画面上）。
                //
                // 但**首次打开不清**：那时还没有"旧片"，而在 `open` 之前挂弹幕是合法用法
                // （命令队列保序，先 `add_track` 再 `open` 是自然写法；库自己的 demo 就这么写）。
                // 清掉的话用户会看到"弹幕没了" —— 踩过一次。
                //
                // 另外 `clear_danmaku` 只清轨道/时间轴/计划，**不动全局偏移**
                // （那是"用户偏好"，换片之后照样该保留）；单轨偏移随轨道一起没了。
                // open 失败时也不清：旧片可能还在放，清了等于把它正在显示的弹幕删掉。
                if had_media {
                    self.presenter.clear_danmaku();
                }

                // 视频尺寸：`open` 是同步 probe，这时轨道已经有了。写给控件算内接矩形 ——
                // 漏了这一步 `export_size` 会退回整块画布（5 倍像素）。
                // （`publish` 里还有一层兜底：尺寸仍是"不知道"时会再查一次。）
                self.frames.set_video_size(video_size_of(&self.presenter));
            }
            Err(e) => {
                // 锁外阻塞
                self.fail(ctx, format!("open: {e}"));
            }
        }
        self.publish(ctx);
    }
    fn play_or_pause(&mut self, ctx: &ActorContext<Self>, playing: bool) {
        // 和 `stop()` 同一条口径：没媒体时什么都不做、不报错。
        if !self.presenter.player().has_open_media() {
            return;
        }
        let result = if playing {
            self.presenter.play()
        } else {
            self.presenter.pause()
        };
        match result {
            Ok(()) => {}
            // "目标状态已经满足了"不是错误：播放/暂停是幂等意图，而声明式的 prop 会把
            // 同一个意图重复送进来（挂载时补发、外部引擎晚到时再补发、app 自己也调过）。
            // 上游的 `play()` / `pause()` 只**返回**这个错误、不发 `Error` 事件，所以吞掉
            // 它是安全的。判据是"目标状态已经成立"，不是"from == to"：
            //   · play  已满足 = 状态本来就是 `Playing`
            //   · pause 已满足 = 状态本来就不是 `Playing`（例如已经 `Stopped`）
            Err(erika::PlayerError::InvalidStateTransition { from, .. })
                if (playing && from == erika::PlayerState::Playing)
                    || (!playing && from != erika::PlayerState::Playing) =>
            {
                log::debug!(
                    "erika: {} 的目标状态已经成立（当前 {from:?}），忽略重复意图",
                    if playing { "play" } else { "pause" }
                );
            }
            Err(e) => self.fail(
                ctx,
                format!("{}: {e}", if playing { "play" } else { "pause" }),
            ),
        }
    }
    /// 停播。状态由 erika 自己维护:停完是 `PlayerState::Stopped`,而**媒体仍然开着**
    /// (`has_open_media()` 为 true、`duration` 还在、位置归 0)。
    fn stop(&mut self, ctx: &ActorContext<Self>) {
        // 没媒体时**什么都不做**：不投命令、不报错。
        // 命令队列保序，所以这里读到的是真实状态（Open 已经执行过），不是意图。
        if !self.presenter.player().has_open_media() {
            return;
        }
        // 画面作废 + 复位请求旗标，否则 stop 之后第一条 Wake 可能把旧帧又推出去
        self.frames.clear();
        self.request.want_frame.store(false, Ordering::Release);
        self.control(ctx, "stop", PresenterRuntime::stop);
    }
}

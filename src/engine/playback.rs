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
use crate::cmd::PlaybackCmd;

impl ErikaEngine {
    /// 生命周期 / 传输命令的总入口。
    pub(super) fn on_playback(&mut self, cmd: PlaybackCmd, ctx: &ActorContext<Self>) {
        match cmd {
            PlaybackCmd::Open(req) => self.open(req, ctx),
            PlaybackCmd::Stop => self.stop(ctx),
            PlaybackCmd::Play => self.control(ctx, "play", PresenterRuntime::play),
            PlaybackCmd::Pause => self.control(ctx, "pause", PresenterRuntime::pause),
            PlaybackCmd::Seek(pos) => self.control(ctx, "seek", move |p| p.seek(pos)),
        }
    }

    fn open(&mut self, req: MediaRequest, ctx: &ActorContext<Self>) {
        // 这一次到底是"首次打开"还是"换片"？只有换片才清弹幕轨（见下面）。
        // 必须在 `presenter.open` 之前取：它一执行媒体就换了。
        let had_media = self.presenter.player().has_open_media();

        // 换片:会话代次 +1(做差值的消费者靠它重置基线)
        // 先让 UI 看到"打开中"，再去做阻塞的活
        self.frames.clear();
        // 清控制面错误（换片了，上一次 open / seek 的失败已经没有意义）。
        // **不动 `video_unavailable`**：那是机器能力（驱动缺扩展 / GSK 接不了 dma-buf），
        // 换片不会让它变好；清了只会让它再失败一次。
        self.error = None;
        ctx.update_state(self.current_status());

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
            }
            Err(e) => {
                // 锁外阻塞
                self.error = Some(format!("open: {e}"));
            }
        }
        ctx.update_state(self.current_status());
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

//! `SettingsCmd`:音量 / 倍速 / 音轨 / 字幕轨。
//!
//! 都不改变"在播/不在播",但都会动 presenter 的内部状态:
//!
//! - **音量**住在 presenter 的 `audio_output` 上,`erika::Player` 根本没有 —— 而且
//!   `set_volume` 没有失败路径(`AudioOutputBackend::set_volume` 返回 `()`);
//! - **倍速**要让音频环做 time-stretch(变速不变调),直连 `Player` 只会让画面变快;
//! - **换轨**要先 quiesce 再 flush 音频环,否则旧音轨的 PCM 还会响几百毫秒。

use actor_lite::ActorContext;

use super::ErikaEngine;
use crate::cmd::SettingsCmd;

impl ErikaEngine {
    /// 参数类命令的总入口。
    pub(super) fn on_settings(&mut self, cmd: SettingsCmd, ctx: &ActorContext<Self>) {
        match cmd {
            SettingsCmd::Rate(rate) => {
                self.control(ctx, "set_rate", move |p| p.set_playback_rate(rate))
            }
            // set_volume / set_muted 都没有失败路径（`AudioOutputBackend::set_volume` 返回 ()）
            SettingsCmd::Volume(v) => self.presenter.set_volume(v),
            SettingsCmd::Muted(on) => self.presenter.set_muted(on),
            SettingsCmd::AudioTrack(id) => {
                self.control(ctx, "select_audio_track", move |p| p.select_audio_track(id))
            }
            SettingsCmd::SubtitleTrack(id) => {
                self.control(ctx, "select_subtitle_track", move |p| {
                    p.select_subtitle_track(id)
                })
            }
        }
    }
}

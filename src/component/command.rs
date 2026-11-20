//! 纯命令：props 路线的"动作口"。
//!
//! # 为什么单独一个口
//!
//! 对外暴露的永远只有三件事：**状态**（props）/ **事件**（`on_*`）/ **命令**（这里）。
//!
//! 判据只有一条：**有等价 prop 的就不进命令**。
//!
//! - `playing` / `volume` / `muted` / `rate` 已经有状态 prop（电平语义：挂载 / 重开自动收敛），
//!   所以**不在这里**；用 prop。
//! - `seek` / 停播 / 弹幕开关 / 加删弹幕轨 / 选轨这类没有 prop、也没有"当前值"可读的动作，
//!   做成"状态"就只是单向写（挂载要不要重放、和引擎实际值不一致怎么办都没法验证），
//!   所以它们是命令。
//!
//! # 语义
//!
//! - **边沿触发**：每次执行都要新建一个 [`CommandRequest`]（它带一个自增 token）。
//!   重复设置同一个值不会再次触发 —— 这正是"跳转到同一个位置两次"也要生效的保证。
//! - **挂载时执行一次**：组件挂载会把 `command` prop 的当前值执行一遍。因为"组件重挂 =
//!   新引擎"，重放通常是你要的；不想要就别在挂载前设命令。
//! - **不承诺返回值**：命令是单向下发，失败走 `status.error`（句柄路线）或
//!   [`VideoEvent::Error`](crate::simple::VideoEvent)。
//!
//! # 什么时候用状态 prop 而不是命令
//!
//! 需要"挂载/重开后自动恢复"的东西用状态 prop（`src` / `playing` / `volume` / `muted` /
//! `rate`）。命令只表达"用户刚刚做了一件事"。
//!
//! 选轨现在还在命令里：它没有等价 prop（`selected` 将来如果加，就会从这里搬走）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use nsh_core::element::{JsHost, JsIn, JsOwned, JsValue, PropError, TsTypes};

use crate::ErikaPlayer;

/// 下一个要发的命令号。单调递增、**永不复用** —— 所以"两次相同的动作"不会被相等比较吃掉。
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// 一次命令请求：带一个自增 token，用来让"同样的命令"也能再执行一次。
///
/// ```ignore
/// // 在事件处理器里造，然后塞进 signal —— 别在组件 body 里造（每次重渲染都会发一条）。
/// // 播放/暂停不在这里：那是状态，用 `playing` prop。
/// let command = signal(None::<CommandRequest>);
/// button().on_press(move || command.set(Some(CommandRequest::new(VideoCommand::Stop))));
/// erika_video().src(uri).command(move || command.get())
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct CommandRequest {
    token: u64,
    command: VideoCommand,
}

impl CommandRequest {
    /// 包一条命令，领一个新 token。
    pub fn new(command: VideoCommand) -> Self {
        Self {
            token: NEXT_TOKEN.fetch_add(1, Ordering::Relaxed),
            command,
        }
    }

    /// 这次请求的号（调试 / 测试用）。
    pub fn token(&self) -> u64 {
        self.token
    }

    /// 里面那条命令。
    pub fn command(&self) -> &VideoCommand {
        &self.command
    }
}

impl JsValue for CommandRequest {
    fn ts_type(_: &mut TsTypes) -> String {
        "never".into()
    }
    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        Err(value.expected("a CommandRequest（命令只能从 Rust 侧传）"))
    }
    fn to_js<'a>(&self, _: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        Err(PropError("CommandRequest 不能跨到 JS".into()))
    }
}

/// 一条命令。**只放纯动作**：没有"当前值"可读、不需要在挂载时恢复的那些。
#[derive(Debug, Clone, PartialEq)]
pub enum VideoCommand {
    /// 停播：位置归 0、画面清空，**媒体仍然开着**。
    ///
    /// 暂停用 `playing(false)`（那是状态），停播没有等价 prop，所以在这里。
    Stop,
    /// 跳转。
    Seek(Duration),
    /// 选音轨（`None` = 关掉音频）。
    ///
    /// ⚠️ 选轨需要已有媒体；还没有媒体时这条命令会被忽略（打 debug 日志），
    /// 因为上游此时会报错。等 `StateChange` 到 `Ready` 之后再发。
    SelectAudio(Option<i64>),
    /// 选字幕轨（`None` = 关字幕）。同上，需要已有媒体。
    SelectSubtitle(Option<i64>),
    /// 弹幕总开关。
    SetDanmakuEnabled(bool),
    /// 追加一条弹幕轨（`uri` 可以是本地文件或 URL，解析在引擎线程上做）。
    ///
    /// 想"替换"就先发 [`ClearDanmaku`](Self::ClearDanmaku) 再发这条 —— 命令队列保序。
    AddDanmakuTrack {
        /// 文件路径或 URL。
        uri: String,
        /// 轨道显示名（`None` 用空串）。
        name: Option<String>,
    },
    /// 移除一条弹幕轨。
    RemoveDanmakuTrack(u64),
    /// 弹幕全局偏移（微秒，正 = 整体晚出）。
    SetDanmakuOffset(i64),
    /// 清空所有弹幕轨。
    ClearDanmaku,
}

impl JsValue for VideoCommand {
    fn ts_type(_: &mut TsTypes) -> String {
        "never".into()
    }
    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        Err(value.expected("a VideoCommand（命令只能从 Rust 侧传）"))
    }
    fn to_js<'a>(&self, _: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        Err(PropError("VideoCommand 不能跨到 JS".into()))
    }
}

/// 把一条命令翻成句柄方法调用。
///
/// 全部是**已有方法**的转发：命令层不引入新的引擎语义，所以引擎侧一行都不用改。
pub(crate) fn dispatch(player: &ErikaPlayer, command: VideoCommand) {
    match command {
        VideoCommand::Stop => player.stop(),
        VideoCommand::Seek(position) => player.seek(position),
        VideoCommand::SelectAudio(track) => {
            if !player.has_media() {
                log::debug!("erika.video: 还没媒体，忽略 SelectAudio（等 StateChange 到 Ready）");
                return;
            }
            player.tracks().select_audio(track)
        }
        VideoCommand::SelectSubtitle(track) => {
            if !player.has_media() {
                log::debug!(
                    "erika.video: 还没媒体，忽略 SelectSubtitle（等 StateChange 到 Ready）"
                );
                return;
            }
            player.tracks().select_subtitle(track)
        }
        VideoCommand::SetDanmakuEnabled(enabled) => player.danmaku().set_enabled(enabled),
        VideoCommand::AddDanmakuTrack { uri, name } => {
            player.danmaku().add_track(uri, name.unwrap_or_default())
        }
        VideoCommand::RemoveDanmakuTrack(track) => player.danmaku().remove_track(track),
        VideoCommand::SetDanmakuOffset(offset) => player.danmaku().set_global_offset(offset),
        VideoCommand::ClearDanmaku => player.danmaku().clear(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 相同的命令每次都要拿到新 token —— 否则 `Prop` 的相等比较会把第二次吞掉。
    #[test]
    fn every_request_gets_its_own_token() {
        let first = CommandRequest::new(VideoCommand::Seek(Duration::from_secs(10)));
        let second = CommandRequest::new(VideoCommand::Seek(Duration::from_secs(10)));
        assert_ne!(first, second, "同样的命令两次请求必须不相等");
        assert_ne!(first.token(), second.token(), "token 必须递增、不复用");
    }
}

use std::{sync::Arc, time::Duration};

use actor_lite::ActorHandle;

use super::source::Request;
use crate::{
    ErikaPlayer,
    core::{
        engine::ErikaEngine,
        source::{FrameSlot, ViewSource},
    },
};

/// 唯一所有者：不可克隆。Drop 即关机。
pub struct ErikaSession {
    pub(crate) id: u64,
    pub(crate) engine: ActorHandle<ErikaEngine>,
    pub(crate) player: erika::Player,
}
impl ErikaSession {
    /// 建一台引擎。**唯一的所有者**由此产生：`Drop` 时摘表 + 停引擎。
    ///
    /// 一般由 [`ErikaPlayer::new`](crate::ErikaPlayer::new) 调 —— 那边会把它装进句柄的票据里，
    /// 等组件挂载时认领。直接调它意味着你自己负责关机（测试 / 无头脚本）。
    pub fn create() -> Result<Self, String> {
        let frames = Arc::new(FrameSlot::new());
        let request = Arc::new(Request::new());
        let (tx, rx) = std::sync::mpsc::channel();
        // `try_spawn_thread` 把"构造阶段 panic"变成可回传的错误
        // (没有 Vulkan 时 `ErikaEngine::new` 就是在这里炸的)。
        let engine = actor_lite::try_spawn_thread("erika-engine", {
            let frames = frames.clone();
            let request = request.clone();
            move || {
                let engine = ErikaEngine::new(frames, request);
                let player = engine.player_clone();
                let _ = tx.send(player);
                engine
            }
        })
        .map_err(|error| format!("引擎线程启动失败: {error}"))?;
        let player = rx
            .recv_timeout(Duration::from_millis(500))
            .map_err(|error| format!("引擎线程没能交出 Player: {error}"))?;

        let id = crate::core::registry::next_id();
        let source = ViewSource::new(id, frames, request, engine.clone());
        crate::core::registry::register(id, source);
        Ok(Self { id, engine, player })
    }
    /// 借出一个**无票**句柄：它永远不能被认领，只用来发命令 / 读状态。
    pub fn player(&self) -> ErikaPlayer {
        ErikaPlayer {
            id: self.id,
            player: self.player.clone(),
            engine: self.engine.clone(),
            session: None,
        }
    }
}
impl Drop for ErikaSession {
    fn drop(&mut self) {
        crate::core::registry::unregister(self.id);
        self.engine.stop();
    }
}

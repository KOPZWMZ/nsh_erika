use std::{sync::Arc, time::Duration};

use actor_lite::ActorHandle;

use super::source::Request;
use crate::{
    ErikaPlayer,
    core::{
        engine::ErikaEngine,
        source::{FrameSlot, PlayerInner},
    },
};

/// 唯一所有者：不可克隆。Drop 即关机。
pub struct ErikaSession {
    engine: ActorHandle<ErikaEngine>,
    inner: Arc<PlayerInner>,
}
impl ErikaSession {
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

        let id = crate::core::source::NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let inner = Arc::new(PlayerInner {
            id,
            player,
            handle: engine.clone(),
            frames,
            request,
        });
        crate::core::source::register(inner.clone());
        Ok(Self { engine, inner })
    }
    pub fn player(&self) -> ErikaPlayer {
        ErikaPlayer {
            inner: self.inner.clone(),
        }
    }
}
impl Drop for ErikaSession {
    fn drop(&mut self) {
        crate::core::source::unregister(self.inner.id);
        self.engine.stop();
    }
}

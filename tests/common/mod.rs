#![allow(dead_code)]
use std::{
    path::PathBuf,
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use erika::MediaRequest;
use nsh_erika::ErikaPlayer;

pub fn new_player() -> ErikaPlayer {
    ErikaPlayer::new().expect("引擎应该能起来")
}
pub fn media(name: &str) -> MediaRequest {
    // name 在用例里写死
    let p = media_path(name);
    assert!(
        p.is_file(),
        "素材不存在: {}（见 media/README.md）",
        p.display()
    );
    MediaRequest::new(p.to_string_lossy().into_owned())
}
fn media_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("media")
        .join(name)
}
pub struct Session {
    pub player: ErikaPlayer,
    _lock: MutexGuard<'static, ()>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.player.stop();
        std::thread::sleep(Duration::from_millis(20));
    }
}

impl Session {
    /// 取锁 + 建 player。**先取锁再建**，这样"创建 player"本身也被串行化。
    pub fn acquire(lock: &'static Mutex<()>) -> Self {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        Session {
            player: new_player(),
            _lock: guard,
        }
    }
}
/// 等到条件成立，超时就 panic。
///
/// 两个刻意的设计：
/// - **超时只有 3 秒**。测试失败时不该让人干等 15 秒（`examples/lifecycle.rs`
///   里用的是 15 秒，那是给"人手跑、可能机器很慢"的场景）。
/// - **panic 信息里带上 state 和 last_error**，失败时一眼看到现场，
///   不用回去翻日志。
pub fn wait<F: Fn(&ErikaPlayer) -> bool>(p: &ErikaPlayer, cond: F, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if cond(p) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "超时: {what}（state={:?}, media={}, err={:?}）",
        p.state(),
        p.has_media(),
        p.last_error()
    );
}

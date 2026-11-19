#![allow(dead_code)]
use std::{
    path::PathBuf,
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use nsh_erika::{ErikaPlayer, ErikaSession, MediaRequest, PlayerState};

pub const TEST_MEDIA: &str = "tiny.mp4";
/// 只有视频轨、没有音频（h264 64x64 / 20 帧 / 2 秒）。
///
/// 出帧测试用它：那条链只关心"解码 → 合成 → 导出"，音频设备在无头环境 / 容器里
/// 是白噪声（PulseAudio 缺失、`pendingAudioFrame` 之类的收尾日志都会干扰），
/// 而播放时钟没有音轨时由墙钟推进，更可控。
pub const SILENT_MEDIA: &str = "silent.mp4";
pub const OTHER_MEDIA: &str = "tiny.mkv";
/// 一个测试用的引擎：**守卫 + 借用句柄**。
///
/// 引擎的所有权在守卫（`ErikaSession`）手里 —— 它不可克隆，`Drop` 时停引擎。
/// 所以它必须活到测试结束，这就是本结构存在的理由（以前 `ErikaPlayer::new()`
/// 自己兼着"建引擎"和"持有引擎"两件事）。
pub struct Player {
    pub player: ErikaPlayer,
    _engine: ErikaSession,
}

impl std::ops::Deref for Player {
    type Target = ErikaPlayer;
    fn deref(&self) -> &ErikaPlayer {
        &self.player
    }
}

pub fn new_player() -> Player {
    let engine = ErikaSession::create().expect("引擎应该能起来");
    let player = engine.player();
    Player {
        player,
        _engine: engine,
    }
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
    pub player: Player,
    _lock: MutexGuard<'static, ()>,
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
pub fn session(lock: &'static Mutex<()>) -> Session {
    Session::acquire(lock) // ← 唯一提到本文件 LOCK 的地方
}
pub fn session_and_opened(name: &str, lock: &'static Mutex<()>) -> Session {
    let s = session(lock);
    let before = s.player.generation();
    s.player.open(media(name));
    // ⚠️ 只等 generation 变化是不够的：`generation()` 是**流代次**（open/seek/stop 一起步就 +1），
    // 会先经过 `Opening` 中间态 —— 机器一忙（刚编完、几个用例并行）就会把中间态当"完成"。
    // 其余用例一直是 `generation 变化 && Ready` 的写法，这里补齐。
    wait(
        &s.player,
        |p| p.generation() > before && p.state() == PlayerState::Ready,
        "open 完成",
    );
    assert_eq!(s.player.state(), PlayerState::Ready, "前置：应到 Ready");
    assert!(s.player.duration().is_some(), "前置：应探测出时长");
    s
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

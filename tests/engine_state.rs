//! 纯状态机测试：**不需要任何素材，也不需要音频设备或显卡**。
//!
//! 从 `examples/lifecycle.rs` 的 ① ② 拆出来。这两个步骤只关心
//! "句柄在没东西可播时的行为"，和具体片源无关 —— 所以它们最容易自动化，
//! 也最适合作为 CI 的第一道网。
//!
//! 运行：
//! ```bash
//! cargo test --test state
//! ```

use nsh_erika::PlayerState;
use std::sync::Mutex;
use std::time::Duration;
mod common;
use common::*;
/// 每个用例都会建一个 `ErikaPlayer`（内部会起引擎线程、建渲染资源）。
/// 并行跑会互相抢资源、日志也会串在一起，所以这里**串行化**。
static LOCK: Mutex<()> = Mutex::new(());

#[test]
fn new_player_starts_idle() {
    let s = session(&LOCK);

    assert_eq!(s.player.state(), PlayerState::Idle);
    assert!(!s.player.has_media(), "刚开始不该有媒体");
    assert!(!s.player.is_playing(), "刚开始不该在播");
    assert!(s.player.duration().is_none(), "没媒体时不该有总时长");
}

#[test]
fn play_without_media_is_a_no_op() {
    let s = session(&LOCK);

    // 没有媒体就播 —— 这正是"用户手滑点了播放"的场景，
    // 而且声明式的 `paused` prop 本来就会在 `open()` **之前**到达。
    // 所以口径是：**不 panic、不报错、什么都不做**（和 `stop()` / `seek` 一致）；
    // 0.2.0 之前这里会写一条 error，等于把正常时序当成错误报给用户。
    s.player.play();
    std::thread::sleep(Duration::from_millis(100));

    assert!(
        s.player.last_error().is_none(),
        "无媒体 play 不该报错（它只是还没东西可播）: {:?}",
        s.player.last_error()
    );
    assert_eq!(s.player.state(), PlayerState::Idle, "状态不该被弄乱");
    assert!(!s.player.is_playing());
    assert!(!s.player.has_media());
}

#[test]
fn seek_stop_pause_without_media_are_noops() {
    let s = session(&LOCK);

    // 这四种操作在"没有媒体"时都必须是安全的 no-op ——
    // 它们都可能被 UI 在任意时刻触发（比如组件卸载、快捷键）。
    s.player.seek(Duration::from_secs(5));
    s.player.stop();
    s.player.pause();
    s.player.set_playing(true); // 目标值版 toggle:两个方向都不该凭空变出媒体
    s.player.set_playing(false);

    // 给引擎线程一点时间处理这些命令，确认它没有把状态搞乱。
    std::thread::sleep(Duration::from_millis(200));

    assert!(!s.player.is_playing(), "没有媒体时不该进到 playing 状态");
    assert!(!s.player.has_media(), "这些操作不该凭空变出媒体");
}

mod common;
use common::*;
use nsh_erika::PlayerState;
use std::{sync::Mutex, time::Duration};
/// 每个用例都会建一个 `ErikaPlayer`（内部会起引擎线程、建渲染资源）。
/// 并行跑会互相抢资源、日志也会串在一起，所以这里**串行化**。
static LOCK: Mutex<()> = Mutex::new(());

#[test]
fn play_and_pause() {
    let s = session_and_opened(TEST_MEDIA, &LOCK);
    let t0 = s.player.position();
    s.player.play();
    wait(&s.player, |p| p.is_playing(), "应该开始播放");
    std::thread::sleep(Duration::from_millis(300));
    let t1 = s.player.position();
    assert!(t1 > t0, "应该往前走了一段时间");
    s.player.pause();
    wait(&s.player, |p| !p.is_playing(), "应该暂停了");
    assert_eq!(
        s.player.state(),
        PlayerState::Paused,
        "状态应是 Paused 而不是 Stopped"
    );

    let t2 = s.player.position();
    std::thread::sleep(Duration::from_millis(300));
    let t3 = s.player.position();
    assert!(t3 == t2, "暂停时时间应该不变");
    assert!(s.player.has_media(), "暂停了媒体还在");
    s.player.play();
    std::thread::sleep(Duration::from_millis(200));
    let t4 = s.player.position();
    assert!(t4 > t3, "应该继续往前走");
}

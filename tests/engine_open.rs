use erika::MediaRequest;
use nsh_erika::PlayerState;
use std::sync::Mutex;
use std::time::Duration;
mod common;
use common::*;
/// 每个用例都会建一个 `ErikaPlayer`（内部会起引擎线程、建渲染资源）。
/// 并行跑会互相抢资源、日志也会串在一起，所以这里**串行化**。
static LOCK: Mutex<()> = Mutex::new(());

/// 故意不存在的路径：**要求文件不存在**
fn missing() -> MediaRequest {
    let p = std::env::temp_dir().join("nsh_erika_definitely_missing.mp4");
    assert!(!p.exists(), "这个路径不该存在: {}", p.display());
    MediaRequest::new(p.to_string_lossy().into_owned())
}

#[test]
fn open_media_not_play() {
    let s = session_and_opened(TEST_MEDIA, &LOCK);
    assert!(!s.player.is_playing(), "打开媒体不应该自动播放");
    assert_eq!(s.player.state(), PlayerState::Ready, "打开文件之后应该就绪");
    assert!(s.player.duration().is_some(), "应该探查到了总时常");
    s.player.play();
    wait(
        &s.player,
        |p| p.state() == PlayerState::Playing,
        "等待开始播放",
    );
    assert!(s.player.is_playing(), "开始播放之后应该显示正在播放");
}
#[test]
fn open_missing_file_reports_error() {
    let s = session(&LOCK);
    s.player.open(missing());
    wait(&s.player, |p| p.last_error().is_some(), "应留下可读错误");
    assert!(!s.player.is_playing(), "打开错误媒体不该播放");
    assert_eq!(s.player.state(), PlayerState::Error, "状态错误");
    assert!(!s.player.has_media(), "打开错误不该有媒体");
}
#[test]
fn can_open_again_after_failed_open() {
    let s = session(&LOCK);
    // 先制造一次失败
    s.player.open(missing());
    wait(&s.player, |p| p.last_error().is_some(), "先失败");

    let before = s.player.generation();
    s.player.open(media(TEST_MEDIA));
    wait(&s.player, |p| p.generation() > before, "重试的 open 应完成");

    assert_eq!(
        s.player.state(),
        PlayerState::Ready,
        "失败后应能恢复到 Ready"
    );
    assert!(s.player.has_media(), "恢复后应有媒体");
    assert!(s.player.duration().is_some(), "恢复后应探测出时长");
    assert!(
        s.player.last_error().is_none(),
        "成功后该清掉上一次的错误，否则 UI 会一直挂着过期提示"
    );
}
#[test]
fn reopen_same_file_resets_playback() {
    let s = session_and_opened(TEST_MEDIA, &LOCK);
    s.player.play();
    wait(&s.player, |p| p.is_playing(), "等待开始播放");
    std::thread::sleep(Duration::from_millis(300)); // 让它真的播一会儿
    assert!(
        s.player.position() > Duration::from_millis(100),
        "前置：位置应该已经前进"
    );
    let before = s.player.generation();
    s.player.open(media(TEST_MEDIA));
    wait(
        &s.player,
        |p| p.generation() > before && p.state() == PlayerState::Ready,
        "片源应该更新",
    );
    assert!(s.player.last_error().is_none(), "重复打开媒体不应该有错误");
    assert!(
        s.player.position() < Duration::from_millis(500),
        "重开应把位置归零 —— 如果这条挂了，说明同 uri 的 open 被当成 no-op 吞掉了"
    );
}
#[test]
fn change_media() {
    let s = session_and_opened(TEST_MEDIA, &LOCK);
    s.player.play();
    wait(&s.player, |p| p.is_playing(), "等待开始播放");
    std::thread::sleep(Duration::from_millis(300)); // 让它真的播一会儿
    assert!(
        s.player.position() > Duration::from_millis(100),
        "前置：位置应该已经前进"
    );
    let before = s.player.generation();
    s.player.open(media("tiny.mkv"));
    wait(
        &s.player,
        |p| p.generation() > before && p.state() == PlayerState::Ready,
        "片源应该更新",
    );
}

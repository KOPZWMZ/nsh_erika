//! 事件通道的端到端：**转发** 与 **不写状态**。
//!
//! 两条断言都是设计契约，不是行为细节（背景见 `dev_doc/api-feedback.md` §7）：
//!
//! 1. 上游的离散事实真的被转发出来了（白名单不是死代码）；
//! 2. **整段播放不再写一次状态通道** —— 一旦有人往 `forward_player_events` 里加
//!    `publish`，第 2 条会立刻红。它守的是"事件不承诺 `ErikaStatus` 已刷新"这条契约：
//!    上游自己改变状态时，状态包允许陈旧，要真值请直读 `player.*()`。
//!
//! 用 `silent.mp4`（无音轨，2 秒）：无头环境里音频 backpressure 会把"自然播完"
//! 变成 `Error`，那是环境噪声，不是这条链的问题 —— 所以下面把上游报错当成
//! "这条机器测不了第 2 条"，只跳过、不失败。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use nsh_erika::PlayerState;
use nsh_erika::events::{ErikaEvent, ErikaEventsListener};

mod common;
use common::*;

/// 每个用例都会建一个 `ErikaPlayer`（引擎线程 + 渲染资源），并行跑会互相抢，串行化。
static LOCK: Mutex<()> = Mutex::new(());

/// 在 `timeout` 内把事件流排空，收集到 `out`。
fn drain_into(listener: &mut ErikaEventsListener, timeout: Duration, out: &mut Vec<ErikaEvent>) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        while let Some(event) = listener.try_recv() {
            out.push(event);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn saw_state(events: &[ErikaEvent], want: PlayerState) -> bool {
    events
        .iter()
        .any(|event| matches!(event, ErikaEvent::StateChanged(state) if *state == want))
}

#[test]
fn events_forward_upstream_facts() {
    let s = session(&LOCK);
    let mut events = s.player.subscribe_events();

    s.player.open(media(SILENT_MEDIA));
    wait(&s.player, |p| p.state() == PlayerState::Ready, "open 完成");
    assert!(s.player.duration().is_some(), "前置：应探测出时长");

    // open 的路上上游会发一串离散事件：状态 / 轨道 / 选轨。这里只要求它们**被转发**
    // （顺序和条数不钉，那是上游的自由度）。
    let mut got = Vec::new();
    drain_into(&mut events, Duration::from_millis(300), &mut got);

    assert!(
        got.iter().any(|e| matches!(e, ErikaEvent::TracksChanged)),
        "open 应转发 TracksChanged，实际收到 {got:?}"
    );
    assert!(
        got.iter()
            .any(|e| matches!(e, ErikaEvent::TrackSelectionChanged(_))),
        "open 应转发 TrackSelectionChanged，实际收到 {got:?}"
    );
    assert!(
        saw_state(&got, PlayerState::Ready) || saw_state(&got, PlayerState::Opening),
        "open 应转发状态变化，实际收到 {got:?}"
    );

    // 高频量绝不进来：整段里不该出现任何跟位置有关的东西 —— `ErikaEvent` 里没有这个
    // 变体，所以这里只能反向确认"事件数量是有限的、没有一条是我猜的"。
    assert!(
        got.len() < 32,
        "事件通道被当成了消息队列？open 一路收到 {} 条",
        got.len()
    );
}

#[test]
fn play_to_end_sends_events_but_never_publishes_status() {
    let s = session_and_opened(SILENT_MEDIA, &LOCK);
    let mut events = s.player.subscribe_events();
    let mut status = s.player.subscribe_status();
    let _ = status.try_take(); // 建基线（订阅只报之后的变化）
    drain_into(&mut events, Duration::from_millis(100), &mut Vec::new()); // 排空 open 的积压

    s.player.play();
    wait(&s.player, |p| p.is_playing(), "开始播放");

    // 让**命令路径**那次 publish 落定，然后从这里开始数：之后的每一帧都只该有事件，
    // 不该有状态通知。
    std::thread::sleep(Duration::from_millis(100));
    let _ = status.try_take();

    let mut saw_playing = false;
    let mut saw_stopped = false;
    let mut upstream_error = None;
    let mut status_wakeups = 0;

    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        while let Some(event) = events.try_recv() {
            match event {
                ErikaEvent::StateChanged(PlayerState::Playing) => saw_playing = true,
                ErikaEvent::StateChanged(PlayerState::Stopped) => saw_stopped = true,
                ErikaEvent::Error(text) => upstream_error = Some(text),
                _ => {}
            }
        }
        if status.try_take().is_some() {
            status_wakeups += 1;
        }
        if saw_stopped && !s.player.is_playing() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(saw_playing, "播放应转发 StateChanged(Playing)");
    assert!(
        saw_stopped,
        "自然播完应转发 StateChanged(Stopped)（state={:?}, err={:?}）",
        s.player.state(),
        s.player.last_error()
    );

    // 直读是权威值：播完之后立刻就是 false / Stopped（不用等任何发布）。
    assert!(!s.player.is_playing(), "直读 is_playing() 应立刻是 false");
    assert_eq!(
        s.player.state(),
        PlayerState::Stopped,
        "直读 state() 应停在 Stopped"
    );

    if let Some(text) = upstream_error {
        // 无头环境（缺音频设备 / 解码抖动）下上游自己报错，第 2 条测不了 —— 但它走的是
        // `fail()`（状态 + 事件一起写），本来就允许写状态。
        eprintln!("上游报错，跳过\"事件不写状态\"的断言：{text}");
        return;
    }

    assert_eq!(
        status_wakeups, 0,
        "整段播放不该再写状态通道：上游事实只走事件。\
         出现通知说明 forward_player_events 里又加了 publish —— 那会推翻\
         \"事件不承诺状态已刷新\"这条契约（见 dev_doc/api-feedback.md §7.4）"
    );
}

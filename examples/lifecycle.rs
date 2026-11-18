//! 无窗口自检之二:**句柄的状态机**。
//!
//! ```bash
//! cargo run --example lifecycle -- examples/player/cache/video.mkv
//! ```
//!
//! 走一遍 Idle → Opening → Ready → 播放/暂停 → seek → stop → 重开 → 同 uri 再 open,
//! 顺带钉住四条容易回归的语义:
//! - `open` 之后**不自动播**(要显式 `play()`);
//! - `stop()` 之后**媒体还在**,但状态是 `Stopped`(**不是** `Ready`;`duration` 还在、
//!   位置归 0),所以 UI 不能用 `duration.is_none()` 判"有没有视频";
//! - `stop()` 不重建 `PresenterRuntime` —— 这是句柄能长期持有 `erika::Player` 的前提;
//! - 读状态全部走 `Player`（`ErikaStatus` 现在只剩 presenter 侧观测量：
//!   音量 / 静音 / 弹幕 / 字幕 / 错误）。
//!
//! 里面夹了两个实测(用 `call` 在引擎线程里量/打印,句柄拿不到 presenter):
//! - ⑦ **实测 C**:`presenter.stop()` 在播放中的真实耗时;
//! - ⑪ **实测 D**:音量是否跨 `stop` / 换片保留(结论回填 ROADMAP §2.6)。
use nsh_erika::tracks::TrackKind;
use nsh_erika::{ErikaPlayer, MediaRequest, PlayerState};
use std::time::{Duration, Instant};

/// 轮询直到条件成立。条件作用在**句柄**上,因为现在读状态不再经过 `ErikaStatus`。
fn wait(player: &ErikaPlayer, cond: impl Fn(&ErikaPlayer) -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if cond(player) {
            println!("  ok: {what}");
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "超时: {what} (state={:?}, position={:?}, err={:?})",
        player.state(),
        player.position(),
        player.last_error()
    );
}

fn main() {
    let media = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "media/tiny.mkv".into());
    // 换片用的小片段(没有就跳过 ⑮⑯):ffmpeg 造一条 8s 的测试片即可
    let other = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "media/tiny.mp4".into());
    // 弹幕测试文件(⑰ 用)
    let danmaku_str = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "media/tiny.json".into());
    let player = ErikaPlayer::new().expect("引擎起不来");
    // 弹幕句柄从头就用:③ 之前要挂一条轨,验"首次打开不清"这条语义。
    let danmaku = player.danmaku();

    println!("① 新建:state={:?}", player.state());
    assert_eq!(player.state(), PlayerState::Idle);

    println!("② 没有媒体就 play —— 不该 panic,但错误要能被看到");
    player.play();
    // 引擎线程还在建 presenter(要建自己的 wgpu device),所以这里等它把错误写出来
    wait(&player, |p| p.last_error().is_some(), "无媒体 play 报错");
    println!("  err={:?}", player.last_error());
    assert_eq!(player.state(), PlayerState::Idle, "报错不该把状态弄乱");

    println!("③ open 后应到 Ready(但不自动播)");
    // 在**首次** open 之前挂一条弹幕轨:队列保序,这是合法写法(demo 就这么写)。
    // 首次打开**不该**清它 —— 清空只发生在"换片"(已经有媒体时再 open)。
    danmaku.add_track(danmaku_str.clone(), "首次打开前挂的");
    player.open(MediaRequest::new(media.clone()));
    wait(
        &player,
        |p| p.state() == PlayerState::Ready,
        "state -> Ready",
    );
    assert!(!player.is_playing(), "open 不该自动播");
    wait(
        &player,
        |_| danmaku.state().tracks.len() == 1,
        "首次 open 之前的弹幕轨仍在(不是换片,不该清)",
    );
    println!("  ok: 首次 open 没清掉 open 之前挂的弹幕");
    // 收拾干净,不影响后面的步骤
    danmaku.clear();
    wait(
        &player,
        |_| danmaku.state().tracks.is_empty(),
        "清空(为后续步骤收拾)",
    );

    println!("④ play/pause");
    player.play();
    // `state()` 是**已提交**状态:引擎线程处理完 `play()` 之后才变 `Playing`,所以这里必须
    // wait,不能立刻断言 —— `play()` 只是把命令投进队列。
    // （想要"点了就变"请自己维护乐观 signal;库不再提供"意图"读法,理由见 `set_playing` 的文档。）
    wait(
        &player,
        |p| p.state() == PlayerState::Playing,
        "state -> Playing",
    );
    wait(&player, |p| p.is_playing(), "playing = true");
    let t1 = player.position();
    std::thread::sleep(Duration::from_millis(600));
    assert!(player.position() > t1, "播放中位置该前进");
    player.pause();
    wait(&player, |p| !p.is_playing(), "playing = false");

    println!("⑤ seek(30s)");
    player.seek(Duration::from_secs(30));
    wait(
        &player,
        |p| p.position() >= Duration::from_secs(29),
        "position ≈ 30s",
    );
    println!("  duration={:?}", player.duration());
    assert!(player.duration().is_some(), "探测出总时长后才算真的打开");

    println!("⑥ stop:位置回 0、画面清空、媒体还在(Stopped + duration 保留)");
    player.stop();
    wait(
        &player,
        |p| p.state() == PlayerState::Stopped,
        "stop -> Stopped",
    );
    wait(
        &player,
        |p| p.position() < Duration::from_millis(500),
        "stop 后位置回 0",
    );
    assert!(
        player.duration().is_some(),
        "stop 不丢 duration —— UI 别用 duration.is_none() 判'有没有视频'"
    );
    assert!(player.has_media(), "stop 之后媒体仍然开着");

    // ── 实测 C:stop 的真实代价 ──
    // 句柄的 stop() 是异步投递,墙钟时间含 20ms 轮询粒度,量不出 stop 本身的开销;
    // 所以在引擎线程里直接量 presenter.stop()。**播放中停**,才会走 quiesce + 音频复位
    // 那条最贵的路径(暂停时停是白测)。
    println!("⑦ 实测 C:播放中 stop 的耗时");
    player.play();
    wait(&player, |p| p.is_playing(), "先播起来再量");
    player.call(|p| {
        let started = Instant::now();
        let result = p.stop();
        println!(
            "  presenter.stop() -> {result:?} in {:?}  (对比:重建 presenter 是几十 ms)",
            started.elapsed()
        );
    });
    wait(&player, |p| !p.is_playing(), "引擎线程上的 stop 生效");
    std::thread::sleep(Duration::from_millis(200));

    println!("⑧ stop 之后还能直接 play(不用重新 open)");
    player.play();
    wait(&player, |p| p.is_playing(), "stop 后 play -> playing");
    player.pause();
    wait(&player, |p| !p.is_playing(), "playing -> false");

    // 语义在 2026-11-04 反转过:删掉 `set_media` 之后,`open()` 的契约就是
    // "**总是**重新打开"(句柄不再记 uri 缓存)。所以这里断言的是"重开真的发生了",
    // 而不是老版本的"同 uri 是 no-op"。
    //
    // ⚠️ 证据只能用**代次计数器**,不能用"位置前进多少":
    //    headless 没有音频消费端(`PLAN §2-5`),音频环顶到 cap 后播放会被 backpressure
    //    卡住 —— `position` 停在原地是**正常**的,拿它当同步点必然偶发超时(踩过)。
    // `ErikaStatus` 已经不放 session 了（现在只剩音量 / 静音 / 弹幕 / 字幕 / 错误），
    // 所以改用 `generation()`：它是 erika 的 playback generation，`open`/`seek`/`stop` 都 +1。
    println!("⑨ open 同 uri 会重新打开:generation +1(真的重开了,不是 no-op)");
    let before_generation = player.generation();
    player.open(MediaRequest::new(media.clone()));
    player.play();
    wait(
        &player,
        |p| p.generation() > before_generation,
        "generation +1(真的重开了)",
    );

    println!("⑩ stop()(状态 Stopped,媒体还在 —— 没媒体时引擎侧自动 no-op)");
    player.stop();
    wait(
        &player,
        |p| p.state() == PlayerState::Stopped && !p.is_playing(),
        "stop() -> Stopped",
    );
    assert!(player.duration().is_some());

    // ── 音量:句柄 API + 实测 D(跨 stop / 换片是否保留)──
    // 音量住在 presenter 的 audio_output 上,`Player` 上根本没有 → 只能经队列。
    // 句柄读的是引擎发布的状态(每条命令后更新),所以设完立刻能读到。
    println!("⑪ 音量:set_volume / volume(),以及跨 stop / 换片是否保留");
    player.set_volume(0.3);
    wait(
        &player,
        |p| (p.volume() - 0.3).abs() < 0.01,
        "volume() -> 0.3",
    );
    // 和 presenter 侧的真值对一次账
    player.call(|p| println!("  presenter 侧 volume = {:.3}", p.volume()));
    std::thread::sleep(Duration::from_millis(200));
    player.stop();
    std::thread::sleep(Duration::from_millis(300));
    player.open(MediaRequest::new(media.clone()));
    player.play();
    wait(&player, |p| p.is_playing(), "重新打开并起播");
    wait(
        &player,
        |p| (p.volume() - 0.3).abs() < 0.01,
        "stop + 重新 open 之后 volume() 仍是 0.3",
    );

    println!("⑫ play() 有媒体时应能到 playing");
    player.open(MediaRequest::new(media.clone()));
    player.play();
    wait(&player, |p| p.is_playing(), "open + play 起播");
    println!("⑬ 倍速:set_rate / rate()");
    player.set_rate(2.0);
    wait(&player, |p| (p.rate() - 2.0).abs() < 1e-6, "rate() -> 2.0");
    player.set_rate(1.5);
    wait(&player, |p| (p.rate() - 1.5).abs() < 1e-6, "rate() -> 1.5");
    player.set_rate(-1.0); // 非法值:句柄直接丢弃,不该打到引擎
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        (player.rate() - 1.5).abs() < 1e-6,
        "非法倍速应被丢弃,rate 仍是 1.5(实际 {})",
        player.rate()
    );
    player.set_rate(1.0);
    wait(&player, |p| (p.rate() - 1.0).abs() < 1e-6, "rate() -> 1.0");

    println!("⑭ 轨道:tracks().list() / tracks().selection() / tracks().select_*");
    let tracks = player.tracks().list();
    let audio: Vec<i64> = tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .map(|t| t.id)
        .collect();
    println!(
        "  {} 条轨道,音轨 {audio:?},当前选择 {:?}",
        tracks.len(),
        player.tracks().selection()
    );
    assert!(!audio.is_empty(), "打开之后应该有音轨");
    if let Some(&second) = audio.get(1) {
        player.tracks().select_audio(Some(second));
        wait(
            &player,
            |p| p.tracks().selection().audio == Some(second),
            "切到第二条音轨",
        );
    } else {
        println!("  (只有一条音轨,跳过切换)");
    }
    player.tracks().select_subtitle(None);
    wait(
        &player,
        |p| p.tracks().selection().subtitle.is_none(),
        "字幕轨已关闭",
    );

    // 换片段缺失时跳过(⑮ 换片 / ⑯ 播到结尾 都依赖它);
    // 以前这里没有判断,导致 /tmp 被清之后本自检会等 15s 再 panic。
    if std::path::Path::new(&other).exists() {
        println!("⑮ 换片:open(别的文件) —— 不自动播,时长跟着换");
        let gen_before = player.generation(); // open 前取
        player.open(MediaRequest::new(other.clone()));
        wait(
            &player,
            |p| p.generation() > gen_before && p.state() == PlayerState::Ready,
            "换片完成（generation 已 +1 且回到 Ready）",
        );
        assert!(!player.is_playing(), "换片不该自动播");
        assert_eq!(player.state(), PlayerState::Ready);
        player.play();
        wait(&player, |p| p.is_playing(), "换片后能播");
        player.call(|p| println!("  换片后 volume = {:.3}  ← 期望仍是 0.300", p.volume()));
        std::thread::sleep(Duration::from_millis(200));

        println!("⑯ 播到结尾:playing 变 false,媒体仍开着");
        player.seek(Duration::from_secs(7));
        wait(&player, |p| !p.is_playing(), "播完自动停(playing -> false)");
        assert!(player.has_media(), "播完媒体仍开着(还能 seek / 重播)");
        // 顺带演示 raw():`is_stopped_at_end()` 没封装(只给诊断用)
        println!(
            "  state={:?} is_stopped_at_end={}",
            player.state(),
            player.raw().is_stopped_at_end()
        );
        // ⚠️ headless 下**没有音频消费端**:音频环会被填满并 backpressure 满 10s,
        //    erika 于是报 `playback_fatal: audio_output_backpressure`,state 变 Error。
        //    这不是本层的 bug(GUI 里是 Stopped + Ended);旧代码看不出来,是因为
        //    状态用的是我们自己的 `ErikaState`。换读 `Player` 之后它才浮出水面。
        assert!(
            matches!(player.state(), PlayerState::Stopped | PlayerState::Error),
            "播完应停在 Stopped(真机)或 Error(headless 音频 backpressure),实际 {:?}",
            player.state()
        );
    } else {
        println!(
            "  ⚠ 跳过 ⑮⑯:换片段不存在({other})—— `./scripts/bootstrap.sh --fix` 会造一个 8s 测试片"
        );
    }

    println!("⑰ 弹幕(ErikaDanmaku):add_track / 开关 / 样式 / 清空");
    // 解析在引擎线程上做(这里是小 JSON,但仍然走异步命令),所以用 wait 等它出现。
    danmaku.add_track(danmaku_str.clone(), "测试弹幕");
    wait(
        &player,
        |_| danmaku.state().tracks.len() == 1,
        "弹幕轨已挂上",
    );
    let tracks = danmaku.state().tracks;
    println!(
        "  {} 条轨:[{}] 共 {} 条弹幕,enabled={}",
        tracks.len(),
        tracks
            .iter()
            .map(|t| format!("{} ({}, {} 条)", t.name, t.source, t.item_count))
            .collect::<Vec<_>>()
            .join(", "),
        tracks.iter().map(|t| t.item_count).sum::<usize>(),
        danmaku.state().enabled,
    );
    assert!(tracks[0].item_count > 0, "解析出来的弹幕不该是空的");
    assert!(danmaku.state().enabled, "默认是开着的");

    danmaku.set_enabled(false);
    wait(&player, |_| !danmaku.state().enabled, "弹幕关掉");
    danmaku.set_enabled(true);
    wait(&player, |_| danmaku.state().enabled, "弹幕开回来");

    // 样式用"读改写":只动 opacity,别的 24 个字段不受影响。
    // 整包配置不在快照里(那是逃生舱的事),所以这里用 `call_with` 在引擎线程上读真值。
    let before = player
        .call_with(|p| p.danmaku_config().cloned())
        .flatten()
        .expect("读 raw 弹幕配置");
    danmaku.update_config(|c| c.opacity = 0.5);
    wait(
        &player,
        |_| (danmaku.state().paint.opacity - 0.5).abs() < 1e-6,
        "opacity -> 0.5",
    );
    let after = player
        .call_with(|p| p.danmaku_config().cloned())
        .flatten()
        .expect("再读一次 raw 弹幕配置");
    assert!(
        (after.font_size - before.font_size).abs() < f32::EPSILON
            && after.scroll_duration_seconds == before.scroll_duration_seconds,
        "update_config 只该改你动过的字段"
    );
    println!("  opacity={} font_size={}", after.opacity, after.font_size);

    danmaku.clear();
    wait(&player, |_| danmaku.state().tracks.is_empty(), "弹幕清空");

    // 挂回去一条,让 ⑱ 的换片来验证"换片会清空弹幕轨(不串片)"
    danmaku.add_track(danmaku_str.clone(), "换片前挂的");
    wait(
        &player,
        |_| danmaku.state().tracks.len() == 1,
        "换片前挂上一条(给 ⑱ 当靶子)",
    );

    println!("⑱ 事件驱动 + 换片:应发事件、generation +1、弹幕轨清空");
    let mut events = player.subscribe_events();
    while events.try_recv().is_some() {} // 排空积压

    let generation_before = player.generation();
    player.open(MediaRequest::new(media.clone())); // 换回主片(已有媒体 = 换片)
    let started = Instant::now();
    let mut got_event = false;
    while started.elapsed() < Duration::from_secs(20) {
        if events.try_recv().is_some() {
            got_event = true;
        }
        if got_event && player.generation() > generation_before {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    println!(
        "  generation {generation_before} -> {},收到事件={got_event}",
        player.generation()
    );
    assert!(got_event, "换片应该产生事件(事件驱动的地基)");
    assert!(
        player.generation() > generation_before,
        "换片后 generation 应该 +1(做差值的消费者靠它重置基线)"
    );
    // 换片(已有媒体时再 open)**必须清空弹幕轨**,否则旧片的弹幕会飘在新画面上。
    wait(
        &player,
        |_| danmaku.state().tracks.is_empty(),
        "换片清空了弹幕轨(不串片)",
    );
    println!("  ok: 换片清掉了 ⑰ 挂的那条弹幕轨");

    println!("⑲ 静音:set_muted 只压输出,不动记住的音量");
    let volume_before = player.volume();
    player.set_muted(true);
    wait(&player, |p| p.is_muted(), "is_muted -> true");
    println!("  ok: 静音打开");
    wait(
        &player,
        |p| (p.volume() - volume_before).abs() < 0.01,
        "静音后 volume 不变",
    );
    println!("  ok: 静音不影响 volume({volume_before:.3})");
    player.set_muted(false);
    wait(&player, |p| !p.is_muted(), "is_muted -> false");
    println!("  ok: 取消静音");

    println!("全部通过");
}

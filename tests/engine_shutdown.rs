//! 收尾路径测试：**最后一个播放句柄 drop 之后，引擎线程必须真的消失**。
//!
//! 为什么这条必须存在：`shutdown_engine` 的唯一依据是 actor 的 `is_alive()`，
//! 而它曾经的语义是"`run()` 已返回"，不是"资源已释放"。那个窗口让进程退出时
//! 与后台线程的 `free()` 赛跑，表现为偶发的
//! `corrupted double-linked list` / `corrupted size vs. prev_size` + `SIGABRT`
//! （actor-lite 0.1.0 → 0.1.1 的修复，见它的 CHANGELOG）。
//!
//! 这里用 `/proc/self/task/*/comm` 直接看线程名，是**端到端**的粗粒度护栏，覆盖：
//!
//! 1. `EngineGuard` 真的被触发（`Arc<Slot>` / `Arc<EngineGuard>` 的引用计数清干净了，
//!    否则引擎线程永远不会停）
//! 2. `shutdown_engine` 的等待真的能等到线程结束（不是等了个假信号就返回）
//! 3. 反复创建/销毁不会累积引擎线程
//!
//! **它不负责钉"语义的精确性"**：这里允许线程在极短时间内才消失（`is_alive()` 变 false
//! 之后内核还要回收 `/proc/<tid>`），所以旧语义（提前 fire）也可能侥幸通过。
//! "`is_alive() == false` 必须意味着资源已释放"这条精确契约由 actor-lite 的
//! `tests/is_alive_semantics.rs` 钉住 —— 两层各管一段。
//!
//! 仅 Linux 有效（依赖 `/proc`）。

mod common;
use common::*;
use std::sync::Mutex;
use std::time::{Duration, Instant};

static LOCK: Mutex<()> = Mutex::new(());

/// 引擎线程名（见 `ThreadActor` 的 spawn 名字；`/proc/*/comm` 上限 15 字节）。
const ENGINE_THREAD: &str = "erika-engine";

/// 打开一个素材并等到 Ready —— **不自带锁**，调用方负责串行化。
fn opened_player(name: &str) -> nsh_erika::ErikaPlayer {
    let player = new_player();
    let before = player.generation();
    player.open(media(name));
    wait(&player, |p| p.generation() > before, "open 完成");
    player
}

fn engine_thread_exists() -> bool {
    std::fs::read_dir("/proc/self/task")
        .map(|entries| {
            entries.filter_map(|e| e.ok()).any(|entry| {
                std::fs::read_to_string(entry.path().join("comm"))
                    .map(|name| name.trim() == ENGINE_THREAD)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// 等到引擎线程真的从进程里消失。
///
/// 允许"稍后才消失"：`is_alive()` 变 false 发生在闭包末尾，线程退出到内核回收
/// `/proc/<tid>` 之间还有极短的一段。断言的是"最终没有残留"，而不是"同一纳秒"。
fn wait_engine_thread_gone(what: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while engine_thread_exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !engine_thread_exists(),
        "{what}：仍有残留的 `{ENGINE_THREAD}` 线程 —— \
         `shutdown_engine` 没有等到资源释放完（actor-lite 的 is_alive 语义退化了？）"
    );
}

#[test]
fn dropping_last_player_leaves_no_engine_thread() {
    // 锁必须**贯穿整个用例体**：`engine_thread_exists()` 看的是整个进程的线程表，
    // 一旦放锁，本文件里另一个用例就可能建起同名的 `erika-engine` 线程，
    // 让"前置应该在跑 / 之后应该消失"两条断言互相踩。
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let player = opened_player(TEST_MEDIA);
    assert!(
        engine_thread_exists(),
        "前置：打开媒体后引擎线程应该在跑"
    );

    drop(player);
    wait_engine_thread_gone("drop 掉最后一个句柄之后");
}

#[test]
fn repeated_create_destroy_does_not_accumulate_engine_threads() {
    // ⚠️ 锁只取一次：`session()` 内部会 lock，同一线程重复取会死锁。
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    for round in 0..3 {
        let player = opened_player(TEST_MEDIA);
        drop(player);
        wait_engine_thread_gone(&format!("第 {round} 轮 drop 之后"));
    }
}

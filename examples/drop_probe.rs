//! 崩溃探针：在一个进程里反复创建/销毁 `ErikaPlayer`，用于复现
//! "进程退出时 SIGABRT / 堆损坏" 的偶发问题。
//!
//! 用法：
//! ```bash
//! MODE=stop   N=20 cargo run --example drop_probe   # 正常收尾（stop + 等一会）
//! MODE=bare   N=20 cargo run --example drop_probe   # 直接 drop，不 stop
//! MODE=forget N=20 cargo run --example drop_probe   # 故意泄漏，不析构
//! ```
//!
//! 三个模式的意义：
//! - `stop`   崩、`forget` 不崩  →  问题在**析构路径**
//! - `bare`   崩、`forget` 不崩  →  同上，且 `stop` 不是关键
//! - `forget` 也崩               →  问题在**构造或运行期**（越界写），与析构无关
//! - 三种都不崩                 →  触发概率还不够高，把 N 加大（100、500）

use std::time::Duration;

fn main() {
    let n: usize = std::env::var("N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let mode = std::env::var("MODE").unwrap_or_else(|_| "stop".to_string());

    println!("mode={mode} n={n}");

    for i in 0..n {
        print!("iter {i:02}: new ... ");
        let p = nsh_erika::ErikaPlayer::new().expect("new 失败");

        match mode.as_str() {
            "forget" => {
                // 故意泄漏：不析构，直接忘掉
                std::mem::forget(p);
                println!("forgot");
            }
            "bare" => {
                drop(p);
                println!("dropped（未 stop）");
            }
            _ => {
                p.stop();
                std::thread::sleep(Duration::from_millis(20));
                drop(p);
                println!("stopped + dropped");
            }
        }
    }

    println!("main 结束，准备退出（崩溃通常发生在这之后）");
}

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
        // 守卫（不可克隆，drop 即关机）+ 借用句柄
        let engine = nsh_erika::ErikaSession::create().expect("create 失败");
        let p = engine.player();

        match mode.as_str() {
            "forget" => {
                // 故意泄漏：连守卫一起忘掉 —— 引擎线程会一直跑到进程退出
                std::mem::forget(p);
                std::mem::forget(engine);
                println!("forgot（守卫也丢了：引擎不会停）");
            }
            "bare" => {
                drop(p);
                drop(engine); // 守卫 drop → unregister + stop()，不等
                println!("dropped（守卫 drop：只 stop，不等）");
            }
            _ => {
                p.stop();
                std::thread::sleep(Duration::from_millis(20));
                drop(p);
                drop(engine);
                println!("stopped + dropped");
            }
        }
    }

    println!("main 结束，准备退出（崩溃通常发生在这之后）");
}

# nsh_erika_demo

`nsh-erika` 的示例应用：一块铺满的 `erika.video` + 一条**手动控制条**（播放/暂停、±10s、
音量、静音、停止）和一个状态行。独立的 NSH 项目（自己的 `nsh.json` / `Cargo.toml`）。

控制条是这套示例的主要用途：出帧、seek、音量这些路径光看日志分不清"引擎没动"还是
"控件没贴图"，手动按几下最快。

## 跑

```sh
cd nsh_erika/examples/nsh_erika_demo

TEST_VIDEO=/path/to/video.mkv nsh run     # 或：nsh run nsh_erika/examples/nsh_erika_demo
cargo run                                    # 不经 nsh 的宿主，直接跑原生后端
```

它是 `nsh_erika` workspace 的成员（共用 `nsh_erika/Cargo.lock`），所以从仓库根也能跑：
`cargo run -p nsh-app-erika-demo`。

`nsh` 必须在有 `nsh.json` 的目录里跑（在 `nsh_erika/` 里跑会报 `no App.tsx in .`）。

## 环境变量

| 变量 | 作用 | 默认 |
| --- | --- | --- |
| `TEST_VIDEO` | 片源路径 | `../../media/tiny.mkv` |
| `TEST_DANMAKU` | 弹幕轨道（bilibili/json/…）；**`off` = 不挂弹幕** | `../../media/danmaku.json` |
| `NSH_ERIKA_TRACE=1` | 打 `engine tick` / `tick ≈ paint ≈ frames` 探针 | 关 |
| `NSH_PLAYER_BUILTIN=1` | 换成 NSH 内置 `<video>`（GStreamer）**做性能对照** | 关 |
| `RUST_LOG=nsh_erika=info` | 控件的启动/出帧日志 | 关 |

`media/` 里带了能跑的素材：`tiny.mkv`（64×64 / 10fps 的极小样本，只用来验「有没有画面」，
不是拿来看画质的）、`danmaku.json`、`tiny.ass`、`tone.m4a`、`silent.mp4`。

## 两个注册入口

| 路径 | 入口 |
| --- | --- |
| `nsh run` | `src/lib.rs` 里的 `#[native_components(linux)]` / `#[native_components(windows)]` |
| `cargo run` | `src/main.rs` 里的 `nsh_erika::register(&options.components)` |

少一个，那条路径上 `erika.video` 就是未知组件。

## 控制条

| 按钮 | 调用 | 顺便验的东西 |
| --- | --- | --- |
| 播放 / 暂停 | `play()` / `pause()` | 暂停→播放的过渡 |
| -10s / +10s | `seek(position() ± 10s)` | **暂停时 seek 能不能刷新** |
| 音量 - / + | `set_volume(± 0.1)` | 音频输出 |
| 静音 / 取消静音 | `set_muted(bool)` | 静音不改 `volume` 的语义 |
| 停止 | `stop()` | 停播后帧被作废 |
| 状态行 | `position` / `duration` / `volume` / `last_error` | 失败可见（`status.error` 直接显示） |

状态不是"猜"的：`set_interval(200ms)` 从引擎拉一次写进信号，按钮按下时再乐观写一遍。

## 量性能

```sh
./measure.sh                                     # 用 VIDEO 指的片源
TEST_DANMAKU=off ./measure.sh                             # 无弹幕对照组
MEASURE_PID=$(pgrep -f nsh-app-erika-demo) ./measure.sh   # 量已在跑的进程
```

输出进程 CPU%、最费 CPU 的线程、AMD GPU busy%，以及日志里的 tick 记录。

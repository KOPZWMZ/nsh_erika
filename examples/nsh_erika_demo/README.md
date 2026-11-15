# nsh_erika_demo

`nsh-erika` 的示例应用：一个铺满窗口的 `erika.video`，用来验证这个原生控件。
独立的 NSH 项目（自己的 `nsh.json` / `Cargo.toml`），和仓库根的 koplay 应用无关。

仓库里只有三个文件是"源"：`src/lib.rs`（94 行）、`Cargo.toml`、`nsh.json`。
`.nsh/` 由 `nsh` 生成（每个平台一个入口 crate），不入库。

## 跑

```sh
cd nsh_erika/examples/nsh_erika_demo

NSH_ERIKA_SRC=/path/to/video.mkv nsh run     # 或：nsh run nsh_erika/examples/nsh_erika_demo
cargo run                                    # 不经 nsh 的宿主，直接跑原生后端
```

它是 `nsh_erika` workspace 的成员（共用 `nsh_erika/Cargo.lock`），所以从仓库根也能跑：
`cargo run -p nsh-app-erika-demo`。`src/main.rs` 里的 `nsh_erika::register` 和
`src/lib.rs` 里的 `#[native_components(linux)]` 是**两个注册入口**，分别服务 `cargo run`
和 `nsh run` —— 少一个，那条路径上 `erika.video` 就是未知组件。

`nsh` 必须在有 `nsh.json` 的目录里跑（在 `nsh_erika/` 里跑会报 `no App.tsx in .`）。

| 环境变量 | 作用 | 默认 |
|---|---|---|
| `NSH_ERIKA_SRC` | 片源路径 | `cache/video.mkv` |
| `DMK` | 弹幕轨道 | `cache/danmaku.json` |
| `NSH_PLAYER_BUILTIN=1` | 换成 NSH 内置 `<video>`（GStreamer），做性能对照组 | 关 |
| `RUST_LOG=nsh_erika=info` | 看控件的启动/出帧日志 | 关 |
| `NSH_ERIKA_TRACE=1` | 每秒一行 `gtk tick` 统计（出帧频率的唯一来源） | 关 |

期望的启动日志：

```
INFO  nsh_erika::linux::native_view] erika.video: 引擎已启动
INFO  nsh_erika::linux::native_view] erika.video: 第一帧 dma-buf 1672x941 stride=… offset=0
```

## 画面与降级

出帧路径只有 **dma-buf** 一条：erika 导出线性 dma-buf，控件用 `GdkDmabufTexture` 导进来贴。
没有 CPU 回读兜底；导不进就不出画面，原因写进 `player.status().error`，应用据此降级到
内置 `<video>`。排查对照表见 `dev_doc/nsh-erika-status.md`。

## API（句柄在外，控件只显示）

```rust
let player = nsh_erika::ErikaPlayer::new()?;             // 控制
player.open(nsh_erika::MediaRequest::new(path));
player.play();

view().child(nsh_erika::ErikaVideo(&player).flex_1())   // 画面
```

控件名、prop（只有 `source` 一个帧源号）、注册方式都封在库里。**没有内置控制条** ——
控制条由 app 用普通 NSH 元素画（本示例的「暂停/播放」就是一个 `button()` 直接调 `player.pause()`）。

## 性能

```sh
./measure.sh                                  # 自己起进程，量 CPU/GPU
MEASURE_PID=$(pgrep -f nsh-app-erika-demo) ./measure.sh   # 量已在跑的进程
NSH_PLAYER_BUILTIN=1 ./measure.sh             # 对照组：NSH 内置 <video>
```

`SECONDS_TO_RUN` / `WARMUP` 可调。

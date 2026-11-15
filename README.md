# nsh-erika

[NSH](https://nsh.dev) 的原生视频控件 `erika.video`：把 [liberika](https://github.com/AimesSoft/liberika)
（erika 播放内核）的画面接进 NSH 的界面树。

- **这是播放内核的桥，不是播放器。** 没有播放列表、没有进度条、没有 UI。它提供
  `ErikaPlayer`（控制与状态）+ 一块显示画面的控件，控制条由调用者用自己的 NSH 元素画。
- **画面零 CPU 回读**：Linux 走线性 dma-buf → `GdkDmabufTexture`；Windows 走 D3D11
  共享纹理 → `OpenSharedResource` → D2D `DrawBitmap`。两条路都不经过内存拷贝。
- **控件只认一个数字**（帧源号），所以引擎放在哪儿都行、控件可以随页面生灭。
- 解码 / 同步 / 字幕（libass）/ 弹幕 / 音频全在 erika：硬解、A/V 同步、色彩、FSR 超分都不需要你写。

| 平台 | 出帧路径 | 状态 |
|---|---|---|
| **Linux** | erika 导出线性 dma-buf → GDK 直接导入 → GSK 当纹理采样 | ✅ 跑通（VA-API 直通硬解 + 弹幕 + 字幕） |
| **Windows** | erika 发布 `MISC_SHARED` 快照 → `OpenSharedResource` → D2D `DrawBitmap` | ✅ 跑通（D3D11VA 硬解 + 弹幕 + 字幕） |
| macOS / Android | — | 未做（编译期 `compile_error!` 挡住） |

## 快速开始

### 1. 依赖

```toml
[dependencies]
nsh-core = "0.5.1"
nsh-base = "0.5.1"                      # 只有示例用；你自己的 UI 用什么都行
nsh-erika = { git = "https://github.com/KOP-zero/nsh_erika", branch = "main" }

[target.'cfg(target_os = "linux")'.dependencies]
nsh-platform-linux = { version = "0.5.1", default-features = false }

[target.'cfg(target_os = "windows")'.dependencies]
nsh-platform-windows = { version = "0.5.1", default-features = false }
```

⚠️ **先别急着 `cargo build`** —— erika 要一些本地原生依赖，两个平台的备法完全不同：

- Linux → [docs/linux.md](docs/linux.md)（系统库 + 两个补丁前置，`bash scripts/prepare-linux-deps.sh` 一键）
- Windows → [docs/windows.md](docs/windows.md)（工具链 + 在 liberika 的 cargo checkout 里跑 `xtask deps build`）

### 2. 注册控件（**应用 crate 的根**）

```rust
use nsh_erika::PlatformComponents;

// `#[native_components(<平台>)]` 自己会加 cfg，所以两个函数平铺写就行，不用包 cfg。
#[native_components(linux)]
fn components_linux(components: &PlatformComponents) {
    nsh_erika::register(components);
}

#[native_components(windows)]
fn components_windows(components: &PlatformComponents) {
    nsh_erika::register(components);
}
```

### 3. 用

```rust
use nsh_core::prelude::*;

let player = nsh_erika::ErikaPlayer::new().expect("引擎起不来");
player.open(nsh_erika::MediaRequest::new("/path/to/video.mkv"));
player.play();

// 画面 + 你自己画的控制条
view()
    .flex_col()
    .flex_1()
    .child(nsh_erika::ErikaVideo(&player).flex_1())
    .child(button().on_press({
        let player = player.clone();
        move || player.pause()
    }).child(text("暂停")))
```

完整可跑的版本（播放/暂停、±10s、音量、静音、停止、状态行）见
[`examples/nsh_erika_demo`](examples/nsh_erika_demo)。

## 示例

```sh
cd examples/nsh_erika_demo
NSH_ERIKA_SRC=/path/to/video.mkv nsh run     # 或：cargo run
```

| 环境变量 | 作用 | 默认 |
|---|---|---|
| `NSH_ERIKA_SRC` | 片源路径 | `../../media/tiny.mkv` |
| `DMK` | 弹幕轨道；`off` = 不挂弹幕 | `../../media/danmaku.json` |
| `NSH_ERIKA_TRACE=1` | 打 `engine tick` / `tick ≈ paint ≈ frames` 探针 | 关 |
| `NSH_PLAYER_BUILTIN=1` | 换成 NSH 内置 `<video>`（GStreamer）**做性能对照** | 关 |
| `RUST_LOG=nsh_erika=info` | 控件的启动/出帧日志 | 关 |

## 文档

| 文件 | 内容 |
|---|---|
| [docs/api.md](docs/api.md) | 控制面、状态读取、弹幕/字幕、生命周期、失败语义、平台差异 |
| [docs/linux.md](docs/linux.md) | Linux 依赖准备、构建、验证、已知限制 |
| [docs/windows.md](docs/windows.md) | Windows 依赖准备、构建、验证、排查、已知限制 |

## 已知限制

| 限制 | 影响 | 备注 |
|---|---|---|
| Windows 每帧一次 `CopyResource` + 一次 GPU 同步等待 | 4K 高帧率时的成本大头 | 上游 legacy `MISC_SHARED` 句柄没有 fence/keystate 原语，生产者只能等完再交 |
| Windows 输出钉死 SDR | HDR 片源会被 tone-map | 想保留 HDR 要上游动渲染管线 |
| Windows 每帧新纹理 + 每帧 `OpenSharedResource` | 分配器压力 | 由上游池化解决 |
| Linux 需要两个**补丁前置**（wgpu-hal、libass） | 外部消费者必须自己备 | 见 [docs/linux.md](docs/linux.md)，一步脚本 |
| 弹幕与视频同在一张纹理 | 出帧频率 = max(视频帧率, 弹幕平滑度) | 弹幕流畅时视频会重复合成；分层导出要上游改动 |
| 4K 高帧率未实测 | — | 这是唯一可能改动路线的未知数 |

## 开发

```sh
cargo test --lib                     # 单元测试（毫秒级）
cargo test --doc                     # 文档示例
bash scripts/test-integration.sh     # 集成测试（需要 GPU + /dev/dri）
```

## 致谢

播放内核是 [liberika](https://github.com/AimesSoft/liberika)。这个仓库只做 NSH 侧的桥接。

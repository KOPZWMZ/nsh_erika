# nsh-erika

> Native video for [Native Shell](https://nsh.dev): the *erika* playback core bridged into the
> `erika.video` native component — zero-copy GPU handoff, hardware decode, libass subtitles, danmaku.

[NSH](https://nsh.dev) 的原生视频控件 `erika.video`：把 [liberika](https://github.com/AimesSoft/liberika)
（erika 播放内核）的画面接进 NSH 的界面树。

## 简介

- **这是播放内核的桥，不是播放器**：没有播放列表、没有进度条、没有 UI，控制条由调用者用自己的
  NSH 元素画。解码 / 同步 / 字幕（libass）/ 弹幕 / 音频全在 erika。
- **画面零 CPU 回读**：Linux 走线性 dma-buf → `GdkDmabufTexture`；Windows 走 D3D11 共享纹理 →
  `OpenSharedResource` → D2D `DrawBitmap`。
- **两条路线**：
  - `erika_video()` —— 引擎由组件建、也由组件销毁；控制走 props / 事件 / 命令。
  - `erika_video_for(player)` —— 页面建引擎，组件认领所有权并显示；句柄拥有全能力。
- 原生控件只认一个数字（帧源号），所以引擎放在哪儿都行，画面可以随页面生灭。

## 快速开始

### 1. 依赖

```toml
[dependencies]
nsh-core = "0.6"
nsh-base = "0.6"
nsh-erika = { git = "https://github.com/KOPZWMZ/nsh_erika", tag = "v0.3.0" }

[target.'cfg(target_os = "linux")'.dependencies]
nsh-platform-linux = { version = "0.6", default-features = false }

[target.'cfg(target_os = "windows")'.dependencies]
nsh-platform-windows = { version = "0.6", default-features = false }

# Linux 必需：erika 的 Vulkan 直接导入要补丁版 wgpu-hal，而 Cargo 不会把依赖仓库里的
# [patch] 传下来 —— 每个外部 workspace 都得自己写一份。两个 tag 必须一致。
[patch.crates-io]
wgpu-hal = { git = "https://github.com/AimesSoft/liberika", tag = "v0.3.7" }
```

⚠️ **先别急着 `cargo build`** —— erika 还要本地原生依赖，两个平台的备法完全不同：
Linux 见 [docs/linux.md](docs/linux.md)（`bash scripts/prepare-linux-deps.sh` 一键备齐），
Windows 见 [docs/windows.md](docs/windows.md)。

### 2. 注册控件（**应用 crate 的根**）

```rust
use nsh_erika::PlatformComponents;

// `#[native_components(<平台>)]` 自己会加 cfg，所以两个函数平铺写就行。
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

// 句柄路线：页面建引擎，组件认领所有权并显示（卸载即销毁）
let player = nsh_erika::ErikaPlayer::new().expect("引擎起不来");
player.open(nsh_erika::MediaRequest::new("/path/to/video.mkv"));
player.play();

view()
    .flex_col()
    .flex_1()
    .child(nsh_erika::erika_video_for(player.clone()).flex_1())
    .child(button().on_press({
        let player = player.clone();
        move || player.pause()
    }).child(text("暂停")))
```

```rust
// 简化路线：引擎由组件建、也由组件销毁；控制走 props / 事件 / 命令
nsh_erika::erika_video().src(uri).playing(true).volume(0.8)
```

`playing` / `muted` / `volume` / `rate` **不写 = 不干预**（`playing` 不写不会自动播）。

完整 API 见 [docs/api.md](docs/api.md)，可跑的完整示例见
[`examples/nsh_erika_demo`](examples/nsh_erika_demo)。

## 重点注意

- **不要把画面放进会重跑的区域**（`when` / `dynamic` / 列表）：区域重跑 = 组件重建 = **新引擎**。
- **props 不写 = 不干预**：`playing` / `muted` / `volume` / `rate` 都是如此，默认值由引擎给。
- **`nsh run` 时 manifest 里的 `[patch]` 不生效**：nsh 生成的 manifest 才是 workspace 根，
  要在 `.cargo/config.toml` 里加 `paths = [".vendor/wgpu-hal"]`（脚本会写）。
- **上游类型只从 `nsh_erika::erika` 拿**：别在 `Cargo.toml` 里再依赖 liberika，git rev 对不齐
  就是两个 crate、两套类型。
- **出帧不可用只上报、不降级**：原因写进 `player.status().error` / `simple::VideoEvent::Error`，
  要不要换实现由调用者决定。

## 适配平台

| 平台 | 出帧路径 | 状态 |
| --- | --- | --- |
| **Linux** | erika 导出线性 dma-buf → GDK 直接导入 → GSK 当纹理采样 | ✅ 跑通（VA-API 硬解 + 弹幕 + 字幕） |
| **Windows** | erika 发布 `MISC_SHARED` 快照 → `OpenSharedResource` → D2D `DrawBitmap` | ✅ 跑通（D3D11VA 硬解 + 弹幕 + 字幕） |
| macOS / Android | — | 未做（编译期 `compile_error!` 挡住） |

已知限制见 [docs/api.md](docs/api.md#已知限制)。

## 开发

```sh
cargo test --lib                     # 单元测试（毫秒级）
cargo test --doc                     # 文档示例
bash scripts/test-integration.sh     # 集成测试（需要 GPU + /dev/dri）
```

## 许可证

**MIT OR Apache-2.0** 双许可，任选其一：[LICENSE-MIT](LICENSE-MIT) / [LICENSE-APACHE](LICENSE-APACHE)。

播放内核 [liberika](https://github.com/AimesSoft/liberika) 有自己的许可；**分发带 erika 的二进制**
时要同时满足它和 LGPL FFmpeg 的要求。各版本的对外变更见 [CHANGELOG.md](CHANGELOG.md)。

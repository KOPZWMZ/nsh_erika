# Windows：依赖准备与验证

目标：**Windows + D3D11 + 共享纹理**。erika 渲染到自己的离屏面，每帧发布一张 `MISC_SHARED`
快照；控件用 `OpenSharedResource` 打开同一块显存 → `CreateBitmapFromDxgiSurface` → D2D
`DrawBitmap` 画进 NSH 的绘制上下文。**数据不经过 CPU**。

实测环境：Windows 11 + Rust 1.92（MSVC）+ D3D11VA 硬解。

---

## 1. 工具链

| 需要 | 为什么 | 怎么装 |
| --- | --- | --- |
| **Rust 1.92+（MSVC 目标）** | erika 的 `rust-version`；`x86_64-pc-windows-msvc` | rustup 默认即可 |
| **Visual Studio Build Tools（C++ 桌面开发）** | erika 编 C 桥（`cc`）+ FFmpeg 的 MSVC 构建 | VS Installer 勾"使用 C++ 的桌面开发" |
| **CMake** | FFmpeg / dav1d / 字体库的构建 | `winget install Kitware.CMake` 或 choco/scoop |
| **NASM（x64）** | FFmpeg 的汇编 | `winget install NASM.NASM`（记得加进 PATH） |
| **msys2 / make** | 上游 x64 构建链用 `msys2: true` | `winget install MSYS2.MSYS2`，或你的包管理器 |
| **meson + python** | vendored libass 用 meson 构建 | `pip install meson` |

---

## 2. 依赖：在 liberika 的 checkout 里跑 `xtask`

Windows 侧**不用**系统 FFmpeg、不用 Vulkan、不用补丁 wgpu-hal、不用 PulseAudio —— 全都由
liberika 自带的 `xtask` vendoring 出来（FFmpeg / dav1d / zlib / libass / freetype / harfbuzz /
fribidi 全都有 Windows 构建）：

```powershell
# ① 定位 cargo 给 erika 拉的 checkout
#    先在 Cargo.lock 里看 erika 的 rev：
Select-String -Path Cargo.lock -Pattern 'name = "erika"' -Context 0,4
#    再去 checkout 目录对号（多个版本并存时挑最新的那个）：
dir $env:USERPROFILE\.cargo\git\checkouts\liberika-*

# ② 进那个 checkout，备依赖（--profile lgpl 是默认，--all 表示全部依赖组）
cd $env:USERPROFILE\.cargo\git\checkouts\liberika-<hash>\<rev>
cargo run --locked -p xtask -- deps build --profile lgpl --all
```

> **不用带 `--target`。** 从 liberika 0.3.7（#214）起，`host` 在 Windows x64/ARM64 上自动解析成
> 对应的 MSVC 三元组，产物落在 `dist/x86_64-pc-windows-msvc/lgpl/` —— 正好是 `erika/build.rs`
> 找的地方。要编别的架构时才显式写 `--target`。

产物落在 **checkout 内部**：

```
third_party/dist/x86_64-pc-windows-msvc/lgpl/
    ffmpeg-8.x/  dav1d/  zlib/  libass/  freetype/  harfbuzz/  fribidi/
```

`erika/build.rs` 默认就从这里找（`<workspace_root>/third_party/dist/<target>/lgpl/<dep>`），
所以你**什么都不用配**。想放到别处就用环境变量指过去：

| 变量 | 覆盖 |
| --- | --- |
| `ERIKA_LIBASS_DIR` | 补丁版 libass |
| `ERIKA_FFMPEG_DIR` | FFmpeg 8 |
| `ERIKA_FREETYPE_DIR` / `ERIKA_HARFBUZZ_DIR` / `ERIKA_FRIBIDI_DIR` | 字体库 |
| `ERIKA_NATIVE_PROFILE` | 依赖 profile（默认 `lgpl`；GPL 全功能用 `gpl-full`） |
| `ERIKA_NATIVE_TARGET` | 依赖目标三元组（交叉编译时才需要） |

> 依赖随 erika 的 tag 变。**升级 `nsh-erika` 里的 erika tag 之后要重跑 ②**（`--force` 可强制重建）。
>
> `nsh-erika` 的 `Cargo.toml` 里有一条 `[patch.crates-io] wgpu-hal = { git = …, tag = … }`。
> 它是**给 Linux** 的（Vulkan 直接导入要用补丁版 HAL），Windows 上跑不到那条路径，但补丁是全局的
> —— 也就是 Windows 构建也会用 liberika 仓里那份 `wgpu-hal`。这不影响什么（liberika 自己的 CI
> 就用同一个 patch 构建 Windows），只是别被 `cargo tree -i wgpu-hal` 显示的 liberika 来源吓到。

### 和 Linux 的对照（Windows 上都不用管）

| Linux 需要 | Windows 对应 |
| --- | --- |
| 系统 FFmpeg 8 + pkg-config | vendored FFmpeg（xtask） |
| **补丁版 libass**（脚本 + meson） | vendored libass（xtask） |
| **补丁 wgpu-hal**（VA-API 直通） | **不需要** —— 那个补丁只为 Linux 的 Vulkan 直通导入 |
| PulseAudio | WASAPI（系统自带） |
| Vulkan loader / VA-API / `/dev/dri` | D3D11 / DXGI（系统自带）+ D3D11VA / NVDEC |
| GTK4 / X11 / Wayland | Win32 + Direct2D（系统自带） |
| freetype/harfbuzz/fribidi/fontconfig 开发包 | vendored（字体库）+ DirectWrite |

---

## 3. 构建与运行

```powershell
cargo check --all-targets
cargo run -p nsh-app-erika-demo
```

示例的环境变量：

```powershell
$env:TEST_VIDEO   = "D:\media\video.mkv"
$env:TEST_DANMAKU             = "D:\media\danmaku.json"   # 或者 "off" 不挂弹幕
$env:NSH_ERIKA_TRACE = "1"
$env:RUST_LOG        = "info"
cargo run -p nsh-app-erika-demo
```

### ⚠️ 日志看不见的两种情况

1. **GUI 子系统没有控制台**：示例的 `main.rs` 里有一行
   `#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]`（现在被注释掉了）。
   打开它 → 双击 exe 不弹黑框，但同时**没有 stdout/stderr**，`println!` / `eprintln!` /
   `env_logger` 全部看不见。要日志就保持注释状态（用控制台跑）。
2. **`env_logger` 默认只打 `error`**：`log::info!` 这类要 `RUST_LOG=info`（或 `RUST_LOG=nsh_erika=info`）。

---

## 4. 验证清单

| # | 看什么 | 期望 |
| --- | --- | --- |
| 1 | **硬解** | `{"event":"video_decoder_changed","activeBackend":"d3d11va","requestedBackend":"d3d11va"}` |
| 2 | **出图面挂上** | `erika.video: attach 出图面 1920x1080 scale=1`（resize 时是 `resize`） |
| 3 | **出帧节奏**（`NSH_ERIKA_TRACE=1`） | `tick ≈ paint ≈ frames`：无弹幕 ≈ 片源帧率，有弹幕 ≈ 60/s |
| 4 | **暂停** | 日志**停住**（没有 tick 就没有请求，也不空转）；画面停在最后一帧 |
| 5 | **暂停 + 拖进度条** | 画面立刻跟到新位置（一次交互换一次重绘） |
| 6 | **拖窗口 / 改 DPI** | 不闪黑、不拉伸，`resize 出图面 WxH` 出现 |
| 7 | **退出** | `playback_engine_teardown` 之后线程全清（`ps`/任务管理器里没了） |

三条核心探针的读法：`frames` = 引擎发布几帧、`paint` = 真正换上几次、`tick` = 控件被调几次。
**三个数相等**说明"一帧一次重绘、没有浪费"。

---

## 5. 已知限制

| 限制 | 说明 |
| --- | --- |
| **每帧一次 `CopyResource` + 一次 GPU 同步等待** | 上游为了交出一张"不可变、GPU 完成态"的快照。1080p 无感；4K 高帧率是成本大头 |
| **输出钉死 SDR** | HDR 片源会被 tone-map 到 SDR |
| 每帧新建纹理 + 每帧 `OpenSharedResource` | 分配器压力，靠上游池化解决 |
| legacy `MISC_SHARED` 句柄没有 fence / keyed mutex | 所以生产者只能 `wait_for_gpu` 阻塞等完再交；换 `_NTHANDLE` + fence 才能追上 Linux 的品质（需要上游改） |
| 出帧由**引擎**驱动（16ms 泵） | 窗口最小化/被遮挡时引擎仍在渲染；省电优化（没人在要帧就退回 50ms 保活）还没做 |

---

## 6. 排查

| 现象 | 可能原因 | 处理 |
| --- | --- | --- |
| 完全没日志 | GUI 子系统吞了 stdout；或 `env_logger` 只有 error | 注释掉 `windows_subsystem` 那行；`RUST_LOG=info` |
| 黑屏，`frames` 不涨 | 渲染器后端选错（wgpu 会把外部纹理拒掉，并让硬解降级成软解） | 看启动日志的 `activeBackend`：应该是 `d3d11va`，不是 `software` |
| 只有第一帧，之后冻住 | 控件不再重绘（"推/拉"接错了） | 看 `tick` 是否停在 1：应该是"引擎发布 → 叫醒控件" |
| `tick` 几千/s（CPU 空转） | 控件自己在 `draw` 里续帧 = 自激回路 | 应该由引擎泵驱动、控件只在有新帧时重绘 |
| `OpenSharedResource 失败（…不在同一块 GPU？）` | 宿主和 erika 不在同一个 adapter | 混合显卡机器上强制同一个 GPU；这条会写进 `status.error` |
| 麦克风/声音没有 | WASAPI 被独占 | 换默认输出设备 |

# Linux：依赖准备与验证

目标：**Linux + Vulkan + 线性 dma-buf**。画面由 erika 导出 dma-buf，GDK 直接导入成纹理，
GSK 采样上屏 —— 我们这侧不建 Vulkan 设备、不做任何像素拷贝。

实测环境：Arch Linux + RADV（AMD iGPU）+ GTK 4.24 + FFmpeg 8。

---

## 1. Rust 与系统库

- **Rust 1.92+**（erika 的 `rust-version`；本库自己只要 edition 2024）
- 需要 `cargo`、`rustc`、`pkg-config`、`bash`、`meson`、`ninja`、`patch`、`curl`、`sha256sum`
- GPU：能跑 Vulkan 的驱动 + `/dev/dri`（硬解还要 VA-API 驱动）

Arch：

```sh
sudo pacman -S --needed base-devel pkgconf meson ninja patch curl \
  gtk4 vulkan-icd-loader vulkan-headers \
  libpulse ffmpeg \
  freetype2 harfbuzz fribidi fontconfig \
  mesa                     # AMD/Intel：RADV + VA-API；N 卡按你的驱动来
```

Debian / Ubuntu：

```sh
sudo apt install build-essential pkg-config meson ninja-build patch curl \
  libgtk-4-dev libvulkan-dev \
  libpulse-dev \
  libavcodec-dev libavformat-dev libavutil-dev libavdevice-dev libswscale-dev libswresample-dev \
  libfreetype-dev libharfbuzz-dev libfribidi-dev libfontconfig-dev \
  mesa-vulkan-drivers libva2 libva-drm2
```

说明：

| 库 | 谁要 | 没有会怎样 |
|---|---|---|
| `gtk4` | NSH 的 Linux 后端（宿主） | 编译期就找不到 |
| Vulkan loader + headers | erika 的 wgpu/Vulkan 直通 | `Linux GPU interop requires Vulkan development headers` |
| `libpulse` | erika 的音频输出 | `Linux audio requires libpulse development files` |
| FFmpeg 8 开发包 | 解码/解封装 | `Linux GPU interop requires FFmpeg 8 development headers` |
| freetype/harfbuzz/fribidi/fontconfig | 补丁版 libass 的编译与运行时 | 补丁 libass 编不出来 |
| `/dev/dri` + VA-API 驱动 | 硬解（软解不需要） | 掉到软解，CPU 高很多 |

---

## 2. 两个"补丁前置"（这是 Linux 唯一麻烦的地方）

erika v0.3.7 在 Linux 上有两样东西**上游没发给外部消费者**，必须自己备：

| 前置 | 为什么必须 | 上游怎么接的 |
|---|---|---|
| **补丁版 `wgpu-hal`** | erika 的 VA-API 直通调用 `Texture::set_external_initial_usage`，这个方法只在 liberika 仓内的 `third_party/wgpu-hal`（wgpu-hal 29.0.3 + `initial_usage` 字段） | 只用 workspace 级 `[patch.crates-io]`，cargo **git 依赖传不出来** |
| **补丁版 `libass` 0.17.5** | 加了"有序内存字体回退"；系统 libass 不能替代 | `erika/build.rs` 断言 `ERIKA_LIBASS_DIR`（或默认 dist 路径）下的 `lib/libass.a` 存在 |

### 一键（推荐）

```sh
bash scripts/prepare-linux-deps.sh          # 幂等，已就绪就跳过
bash scripts/prepare-linux-deps.sh --force  # 强制重建
```

它做的事：

1. 从 `Cargo.toml` 读 erika 的 tag，再从 `Cargo.lock` 找对应的 commit，定位 cargo 给 erika 拉的
   checkout（`$CARGO_HOME/git/checkouts/liberika-*/<rev>/`）；
2. 把 checkout 里的 `third_party/wgpu-hal` 复制到 `.vendor/wgpu-hal`；
3. 跑 checkout 里的 `scripts/build_linux_libass.sh`（meson/ninja + 打补丁），把产物放到
   `.vendor/libass`；
4. 生成 `.cargo/config.toml`（**不进 git**）把它们接上：

   ```toml
   paths = [".vendor/wgpu-hal"]

   [env]
   ERIKA_LIBASS_DIR = { value = ".vendor/libass", relative = true }
   ```

5. 写 `.vendor/erika-deps.stamp`（tag + libass 补丁集 hash）作为幂等依据。
   **libass 的补丁集会随 erika 版本变**，所以升级 erika 后 patch hash 变了会自动重建 ——
   复用旧的 `.a` 能编过但会静默退回旧行为。

### 手工（你的 app 不在这个仓库时）

`.vendor/` 和 `.cargo/config.toml` 都按 **workspace** 生效，所以外部消费者要在**自己应用的根**上做：

```sh
# ① 找到 cargo 给 erika 拉的 checkout（版本号看 Cargo.lock 里 erika 的 source rev）
ls -d $CARGO_HOME/git/checkouts/liberika-*/*/

# ② 补丁版 libass：在 checkout 里跑，产物落在 <checkout>/third_party/dist/<target>/system/libass
cd <liberika-checkout> && bash scripts/build_linux_libass.sh

# ③ 若不想依赖 ② 的默认路径，把它和 wgpu-hal 一起放进你应用的 .vendor/
mkdir -p .vendor
cp -r <liberika-checkout>/third_party/wgpu-hal  .vendor/wgpu-hal
cp -r <liberika-checkout>/third_party/dist/<target>/system/libass .vendor/libass

# ④ 在你应用根写 .cargo/config.toml（内容同上）
```

> `paths = [".vendor/wgpu-hal"]` 是**覆盖依赖图**的写法，必须放在你实际编译的那个 workspace 根；
> 放在别处不生效。这也是本仓库把 `.cargo/config.toml` 列入 `.gitignore` 的原因（它是本机生成物）。

---

## 3. 构建与运行

```sh
cargo check --all-targets          # 编译体检
cargo run -p nsh-app-erika-demo    # 从仓库根跑示例
```

示例里用环境变量换片源：

```sh
cd examples/nsh_erika_demo
NSH_ERIKA_SRC=/path/to/video.mkv DMK=/path/to/danmaku.json nsh run
DMK=off NSH_ERIKA_SRC=/path/to/video.mkv nsh run      # 不挂弹幕（量性能的对照组）
```

（`nsh` 必须在有 `nsh.json` 的目录里跑；`cargo run` 走 `main.rs` 那条路径，两者需要各自注册一次
原生控件，示例里都已经写好。）

---

## 4. 验证

**① 硬解生效**（`RUST_LOG=nsh_erika=info` 或看 erika 的 event 日志）：

```json
{"event":"video_decoder_changed","activeBackend":"vaapi","codec":"hevc","requestedBackend":"vaapi"}
```

看到 `software` 就说明硬解没接上（驱动/权限/`/dev/dri`）。

**② 出帧节奏**（`NSH_ERIKA_TRACE=1`）：

```
[nsh-erika] engine tick ≈100/s render=true has_media=true
[nsh-erika] tick ≈60/s paint ≈60/s frames ≈60/s maxgap=17ms
```

- `paint == frames` → 引擎发多少、控件画多少，没丢；
- 有弹幕时 `≈60/s`（弹幕每帧都在动），无弹幕时 `≈ 片源帧率`；
- 暂停时**一行都不打印**（没有 tick 就没有请求，也不空转）。

**③ 集成测试**（需要 GPU + `/dev/dri`）：

```sh
bash scripts/test-integration.sh --keep-going
```

四个用例：`engine_state` / `engine_open` / `engine_playback` / `frame_output`。
`frame_output` 会 mmap 读 dma-buf 断言"不是纯黑、画面在动"——**RADV 不允许 mmap dma-buf**，
所以脚本会额外用 lavapipe（`vulkan-swrast`）再跑一遍像素级断言；装了软件 Vulkan 才有这一遍。

---

## 5. 已知限制

| 限制 | 说明 |
|---|---|
| 需要两个补丁前置 | 见 §2，一步脚本；升级 erika 要重跑 |
| 只能"拉"模式 | GTK4 没有 per-widget 原生窗口，Wayland 也不允许外来 subsurface，所以注入/直通两条路都不可行 |
| 弹幕与视频同纹理 | 出帧频率 = max(视频帧率, 弹幕平滑度)：有弹幕时 30fps 片源也会按 60Hz 合成 |
| 4K 高帧率未实测 | dma-buf 路本来没有拷贝，风险主要在导出/提交开销 |

## 6. 常见报错

| 报错 | 原因 / 处理 |
|---|---|
| `Patched Linux libass is missing at … Run bash scripts/build_linux_libass.sh first` | 没备前置 → 跑 `scripts/prepare-linux-deps.sh` |
| `Linux audio requires libpulse development files` | 缺 `libpulse-dev` / `libpulse` |
| `Linux GPU interop requires Vulkan development headers` | 缺 `vulkan-headers` / `libvulkan-dev` |
| `Linux GPU interop requires FFmpeg 8 development headers` | FFmpeg 版本太旧；要 8.x 的开发包 |
| `set_external_initial_usage` 找不到 / wgpu-hal 版本冲突 | `.vendor/wgpu-hal` 没接上或与 erika 的 tag 不匹配 → `prepare-linux-deps.sh --force` |
| 画面黑但 `frames` 在涨 | 控件的 dma-buf 导入失败：看 `status.error` 与一次性日志 |

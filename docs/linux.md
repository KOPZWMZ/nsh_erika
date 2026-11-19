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
| --- | --- | --- |
| `gtk4` | NSH 的 Linux 后端（宿主） | 编译期就找不到 |
| Vulkan loader + headers | erika 的 wgpu/Vulkan 直通 | `Linux GPU interop requires Vulkan development headers` |
| `libpulse` | erika 的音频输出 | `Linux audio requires libpulse development files` |
| FFmpeg 8 开发包 | 解码/解封装 | `Linux GPU interop requires FFmpeg 8 development headers` |
| freetype/harfbuzz/fribidi/fontconfig | 补丁版 libass 的编译与运行时 | 补丁 libass 编不出来 |
| `/dev/dri` + VA-API 驱动 | 硬解（软解不需要） | 掉到软解，CPU 高很多 |

---

## 2. 两个"补丁前置"（这是 Linux 唯一麻烦的地方）

erika v0.3.7 在 Linux 上有两样东西**上游没发给外部消费者**，必须自己备：

| 前置 | 为什么必须 | 怎么备 |
| --- | --- | --- |
| **补丁版 `wgpu-hal`** | erika 的 Vulkan 直接导入要用 `Texture::set_external_initial_usage`（只在 liberika 仓内的 `third_party/wgpu-hal` 里，29.0.3 + `initial_usage`） | 自己根 `Cargo.toml` 里写 `[patch.crates-io]`（**下面 §2.1**） |
| **补丁版 `libass` 0.17.5** | 加了"有序内存字体回退"；**系统 libass 不能替代** | 跑 upstream 的 `scripts/build_linux_libass.sh`（**下面 §2.2**，脚本一键） |

### 2.1 `wgpu-hal`：写在清单里的 `[patch.crates-io]`

Cargo **不会**把依赖仓库里的 `[patch.crates-io]` 传下来，所以每个外部 workspace 都得自己写一份
（liberika `docs/linux.zh.md`「仓库外的 Cargo workspace」）：

```toml
[dependencies]
erika = { git = "https://github.com/AimesSoft/liberika", tag = "v0.3.7", features = ["wgpu"] }

[patch.crates-io]
wgpu-hal = { git = "https://github.com/AimesSoft/liberika", tag = "v0.3.7" }
```

- **两个 tag 必须一致**：HAL 和引擎来自同一个 commit，否则可能对不上。
- ⚠️ **不要用 Cargo 的 `paths`（`.cargo/config.toml`）覆盖**。上游明确要求这一点，原因也很实在：
  `paths` 是"解析之后硬改源码位置"，会带一条
  `path override for crate wgpu-hal has altered the original list of dependencies` 警告，
  而且 `.cargo/config.toml` 通常不入 git —— 别人 clone 下来编不过。
  `[patch]` 是解析阶段换源，`Cargo.lock` 里记的是真实来源。
- 本地改 `wgpu-hal` 本身时才临时换成 path 形式：
  `wgpu-hal = { path = "../liberika/third_party/wgpu-hal" }`（仍在 `[patch.crates-io]` 下）。
- 锁文件里如果还指着 registry 的 HAL：

  ```sh
  cargo update -p wgpu-hal --precise 29.0.3
  cargo tree -i wgpu-hal     # 来源应显示 liberika
  ```

#### 用 `nsh run` 时的额外一步（nsh 0.5.0 的限制）

`[patch]` **只在 workspace 根 manifest 生效**。nsh 0.5.0 会把生成的 `.nsh/rust/Cargo.toml`
当作虚拟 workspace 根，应用自己 manifest 里那份 `[patch]` 因此被忽略（cargo 只打一条
"patch for the non root package will be ignored"），`nsh run` 于是退回 registry 的 `wgpu-hal`，
报 `no method named set_external_initial_usage`。而 `cargo check` / `cargo run` 是好的
（那时应用的 manifest 就是根）。

所以 `nsh run` 还要在 `.cargo/config.toml` 里放一条**配置级**覆盖 —— 它不受 workspace 根限制：

```toml
# 路径按「本 config 所在目录的上一级」解析
paths = [".vendor/wgpu-hal"]
```

代价是 cargo 会打一条 `path override for crate wgpu-hal has altered the original list of
dependencies` 警告（`paths` 是在解析之后硬改源码位置，cargo 自己说这"不保证行为正确"）。
它没有实际影响，只是噪音：**等 nsh 支持把应用的 `[patch]` 带进生成的根之后就能删掉。**

### 2.2 `libass`：跑一次 upstream 的构建脚本

```sh
bash scripts/prepare-linux-deps.sh          # 幂等，已就绪就跳过
bash scripts/prepare-linux-deps.sh --force  # 强制重建
```

它做的事：

1. 从 `Cargo.toml` 读 erika 的 tag，再从 `Cargo.lock` 找对应 commit，定位 cargo 给 erika 拉的
   checkout（`$CARGO_HOME/git/checkouts/liberika-*/<rev>/`）；
2. 校验 `[patch.crates-io]` 里的 `wgpu-hal` tag 和 erika 的 tag 一致（不一致直接报错）；
3. 用 checkout 里的 `scripts/build_linux_libass.sh`（meson/ninja + 打补丁）编出补丁版 libass，
   安装到 `.vendor/libass`；
4. 写 `.cargo/config.toml`（**不进 git**，只放这一项）：

   ```toml
   [env]
   ERIKA_LIBASS_DIR = { value = ".vendor/libass", relative = true }
   ```

5. 写 `.vendor/erika-deps.stamp`（tag + libass 补丁集 hash）作为幂等依据。
   **libass 的补丁集会随 erika 版本变**，所以升级 erika 后 hash 变了会自动重建 ——
   复用旧的 `.a` 能编过但会静默退回旧行为。

不想要 `.vendor/` 也行：直接在 **erika 的 checkout 里**跑 `bash scripts/build_linux_libass.sh`，
产物落在 `<checkout>/third_party/dist/<triple>/system/libass`，`erika/build.rs` 默认就去那儿找，
不必设 `ERIKA_LIBASS_DIR`。缺点是它在 `~/.cargo` 里，换机器要重做、也容易忘。

### 2.3 可选：`-fPIC`

上游还给 Linux Rust 消费者准备了这组环境变量（内嵌 `soundtouch-ffi` 默认非 PIC，而 erika 会编
共享库）。我们的构建没它也能过，所以默认没加；一旦遇到
`relocation R_X86_64_PC32 against symbol … can not be used when making a shared object`，
在 `.cargo/config.toml` 里加上：

```toml
[env]
CFLAGS_x86_64_unknown_linux_gnu = "-fPIC"
CXXFLAGS_x86_64_unknown_linux_gnu = "-fPIC"
```

---

## 3. 构建与运行

```sh
cargo check --all-targets          # 编译体检
cargo run -p nsh-app-erika-demo    # 从仓库根跑示例
```

示例里用环境变量换片源：

```sh
cd examples/nsh_erika_demo
TEST_VIDEO=/path/to/video.mkv TEST_DANMAKU=/path/to/danmaku.json nsh run
TEST_DANMAKU=off TEST_VIDEO=/path/to/video.mkv nsh run      # 不挂弹幕（量性能的对照组）
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
| --- | --- |
| 需要两个补丁前置 | 见 §2，一步脚本；升级 erika 要重跑 |
| 只能"拉"模式 | GTK4 没有 per-widget 原生窗口，Wayland 也不允许外来 subsurface，所以注入/直通两条路都不可行 |
| 弹幕与视频同纹理 | 出帧频率 = max(视频帧率, 弹幕平滑度)：有弹幕时 30fps 片源也会按 60Hz 合成 |
| 4K 高帧率未实测 | dma-buf 路本来没有拷贝，风险主要在导出/提交开销 |

## 6. 常见报错

| 报错 | 原因 / 处理 |
| --- | --- |
| `Patched Linux libass is missing at … Run bash scripts/build_linux_libass.sh first` | 没备前置 → 跑 `scripts/prepare-linux-deps.sh` |
| `Linux audio requires libpulse development files` | 缺 `libpulse-dev` / `libpulse` |
| `Linux GPU interop requires Vulkan development headers` | 缺 `vulkan-headers` / `libvulkan-dev` |
| `Linux GPU interop requires FFmpeg 8 development headers` | FFmpeg 版本太旧；要 8.x 的开发包 |
| `no method named set_external_initial_usage` | `Cargo.toml` 里没有 `[patch.crates-io]`，或 tag 与 erika 不一致 → 按 §2.1 补上，再 `cargo update -p wgpu-hal --precise 29.0.3` |
| `path override for crate wgpu-hal has altered the original list of dependencies` | 你在 `.cargo/config.toml` 里用了 `paths` 覆盖 → 删掉它，改用 `[patch.crates-io]`（§2.1） |
| 画面黑但 `frames` 在涨 | 控件的 dma-buf 导入失败：看 `status.error` 与一次性日志 |

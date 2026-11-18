# 更新日志

版本号用 semver（`publish = false`，所以版本号主要服务于 git tag）。这个文件只记**对外**的
变更；内部的设计记录 / 踩坑过程在 `dev_doc/`（不入库）。

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)：每版分「新增 / 变更
（破坏性）/ 修复 / 文档」几节，破坏性的写在最显眼的位置。

## [0.2.0] - 2026-11-18

首个**对外**发布。仓库内曾有一个内部 tag `v0.1.0`（未发布）；导出面在 0.2.0 大改，
所以对外从 0.2.0 起步 —— 下面「变更」一节对 0.1.0 是破坏性的。

### 新增

- **`escape-hatch` feature（默认关闭）**：打开才导出"绕开封装"的那批 API ——
  `video::*`（自写控件 / 出帧自检）、`ErikaPlayer::{video_source, call, call_with, raw}`、
  弹幕整包样式（`update_config` + `danmaku::DanmakuLayoutConfig`）。
  默认不导出不是"删了"，是"不推荐、不承诺"。
- **`ErikaEvent::Ended`**：转发上游 `PlayerEvent::Ended`（媒体自己放完了，不是 `stop()`），
  紧跟在 `StateChanged(Stopped)` 之后。连播 / 结算用它，别拿 `position ≈ duration` 去猜。
- **`ErikaEvent::Resync { missed }`**：`broadcast` 落后时明确说"你漏了几条"，
  不再静默折成一条笼统的事件（也修掉了把它当"引擎没了"的 bug，见下）。
- `tests/events.rs`：钉住两条契约 —— 上游离散事件会被转发（白名单不是死代码）、
  以及**事件不会写状态通道**。
- CI：加 feature 矩阵 —— `cargo check --all-targets` / `cargo test --lib` / `cargo doc`
  各跑「默认」与「`--features escape-hatch`」两遍，并把 `rustdoc::broken_intra_doc_links`
  提成 error（`cargo test --doc` 只跑 doctest，断链只出 warning，不会红）。

### 变更（破坏性）

- **导出面收窄**：根上 45 个名字 → 7 个（`ErikaPlayer` / `ErikaVideo` / `register` /
  `COMPONENT_NAME` / `PlatformComponents` / `MediaRequest` / `PlayerState`），其余按
  "屏幕上的关注点"进 `danmaku` / `subtitle` / `tracks` / `state` / `events` / `video` 模块。
  上游类型一律走 `nsh_erika::erika::…` 兜底（**别再自己依赖 liberika**，git rev 对不齐
  就是两个 crate、两套类型）。
- **删掉 `DanmakuSnapshot.config`**（上游 24 字段整包）：读整包改用
  `player.call_with(|p| p.danmaku_config().cloned())`，写入仍走 `update_config`（逃生舱）。
  顺带 `publish()` 在每条命令上少克隆一份整包。
- **事件通道改成"上游离散事件的转发"**：删掉笼统的 `ErikaEvent::Changed`。白名单是
  `StateChanged` / `Ended` / `Buffering` / `TracksChanged` / `TrackSelectionChanged` / `Error`
  （外加通道自己的 `Resync`）；`PositionChanged` 永不进入（500 条/秒，是采样量不是事实）。
- **状态通道退成"命令侧快照"**：只在命令路径上发布，上游自己改变状态（自然播完、上游报错）
  时不刷新 —— `is_playing` / `duration` / `selection` 要准请直读 `player.*()`。
- **失败只有一个出口**：`status.error` 与 `Error` 事件始终同时写、文案同一份。
- **音量统一到 `f32`**：`set_volume(f32)` / `volume() -> f32` / `ErikaStatus.volume: f32`。
  上游 presenter 对外是 `f64`（同时也是 Flutter 桥的边界类型），但**内部存的是 f32**
  （`set_volume` 落地时 `normalize_volume(volume as f32)`），真实精度就是 f32 —— 之前
  `set_volume(f64)` + `volume() -> f64` 两头都掩盖了这点（写侧静默截断、读侧伪造精度）。

### 修复

- **`Lagged` 不再被当成"引擎没了"**：以前 `recv()` 把 `Lagged` 和 `Closed` 一起映射成
  `None`，一次落后就会让订阅任务永久收摊、静默失去后续事件。现在 `Lagged` →
  `Resync { missed }`（跳过并告知），只有 `Closed` → `None`。

### 文档

- `docs/api.md`：新增「路径：从哪儿 import」和「逃生舱（`escape-hatch` feature）」两节；
  重写「读状态」（三条订阅通道 + 三条规矩）；修掉早已不存在的 API 名
  （`toggle` / `media_time` / `intent_state` / `playback`）。
- `README.md`：依赖 / 路径 / 逃生舱 / 校验命令对齐。
- `ErikaTracks::add_external_subtitle` 补顺序警告（见「已知限制」）。
- 示例应用改成**订阅驱动**（也是新 API 的参考写法）：进度走 `subscribe_progress`（10Hz），
  播放/暂停由事件 `StateChanged` 的 payload 驱动（**不持句柄**，否则它自己就成了"最后一个
  句柄"，页面退出时引擎不会退），音量 / 静音 / 错误走 `subscribe_status`。

### 已知限制

- **只上报，不降级**：出帧路径挂掉（导不出 dma-buf / 拿不到共享纹理）就 latch 一次写进
  `status.error`，之后不再重试。换实现 / 提示用户 / 整页替换都是调用方的事。
- **外挂字幕的直连写与 `open()` 有顺序竞态**：`open()` 之后**同一帧**调
  `tracks().add_external_subtitle()` 会撞上 open 的同步重建窗口（轻则字幕被丢，重则可阻塞到
  报 `playback worker stopped …`）。要么等 `StateChanged(Ready)`，要么
  `player.call(|p| { p.player().add_external_subtitle(uri); })` 排进队列。
- macOS / Android 未适配（`compile_error!` 挡住）；Windows 分支不在本仓 CI 上编译
  （self-hosted runner 是 Linux），只有 Linux 侧有回归。

## [0.1.0] - 2026-11-15（内部 tag，未对外发布）

首个可用版本：`erika.video` 控件、`ErikaPlayer` 句柄与状态/订阅的雏形、Linux（线性 dma-buf）
与 Windows（D3D11 共享纹理）两条出帧路径。导出面在 0.2.0 重排，故未单独发布。

# API 用法

库对外的全部东西就三块：**一个句柄**（`ErikaPlayer`）、**一块显示区域**（`ErikaVideo`）、
**两个附加句柄**（弹幕 / 字幕）。控件的 prop 表只有 `source`，所以控制面永远是函数调用，
不需要命令、事件或回调链。

```rust
use nsh_core::prelude::*;

let player = nsh_erika::ErikaPlayer::new()?;   // ① 建引擎（设备 + 音频，没有媒体）
player.open(nsh_erika::MediaRequest::new(uri)); // ② 打开片源（这才会起解码线程）
player.play();                                  // ③ 播

view()
    .flex_col()
    .flex_1()
    .child(nsh_erika::ErikaVideo(&player).flex_1())  // ④ 画面
    .child(/* 你自己的控制条，直接调 player */)
```

---

## 句柄

| 句柄 | 拿法 | 能干什么 |
|---|---|---|
| `ErikaPlayer` | `ErikaPlayer::new()` | 控制播放、读状态、派发弹幕/字幕/轨道 |
| `ErikaVideoSource` | `player.video_source()` | **只读**：帧槽 + `request_frame`。一般不用，直接写 `ErikaVideo(&player)` |
| `ErikaDanmaku` | `player.danmaku()` | 弹幕开关、轨道、样式、偏移 |
| `ErikaSubtitle` | `player.subtitle()` | 字幕样式、位置、延迟 |
| `ErikaTracks` | `player.tracks()` | 轨道列表与选择、外挂字幕 |

`ErikaPlayer` 很便宜可以随便 clone：`Clone` = 两次引用计数加一；`PartialEq` 按身份
（`Arc::ptr_eq`）比较，所以能直接塞 `signal()`（信号是 `Copy`，闭包捕获它就不用每个闭包
都写一次 `clone()`）。

```rust
let player = signal(player);                       // Signal<ErikaPlayer>，Copy
button().on_press(move || player.get().pause());   // .get() 返回一份 clone（2 条原子指令）
```

---

## 注册控件

必须写在**应用 crate 的根**：nsh 生成的宿主按平台去那里找固定名字的入口函数。

```rust
use nsh_erika::PlatformComponents;

#[native_components(linux)]
fn components_linux(components: &PlatformComponents) { nsh_erika::register(components); }

#[native_components(windows)]
fn components_windows(components: &PlatformComponents) { nsh_erika::register(components); }
```

- `#[native_components(<平台>)]` 自己会补 `#[cfg(target_os = …)]`，所以**两个函数平铺写**即可；
  千万别把两个属性叠在同一个函数上（外层展开后内层会被 cfg 剥掉，那个平台就悄悄没有注册）。
- `cargo run` 直跑（不经 `nsh run`）时，`main.rs` 里也要调一次 `nsh_erika::register(&options.components)`。

---

## 显示

```rust
nsh_erika::ErikaVideo(&player)        // 或 player.clone()（参数是 impl Borrow<ErikaPlayer>）
    .flex_1()                          // 返回的是普通 NSH 元素，样式照常链式调用
```

它内部把句柄换成一个号、生成 `native("erika.video")` 元素、把号挂到 `source` prop 上。
手写也可以（一般不需要）：

```rust
native(nsh_erika::COMPONENT_NAME).prop("source", player.video_source().id())
```

**画面比例**：控件按视频宽高比内接并居中，多出来的留黑（等价 `ContentFit::Contain`）。
画面尺寸由控件自己按「控件尺寸 × DPI」算，跟随窗口 resize 与 DPI 变化自动重挂出图面。

---

## 控制

```rust
player.open(MediaRequest::new(uri));  // 打开（异步：命令进队列，立刻返回）
player.play();                        // 播放
player.pause();                       // 暂停
player.toggle();                      // 按**意图**切换（play() 是异步的，别读状态再判断）
player.stop();                        // 位置归 0、画面清空，**媒体仍然开着**（能立刻再 play）
player.seek(Duration::from_secs(30)); // 跳转
player.set_volume(0.8);               // 0.0..=1.0
player.set_muted(true);               // 静音（**不改变** volume）
player.set_rate(1.5);                 // 倍速
```

- 这些方法**都是立即返回**的：命令进引擎的队列，引擎在自己的线程里处理。
- `open` 换片时旧画面会被作废（帧槽清空），控件下一帧自己撤掉上一张图。
- `stop()` 之后 `play()` 可以直接续上：媒体没关，不需要重新 `open`。

`MediaRequest` 是 erika 的类型（本库直接 re-export），字段：

| 字段 | 作用 |
|---|---|
| `uri` | 路径 / `http(s)://` / 其它 FFmpeg 认的 URL |
| `source_hint` | 来源提示（影响某些 demux 选择） |
| `http_headers` | HTTP 头（`Authorization`、Cookie…；`Debug` 刻意不打印它们） |
| `http_read_ahead_bytes` / `http_back_buffer_bytes` | HTTP 预读与回退缓存（`None` = 引擎默认） |
| `start_position` | 打开后立刻定位到哪（片头跳过、续播） |

---

## 读状态

控制是"发命令"，读是"拉值"——**没有回调链**：`ErikaPlayer` 上那批只读方法全是直连
`erika::Player`，零延迟、不需要订阅。另有三条**订阅通道**（各是一对"轮询读 + 订阅"）：

| 通道 | 装什么 | 出口 | 语义 |
|---|---|---|---|
| 状态 | presenter 侧：`volume` / `muted` / 弹幕字幕配置 / `error` | `status()` / `subscribe_status()` | **命令侧快照**：只在命令处理完之后是新的 |
| 进度 | 位置 + 总时长（引擎算好） | `progress()` / `subscribe_progress()` | 10Hz，中间值可丢 |
| 事件 | erika 的离散事件：状态 / 缓冲 / 轨道 / 选轨 / 错误 | `subscribe_events()` | **每条转发**，晚订阅看不到 |

三条规矩：

1. **要"值"就直读**——`is_playing()` / `state()` / `position()` / `duration()` / `tracks()`
   都是一次锁、永远新鲜，别等订阅；
2. **要"发生的那一刻"用事件**——播完切下一集、弹缓冲指示器、出错弹提示；
3. **事件不刷新状态**：收到事件后 `status()` 里那几项可能还是旧值，读直读方法。

⚠️ **自然播完和 `stop()` 在上游是同一个值**（`PlayerState::Stopped`），本库不替你猜：
`state() == Stopped` 且 `generation` 没变（`open` / `seek` / `stop` 都会 +1）就是自然播完。

| 方法 | 语义 | 延迟 |
|---|---|---|
| `media_time()` / `duration()` | 当前时间 / 总时长 | 直连 `Player`，**零延迟** |
| `state()` | 引擎**已提交**的播放状态 | 上一个引擎 tick 的值 |
| `intent_state()` | 用户**意图**状态（`play()` 还没被处理时也准） | 立即 |
| `is_playing()` | 同上，布尔版 | 立即 |
| `playback()` | `PlaybackSnapshot`（位置/时长/状态/速率…一次读全） | 混合 |
| `tracks()` / `chapters()` | 轨道列表 / 章节 | 直连 |
| `has_media()` / `generation()` | 有没有媒体 / 换片计数 | 直连 |
| `status()` | `ErikaStatus`：`volume`、`muted`、弹幕面板初值、字幕面板初值、`error` | presenter 侧 |
| `last_error()` | `status().error` 的快捷方式 | 同上 |

**UI 推荐做法**：`set_interval(200ms)` 拉一次写进信号；按钮按下时再乐观写一遍（否则要等下一轮
才看到反馈）。示例见 [`examples/nsh_erika_demo`](../examples/nsh_erika_demo)。

```rust
let playing = signal(player.is_playing());
let ticker = player.clone();
set_interval(Duration::from_millis(200), move || {
    playing.set(ticker.is_playing());
    // …
});
```

---

## 弹幕

```rust
let danmaku = player.danmaku();

danmaku.add_track(uri, "bilibili");        // 挂轨道（bilibili/xml/json/…；见 erika 文档）
danmaku.set_enabled(true);
danmaku.set_track_enabled(track_id, true);
danmaku.set_track_offset(track_id, -1_500_000);  // 单轨时间偏移（微秒）
danmaku.set_global_offset(0);
danmaku.update_config(|config| {                 // 样式/密度/屏蔽，一次改一批
    config.scroll_speed_factor = 1.2;
});
danmaku.clear();
let snapshot = danmaku.state();                  // 面板初始值：轨道、paint/text/placement/density/filter
```

- 弹幕**和视频在同一张纹理**里（erika 渲染时合成），所以出帧频率 = max(视频帧率, 弹幕平滑度)。
- 样式配置读 `DanmakuSnapshot` 的**类型化视图**（`paint`/`text`/`placement`/`density`/`filter`）当
  初始值，别用 `default()`，否则会冲掉用户上次的设置。

## 字幕

```rust
let subtitle = player.subtitle();
subtitle.set_style(SubtitleTextStyle { … });     // 外观
subtitle.set_placement(SubtitlePlacement { … }); // 版面
subtitle.set_delay(250_000);                     // 微秒
let snapshot = subtitle.state();
```

外挂字幕走轨道句柄：

```rust
player.tracks().add_external_subtitle(path, …);
player.tracks().select_subtitle(Some(track_id));
player.tracks().select_audio(Some(track_id));
player.tracks().remove_subtitle(track_id);
```

---

## 失败语义

**这个库只上报，不降级。** 出帧路径挂掉时（Linux 导不出 dma-buf、Windows 拿不到共享纹理、
宿主 device 不兼容…）：

1. 原因写进 `player.status().error`；
2. 之后**不再尝试**（一次失败就 latch，不会每帧重试刷屏）；
3. 上一张画面继续显示，音频继续由保活泵喂。

要不要换别的实现、要不要提示用户、要不要整页换掉，**是调用者的事** —— 库不知道宿主还有没有
别的播放器可用。所以给用户看的界面里最好把 `status.error` 显示出来（示例的状态行就是这么做的）。

`ErikaPlayer::new()` 失败（没有可用的 GPU 设备 / 引擎线程起不来）是**同步 `Err(String)`**，
那时压根别建控件。

---

## 生命周期与线程

| 档 | 内容 | 何时建 / 收 |
|---|---|---|
| **页面级（会话级）** | `ErikaPlayer` + 引擎线程 + GPU 设备 + 音频输出 + 解码线程 + libass | 你 `new()` 时建；**最后一个句柄 drop 时全收** |
| 媒体级 | demux / 解码 / preroll | `open()` 起；`stop()` 保持打开；换片重建 |
| 进程级 | 帧源注册表（空表时什么都不留） | 应用期 |

- **"最后一个句柄"包括控件持有的帧源**（`ErikaVideoSource`）。所以删掉控件 ≠ 引擎退出，
  句柄 `drop` 才是。
- 想在退出进程前**确定地**收干净：`player.shutdown()` —— 它会等引擎线程把 `PresenterRuntime`
  （GPU 设备 / 解码器 / libass / 字体库）析构完（最多 5 秒，超时打 warn 放弃）。不调也行，
  最后一个句柄 drop 时走同一套等待，只是时机不可控。
- `shutdown()` 之后这个玩家就废了（命令静默失败、`status` 报错），要再播只能重新 `new()`。
- 线程名：`erika-engine` / `erika-demux` / `erika-playback` / `erika-danmaku` / `erika-audio`。
  退出后 `ps -T` 一个都不剩，是最直接的验收。

页面级就够了：离开播放页 → `ErikaPlayer` 被 drop → 解码线程和 GPU 设备一起没。
只有当你要**离开播放页还继续出声出画**（后台播放 / 画中画 / 迷你播放器）时，才需要把
`ErikaPlayer` 抬到应用级。

---

## 平台差异（调用方需要知道的）

| | Linux | Windows |
|---|---|---|
| 出帧 | erika 导出线性 dma-buf，GDK 直接当纹理 | erika 发布 `MISC_SHARED` 快照，`OpenSharedResource` → D2D bitmap |
| 每帧额外成本 | 无（池化 3 张，隐式 fence） | 一次 `CopyResource` + 一次 GPU 同步等待 |
| 谁驱动出帧 | 宿主：GTK frame clock 每帧"要一帧" | 引擎：自己按 16ms 泵，有新画面再**叫醒**控件重绘 |
| 暂停时 | 不做任何事（没有 tick 就没有请求） | 同样：没有发布就没有重绘 |
| 后端 | `WgpuFallback`（Vulkan） | `PlatformNative`（D3D11） |
| 硬解 | VA-API 直通 | D3D11VA |

调用方看到的 API 两边完全一致，差异只在成本与内部机制上。

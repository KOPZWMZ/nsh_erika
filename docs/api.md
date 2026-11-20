# API 用法

库对外的全部东西就三块：**一个句柄**（`ErikaPlayer`）、**一块画面**（`erika_video()`）、
**两个附加句柄**（弹幕 / 字幕）。**原生控件**的 prop 表只有 `source`（帧源号）；
控制面有两条路：句柄方法（全能力），或者画面元素自己的 props（`src` / `playing` /
`volume` / `muted` / `rate`，引擎由组件建、也由组件销毁）。

谁负责关机只有一条规则：**引擎归组件**。组件创建或认领引擎，卸载即销毁 ——
页面拿到的句柄只有"使用权"（见「生命周期与线程」）。

```rust
use nsh_core::prelude::*;

let player = nsh_erika::ErikaPlayer::new()?;   // ① 建引擎（设备 + 音频，没有媒体）
player.open(nsh_erika::MediaRequest::new(uri)); // ② 打开片源（这才会起解码线程）
player.play();                                  // ③ 播

view()
    .flex_col()
    .flex_1()
    .child(nsh_erika::erika_video().bind_player(player.clone()).flex_1())  // ④ 画面（组件接管引擎）
    .child(/* 你自己的控制条，直接调 player */)
```

---

## 路径：从哪儿 import

根上只留"每个 app 都要手写"的几个名字，其余按**屏幕上的关注点**分模块（内部的 `core::`
永远私有，模块只是出口）。

**简化模式（只用 props）的全部出口在 `simple` 一个模块里**，根上的名字是给句柄 / 注册用的。

| 你要的东西 | 路径 |
|---|---|
| **简化模式的入口**（画面 / props） | `nsh_erika::{erika_video, ErikaVideo, ErikaVideoProps}` |
| **简化模式的数据结构**（`src` 载荷 / 命令 / 事件） | `nsh_erika::simple::*`（`MediaSource`、`CommandRequest`、`VideoCommand`、`VideoEvent`、`VideoState`、`VideoProgress`、`VideoTrack` …） |
| 播放器句柄 / 注册 / 控件名 / **画面（句柄路线）** | `nsh_erika::{ErikaPlayer, erika_video_for, register, COMPONENT_NAME, PlatformComponents}` |
| 打开媒体的请求、播放状态 | `nsh_erika::{MediaRequest, PlayerState}`（高频，特意留在根上） |
| 弹幕：句柄 + 快照 + 五组样式 | `nsh_erika::danmaku::*` |
| 字幕：句柄 + 快照 + 外观 / 版面 / 延迟 | `nsh_erika::subtitle::*` |
| 轨道：列表 / 选择 / 外挂字幕 | `nsh_erika::tracks::*` |
| 状态快照 `ErikaStatus` / `ErikaProgress` | `nsh_erika::state::*` |
| **句柄侧**事件 `ErikaEvent` + 三个 Listener | `nsh_erika::events::*` |
| 会话 / 帧（只有自写控件才用） | `nsh_erika::video::*` ← **需要 `escape-hatch` feature** |
| **其它一切上游类型** | `nsh_erika::erika::…`（全量兜底） |

⚠️ 上游类型**只从 `nsh_erika::erika` 拿**，别在 `Cargo.toml` 里再直接依赖 liberika：
git rev 对不齐就是两个 crate、两套类型，报错会变成 `expected erika::X, found erika::X`。

---

## 逃生舱（`escape-hatch` feature）

有一小批 API 是**给你绕开封装**用的，默认**不导出** —— 它们出现在 rustdoc 首页，会让新用户
以为"正常用法需要它们"。要用就开 feature：

```toml
# 应用自己的 Cargo.toml
nsh-erika = { path = "…", features = ["escape-hatch"] }
```

现在它解锁：

| 打开后多出什么 | 用来干什么 |
|---|---|
| `ErikaSession`（**根**上） | 自己建引擎、自己负责关机：`ErikaSession::create()` → `session.player()` 借一个**无票**句柄。集成测试 / 无头脚本 / 出帧自检用它 |
| `ErikaPlayer::view()` | 帧源本体（`ViewSource`）：`request_frame` + `frame_snapshot`，自写控件或出帧自检用 |
| `video::{FrameImage, FrameRequest}`（外加平台的 `DmabufFrame` / `SharedTexture`） | 上面那条路的载荷类型 |
| `ErikaPlayer::call()` / `call_with()` | 在引擎线程上跑 presenter 独有能力：超分 / HDR / 截图 / HUD，以及还没封装的上游 getter |
| `ErikaPlayer::raw()` | 裸 `erika::Player`。⚠️ 它的**写方法全部绕过 presenter 的音频协调**（实测：直连 `set_playback_rate` 画面变快声音不变）—— 只用来读 |
| `danmaku::ErikaDanmaku::update_config()` / `danmaku::DanmakuLayoutConfig` | 上游**整包**样式的写侧（24 字段）。未成组的字段（`scroll_duration_seconds` / `custom_font_face_index` …）只能走它；日常改样式用五组类型化视图。**读**整包也走逃生舱：`player.call_with(\|p\| p.danmaku_config().cloned())` —— 快照里不再放第二份 |

约定：**逃生舱里的东西不进日常路径**。关掉 feature 时它们不是"消失了"，而是"不推荐、
不承诺"：上游类型照样能从 `nsh_erika::erika::…` 摸到，我们自己的那几个（`ErikaSession`
等）才真的不导出。

对 99% 的应用来说，正确的做法一直是 `erika_video_for(player)`（= `erika_video().bind_player(player)`）
—— 那条路不需要这个 feature。

---

## 句柄

| 句柄（路径） | 拿法 | 能干什么 |
|---|---|---|
| `ErikaPlayer`（根） | `ErikaPlayer::new()` | 控制播放、读状态、派发弹幕/字幕/轨道 |
| `danmaku::ErikaDanmaku` | `player.danmaku()` | 弹幕开关、轨道、样式、偏移 |
| `subtitle::ErikaSubtitle` | `player.subtitle()` | 字幕样式、位置、延迟 |
| `tracks::ErikaTracks` | `player.tracks()` | 轨道列表与选择、外挂字幕 |
| `ErikaSession`（**逃**，根上） | `ErikaSession::create()` | 引擎的**唯一所有者**（不可克隆，`Drop` 即关机）。有画面时它归 `ErikaVideo`，正常用法拿不到 |

`ErikaPlayer` 很便宜可以随便 clone：`Clone` = 几次引用计数加一；`PartialEq` 比**号**
（号单调递增、永不复用），所以能直接塞 `signal()`（信号是 `Copy`，闭包捕获它就不用每个闭包
都写一次 `clone()`）。

⚠️ **句柄不是所有者**：把句柄交给组件后，关机权就在组件手里了。卸载之后再留着 clone 也没用 ——
命令静默失败、读侧冻结在最后一帧，`status().error` 是 `"engine thread is gone"`。

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
// ① 只用 props：组件自己建引擎、自己关机
nsh_erika::erika_video().src(uri).playing(true).volume(0.8)

// ② 要句柄控制：把句柄交出去，组件挂载时认领它
nsh_erika::erika_video_for(player.clone())   // = erika_video().bind_player(player.clone())
```

- `src` 接受 `&str` / `String` / `MediaRequest`：需要 HTTP 头（鉴权 / Cookie）或中途续播时，
  用 builder 构造好再传 —— `erika_video().src(MediaRequest::new(url).with_http_headers(headers))`。
- `bind_player` 的参数是 `impl Into<Prop<Option<ErikaPlayer>>>`：给 `player` 或 `player.clone()` 都行，
  **`&player` 不行**。
- 返回的是普通 NSH 元素，`.flex_1()` 之类的样式照常链式调用。
- 组件内部把句柄换成一个号、生成 `native("erika.video")` 元素、把号挂到 `source` prop 上。
  **手写 `native(...)` 走不通**：号是 `pub(crate)` 的，`erika_video()` 是唯一入口。

**props 的三种写法**（`src` / `bind_player` / `playing` / `muted` / `volume` / `rate` / `command`）：

| 写法 | 含义 |
|---|---|
| 固定值（`.playing(true)` / `.volume(0.8)`） | 初始配置：挂载时应用一次 |
| 闭包（`.playing(move \|\| want.get())`） | 控制端：读到变化就发一条命令 |
| 不写 | 不干预：用引擎默认值（`playing` 不写**不会自动播**） |

**画面比例**：控件按视频宽高比内接并居中，多出来的留黑（等价 `ContentFit::Contain`）。
画面尺寸由控件自己按「控件尺寸 × DPI」算，跟随窗口 resize 与 DPI 变化自动重挂出图面。

---

## 控制

```rust
player.open(MediaRequest::new(uri));  // 打开（异步：命令进队列，立刻返回）
player.play();                        // 播放
player.pause();                       // 暂停
player.set_playing(true);            // 按**目标值**设（幂等；切换请配自己的乐观 signal）
player.stop();                        // 位置归 0、画面清空，**媒体仍然开着**（能立刻再 play）
player.seek(Duration::from_secs(30)); // 跳转
player.set_volume(0.8);               // 0.0..=1.0（f32）
player.set_muted(true);               // 静音（**不改变** volume）
player.set_rate(1.5);                 // 倍速
```

- 这些方法**都是立即返回**的：命令进引擎的队列，引擎在自己的线程里处理。
- `open` 换片时旧画面会被作废（帧槽清空），控件下一帧自己撤掉上一张图。
- `stop()` 之后 `play()` 可以直接续上：媒体没关，不需要重新 `open`。
- **没有媒体时 `play` / `pause` / `seek` / `stop` 全是安全的 no-op**（不报错）：
  没有片源只是"还没有东西可做"，不是失败。`status.error` 只装真正的失败
  （`open` 失败、解码 / 音频错误、出帧路径不可用）—— 否则组件挂载时那条声明式意图
  会把真正的原因覆盖成一句 `no media is open`。

`MediaRequest` 是 erika 的类型（本库直接 re-export），字段：

| 字段 | 作用 |
|---|---|
| `uri` | 路径 / `http(s)://` / 其它 FFmpeg 认的 URL |
| `source_hint` | 来源提示（影响某些 demux 选择） |
| `http_headers` | HTTP 头（`Authorization`、Cookie…；`Debug` 刻意不打印它们） |
| `http_read_ahead_bytes` / `http_back_buffer_bytes` | HTTP 预读与回退缓存（`None` = 引擎默认） |
| `start_position` | 打开后立刻定位到哪（片头跳过、续播） |

---

## 命令（props 路线的"动作口"）

对外只暴露三样东西：**状态**（props）、**事件**（`on_*`）、**命令**（`command`）。
判据只有一条：**能回读的才配当状态**。

| 东西 | 归属 | 理由 |
|---|---|---|
| `src` / `playing` / `volume` / `muted` / `rate` | 状态 prop | 引擎持有，挂载/重开要能恢复 |
| `seek` / 停播 / 弹幕开关 / 加删弹幕轨 / 选轨 … | **命令** | 没有等价 prop，也没有"当前值"可读 |

规则：**有等价 prop 的动作一律不进命令** —— 播放/暂停用 `playing`，音量/静音/倍速用
`volume` / `muted` / `rate`。命令只留给"没有状态可表达"的动作。

```rust
use nsh_erika::simple::{CommandRequest, VideoCommand};

// 每次动作**新建**一个 request（它带自增 token，所以同样的动作再来一次也生效）
let command = signal(Option::<CommandRequest>::None);
button().on_press(move || {
    command.set(Some(CommandRequest::new(VideoCommand::Seek(Duration::from_secs(30)))))
});
erika_video().src(uri).command(move || command.get())
```

- **边沿触发**：`command` prop 变了才执行；重复设置同一个值不会再次触发 —— 要重复就再 `new` 一个。
- **挂载时执行一次**：组件重挂 = 新引擎，所以重放当前值通常是你要的。
- **没有返回值**：失败进 `status.error`（句柄路线）或 `VideoEvent::Error`。
- ⚠️ `SelectAudio` / `SelectSubtitle` 需要已有媒体；还没打开时会被忽略（打 debug 日志）。
  等 `StateChange` 到 `Ready` 之后再发。
- ⚠️ 别在组件 body 里现造 request：每次重渲染都会发一条。放在事件处理器里。

---

## 读状态

控制是"发命令"，读是"拉值"——**没有回调链**：`ErikaPlayer` 上那批只读方法全是直连
`erika::Player`，零延迟、不需要订阅。另有三条**订阅通道**（各是一对"轮询读 + 订阅"）：

| 通道 | 装什么 | 出口 | 语义 |
|---|---|---|---|
| 状态 | presenter 侧：`volume` / `muted` / 弹幕字幕配置 / `error` | `status()` / `subscribe_status()` | **命令侧快照**：只在命令处理完之后是新的 |
| 进度 | 位置 + 总时长（引擎算好） | `progress()` / `subscribe_progress()` | 10Hz，中间值可丢 |
| 事件 | erika 的离散事件：状态 / **播完** / 缓冲 / 轨道 / 选轨 / 错误 | `subscribe_events()` | **每条转发**，晚订阅看不到 |

三条规矩：

1. **要"值"就直读**——`is_playing()` / `state()` / `position()` / `duration()` / `tracks()`
   都是一次锁、永远新鲜，别等订阅；
2. **要"发生的那一刻"用事件**——播完切下一集、弹缓冲指示器、出错弹提示；
3. **事件不刷新状态**：收到事件后 `status()` 里那几项可能还是旧值，读直读方法。

**播完**有专门的事件：`events::ErikaEvent::Ended`（紧跟在 `StateChanged(Stopped)` 之后，
上游判好的"媒体自己放完了，不是 `stop()`"）。连播 / 结算用它，**别**拿
`position ≈ duration` 去猜（seek 到片尾、没有 duration 的源都会误判）。
要读 level 版用 `raw().is_stopped_at_end()`（逃生舱，默认关闭）。

| 方法 | 语义 | 来源 |
|---|---|---|
| `position()` | 当前位置（一次锁，永远新鲜） | 直连 `Player` |
| `duration()` | 总时长 | 直连 |
| `progress()` | `state::ErikaProgress`：位置 + 时长，**同一次采样** | 引擎 10Hz |
| `state()` | `PlayerState` 8 态（**已提交**，不是意图） | 直连 |
| `is_playing()` | 布尔版 | 直连 |
| `rate()` | 当前**生效**的倍速（变速提交后才变） | 直连 |
| `tracks()` / `chapters()` | 轨道列表 / 章节 | 直连 |
| `has_media()` / `generation()` | 有没有媒体 / 换片计数 | 直连 |
| `status()` | `state::ErikaStatus`：音量 / 静音 / 弹幕字幕配置 / `error` | 引擎（**命令侧快照**） |
| `volume()` / `is_muted()` | `status()` 里那两个字段的便捷读法（音量是 **f32**，比较用容差） | 同上 |
| `last_error()` | `status().error` 的快捷方式 | 同上 |

**UI 推荐做法**：低频界面 `set_interval(200ms)` 拉一次就够（按钮按下时再乐观写一遍）；
要"发生那一刻立刻反应"（播完切下一集、缓冲指示器、错误提示）就订阅事件，见上面三条通道。
示例见 [`examples/nsh_erika_demo`](../examples/nsh_erika_demo)。

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
// 整包样式（需要 escape-hatch）：未成组的字段只能这样改；日常用 set_paint/set_text/…
// danmaku.update_config(|config| { config.scroll_duration_seconds = 8.0; });
danmaku.clear();
let snapshot = danmaku.state();                  // 面板初始值：轨道、paint/text/placement/density/filter
```

- 弹幕**和视频在同一张纹理**里（erika 渲染时合成），所以出帧频率 = max(视频帧率, 弹幕平滑度)。
- 样式配置读 `danmaku::DanmakuSnapshot` 的**类型化视图**（`paint`/`text`/`placement`/`density`/
  `filter`）当初始值，别用 `default()`，否则会冲掉用户上次的设置。
- 类型都在 `nsh_erika::danmaku::` 下（`DanmakuPaint` / `DanmakuText` / `DanmakuPlacement` /
  `DanmakuDensity` / `DanmakuFilter` / `FilterRule` / `BlockedModes`）。

## 字幕

```rust
use nsh_erika::subtitle::{SubtitlePlacement, SubtitleTextStyle};

let subtitle = player.subtitle();
subtitle.set_style(SubtitleTextStyle { … });     // 外观
subtitle.set_placement(SubtitlePlacement { … }); // 版面
subtitle.set_delay(250_000);                     // 微秒
let snapshot = subtitle.state();                 // subtitle::SubtitleSnapshot
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

**一条规则：引擎归组件。** 组件创建或认领引擎，卸载即销毁；页面拿到的句柄只有使用权。

| 用法 | 引擎谁建 | 控制 | 关机 |
|---|---|---|---|
| `erika_video().src(uri)`（纯 props） | 组件（挂载时同步建） | 只能 props | 组件 |
| `ErikaPlayer::new()` + `erika_video_for(p)` | 页面（`new()` 那一刻） | 句柄全能力 | 组件（挂载时**认领票据**后） |
| `ErikaSession::create()` + `session.player()`（**逃生舱**） | 你自己 | 借用句柄 | 你自己（守卫 `Drop`） |

"认领"就是 `ErikaPlayer::new()` 在句柄里放的那张**所有权票据**：组件挂载时取走它、放进自己的
owner。所以：**卸载 = 关机**（摘表 + `engine.stop()`）；没人认领的票据随最后一根 clone drop；
同一个句柄挂两次（镜像）时第二次只按同一个号显示；卸载之后句柄的 clone 也不再能让引擎活过来。

| 档 | 内容 | 何时建 / 收 |
|---|---|---|
| 会话级 | 引擎线程 + GPU 设备 + 音频输出 + 解码线程 + libass | 组件挂载（或 `new()`）时建；**组件卸载**时收 |
| 媒体级 | demux / 解码 / preroll | `open()` 起；`stop()` 保持打开；换片重建 |
| 进程级 | 会话注册表（空表时什么都不留） | 应用期 |

- **没有 `shutdown()`**：关机只有一个入口（守卫 `Drop`）。`engine.stop()` 是**协作式取消**
  （不 join），所以"卸载返回"≠"GPU 设备已析构"；要确认真的干净就看线程数
  （示例首页那个读数就是这么用的）。
- 线程名：`erika-engine` / `erika-demux` / `erika-playback` / `erika-danmaku` / `erika-audio`。
- 想在**退出进程前**确定地收干净：先卸载画面，再确认线程都退了（`examples/drop_probe.rs`
  就是压这条的探针）。别在引擎还在析构时直接 `exit`。

页面级就够了：离开播放页 → 组件卸载 → 引擎跟着没。只有当你要**离开播放页还继续出声出画**
（后台播放 / 画中画 / 迷你播放器）时，才需要把画面元素和句柄抬到应用级常驻。

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

## 已知限制

| 限制 | 影响 | 备注 |
|---|---|---|
| Windows 每帧一次 `CopyResource` + 一次 GPU 同步等待 | 4K 高帧率时的成本大头 | 上游 legacy `MISC_SHARED` 句柄没有 fence/keystate 原语，生产者只能等完再交 |
| Windows 输出钉死 SDR | HDR 片源会被 tone-map | 想保留 HDR 要上游动渲染管线 |
| Windows 每帧新纹理 + 每帧 `OpenSharedResource` | 分配器压力 | 由上游池化解决 |
| Linux 需要两个**补丁前置**（wgpu-hal、libass） | 外部消费者必须自己备 | 见 [linux.md](linux.md)，一步脚本 |
| 弹幕与视频同在一张纹理 | 出帧频率 = max(视频帧率, 弹幕平滑度) | 弹幕流畅时视频会重复合成；分层导出要上游改动 |
| 4K 高帧率未实测 | — | 这是唯一可能改动路线的未知数 |

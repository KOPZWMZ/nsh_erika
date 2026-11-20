use nsh_core::prelude::*;

use crate::ErikaPlayer;
/// 画面的 props。
///
/// **引擎由组件创建**（一次性、同步），失败就渲染错误界面。
///
/// | prop | 方向 | 说明 |
/// |---|---|---|
/// | `src` | 进 | 片源；空串 = 没有媒体（黑屏，什么都不做）；变了才 `open` |
/// | `bind_player` | 进 | 句柄；挂载时认领所有权票据，外部照样可以自己 `open` / 控制 |
/// | `playing` | 进 | 播 / 暂停意图；**不写** = 不干预（**不会自动播**，交给句柄或调用方决定） |
/// | `muted` / `volume` / `rate` | 进 | 引擎设置；**不写** = 不干预（用引擎默认值） |
///
/// 三种写法都合法：**固定值**（= 初始配置）、**闭包**（`move || signal.get()`，= 控制端）、
/// 或者干脆不写（= 不干预）。固定值只在挂载时应用一次，之后不会再把它拉回来。
///
/// ```ignore
/// // 静态页：全是固定值
/// erika_video().src("/a.mkv").playing(true).volume(0.8)
/// // 控制页：绑信号
/// erika_video().src(move || uri.get()).playing(move || want.get())
/// ```
///
/// ⚠️ **不要把元素放进会重跑的区域**（`when` / `dynamic` / 列表）：区域重跑 = 组件重建 =
/// **新引擎**（从头开始、弹幕轨道全没）。这块画面要常驻，只换它的 props。
#[derive(Debug, Clone, PartialEq, Props)]
pub struct ErikaVideoProps {
    /// 句柄：组件挂载时认领它的所有权票据。不给 = 组件自己建引擎。
    #[prop(default)]
    pub bind_player: Prop<Option<ErikaPlayer>>,
    /// 片源。空串 = 没有媒体（引擎照样建，只是没东西可放）。
    #[prop(default)]
    pub src: Prop<String>,
    /// 播 / 暂停意图。不写 = 不干预（既不播也不暂停）。
    #[prop(default)]
    pub playing: Prop<Option<bool>>,
    /// 静音。不写 = 不干预。
    #[prop(default)]
    pub muted: Prop<Option<bool>>,
    /// 音量 0..1。不写 = 不干预（引擎默认 1.0，所以不会有"默认静音"那个坑）。
    #[prop(default)]
    pub volume: Prop<Option<f32>>,
    /// 倍速（> 0；非法值会被 `set_rate` 丢掉）。不写 = 不干预。
    #[prop(default)]
    pub rate: Prop<Option<f64>>,
}
impl ErikaVideoProps {
    /// 挂载时的**设置类**意图：必须排在片源 `open` 前面，否则首帧是满音量 / 默认样式。
    ///
    /// `playing` 不在这里处理：不写 = 不干预（**不会自动播**），要播请显式给 `playing`。
    fn init_settings(&self, player: &ErikaPlayer) {
        if let Some(volume) = self.volume.get_untracked() {
            player.set_volume(volume)
        }
        if let Some(muted) = self.muted.get_untracked() {
            player.set_muted(muted)
        }
        if let Some(rate) = self.rate.get_untracked() {
            player.set_rate(rate)
        }
    }
    /// 把 props 变成命令。**只调用一次**（每调一次就重新登记一遍 effect）。
    /// 顺序有意义：设置类 → 片源（effect 一登记就立刻跑）→ 播放意图。
    pub(crate) fn apply(self, player: &ErikaPlayer) {
        self.init_settings(player);
        // 片源：`effect` 是"立刻跑一次、之后读到变化再跑"，所以第一次运行就是挂载时打开 ——
        // body 里**不能**再单独 open 一次，否则同一份源会被打开两遍（解码器重建、
        // 换片路径还会清掉弹幕轨）。
        // 空串 = 没有媒体：不打开；从有到空也不动作（保持当前媒体）。
        effect({
            let p = player.clone();
            move || {
                let uri = self.src.get();
                if !uri.is_empty() {
                    p.open(crate::MediaRequest::new(uri));
                }
            }
        });
        effect({
            let p = player.clone();
            move || {
                if let Some(on) = self.playing.get() {
                    p.set_playing(on)
                }
            }
        });
        effect({
            let p = player.clone();
            move || {
                if let Some(muted) = self.muted.get() {
                    p.set_muted(muted)
                }
            }
        });
        effect({
            let p = player.clone();
            move || {
                if let Some(volume) = self.volume.get() {
                    p.set_volume(volume)
                }
            }
        });
        effect({
            let p = player.clone();
            move || {
                if let Some(rate) = self.rate.get() {
                    p.set_rate(rate)
                }
            }
        });
    }
}

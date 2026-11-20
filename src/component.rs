use nsh_core::JsonValue;

use nsh_core::prelude::*;

use crate::COMPONENT_NAME;
use crate::ErikaPlayer;
use crate::SessionOwner;
pub(crate) mod events;
pub use events::VideoEvent;
mod props;
pub use props::ErikaVideoProps;
/// 一块播放器的画面。**引擎的生死由它负责**。
///
/// ```ignore
/// // ① 只给 src：组件自己建引擎，控制全走 props
/// erika_video().src(uri).playing(true)
///
/// // ② 给一台外部引擎：两种写法等价（`erika_video_with` 更一眼看出"给的是引擎"）
/// let player = ErikaPlayer::new()?;                     // 引擎此刻就在
/// erika_video().src(uri).bind_player(player.clone())
/// erika_video_with(player)
/// ```
///
/// | 给了什么 | 引擎谁建 | 控制 | 关机 |
/// |---|---|---|---|
/// | 只有 `src` | 组件 | 只能 props | 组件 |
/// | `src` + `bind_player` | 页面（`new()` 那一刻） | 句柄全能力 | 组件（认领后） |
///
/// 卸载即销毁：组件没了，引擎就没（守卫那一份只在组件手里）。
#[component]
pub fn ErikaVideo(props: ErikaVideoProps, cx: Scope<VideoEvent>) -> impl IntoElement {
    let given = props.bind_player.get_untracked();

    let (owned, player) = match given {
        Some(handle) => match handle.adopt() {
            Some(session) => {
                let p = session.player();
                (Some(session), p)
            }
            None => {
                // 票据已被认领（镜像），或者上一次挂载已经把它用掉了
                if handle.is_engine_alive() {
                    log::debug!("erika.video: 句柄已被认领，只显示")
                } else {
                    log::error!(
                        "erika.video: 这个 ErikaPlayer 已被上一次挂载消费（元素不能放进条件区域）"
                    )
                }
                (None, handle)
            }
        },
        None => match SessionOwner::create() {
            Ok(session) => {
                let p = session.player();
                (Some(session), p)
            }
            Err(error) => {
                log::error!("erika.video: 引擎起不来: {error}");
                return view().child(text(error)).into_element();
            }
        },
    };
    let _keep = owned.map(stored);
    check_unbound(&player);
    events::wire(&player, cx);
    props.apply(&player);

    // 号在这个组件的整个生命里是**常量**（一个组件 = 一个引擎），所以直接写常量，
    // 不去读 `bind_player` 的 signal：那个节点可能比组件先被 dispose，那时读信号会 panic。
    // 「画面解绑」由元素销毁 + `Session::drop` 的 `unregister` 负责（C2 起守卫负责关机）。
    native(COMPONENT_NAME)
        .prop("source", JsonValue::from(player.session_id()))
        .into_element()
}
/// 用一台**外部引擎**做画面的简写：等价于 `erika_video().bind_player(player)`。
///
/// 和链式写法比，它的用处是让调用点一眼看出"这不是控制，而是把一台引擎交给画面"：
///
/// ```ignore
/// let player = ErikaPlayer::new()?;   // 引擎此刻就在，句柄全能力
/// view().child(erika_video_with(player).flex_1())
/// ```
///
/// 组件挂载时**认领**这张所有权票据，从此卸载即销毁；返回的元素和 `erika_video()`
/// 完全一样，样式照常链式调用。
pub fn erika_video_with(player: ErikaPlayer) -> ErikaVideoElement {
    erika_video().bind_player(player)
}

fn check_unbound(player: &ErikaPlayer) {
    let me = player.session_id();
    let orphans = crate::core::registry::unbound_except(me);
    if !orphans.is_empty() {
        log::info!(
            "erika.video: 本进程还有 {} 台引擎没有任何画面绑定：{orphans:?}（这台是 #{me}）。\
         如果你本来就要纯声明式的多路画面、或画面稍后才挂，忽略这条；\
         否则检查是不是 `ErikaPlayer::new()` 之后忘了 `.bind_player(handle)`。",
            orphans.len()
        );
    }
}

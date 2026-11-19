//! `erika.video`：只要画面、不带控制条的原生控件（Linux/GTK 版）。
//!
//! 这是 **Linux 适配层**：`gtk::Picture` + `FrameBinder`。Windows 的那份在
//! `src/windows/native_view.rs`（`draw(&ID2D1DeviceContext, size)`，已跑通）。
//! `lib.rs` 用 `backend` 别名按平台指过来，外面只看见一个 `ErikaView`。
//!
//! 它就是一个 `gtk::Picture`：帧由引擎线程导出成线性 dma-buf，tick 回调里导入成
//! `GdkDmabufTexture` 贴上去（那套在 [`crate::linux::frames`] 里）。
//!
//! ## 控件只有"画"
//!
//! 引擎归组件层（[`ErikaVideo`](crate::ErikaVideo)）：外部给了句柄就用外部那个，没给就自己
//! 异步建一个。控件拿到的是一个**号**，`ViewSource ::resolve` 之后只做两件事：
//! 每帧要帧、把帧贴到 `gtk::Picture` 上。没有号就只显示空白。
//!
//! ## prop
//!
//! | prop | 类型 | 含义 |
//! |---|---|---|
//! | `source` | number \| null | **要显示的帧源号**；`null` = "还没有号"，保持现状（不解绑、不报错） |
//!
//! 只有这一个 prop。它是"身份"不是"控制"：`paused` / `muted` / `volume` / `rate` 这些
//! prop 在组件层就被翻译成引擎命令，**根本到不了这里** —— 控件不碰播放语义。

use std::rc::Rc;

use gtk::prelude::*;
use nsh_core::JsonValue;
use nsh_platform_linux::{ComponentContext, ComponentFactory, NativeComponent, gtk4 as gtk};

use crate::core::source::ViewSource;
use crate::linux::frames::FrameBinder;

/// 注册用的工厂。
pub(crate) fn factory() -> ComponentFactory {
    Rc::new(|cx: ComponentContext| -> Box<dyn NativeComponent> { Box::new(ErikaView::new(cx)) })
}

/// erika 的画面控件：一块只负责显示的 `gtk::Picture`。
pub struct ErikaView {
    picture: gtk::Picture,
    frames: FrameBinder,
    /// "source 无效"只报一次。
    warned: bool,
}

impl ErikaView {
    /// 建一个还没有帧源的控件（`set_prop("source", …)` 之后才开始出画面）。
    pub fn new(_cx: ComponentContext) -> Self {
        let picture = gtk::Picture::new();
        // 画面按控件尺寸等比缩放，多出来的留黑，不裁切。
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        Self {
            frames: FrameBinder::new(&picture),
            picture,
            warned: false,
        }
    }
}

impl NativeComponent for ErikaView {
    fn widget(&self) -> gtk::Widget {
        self.picture.clone().upcast()
    }

    /// 平台层唯一认识的 prop：`source`（要显示的帧源号）。
    ///
    /// 控制类 prop（`paused` / `muted` / `volume` / `rate`）**不经过这里** —— 它们在组件层
    /// 就被翻译成引擎命令了。控件只回答"显示谁的帧"，别的什么都不管。
    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        match name {
            "source" => {
                // `null` = "还没有号"（引擎正在异步创建）：保持现状，不解绑也不报错。
                let Some(id) = value.and_then(JsonValue::as_u64) else {
                    return;
                };
                match ViewSource::resolve(id) {
                    Some(source) => {
                        self.warned = false;
                        self.frames.bind(source);
                    }
                    None => {
                        self.frames.unbind();
                        if !self.warned {
                            self.warned = true;
                            log::warn!(
                                "erika.video: source={id} 找不到帧源（句柄已经被 drop 了？）"
                            );
                        }
                    }
                }
            }
            other => log::warn!("erika.video: 没有名为 `{other}` 的 prop"),
        }
    }
}

//! `erika.video`：只要画面、不带控制条的原生控件。
//!
//! TODO(platform): 这个文件实际是 **Linux/GTK 适配层**（`gtk::Picture` + `FrameBinder`），
//! 现在放在 crate 根只是历史位置。跨平台前建议和 `linux/frames.rs` 一起挪进 `src/linux/`
//! （`component.rs` → `linux/component.rs`），`lib.rs` 里的 `cfg(target_os = "linux")` 不用变。
//!
//! 它就是一个 `gtk::Picture`：帧由引擎线程导出成线性 dma-buf，tick 回调里导入成
//! `GdkDmabufTexture` 贴上去（那套在 [`crate::linux::frames`] 里）。
//!
//! ## 控件不自建引擎
//!
//! 播放器和帧源都在**外面**创建（[`ErikaPlayer::new`](crate::ErikaPlayer::new)），控件
//! 只负责显示：页面里用 [`ErikaVideo`](crate::ErikaVideo)（或
//! [`ErikaPlayer::video`](crate::ErikaPlayer::video)）把帧源传进来，控制全在
//! [`ErikaPlayer`](crate::ErikaPlayer) 上。没拿到帧源就只显示空白。
//!
//! ## prop
//!
//! | prop | 类型 | 含义 |
//! |---|---|---|
//! | `source` | number \| null | **要显示的帧源号**（`ErikaVideoSource::id()`）；`null` = 解绑 |
//!
//! 只有这一个 prop，而且它是"身份"不是"控制"：播放、暂停、选轨、弹幕、字幕全在
//! [`ErikaPlayer`](crate::ErikaPlayer) 上，控件一概不碰。

use std::rc::Rc;

use gtk::prelude::*;
use nsh_core::JsonValue;
use nsh_platform_linux::{ComponentContext, ComponentFactory, NativeComponent, gtk4 as gtk};

use crate::ErikaVideoSource;
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

    /// 唯一的 prop：`source`（要显示的帧源号）。
    ///
    /// 控件**不自建引擎、也不控制播放** —— 播放器句柄在应用侧，这里只回答"显示谁"。
    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        match name {
            "source" => {
                let id = value.and_then(JsonValue::as_u64);
                match id.and_then(ErikaVideoSource::resolve) {
                    Some(source) => {
                        self.warned = false;
                        self.frames.bind(source);
                    }
                    None => {
                        self.frames.unbind();
                        if let Some(id) = id.filter(|_| !self.warned) {
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

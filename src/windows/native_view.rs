use std::rc::Rc;

use crate::ErikaVideoSource;
use nsh_core::JsonValue;
use nsh_platform_windows::{
    ComponentContext, ComponentFactory, NativeComponent,
    windows::Win32::Graphics::Direct2D::ID2D1DeviceContext,
};
/// 注册用的工厂。
pub(crate) fn factory() -> ComponentFactory {
    Rc::new(|cx: ComponentContext| -> Box<dyn NativeComponent> { Box::new(ErikaView::new(cx)) })
}
pub struct ErikaView {
    source: Option<ErikaVideoSource>,
    cx: ComponentContext,
    warned: bool,
}
impl ErikaView {
    pub fn new(cx: ComponentContext) -> Self {
        Self {
            source: None,
            cx,
            warned: false,
        }
    }
}
impl NativeComponent for ErikaView {
    fn draw(&self, _dc: &ID2D1DeviceContext, _size: (f32, f32)) {
        let _ = self.source;
    }
    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        if name != "source" {
            return;
        }
        let id = value.and_then(JsonValue::as_u64);
        match id.and_then(ErikaVideoSource::resolve) {
            Some(source) => {
                self.warned = false;
                self.source = Some(source);
                self.cx.redraw();
            }
            None => {
                self.source = None;
                if let Some(id) = id.filter(|_| !self.warned) {
                    self.warned = true;
                    log::warn!("erika.video: source={id} 找不到帧源（句柄已经被 drop 了？）");
                }
            }
        }
    }
}

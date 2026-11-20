//! `src` prop 的载荷：把上游的 [`MediaRequest`] 包一层。
//!
//! **为什么要包**：`Props` 的类型要求实现 `JsValue`，而 `JsValue` 是 nsh-core 的 trait、
//! `MediaRequest` 是上游类型 —— 孤儿规则不允许我们给它补 impl。包一层我们自己的类型，
//! 既能带上 HTTP 头 / 起始位置，又不用要求应用直接写 JSON。
//!
//! **为什么不是 `Option`**：`&str` / `String` / `MediaRequest` 都要能直接传给 `.src(..)`，
//! 而 `Prop<Option<T>>` 只认 `Option<T>` 本身。所以"没有媒体"用**空 uri** 表示
//! （[`MediaSource::is_empty`]），`Default` 就是那个空值。

use nsh_core::element::{JsHost, JsIn, JsOwned, JsValue, PropError, TsTypes};

use crate::MediaRequest;

/// 片源：一个 [`MediaRequest`]（uri + HTTP 头 + 起始位置）。
///
/// `.src(..)` 接受 `&str` / `String` / [`MediaRequest`] / `MediaSource` 四种写法；
/// 需要头或起始位置时用 [`MediaRequest`] 的 builder 构造好再传进来：
///
/// ```ignore
/// erika_video().src(MediaRequest::new(url).with_http_headers(headers))  // 续播 / 鉴权
/// erika_video().src("/a.mkv")                                          // 只要路径
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct MediaSource(MediaRequest);

impl MediaSource {
    /// 只有 uri 的片源。
    pub fn new(uri: impl Into<String>) -> Self {
        Self(MediaRequest::new(uri))
    }

    /// 借用里面的请求。
    pub fn request(&self) -> &MediaRequest {
        &self.0
    }

    /// 取出里面的请求（`open` 要一份新的）。
    pub fn into_request(self) -> MediaRequest {
        self.0
    }

    /// uri（可能是空串）。
    pub fn uri(&self) -> &str {
        &self.0.uri
    }

    /// 空 uri = 没有媒体（黑屏，什么都不做）。
    pub fn is_empty(&self) -> bool {
        self.0.uri.is_empty()
    }
}

impl Default for MediaSource {
    /// **没有媒体**（空 uri）。
    fn default() -> Self {
        Self(MediaRequest::new(String::new()))
    }
}

impl From<MediaRequest> for MediaSource {
    fn from(request: MediaRequest) -> Self {
        Self(request)
    }
}

impl From<&str> for MediaSource {
    fn from(uri: &str) -> Self {
        Self(MediaRequest::new(uri))
    }
}

impl From<String> for MediaSource {
    fn from(uri: String) -> Self {
        Self(MediaRequest::new(uri))
    }
}

impl JsValue for MediaSource {
    fn ts_type(_: &mut TsTypes) -> String {
        // JS 侧只能给一个 uri；头 / 起始位置属于 Rust API（和上游一样不给 JS）。
        "string".into()
    }
    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        String::from_js(value).map(Self::new)
    }
    fn to_js<'a>(&self, host: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        self.0.uri.to_js(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_no_media() {
        assert!(MediaSource::default().is_empty());
        assert!(!MediaSource::from("/a.mkv").is_empty());
    }

    #[test]
    fn keeps_the_request_intact() {
        let request = MediaRequest::new("/a.mkv")
            .with_start_position(Some(std::time::Duration::from_secs(90)));
        let source = MediaSource::from(request.clone());
        assert_eq!(source.request(), &request, "头 / 起始位置不能被丢掉");
        assert_eq!(source.into_request(), request);
    }
}

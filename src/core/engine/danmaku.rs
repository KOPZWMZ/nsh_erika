//! `DanmakuCmd`:弹幕开关 / 加轨 / 样式读改写 / 清空。
//!
//! 弹幕整块都住在 presenter 上,`erika::Player` 完全不知道
//! 它的存在 —— 所以这一族**没有"直连"这个选项**,只能走队列。
//!
//! 解析不用我们写:`DanmakuTimeline::parse_auto` 已经能吃 B 站 XML / JSON / JSONL,
//! 并且自带整份容错契约(缺正文跳过、非法字段回默认、id 顺序分配、u64 精确解析)。

use actor_lite::ActorContext;
use erika::danmaku::{DanmakuTimeline, DanmakuTrackSource};

use super::ErikaEngine;
use crate::core::cmd::DanmakuCmd;

impl ErikaEngine {
    /// 弹幕命令的总入口。
    ///
    /// 要 `ctx`:上游这几个都是 `bool` 返回(找不到轨道 = false),失败要走 `fail`
    /// (状态 + 事件一起写),不是只记一笔。
    pub(super) fn on_danmaku(&mut self, cmd: DanmakuCmd, ctx: &ActorContext<Self>) {
        match cmd {
            DanmakuCmd::Enabled(on) => self.presenter.set_danmaku_enabled(on),
            DanmakuCmd::AddTrack { uri, name } => self.add_danmaku_track(uri, name, ctx),
            DanmakuCmd::UpdateConfig(f) => {
                // 读改写：UI 不用持有整份 config，也就不会把别的字段冲掉
                let mut config = self.presenter.danmaku_config().cloned().unwrap_or_default();
                f(&mut config);
                self.presenter.set_danmaku_config(config);
            }
            DanmakuCmd::Clear => self.presenter.clear_danmaku(),
            // 上游这几个都返回 `bool`(找不到轨道 = false)。命令是单向的,所以失败只能
            // 通过 `fail` 告诉 UI(状态 + 事件),比静默 no-op 好排查。
            DanmakuCmd::RemoveTrack(track_id) => {
                if !self.presenter.remove_danmaku_track(track_id) {
                    self.fail(ctx, format!("移除弹幕轨失败:没有 id = {track_id} 的轨道"));
                }
            }
            DanmakuCmd::TrackEnabled { track_id, enabled } => {
                if !self.presenter.set_danmaku_track_enabled(track_id, enabled) {
                    self.fail(ctx, format!("切换弹幕轨失败:没有 id = {track_id} 的轨道"));
                }
            }
            DanmakuCmd::TrackOffset {
                track_id,
                offset_micros,
            } => {
                if !self
                    .presenter
                    .set_danmaku_track_offset(track_id, offset_micros)
                {
                    self.fail(
                        ctx,
                        format!("设置弹幕轨偏移失败:没有 id = {track_id} 的轨道"),
                    );
                }
            }
            DanmakuCmd::GlobalOffset(offset_micros) => {
                self.presenter.set_danmaku_global_offset(offset_micros)
            }
        }
    }

    /// 解析弹幕源(本地路径**或 URL**)并加成一条命名轨。
    ///
    /// 用 `parse_auto`(按内容嗅探)而不是 `from_file`(按扩展名分派):URL 没有扩展名,
    /// 而 `read_uri_to_end` 对文件和 http 是同一套接口 —— 两条路必须行为一致。
    ///
    /// 解析在引擎线程上做,所以大文件(十万条 B 站 XML,几百毫秒)不会卡 UI。
    fn add_danmaku_track(&mut self, uri: String, name: String, ctx: &ActorContext<Self>) {
        let parsed = erika::source::read_uri_to_end(&uri)
            .map_err(|error| format!("读取弹幕源失败: {error}"))
            .and_then(|bytes| {
                String::from_utf8(bytes).map_err(|error| format!("弹幕源不是 UTF-8: {error}"))
            })
            .and_then(|text| {
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                DanmakuTimeline::parse_auto(text).map_err(|error| format!("弹幕解析失败: {error}"))
            });
        match parsed {
            Ok(timeline) => {
                let source = if uri.starts_with("http://") || uri.starts_with("https://") {
                    DanmakuTrackSource::Remote(uri.clone())
                } else {
                    DanmakuTrackSource::File(std::path::PathBuf::from(&uri))
                };
                // 新轨道的 id 拿不到(命令是单向的)—— UI 从 `danmaku().tracks` 看到它出现。
                self.presenter.add_danmaku_track(timeline, name, source, 0);
            }
            Err(error) => {
                self.fail(ctx, error);
            }
        }
    }
}

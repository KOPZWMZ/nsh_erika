//! Windows 的帧载荷：erika 交出来的那张 **D3D11 共享纹理**。
//!
//! erika 的 d3d11 渲染器没有 `export_frame_texture`（只有 Metal / Linux Vulkan 有），
//! 所以 Windows 走它给 Flutter 准备的那条出口：attach 一次 `FlutterTexture` surface，
//! erika 每帧渲染完会 `CopyResource` 出一张 `D3D11_RESOURCE_MISC_SHARED` 快照，
//! 再把这个快照的 `IUnknown` 交给我们（[`SharedTexture`]）。
//!
//! 引用计数归我们：上游 `windows_flutter_texture_iunknown()` 里是 `into_raw()`，
//! 拿到的就是**已经 AddRef 过**的引用，最后一帧被丢掉时由 [`Drop`] 释放。
//!
//! 为什么存裸指针而不是 `windows::core::IUnknown`：windows-rs 的接口类型不是
//! `Send`/`Sync`，而帧要跨线程（引擎线程发布 → UI 线程画）。这里只做一件事：
//! 拿住一个引用，直到最后一帧被丢掉。

use std::ffi::c_void;

use nsh_platform_windows::windows::{
    Win32::Graphics::Dxgi::IDXGIResource, core::IUnknown, core::Interface,
};

/// 一帧 Windows 画面：erika 发布的 D3D11 共享纹理（持有 COM 引用）。
///
/// 拿到它以后唯一的用法是 [`SharedTexture::resource`] → `QI IDXGIResource` →
/// `GetSharedHandle()` → 在宿主的 D3D11 device 上 `OpenSharedResource` → 交给 D2D。
pub struct SharedTexture {
    /// AddRef 过的 `IUnknown`（erika 的 snapshot 纹理）。`Drop` 时 Release。
    raw: *mut c_void,
}

impl SharedTexture {
    /// 接管一个 **已经 AddRef 过**的 `IUnknown` 裸指针（不会再 AddRef）。
    ///
    /// # Safety
    ///
    /// `raw` 必须是调用方拥有的一次有效 COM 引用（上游 `into_raw()` 的产物），
    /// 且此后不再被别处释放。
    pub(crate) unsafe fn from_owned_raw(raw: *mut c_void) -> Self {
        Self { raw }
    }

    /// 裸指针，**只用来比"是不是同一张纹理"**（不要把它传出去）。
    ///
    /// 我们持有这个引用期间旧对象不会释放，所以它的地址不会被别的对象复用 ——
    /// 指针相等就是"同一张"，没有 ABA 问题（见 dev_doc §5.7.1）。
    pub fn raw(&self) -> *mut c_void {
        self.raw
    }

    /// `IDXGIResource`，用来取 legacy 共享句柄（`GetSharedHandle`）。
    ///
    /// 纹理不带 `_KEYEDMUTEX`（上游用 `wait_for_gpu` 保证交出来时已渲染完），
    /// 所以打开与读取之间不需要额外的同步协议。
    pub fn resource(&self) -> Option<IDXGIResource> {
        if self.raw.is_null() {
            return None;
        }
        // ManuallyDrop：这里只是**借**一个引用做 QI，不能替 self.raw 那份 Release。
        let borrowed = std::mem::ManuallyDrop::new(unsafe { IUnknown::from_raw(self.raw) });
        borrowed.cast::<IDXGIResource>().ok()
    }
}

impl Drop for SharedTexture {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: `raw` 是 from_owned_raw 接管的那次引用，只在这里释放一次。
            unsafe { drop(IUnknown::from_raw(self.raw)) };
        }
    }
}

// SAFETY: 只搬运一个 COM 引用计数指针；AddRef/Release 本身是线程安全的，
// 跨线程共享的也只是"哪张纹理"这个身份，纹理内容由 erika 的 GPU 同步保证。
unsafe impl Send for SharedTexture {}
unsafe impl Sync for SharedTexture {}

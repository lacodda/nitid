//! COM for the length of one call, undone afterwards if this call began it.
//!
//! The shell, the Task Scheduler and the print system are all reached through
//! COM, from whichever thread asked: the command line's, a worker of the
//! viewer's, or the window's own.

/// An apartment held for as long as this lives.
pub(crate) struct Com(bool);

impl Com {
    pub(crate) fn begin() -> Self {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        // SAFETY: balanced by `drop`, and only when it succeeded here; a
        // thread already in another apartment keeps it and is not undone.
        Self(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

/// The handle of a window, as the shell's and the print system's dialogs want
/// their owner.
pub(crate) fn hwnd_of(window: &winit::window::Window) -> anyhow::Result<windows::Win32::Foundation::HWND> {
    use anyhow::Context;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().context("the window has no handle")?.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut core::ffi::c_void)),
        _ => anyhow::bail!("the window is not a Windows window"),
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: matches the successful initialisation above.
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

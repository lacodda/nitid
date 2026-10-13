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

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: matches the successful initialisation above.
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

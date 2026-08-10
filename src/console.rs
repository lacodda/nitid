//! Keeping the console out of the way of the viewer.
//!
//! nitid is linked as a console application, which is what makes `install`,
//! `--version` and error messages work when it is run from a terminal: a GUI
//! subsystem binary starts with no standard handles, and Rust binds `println!`
//! to them before `main` runs, so attaching a console afterwards is too late —
//! output vanishes even when redirected to a file.
//!
//! The cost of a console binary is a console window when the shell launches
//! it by double-click. That is paid off here: the viewer hides the window it
//! was given, but only when the window it owns is the one the user asked for.

/// Hide the console window this process was given, if it owns one.
///
/// Called on the viewing path only. A process launched from a terminal shares
/// that terminal's window and must leave it alone — hiding it would take the
/// user's shell down with it, so ownership is checked first.
pub fn hide_if_ours() {
    #[cfg(windows)]
    windows_impl::hide_if_ours();
}

#[cfg(windows)]
mod windows_impl {
    use windows::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

    pub fn hide_if_ours() {
        // SAFETY: both calls are reads of process-global console state and
        // report absence through their return values.
        let window = unsafe { GetConsoleWindow() };
        if window.is_invalid() {
            return;
        }

        // A console created for this process alone lists exactly one process:
        // us. Anything more means we joined someone else's terminal.
        let mut processes = [0u32; 2];
        let count = unsafe { GetConsoleProcessList(&mut processes) };
        if count != 1 {
            return;
        }

        // SAFETY: the handle came from `GetConsoleWindow` and was checked.
        let _ = unsafe { ShowWindow(window, SW_HIDE) };
    }
}

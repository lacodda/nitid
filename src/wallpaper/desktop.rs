//! Putting a picture on the desktop, through the shell's own
//! `IDesktopWallpaper` — what the Settings app itself uses, so the picture
//! stays after a restart and the Settings app shows it as chosen.

use std::path::Path;

use anyhow::Result;

use super::Fit;

/// Put `path` on every display's desktop, fitted as asked.
pub fn show(path: &Path, fit: Fit) -> Result<()> {
    if let Some(recorded) = record(&format!("show {} {}", fit.key(), path.display())) {
        return recorded;
    }
    imp::show(path, fit)
}

/// Change how the picture already on the desktop meets the display.
pub fn set_fit(fit: Fit) -> Result<()> {
    if let Some(recorded) = record(&format!("fit {}", fit.key())) {
        return recorded;
    }
    imp::set_fit(fit)
}

/// What the desktop would have been told, written to a file instead, when
/// `NITID_DESKTOP_RECORD` names one.
///
/// How the whole way to the desktop is run on a machine whose desktop is not
/// the test's to change — the owner's, with a picture of their own choosing on
/// it. Everything before the last call is real: the folder, the deck, the
/// picture made for the desktop, the schedule. Nothing in normal use sets it,
/// the same as `NITID_SECOND_SCREEN_HERE` for the other display.
fn record(line: &str) -> Option<Result<()>> {
    use std::io::Write;

    let file = std::env::var_os("NITID_DESKTOP_RECORD")?;
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .and_then(|mut file| writeln!(file, "{line}"));
    Some(written.map_err(anyhow::Error::from))
}

/// The size of every display, in physical pixels. Empty when they cannot be
/// asked for, which the caller answers by keeping the picture's own size.
pub fn screens() -> Vec<(u32, u32)> {
    imp::screens()
}

#[cfg(windows)]
mod imp {
    use std::path::Path;

    use anyhow::{Context, Result};
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
    use windows::Win32::UI::Shell::{DESKTOP_WALLPAPER_POSITION, DWPOS_FILL, DWPOS_FIT, DesktopWallpaper, IDesktopWallpaper};
    use windows::core::{BOOL, HSTRING, PCWSTR};

    use super::Fit;
    use crate::wallpaper::Com;

    fn desktop() -> Result<IDesktopWallpaper> {
        // SAFETY: COM is initialised on this thread by the caller's `Com`.
        unsafe { CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL) }.context("reaching the desktop")
    }

    fn position(fit: Fit) -> DESKTOP_WALLPAPER_POSITION {
        match fit {
            Fit::Fill => DWPOS_FILL,
            Fit::Fit => DWPOS_FIT,
        }
    }

    pub fn show(path: &Path, fit: Fit) -> Result<()> {
        let _com = Com::begin();
        let desktop = desktop()?;
        // The fit first, so the picture never appears for a moment fitted the
        // way the previous one was.
        // SAFETY: a live interface, and a valid wide path for the call.
        unsafe { desktop.SetPosition(position(fit)) }.context("setting how the picture fits")?;
        // A null monitor is every monitor: one picture for all of them.
        unsafe { desktop.SetWallpaper(PCWSTR::null(), &HSTRING::from(path.as_os_str())) }.context("putting the picture on the desktop")
    }

    pub fn set_fit(fit: Fit) -> Result<()> {
        let _com = Com::begin();
        let desktop = desktop()?;
        // SAFETY: a live interface.
        unsafe { desktop.SetPosition(position(fit)) }.context("setting how the picture fits")
    }

    pub fn screens() -> Vec<(u32, u32)> {
        // Asked as a program that knows about scaling. A process that has not
        // said so — the command line, the scheduled task — is told the
        // scaled-down size of a scaled display, and a 4K television at 150%
        // would get a picture made for 2560 pixels and shown blurred.
        // SAFETY: changes this thread's awareness only, and it is put back.
        let previous = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

        let mut found: Vec<(u32, u32)> = Vec::new();
        unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, gathered: LPARAM) -> BOOL {
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: `gathered` is the vector passed below, alive for the
            // whole enumeration; `info` is sized as the call requires.
            unsafe {
                if GetMonitorInfoW(monitor, &mut info).as_bool() {
                    let rect = info.rcMonitor;
                    let found = &mut *(gathered.0 as *mut Vec<(u32, u32)>);
                    found.push(((rect.right - rect.left).max(0) as u32, (rect.bottom - rect.top).max(0) as u32));
                }
            }
            true.into()
        }
        // SAFETY: the callback only touches the vector, which outlives the call.
        let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut found as *mut _ as isize)) };

        if !previous.is_invalid() {
            // SAFETY: restoring what the thread had.
            unsafe { SetThreadDpiAwarenessContext(previous) };
        }
        found.retain(|&(width, height)| width > 0 && height > 0);
        found
    }
}

/// Elsewhere there is no desktop this knows how to reach.
#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    use anyhow::{Result, bail};

    use super::Fit;

    pub fn show(_path: &Path, _fit: Fit) -> Result<()> {
        bail!("the desktop's picture is set on Windows only")
    }

    pub fn set_fit(_fit: Fit) -> Result<()> {
        bail!("the desktop's picture is set on Windows only")
    }

    pub fn screens() -> Vec<(u32, u32)> {
        Vec::new()
    }
}

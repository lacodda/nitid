//! What Explorer has selected, in the window in front.
//!
//! Explorer answers this through the shell's own automation: every folder
//! window it has open is listed by `ShellWindows`, and each one hands out its
//! view of files (`IFolderView`), which knows the selection as shell items.
//! The desktop is a folder view of the same kind, found the same way. No
//! window is read from the outside — no list-view messages into another
//! process, no guessing from the screen.
//!
//! And Explorer is what starts the viewer for a look. The window in front is
//! the only one Windows lets come forward on its own say, and a program it
//! starts inherits that right — which is how a double-click's viewer gets in
//! front, and how it hands the right on to a window already open (ADR 0016).
//! A program started by the resident, which is never in front, would open
//! behind Explorer with its taskbar button flashing; measured, on a machine
//! whose foreground lock never times out, even the press-of-Alt trick winit
//! keeps for this cannot get past it. So the resident asks the window in front
//! to start the viewer, through the shell object behind its view
//! (`IShellDispatch2::ShellExecute`), and Explorer starts it as its own.
//!
//! The question is asked only when the keyboard is in the view of files
//! itself. A space typed while renaming a file, in the address bar or in the
//! search box is a space in a name, and Explorer's tree of folders on the left
//! is not a selection of pictures.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance, CoTaskMemFree, IServiceProvider};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{
    CSIDL_DESKTOP, IFolderView, IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellItemArray, IShellView, IShellWindows, IWebBrowserApp,
    SID_STopLevelBrowser, SIGDN_FILESYSPATH, SVGIO_BACKGROUND, SVGIO_SELECTION, SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellWindows,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GA_PARENT, GUITHREADINFO, GetAncestor, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId,
};
use windows::core::{BSTR, Interface, w};

/// Which of Explorer's windows is in front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Front {
    /// A folder window.
    Folder,
    /// The desktop, whose icons are a folder view too.
    Desktop,
}

/// Tell Explorer's windows from everyone else's by their class.
///
/// `CabinetWClass` is a folder window. The desktop is `Progman`, or one of the
/// `WorkerW` windows Windows puts in front of it while the wallpaper changes
/// or after `Win+D` — a desktop clicked on can be either.
pub fn front_of(class: &str) -> Option<Front> {
    match class {
        "CabinetWClass" => Some(Front::Folder),
        "Progman" | "WorkerW" => Some(Front::Desktop),
        _ => None,
    }
}

/// Whether the keyboard is in a view of files, from the class of the window
/// that has it outwards.
///
/// The files themselves are drawn by a `DirectUIHWND` in a folder window and
/// a `SysListView32` on the desktop, and both sit inside the shell's view
/// window, `SHELLDLL_DefView`. A rename is an `Edit` on top of them; the tree
/// of folders is a `SysTreeView32` outside the view; the address bar and the
/// search box are neither — so the focused window has to be the list *and*
/// be inside the view.
pub fn in_view_of_files(chain: &[String]) -> bool {
    let Some(focus) = chain.first() else {
        return false;
    };
    matches!(focus.as_str(), "DirectUIHWND" | "SysListView32") && chain.iter().skip(1).any(|class| class == "SHELLDLL_DefView")
}

/// What is selected in Explorer's window in front, and the view it is
/// selected in — which is how that window is asked to start the viewer.
pub struct Selection {
    pub paths: Vec<PathBuf>,
    view: IShellView,
}

/// The files selected in Explorer's window in front.
///
/// `None` when the window in front is not Explorer's, when the keyboard is
/// somewhere in it other than the files, or when nothing selected is a file
/// on disk — a library, a zip, "This PC". The order is the view's.
pub fn selected_in_front() -> Option<Selection> {
    let front = unsafe { GetForegroundWindow() };
    let kind = front_of(&class_of(front))?;
    if !in_view_of_files(&focus_chain(front)) {
        return None;
    }
    let view = match kind {
        Front::Folder => folder_view(front),
        Front::Desktop => desktop_view(),
    }?;
    let paths = selection(&files_of(&view)?);
    (!paths.is_empty()).then_some(Selection { paths, view })
}

impl Selection {
    /// Have the Explorer that owns this view start `exe` with `arguments`,
    /// as a double-click in it would.
    pub fn start(&self, exe: &Path, arguments: &str) -> Result<()> {
        start_from(&self.view, exe, arguments, SW_SHOWNORMAL.0)
    }
}

/// Have the Explorer behind `view` start a program, shown as `show` says.
fn start_from(view: &IShellView, exe: &Path, arguments: &str, show: i32) -> Result<()> {
    // Asked for as a dispatch object and only then for the dual interface:
    // asked for the dual interface directly, the view answers that it has
    // none (measured, `E_NOINTERFACE`).
    let background: IShellFolderViewDual = unsafe { view.GetItemObject::<windows::Win32::System::Com::IDispatch>(SVGIO_BACKGROUND) }
        .context("asking the view for its folder")?
        .cast()
        .context("the view's folder is not a shell folder view")?;
    let shell: IShellDispatch2 = unsafe { background.Application() }
        .context("asking the view for its shell")?
        .cast()
        .context("the view's shell cannot start programs")?;
    unsafe {
        shell.ShellExecute(
            &BSTR::from(exe.as_os_str().to_string_lossy().as_ref()),
            &VARIANT::from(BSTR::from(arguments)),
            &VARIANT::default(),
            &VARIANT::from(BSTR::from("open")),
            &VARIANT::from(show),
        )
    }
    .with_context(|| format!("asking Explorer to start {}", exe.display()))
}

/// A window's class name.
fn class_of(window: HWND) -> String {
    let mut name = [0u16; 128];
    let length = unsafe { GetClassNameW(window, &mut name) };
    String::from_utf16_lossy(&name[..length.max(0) as usize])
}

/// The classes from the window with the keyboard in `front`'s thread out to
/// the top, the focused one first.
fn focus_chain(front: HWND) -> Vec<String> {
    let thread = unsafe { GetWindowThreadProcessId(front, None) };
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetGUIThreadInfo(thread, &mut info) }.is_err() || info.hwndFocus.is_invalid() {
        return Vec::new();
    }
    let mut chain = Vec::new();
    let mut window = info.hwndFocus;
    // Bounded: a chain of windows is a handful deep, and a loop that trusted
    // another process's window tree to end would be a loop that might not.
    for _ in 0..32 {
        if window.is_invalid() {
            break;
        }
        chain.push(class_of(window));
        window = unsafe { GetAncestor(window, GA_PARENT) };
    }
    chain
}

/// The view of files of the folder window `front`, in the tab that is
/// showing.
///
/// Windows 11 puts several tabs in one folder window, and `ShellWindows` lists
/// each tab on its own with the same window. The tab showing is the first
/// `ShellTabWindowClass` child, the one on top; its browser is the one whose
/// window is that tab.
fn folder_view(front: HWND) -> Option<IShellView> {
    let windows: IShellWindows = unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER) }.ok()?;
    let tab = unsafe { FindWindowExW(Some(front), None, w!("ShellTabWindowClass"), None) }.ok();
    let count = unsafe { windows.Count() }.ok()?;
    for index in 0..count {
        let Ok(item) = (unsafe { windows.Item(&VARIANT::from(index)) }) else {
            continue;
        };
        let Ok(app) = item.cast::<IWebBrowserApp>() else {
            continue;
        };
        if unsafe { app.HWND() }.ok().map(|handle| handle.0) != Some(front.0 as isize) {
            continue;
        }
        let Some(browser) = browser_of(&app) else {
            continue;
        };
        if let Some(tab) = tab
            && unsafe { browser.GetWindow() }.ok() != Some(tab)
        {
            continue;
        }
        return view_of(&browser);
    }
    None
}

/// The desktop's view of files.
fn desktop_view() -> Option<IShellView> {
    let windows: IShellWindows = unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER) }.ok()?;
    let location = VARIANT::from(CSIDL_DESKTOP as i32);
    let root = VARIANT::default();
    let mut handle = 0i32;
    let dispatch = unsafe { windows.FindWindowSW(&location, &root, SWC_DESKTOP, &mut handle, SWFO_NEEDDISPATCH) }.ok()?;
    view_of(&browser_of(&dispatch)?)
}

/// The browser behind one of `ShellWindows`' entries.
fn browser_of(entry: &impl Interface) -> Option<IShellBrowser> {
    let provider: IServiceProvider = entry.cast().ok()?;
    unsafe { provider.QueryService(&SID_STopLevelBrowser) }.ok()
}

fn view_of(browser: &IShellBrowser) -> Option<IShellView> {
    unsafe { browser.QueryActiveShellView() }.ok()
}

/// The view as a list of files, which is what knows the selection.
fn files_of(view: &IShellView) -> Option<IFolderView> {
    view.cast().ok()
}

/// The selection, as the paths of the files behind it.
///
/// An item with no path on disk — a shortcut into a library, a file inside a
/// zip — is left out rather than refused with the rest.
fn selection(view: &IFolderView) -> Vec<PathBuf> {
    let Ok(items) = (unsafe { view.Items::<IShellItemArray>(SVGIO_SELECTION) }) else {
        return Vec::new();
    };
    let count = unsafe { items.GetCount() }.unwrap_or(0);
    (0..count)
        .filter_map(|index| {
            let item = unsafe { items.GetItemAt(index) }.ok()?;
            let name = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok()?;
            let path = unsafe { name.to_string() }.ok();
            // The shell allocated the name; the caller frees it.
            unsafe { CoTaskMemFree(Some(name.0 as *const _)) };
            path.map(PathBuf::from)
        })
        .collect()
}

/// The folder window showing `file`, its view, and where `file` is in it:
/// for the live test, which opens one and cannot rely on it being in front.
#[cfg(test)]
fn window_showing(file: &std::path::Path) -> Option<(IShellView, IWebBrowserApp, i32)> {
    use windows::Win32::UI::Shell::{SVGIO_ALLVIEW, SVGIO_FLAG_VIEWORDER};

    let windows: IShellWindows = unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER) }.ok()?;
    let count = unsafe { windows.Count() }.ok()?;
    for index in 0..count {
        let Ok(item) = (unsafe { windows.Item(&VARIANT::from(index)) }) else {
            continue;
        };
        let Some(shell_view) = browser_of(&item).as_ref().and_then(view_of) else {
            continue;
        };
        let Some(view) = files_of(&shell_view) else {
            continue;
        };
        // In the view's own order, so a position here is a position to
        // select by.
        let Ok(items) = (unsafe { view.Items::<IShellItemArray>(SVGIO_ALLVIEW | SVGIO_FLAG_VIEWORDER) }) else {
            continue;
        };
        let at = (0..unsafe { items.GetCount() }.unwrap_or(0)).find(|&at| {
            let Ok(shell_item) = (unsafe { items.GetItemAt(at) }) else {
                return false;
            };
            let Ok(name) = (unsafe { shell_item.GetDisplayName(SIGDN_FILESYSPATH) }) else {
                return false;
            };
            let path = unsafe { name.to_string() }.ok();
            unsafe { CoTaskMemFree(Some(name.0 as *const _)) };
            path.as_deref().map(std::path::Path::new) == Some(file)
        });
        if let (Some(at), Ok(app)) = (at, item.cast::<IWebBrowserApp>()) {
            return Some((shell_view, app, at as i32));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(classes: &[&str]) -> Vec<String> {
        classes.iter().map(|class| class.to_string()).collect()
    }

    /// The two places files are selected: Explorer's list and the desktop's.
    #[test]
    fn the_list_of_files_is_a_view_of_files() {
        let folder = chain(&[
            "DirectUIHWND",
            "SHELLDLL_DefView",
            "DUIViewWndClassName",
            "ShellTabWindowClass",
            "CabinetWClass",
        ]);
        assert!(in_view_of_files(&folder));
        let desktop = chain(&["SysListView32", "SHELLDLL_DefView", "Progman"]);
        assert!(in_view_of_files(&desktop));
    }

    /// A space typed into a name — a rename, the address bar, the search
    /// box — and the tree of folders beside the files are not the files.
    #[test]
    fn a_box_for_typing_and_the_tree_are_not() {
        let rename = chain(&["Edit", "DirectUIHWND", "SHELLDLL_DefView", "CabinetWClass"]);
        assert!(!in_view_of_files(&rename), "a space in a rename was taken for a look");
        let tree = chain(&["SysTreeView32", "NamespaceTreeControl", "CabinetWClass"]);
        assert!(!in_view_of_files(&tree));
        // The ribbon and the details pane are drawn by DirectUI as well, but
        // outside the view of files.
        let elsewhere = chain(&["DirectUIHWND", "CtrlNotifySink", "CabinetWClass"]);
        assert!(!in_view_of_files(&elsewhere));
        assert!(!in_view_of_files(&[]));
    }

    /// A folder window on a temporary folder, its view, and where `name` is
    /// in it.
    fn open_folder_window(dir: &std::path::Path, name: &str) -> (IShellView, IWebBrowserApp, i32) {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok().expect("entering COM");
        for each in ["a.png", "b.png"] {
            std::fs::write(dir.join(each), b"placeholder").expect("writing a temporary file");
        }
        // Waited for: this `explorer.exe` hands the folder to the Explorer
        // already running and exits, with a code that means nothing.
        let _ = std::process::Command::new("explorer.exe").arg(dir).status().expect("starting Explorer");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Some(found) = window_showing(&dir.join(name)) {
                return found;
            }
            assert!(std::time::Instant::now() < deadline, "no folder window came up showing the file");
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    fn select(view: &IShellView, at: i32) {
        use windows::Win32::UI::Shell::{SVSI_DESELECTOTHERS, SVSI_FOCUSED, SVSI_SELECT};
        let how = (SVSI_SELECT.0 | SVSI_DESELECTOTHERS.0 | SVSI_FOCUSED.0) as u32;
        unsafe { files_of(view).expect("a list of files").SelectItem(at, how) }.expect("selecting the file");
    }

    /// Explorer itself, asked: a folder window with a file selected says so
    /// through the shell's automation, with the file's path; and it starts a
    /// program when asked through that view, with its arguments whole — the
    /// way a look is started.
    ///
    /// The file is selected through the same automation rather than by
    /// `explorer /select`, which on Windows 11 opens the folder with the file
    /// focused but, measured, nothing selected.
    ///
    /// Run by hand (`cargo test explorer -- --ignored`) and in the live run:
    /// it opens a window on the screen.
    #[test]
    #[ignore = "opens an Explorer window on the screen"]
    fn explorer_says_what_it_has_selected_and_starts_what_it_is_asked_to() {
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

        let dir = tempfile::tempdir().expect("a temporary folder");
        let chosen = dir.path().join("b.png");
        let (view, window, at) = open_folder_window(dir.path(), "b.png");
        select(&view, at);
        let selected = selection(&files_of(&view).expect("a list of files"));

        // A program Explorer starts writes where it was told to: the path,
        // with its space, arrives as one argument.
        let written = dir.path().join("started by explorer.txt");
        let arguments = format!("/c echo started> \"{}\"", written.display());
        let started = start_from(&view, std::path::Path::new(r"C:\Windows\System32\cmd.exe"), &arguments, SW_HIDE.0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !written.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = unsafe { window.Quit() };

        assert_eq!(selected, vec![chosen]);
        started.expect("Explorer would not start a program");
        assert!(written.exists(), "Explorer said it started the program, and nothing ran");
    }

    /// The window in front, with the keyboard in its list, answers what the
    /// resident asks when the space bar goes down.
    ///
    /// Needs a desktop someone is at: the window is brought in front by a
    /// click on its title bar — measured, nothing short of input gets past
    /// the foreground lock — and a locked screen takes no input at all.
    #[test]
    #[ignore = "clicks an Explorer window on an unlocked screen"]
    fn the_window_in_front_answers_as_the_resident_asks() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let chosen = dir.path().join("b.png");
        let (view, window, at) = open_folder_window(dir.path(), "b.png");

        let opened = HWND(unsafe { window.HWND() }.map(|handle| handle.0).unwrap_or_default() as *mut _);
        click_title_of(opened);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while unsafe { GetForegroundWindow() } != opened && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        select(&view, at);
        let in_front = selected_in_front().map(|selection| selection.paths);
        let _ = unsafe { window.Quit() };

        assert_eq!(in_front, Some(vec![chosen]), "the window in front did not answer as the resident asks it");
    }

    /// Click the empty part of a window's title bar, left of its buttons,
    /// and put the pointer back where it was.
    fn click_title_of(window: HWND) {
        use windows::Win32::Foundation::{POINT, RECT};
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_MOUSE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT, SendInput,
        };
        use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect, SetCursorPos};

        let mut frame = RECT::default();
        let mut was = POINT::default();
        unsafe {
            let _ = GetWindowRect(window, &mut frame);
            let _ = GetCursorPos(&mut was);
        }
        let button = |flags: MOUSE_EVENT_FLAGS| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        };
        unsafe {
            let _ = SetCursorPos(frame.right - 220, frame.top + 12);
            SendInput(&[button(MOUSEEVENTF_LEFTDOWN), button(MOUSEEVENTF_LEFTUP)], std::mem::size_of::<INPUT>() as i32);
            std::thread::sleep(std::time::Duration::from_millis(300));
            let _ = SetCursorPos(was.x, was.y);
        }
    }

    #[test]
    fn explorers_windows_are_told_from_the_rest() {
        assert_eq!(front_of("CabinetWClass"), Some(Front::Folder));
        assert_eq!(front_of("Progman"), Some(Front::Desktop));
        assert_eq!(front_of("WorkerW"), Some(Front::Desktop));
        assert_eq!(front_of("Notepad"), None);
        // The viewer itself: a space there is the viewer's.
        assert_eq!(front_of("Window Class"), None);
    }
}

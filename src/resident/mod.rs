//! The part of nitid that stays behind once its windows close.
//!
//! A viewer is a process that ends with its window (ADR 0016), so anything
//! that has to answer while no picture is open needs a process of its own.
//! This is it. Started at sign-in by the entry `nitid install` writes, it
//! holds no window anyone sees, no graphics device and no picture — a few
//! megabytes — and does two things:
//!
//! - **Explorer's space bar.** Space on a picture selected in Explorer or on
//!   the desktop puts it on screen; Space again sends it back. `keys` decides
//!   which presses count, `explorer` reads what is selected, and Explorer
//!   itself starts the viewer for it, as it does for a double-click — the
//!   one way the viewer is let in front.
//! - **An icon in the notification area**, which is how someone who did not
//!   ask for anything running in the background sees that it is, and tells it
//!   to go.
//! - **Keeping the installed copy current**, on a thread of its own: it is the
//!   one part of nitid that is always there and has nothing on screen to
//!   interrupt, and the viewer stays a program that reaches no network. See
//!   `update`.
//!
//! See `docs/adr/0041-the-space-bar-in-explorer-is-heard-by-a-resident.md`.

mod explorer;
mod keys;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateMutexW, OpenProcess, PROCESS_SYNCHRONIZE, ReleaseMutex, WaitForSingleObject};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_NONE, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_BALLOONUSERCLICK, NIN_SELECT,
    NOTIFY_ICON_DATA_FLAGS, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, FindWindowW, GetForegroundWindow, GetMessageW,
    GetSystemMetrics, GetWindowThreadProcessId, HICON, IMAGE_ICON, IsWindow, LR_DEFAULTCOLOR, LoadImageW, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON, SW_SHOWNORMAL, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_NULL, WNDCLASSW,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::single::{self, Kind};

/// The message the notification-area icon sends its window.
const WM_TRAY: u32 = WM_APP + 2;

/// The updater's thread has something for a person to read.
const WM_UPDATE_SAID: u32 = WM_APP + 3;

/// The icon's number among this window's icons: it has one.
const TRAY_ID: u32 = 1;

/// A left click's event is `NIN_SELECT`; the keyboard's Enter or Space on the
/// icon is the same with `NINF_KEY` set, which the bindings do not name.
const NIN_KEYSELECT: u32 = NIN_SELECT | 0x1;

/// What the icon's menu offers.
const MENU_OPEN: usize = 1;
const MENU_QUIT: usize = 2;
const MENU_CHECK: usize = 3;

/// How to ask the updater's thread to ask now. Not there on a copy that is not
/// the installed one, which has nothing to update.
static CHECK: OnceLock<Sender<()>> = OnceLock::new();

/// What the updater's thread last had to say, for the window's thread to show.
static SAID: Mutex<Option<(String, String)>> = Mutex::new(None);

/// Where a click on the notification on screen leads, if anywhere.
static CLICK_LEADS_TO: Mutex<Option<String>> = Mutex::new(None);

/// The resident's window class, and its lock, for this user's session.
///
/// Both are per session already — a window class and a `Local\` name belong
/// to the desktop they were made on — so no user name is needed, only the
/// tests' own tag (see `single::instance_tag`).
///
/// The class never changes: it is how a new version finds an old version's
/// resident to tell it to go, and an update that cannot would be refused the
/// lock by the one it was replacing.
fn class_name() -> String {
    format!("lacodda.nitid.resident{}", single::instance_tag())
}

fn lock_name() -> String {
    format!(r"Local\lacodda.nitid.resident{}", single::instance_tag())
}

/// Be the resident, until told to go.
///
/// A second resident started while one is running — a sign-in entry and an
/// install racing, say — finds the lock taken and leaves at once, without an
/// error: what was asked for is already there.
pub fn run() -> Result<()> {
    let lock = unsafe { CreateMutexW(None, true, &HSTRING::from(lock_name())) }.context("taking the resident's lock")?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(lock);
        }
        return Ok(());
    }

    // The icon is drawn at the size the notification area asks for, and a
    // process that does not say it knows the display's scale is handed a
    // 16-pixel answer and stretched.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // Explorer's automation is COM, asked from this thread.
    let initialised = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();

    let result = serve();

    if initialised {
        unsafe { CoUninitialize() };
    }
    unsafe {
        let _ = ReleaseMutex(lock);
        let _ = CloseHandle(lock);
    }
    result
}

/// The window, the icon and the hook, and the loop that serves them.
fn serve() -> Result<()> {
    let instance = unsafe { GetModuleHandleW(None) }.context("finding the executable's module")?;
    let class = HSTRING::from(class_name());
    let registered = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        lpszClassName: PCWSTR(class.as_ptr()),
        ..Default::default()
    };
    if unsafe { RegisterClassW(&registered) } == 0 {
        bail!("registering the resident's window class");
    }

    // A window of its own, never shown, rather than a message-only one: the
    // notification area announces a restarted Explorer to top-level windows
    // only, and an icon that is not put back then is gone until sign-in.
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            &class,
            w!("nitid"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .context("creating the resident's window")?;

    add_icon(window);
    let listening = keys::listen(window);
    if let Err(error) = &listening {
        // A resident that cannot hear the keyboard still has its icon, which
        // is how this is said; the log line is for whoever runs it by hand.
        eprintln!("nitid: {error:#}");
    }

    keep_up_to_date(window);

    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    // The hook comes off before the process goes, rather than being left to
    // Windows: a hook whose thread has stopped answering slows every key on
    // the machine until it is noticed.
    drop(listening);
    Ok(())
}

/// Say what arrived, sweep up after it, and keep asking.
fn keep_up_to_date(window: HWND) {
    // Only the copy that is installed. A build run from anywhere else has
    // nothing to replace, no news meant for it, and replacing the installed
    // copy from under it is not what anybody running a build expected.
    if !crate::install::is_installed_copy() {
        return;
    }
    crate::update::tidy();
    if let Some(version) = crate::update::take_news() {
        *lock(&CLICK_LEADS_TO) = crate::update::notes(&version);
        notify(window, &format!("nitid updated to v{version}"), "Click to read what changed.");
    }

    let target = window.0 as isize;
    let asking = crate::update::watch(move |said| {
        if let Some(words) = said.words() {
            *lock(&SAID) = Some(words);
            // SAFETY: a window that has gone refuses the post, and the process
            // is on its way out with it.
            let _ = unsafe { PostMessageW(Some(HWND(target as *mut _)), WM_UPDATE_SAID, WPARAM(0), LPARAM(0)) };
        }
    });
    let _ = CHECK.set(asking);
}

/// A lock whose holder cannot have left anything half done: these only ever
/// hold one value, whole.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A notification from the icon.
fn notify(window: HWND, title: &str, text: &str) {
    let mut data = icon_data(window, NIF_INFO);
    data.dwInfoFlags = NIIF_NONE;
    let copy = |into: &mut [u16], from: &str| {
        let words: Vec<u16> = from.encode_utf16().take(into.len() - 1).collect();
        into[..words.len()].copy_from_slice(&words);
    };
    copy(&mut data.szInfoTitle, title);
    copy(&mut data.szInfo, text);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

/// What the notification area says when the pointer rests on the icon.
const TIP: &str = "nitid - Space in Explorer shows the picture";

/// The structure every call about the icon carries.
fn icon_data(window: HWND, flags: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: TRAY_ID,
        uFlags: flags,
        uCallbackMessage: WM_TRAY,
        ..Default::default()
    };
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    let tip: Vec<u16> = TIP.encode_utf16().take(data.szTip.len() - 1).collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    data
}

/// Put the icon in the notification area.
///
/// Best effort: the resident does its work without it, and a shell with no
/// notification area — a server core, a kiosk — is no reason to stop.
fn add_icon(window: HWND) {
    let mut data = icon_data(window, NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP);
    data.hIcon = mark();
    unsafe {
        if Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
            // Version 4 is what sends the menu key and a left click as their
            // own events, with the point they happened at.
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
        }
    }
}

fn remove_icon(window: HWND) {
    let data = icon_data(window, NOTIFY_ICON_DATA_FLAGS(0));
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

/// The mark, at the size of the notification area.
///
/// The executable's own icon resource — the one Explorer shows for it — so
/// the icon in the corner is the icon on the desktop. Resource 1 is where the
/// build puts it (`build.rs`).
fn mark() -> HICON {
    let Ok(instance) = (unsafe { GetModuleHandleW(None) }) else {
        return HICON::default();
    };
    let (width, height) = unsafe { (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) };
    unsafe { LoadImageW(Some(instance.into()), PCWSTR(1 as _), IMAGE_ICON, width, height, LR_DEFAULTCOLOR) }
        .map(|handle| HICON(handle.0))
        .unwrap_or_default()
}

/// The message Explorer sends every top-level window when it has started
/// again — after a crash, or a restart from the Task Manager — and the
/// notification area is new and empty.
fn taskbar_created() -> u32 {
    static MESSAGE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        keys::WM_LOOK => {
            look();
            LRESULT(0)
        }
        WM_TRAY => {
            // Version 4: the event in the low word of `lparam`, the point in
            // `wparam`.
            match (lparam.0 & 0xFFFF) as u32 {
                WM_CONTEXTMENU => menu(window, wparam),
                NIN_SELECT | NIN_KEYSELECT => open_viewer(),
                NIN_BALLOONUSERCLICK => {
                    if let Some(address) = lock(&CLICK_LEADS_TO).take() {
                        open_address(&address);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_UPDATE_SAID => {
            if let Some((title, text)) = lock(&SAID).take() {
                // A click on this one goes nowhere, whatever the last did.
                *lock(&CLICK_LEADS_TO) = None;
                notify(window, &title, &text);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(window);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            remove_icon(window);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ if message == taskbar_created() => {
            add_icon(window);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

/// The icon's menu, where it was asked for.
fn menu(window: HWND, at: WPARAM) {
    let point = POINT {
        x: (at.0 & 0xFFFF) as i16 as i32,
        y: ((at.0 >> 16) & 0xFFFF) as i16 as i32,
    };
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };
    unsafe {
        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("Open nitid"));
        if CHECK.get().is_some() {
            let check = HSTRING::from(format!("Check for updates - this is v{}", env!("CARGO_PKG_VERSION")));
            let _ = AppendMenuW(menu, MF_STRING, MENU_CHECK, &check);
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit - Space in Explorer stops showing pictures"));
        // A menu from the notification area closes when clicked away from
        // only if its window is in front while it is up — the documented
        // dance, and the reason for the `WM_NULL` after it.
        let _ = SetForegroundWindow(window);
        let chosen = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_RETURNCMD, point.x, point.y, Some(0), window, None);
        let _ = PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match chosen.0 as usize {
            MENU_OPEN => open_viewer(),
            MENU_CHECK => {
                if let Some(asking) = CHECK.get() {
                    let _ = asking.send(());
                }
            }
            MENU_QUIT => {
                let _ = DestroyWindow(window);
            }
            _ => {}
        }
    }
}

/// The space bar was pressed in a way that asks for a look: if Explorer is in
/// front with pictures selected, show them.
fn look() {
    let Some(selection) = explorer::selected_in_front() else {
        return;
    };
    let Some(wanted) = worth_a_look(selection.paths.clone()) else {
        return;
    };
    // Started by Explorer, the viewer is started by the window in front,
    // which is what lets it come forward — and, as a messenger, hand that on
    // to a window already open. See `explorer`.
    if let (Some(viewer), Some(arguments)) = (viewer(), peek_arguments(&wanted)) {
        match selection.start(&viewer, &arguments) {
            Ok(()) => return,
            Err(error) => eprintln!("nitid: {error:#}"),
        }
    }
    // Explorer would not start it, or the selection is too long for a
    // command line: shown all the same, if behind.
    hand_over(&wanted);
}

/// The longest command line a look is started with.
///
/// Windows takes 32,767 characters; a selection of thousands of pictures is
/// past that, and is handed over down the pipe instead.
const LONGEST_ARGUMENTS: usize = 30_000;

/// The arguments that start a look at `paths`, quoted for the command line —
/// or `None` when they would not fit on one.
fn peek_arguments(paths: &[PathBuf]) -> Option<String> {
    let mut arguments = crate::PEEK_ARGUMENT.to_string();
    for path in paths {
        arguments.push(' ');
        arguments.push_str(&quoted(&path.to_string_lossy()));
    }
    (arguments.len() <= LONGEST_ARGUMENTS).then_some(arguments)
}

/// One argument, quoted the way Windows splits a command line.
///
/// A path cannot hold a quote, but it can end in a backslash — a drive, `D:\`
/// — and a backslash before the closing quote would escape it, running the
/// argument into the next. Doubled there, it is a backslash again.
fn quoted(argument: &str) -> String {
    let trailing = argument.len() - argument.trim_end_matches('\\').len();
    format!("\"{argument}{}\"", "\\".repeat(trailing))
}

/// What of a selection a look shows.
///
/// One folder is a place to walk, as "Open with nitid" on it is. Otherwise
/// the pictures among what is selected: one opens its folder, several walk
/// themselves — what a double-click on them would do. A selection with no
/// picture in it is not a look, and Explorer keeps its space bar.
fn worth_a_look(selected: Vec<PathBuf>) -> Option<Vec<PathBuf>> {
    if let [one] = selected.as_slice()
        && one.is_dir()
    {
        return Some(selected);
    }
    let pictures: Vec<PathBuf> = selected
        .into_iter()
        .filter(|path| crate::image_source::is_supported(path) && path.is_file())
        .collect();
    (!pictures.is_empty()).then_some(pictures)
}

/// Show `paths` as a look: in the window that is open, or in one started for
/// it.
fn hand_over(paths: &[PathBuf]) {
    if single::channel::send(&single::pipe_name(), paths, Kind::Peek, single::HANDOVER_PATIENCE).is_ok() {
        return;
    }
    let Some(viewer) = viewer() else {
        return;
    };
    let mut command = std::process::Command::new(viewer);
    command.arg(crate::PEEK_ARGUMENT).args(paths);
    if let Err(error) = quiet(&mut command).spawn() {
        eprintln!("nitid: starting the viewer: {error}");
    }
}

/// Open the viewer on the folders it has been to, as its icon on the desktop
/// does.
fn open_viewer() {
    let Some(viewer) = viewer() else {
        return;
    };
    let _ = quiet(&mut std::process::Command::new(viewer)).spawn();
}

/// Open an address in whatever opens addresses here.
fn open_address(address: &str) {
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), &HSTRING::from(address), None, None, SW_SHOWNORMAL);
    }
}

/// The windowed binary beside this one: the resident is the windowed binary,
/// so it is this one.
fn viewer() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// A child with nothing of this process's own streams.
fn quiet(command: &mut std::process::Command) -> &mut std::process::Command {
    use std::process::Stdio;
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
}

/// Start the resident from the installed copy in `dir`, detached from this
/// process.
pub fn start(dir: &Path) -> Result<()> {
    detached(&dir.join("nitidw.exe"), &[crate::RESIDENT_ARGUMENT])
}

/// Start a windowed executable that will outlive this process, owing it
/// nothing.
///
/// `nitid install` and `nitid update` run in a terminal, a script or a CI step,
/// and whatever reads their output waits for every holder of that output to let
/// go, so a child meant to run until sign-out must hold none of it. Clearing the
/// inherit flag on the three standard handles is not enough: a process started
/// with redirected output carries that pipe under a second handle it has no name
/// for, inherited from its own parent, and `std::process::Command` always asks
/// Windows to pass on everything inheritable. So this asks Windows to pass on
/// nothing.
///
/// It starts in the executable's own directory rather than the caller's, which
/// it would otherwise keep from being deleted or renamed for as long as it runs.
pub fn detached(exe: &Path, arguments: &[&str]) -> Result<()> {
    use windows::Win32::System::Threading::{CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW};
    use windows::core::PWSTR;

    let mut line = quoted(&exe.to_string_lossy());
    for argument in arguments {
        line.push(' ');
        line.push_str(&quoted(argument));
    }
    let mut line: Vec<u16> = line.encode_utf16().chain(std::iter::once(0)).collect();
    let directory = exe.parent().context("an executable with no directory")?;
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    // SAFETY: the command line is a writable, terminated buffer as the call
    // requires, and both handles it returns are closed here.
    unsafe {
        CreateProcessW(
            &HSTRING::from(exe.as_os_str()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(0),
            None,
            &HSTRING::from(directory.as_os_str()),
            &startup,
            &mut process,
        )
    }
    .with_context(|| format!("starting {}", exe.display()))?;
    unsafe {
        let _ = CloseHandle(process.hProcess);
        let _ = CloseHandle(process.hThread);
    }
    Ok(())
}

/// Tell a running resident to go, and wait until it has.
///
/// `Ok(false)` when none is running. Waited for because the caller is about
/// to replace or remove the executable it runs from, or to start a new one
/// that would otherwise find the old one's lock still held and leave.
pub fn stop() -> Result<bool> {
    let class = HSTRING::from(class_name());
    let Ok(window) = (unsafe { FindWindowW(&class, None) }) else {
        return Ok(false);
    };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.ok();

    unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) }.context("telling the resident to go")?;

    let Some(process) = process else {
        // Told, but not watchable: the best that can be said is that the
        // message was delivered.
        return Ok(true);
    };
    let waited = unsafe { WaitForSingleObject(process, 5000) };
    unsafe {
        let _ = CloseHandle(process);
    }
    if waited != WAIT_OBJECT_0 {
        bail!("the resident was told to go and is still running");
    }
    Ok(true)
}

/// Whether a resident is running for this session.
pub fn running() -> bool {
    unsafe { FindWindowW(&HSTRING::from(class_name()), None) }.is_ok()
}

/// The window in front, as a number the viewer can hold on to.
///
/// What a look from Explorer gives the foreground back to once the space bar
/// ends it.
pub fn foreground() -> isize {
    unsafe { GetForegroundWindow() }.0 as isize
}

/// Bring `window` to the front, if it is still a window.
///
/// The viewer is in front when it asks — the person just pressed a key in
/// it — and a window in front may hand the foreground to another.
pub fn give_foreground(window: isize) {
    let handle = HWND(window as *mut _);
    if window != 0 && unsafe { IsWindow(Some(handle)) }.as_bool() {
        unsafe {
            let _ = SetForegroundWindow(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path with a space is one argument, and a drive's root keeps its
    /// backslash without swallowing the quote that closes it.
    #[test]
    fn a_look_is_started_with_its_paths_quoted() {
        assert_eq!(quoted(r"C:\My Photos\a.png"), r#""C:\My Photos\a.png""#);
        assert_eq!(quoted(r"D:\"), r#""D:\\""#);
        let arguments = peek_arguments(&[PathBuf::from(r"C:\a b\x.png"), PathBuf::from(r"D:\")]).expect("two paths fit");
        assert_eq!(arguments, r#"--peek "C:\a b\x.png" "D:\\""#);
        // And Windows splits them back into exactly those paths.
        let crate::Command::Peek(paths) = crate::parse(split(&arguments).into_iter().map(std::ffi::OsString::from)) else {
            panic!("the arguments did not read back as a look");
        };
        assert_eq!(paths, vec![PathBuf::from(r"C:\a b\x.png"), PathBuf::from(r"D:\")]);
    }

    /// Thousands of pictures do not fit on a command line, and are handed
    /// over another way rather than cut short.
    #[test]
    fn a_selection_too_long_for_a_command_line_is_not_started_with_one() {
        let many: Vec<PathBuf> = (0..2000)
            .map(|index| PathBuf::from(format!(r"C:\Pictures\Holiday 2026\IMG_{index:05}.jpg")))
            .collect();
        assert_eq!(peek_arguments(&many), None);
    }

    /// Split a command line the way Windows does, by asking Windows.
    fn split(line: &str) -> Vec<String> {
        use windows::Win32::UI::Shell::CommandLineToArgvW;
        let wide = HSTRING::from(format!("nitidw.exe {line}"));
        let mut count = 0;
        let argv = unsafe { CommandLineToArgvW(&wide, &mut count) };
        let words = (1..count as usize)
            .map(|at| unsafe { (*argv.add(at)).to_string() }.expect("an argument"))
            .collect();
        unsafe {
            let _ = windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(argv as *mut _)));
        }
        words
    }

    /// A folder selected on its own is walked; with other things, it is
    /// left out like any other thing that is not a picture.
    #[test]
    fn a_look_shows_the_pictures_of_a_selection() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let picture = dir.path().join("a.png");
        let notes = dir.path().join("notes.txt");
        let inner = dir.path().join("shoot");
        std::fs::write(&picture, b"placeholder").unwrap();
        std::fs::write(&notes, b"placeholder").unwrap();
        std::fs::create_dir(&inner).unwrap();

        assert_eq!(worth_a_look(vec![picture.clone()]), Some(vec![picture.clone()]));
        assert_eq!(
            worth_a_look(vec![inner.clone()]),
            Some(vec![inner.clone()]),
            "a folder on its own was not walked"
        );
        assert_eq!(worth_a_look(vec![notes.clone(), picture.clone(), inner.clone()]), Some(vec![picture.clone()]));
        assert_eq!(worth_a_look(vec![notes.clone()]), None, "a look at a text file opened the viewer");
        assert_eq!(worth_a_look(Vec::new()), None);
    }
}

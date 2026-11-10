//! The taskbar: which button nitid's windows are, and what a right click on it
//! offers.
//!
//! **One name.** Windows groups windows on the taskbar by an application id,
//! and works one out from the executable's path when a program names none.
//! nitid is two executables (ADR 0004) — a window opened from a terminal is
//! `nitid.exe`'s, one opened from Explorer is `nitidw.exe`'s — so without a name
//! of its own the same viewer would be two buttons, and a Jump List written for
//! one would be missing from the other. Every viewer names itself `APP_ID`
//! before its first window, and the desktop shortcut carries the same name, so
//! a pin made from either is the one button.
//!
//! **The Jump List is the start screen's list.** Right-click the button and the
//! folders the viewer has been to are there, newest first, each opening on the
//! picture last looked at in it (`recent`). The same list, not a second one:
//! a folder the start screen forgets is gone from the Jump List at the next
//! write, and an entry a person removes from the Jump List is forgotten by the
//! start screen too.
//!
//! **Written off the event loop**, by a thread of its own: the shell's list is
//! COM, loads part of Explorer into the process and costs milliseconds — fine
//! behind a picture already on screen, not in front of one.
//!
//! `NITID_JUMP_LIST_RECORD=<file>` writes what would go to the shell into the
//! file instead, the way `NITID_DESKTOP_RECORD` does for the desktop: every
//! gate that starts a viewer sets it (`tests/common`), so no test reaches the
//! Jump List of whoever runs it. The shell's half has a test of its own, under
//! an id of its own (see the tests below).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use anyhow::{Context, Result};
use windows::Win32::Foundation::E_ACCESSDENIED;
use windows::Win32::Storage::EnhancedStorage::{PKEY_AppUserModel_ID, PKEY_Title};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW, SHGSI_ICONLOCATION, SHGetStockIconInfo, SHSTOCKICONINFO, SHStrDupW,
    SIID_FOLDER, SetCurrentProcessExplicitAppUserModelID, ShellLink,
};
use windows::core::{HSTRING, Interface};

use crate::recent::Entry;

/// The name every nitid window goes by on the taskbar.
///
/// Never changes: a pin, a shortcut and a Jump List are all filed under it,
/// and a new name would orphan every one of them on every machine.
pub const APP_ID: &str = "lacodda.nitid";

/// What the Jump List calls the folders.
const CATEGORY: &str = "Recent";

/// Name this process `APP_ID`, before it opens a window.
///
/// Silent on failure: a viewer that groups under the name Windows makes up is
/// still a viewer.
pub fn claim_name() {
    // SAFETY: a string the call copies; no other state.
    let _ = unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_ID)) };
}

/// Give a shortcut `APP_ID`, so a pin made from it is the viewer's button.
pub(crate) fn name_link(link: &IShellLinkW) -> Result<()> {
    let store: IPropertyStore = link.cast().context("the shortcut has no properties")?;
    // SAFETY: the value outlives the call, and the store copies it.
    unsafe { store.SetValue(&PKEY_AppUserModel_ID, &text(APP_ID)?) }.context("naming the shortcut")?;
    unsafe { store.Commit() }.context("naming the shortcut")?;
    Ok(())
}

/// One line of the Jump List.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The folder's name, as the start screen's tile has it.
    pub title: String,
    /// Where it is, and which picture it opens on.
    pub tooltip: String,
    /// What it opens.
    pub picture: PathBuf,
}

/// The lines for `entries`: the folders that are there to be opened, newest
/// first, as many as the list has room for.
///
/// A folder on a drive that is not plugged in is left off, as the start
/// screen leaves it off: a line that opens nothing is worse than no line.
/// `there` says whether a folder is; it is the disk, outside the tests.
pub fn items(entries: &[Entry], room: usize, there: impl Fn(&Path) -> bool) -> Vec<Item> {
    entries
        .iter()
        .filter(|entry| entry.folder().is_some_and(&there))
        .take(room)
        .map(|entry| Item {
            title: entry.title(),
            tooltip: format!(
                "{} - {}",
                entry.folder().map(Path::display).map(|folder| folder.to_string()).unwrap_or_default(),
                entry.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
            ),
            picture: entry.path.clone(),
        })
        .collect()
}

/// What a line passes the viewer: the picture's path, quoted.
///
/// A Windows path cannot hold a quote, so quoting it is the whole of the
/// escaping it needs.
pub fn argument(picture: &Path) -> String {
    format!("\"{}\"", picture.display())
}

/// The picture a line opens, read back from what it passes — for the lines a
/// person removed, which come back from the shell as lines.
pub fn picture_of(arguments: &str) -> Option<PathBuf> {
    let inner = arguments.trim().strip_prefix('"')?.strip_suffix('"')?;
    (!inner.is_empty() && !inner.contains('"')).then(|| PathBuf::from(inner))
}

/// Where the Jump List is written: the shell, or a file standing in for it.
enum Shell {
    Real { app_id: String, viewer: PathBuf },
    Record(PathBuf),
}

impl Shell {
    fn here() -> Option<Self> {
        if let Some(file) = std::env::var_os("NITID_JUMP_LIST_RECORD") {
            return Some(Shell::Record(PathBuf::from(file)));
        }
        Some(Shell::Real {
            app_id: APP_ID.to_string(),
            viewer: viewer()?,
        })
    }

    /// Put `entries` on the list, returning the pictures of the lines the
    /// person removed since it was last written.
    fn write(&self, entries: &[Entry]) -> Result<Vec<PathBuf>> {
        match self {
            Shell::Real { app_id, viewer } => commit(app_id, viewer, entries),
            Shell::Record(file) => record(file, entries),
        }
    }
}

/// The windowed executable beside this one: what a line starts.
///
/// `nitidw.exe` whichever binary is running, so a line never flashes a console
/// (ADR 0004). `None` when it is not there, and then no list is written — a
/// line that starts nothing is a broken promise.
fn viewer() -> Option<PathBuf> {
    let mut exe = std::env::current_exe().ok()?;
    exe.set_file_name("nitidw.exe");
    exe.is_file().then_some(exe)
}

/// Write the list to the shell.
fn commit(app_id: &str, viewer: &Path, entries: &[Entry]) -> Result<Vec<PathBuf>> {
    let _com = crate::com::Com::begin();
    // SAFETY: COM calls on objects created here, on this thread's apartment.
    unsafe {
        let list: ICustomDestinationList = CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER).context("reaching the Jump List")?;
        list.SetAppID(&HSTRING::from(app_id)).context("naming the Jump List")?;
        let mut room = 0u32;
        let removed: IObjectArray = list.BeginList(&mut room).context("starting the Jump List")?;
        let removed = removed_pictures(&removed);

        // A line the person took off may not go back on: the shell refuses the
        // whole category if it does. The folder is forgotten instead, by the
        // caller, the way the start screen's Delete forgets it.
        let kept: Vec<Entry> = entries
            .iter()
            .filter(|entry| !removed.iter().any(|gone| same_folder(gone, &entry.path)))
            .cloned()
            .collect();
        let lines = items(&kept, room as usize, Path::is_dir);

        if !lines.is_empty() {
            let collection: IObjectCollection = CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER).context("making the list's lines")?;
            for line in &lines {
                collection.AddObject(&link(viewer, line)?).context("adding a line")?;
            }
            let array: IObjectArray = collection.cast().context("making the list's lines")?;
            match list.AppendCategory(&HSTRING::from(CATEGORY), &array) {
                Ok(()) => {}
                // The person told Windows to keep no recent items (Settings >
                // Personalisation > Start). The list goes out without them,
                // which is what they asked for.
                Err(error) if error.code() == E_ACCESSDENIED => {}
                Err(error) => {
                    let _ = list.AbortList();
                    return Err(error).context("writing the folders to the Jump List");
                }
            }
        }
        list.CommitList().context("committing the Jump List")?;
        Ok(removed)
    }
}

/// The pictures of the lines in `removed`.
fn removed_pictures(removed: &IObjectArray) -> Vec<PathBuf> {
    // SAFETY: reads of an array the shell handed over, on its thread.
    let count = unsafe { removed.GetCount() }.unwrap_or(0);
    (0..count)
        .filter_map(|index| {
            let link: IShellLinkW = unsafe { removed.GetAt(index) }.ok()?;
            let mut buffer = [0u16; 1024];
            unsafe { link.GetArguments(&mut buffer) }.ok()?;
            let length = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
            picture_of(&String::from_utf16_lossy(&buffer[..length]))
        })
        .collect()
}

/// One line, as the shell takes it: a shortcut to the viewer with the picture.
fn link(viewer: &Path, line: &Item) -> Result<IShellLinkW> {
    // SAFETY: COM calls on an object created here.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).context("making a line")?;
        link.SetPath(&HSTRING::from(viewer.as_os_str())).context("pointing a line at the viewer")?;
        link.SetArguments(&HSTRING::from(argument(&line.picture)))
            .context("giving a line its picture")?;
        link.SetDescription(&HSTRING::from(line.tooltip.as_str())).context("describing a line")?;
        if let Some((file, index)) = folder_icon() {
            link.SetIconLocation(&HSTRING::from(file), index).context("giving a line its icon")?;
        }
        let store: IPropertyStore = link.cast().context("a line has no properties")?;
        store.SetValue(&PKEY_Title, &text(&line.title)?).context("naming a line")?;
        store.Commit().context("naming a line")?;
        Ok(link)
    }
}

/// Where the shell keeps the folder icon: every line is a folder, and says so.
fn folder_icon() -> Option<(String, i32)> {
    let mut info = SHSTOCKICONINFO {
        cbSize: std::mem::size_of::<SHSTOCKICONINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: the struct is sized as the call expects.
    unsafe { SHGetStockIconInfo(SIID_FOLDER, SHGSI_ICONLOCATION, &mut info) }.ok()?;
    let length = info.szPath.iter().position(|unit| *unit == 0).unwrap_or(info.szPath.len());
    (length > 0).then(|| (String::from_utf16_lossy(&info.szPath[..length]), info.iIcon))
}

/// `value` as the shell reads a text property: a `VT_LPWSTR`.
///
/// `PROPVARIANT`'s own `From<&str>` makes a `BSTR`, which a title is not read
/// as. The copy is the shell's allocator's, and the value's `Drop` frees it.
fn text(value: &str) -> Result<PROPVARIANT> {
    // SAFETY: the copy is owned by the value from here on.
    let copy = unsafe { SHStrDupW(&HSTRING::from(value)) }.context("copying a text")?;
    Ok(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_LPWSTR,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 { pwszVal: copy },
            }),
        },
    })
}

/// Write what would have gone to the shell, one line to a line.
///
/// The person's removals are read from the same file with `.removed` added,
/// a picture a line, and taken as the shell gives them: once.
fn record(file: &Path, entries: &[Entry]) -> Result<Vec<PathBuf>> {
    let mut removals = file.as_os_str().to_owned();
    removals.push(".removed");
    let removals = PathBuf::from(removals);
    let removed: Vec<PathBuf> = std::fs::read_to_string(&removals)
        .map(|text| text.lines().filter(|line| !line.is_empty()).map(PathBuf::from).collect())
        .unwrap_or_default();
    let _ = std::fs::remove_file(&removals);

    let kept: Vec<Entry> = entries
        .iter()
        .filter(|entry| !removed.iter().any(|gone| same_folder(gone, &entry.path)))
        .cloned()
        .collect();
    // Ten, the room the shell gives a list unless the person changed it.
    let lines = items(&kept, 10, Path::is_dir);
    let mut out = String::from("# nitid: the Jump List as it would be written\n");
    for line in &lines {
        out.push_str(&format!("{}\t{}\n", line.title, argument(&line.picture)));
    }
    std::fs::write(file, out).with_context(|| format!("writing {}", file.display()))?;
    Ok(removed)
}

/// Whether two pictures are in the same folder, as the start screen counts it.
fn same_folder(a: &Path, b: &Path) -> bool {
    match (a.parent(), b.parent()) {
        (Some(a), Some(b)) => crate::recent::same_path(a, b),
        _ => false,
    }
}

/// What the writer is asked.
enum Asked {
    /// Bring the list in line with these entries.
    Show(Vec<Entry>),
    /// The same, and then stop, handing back what the person removed: the
    /// viewer is closing and hears no more events.
    Finish(Vec<Entry>),
}

/// The thread that writes the Jump List, and the way to it.
pub struct JumpList {
    asking: Option<mpsc::Sender<Asked>>,
    finished: mpsc::Receiver<()>,
    /// The pictures of the lines a person removed, until the viewer takes
    /// them. A box rather than an event's payload: the last write happens as
    /// the viewer closes, when no event is heard any more, and what it learns
    /// is still taken — once, by whichever comes first.
    removed: Arc<Mutex<Vec<PathBuf>>>,
}

impl JumpList {
    /// Start the writer. `wake` is called on the writer's thread when a person
    /// has removed lines, for the viewer to `take_removed` and forget their
    /// folders where the start screen keeps them.
    pub fn new(wake: impl Fn() + Send + 'static) -> Self {
        let (asking, asked) = mpsc::channel::<Asked>();
        let (done, finished) = mpsc::channel();
        let removed: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
        let inbox = removed.clone();
        let spawned = std::thread::Builder::new().name("jump list".into()).spawn(move || {
            let shell = Shell::here();
            let mut written: Option<Vec<Entry>> = None;
            while let Ok(mut next) = asked.recv() {
                // A burst of folders is one list: only the newest matters,
                // and a finish among them is still a finish.
                while let Ok(newer) = asked.try_recv() {
                    next = match (next, newer) {
                        (Asked::Finish(_), Asked::Show(entries) | Asked::Finish(entries)) => Asked::Finish(entries),
                        (Asked::Show(_), newer) => newer,
                    };
                }
                let (entries, last) = match next {
                    Asked::Show(entries) => (entries, false),
                    Asked::Finish(entries) => (entries, true),
                };
                // The same list again is nothing to do. The first is always
                // written, which is what brings a list up to date after an
                // install or a change made while the viewer was closed.
                //
                // Every failure is silent, for the reason the list's own
                // file's are (`recent`): a viewer is not worse for a Jump List
                // that could not be written, and has nobody to tell.
                let removed = match &shell {
                    Some(shell) if written.as_ref() != Some(&entries) => match shell.write(&entries) {
                        Ok(removed) => {
                            written = Some(entries);
                            removed
                        }
                        Err(_) => Vec::new(),
                    },
                    _ => Vec::new(),
                };
                if !removed.is_empty() {
                    inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend(removed);
                    if !last {
                        wake();
                    }
                }
                if last {
                    let _ = done.send(());
                    return;
                }
            }
        });
        Self {
            asking: spawned.is_ok().then_some(asking),
            finished,
            removed,
        }
    }

    /// The pictures of the lines a person removed, since this was last asked.
    pub fn take_removed(&self) -> Vec<PathBuf> {
        std::mem::take(&mut *self.removed.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    /// Bring the list in line with `entries`, newest first.
    pub fn show(&self, entries: Vec<Entry>) {
        if let Some(asking) = &self.asking {
            let _ = asking.send(Asked::Show(entries));
        }
    }

    /// The viewer is closing: write `entries` — the picture last looked at
    /// in the open folder is only known now — and wait, for at most
    /// `patience`, for the writer to finish.
    ///
    /// Returns every removal not yet taken: those of the last write, and any
    /// whose wake came too late to be heard. A removal not acted on now would
    /// be undone by the next start's list.
    pub fn finish(&mut self, entries: Vec<Entry>, patience: Duration) -> Vec<PathBuf> {
        if let Some(asking) = self.asking.take()
            && asking.send(Asked::Finish(entries)).is_ok()
        {
            let _ = self.finished.recv_timeout(patience);
        }
        self.take_removed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &Path, when: u64) -> Entry {
        Entry {
            path: path.to_path_buf(),
            when,
        }
    }

    #[test]
    fn a_line_is_a_folder_named_as_its_tile_opening_on_its_picture() {
        let entries = [entry(Path::new(r"C:\Shoot\IMG_0042.jpg"), 2), entry(Path::new(r"D:\Screens\a.png"), 1)];
        let lines = items(&entries, 10, |_| true);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].title, "Shoot");
        assert_eq!(lines[0].picture, PathBuf::from(r"C:\Shoot\IMG_0042.jpg"));
        assert!(
            lines[0].tooltip.contains(r"C:\Shoot") && lines[0].tooltip.contains("IMG_0042.jpg"),
            "{}",
            lines[0].tooltip
        );
        assert_eq!(lines[1].title, "Screens", "the order of the list was not kept");
    }

    /// The list has the room the shell gives it and no folder that is not
    /// there: the same rule as the start screen's.
    #[test]
    fn only_the_folders_that_are_there_and_fit_are_listed() {
        let entries: Vec<Entry> = (0..20).map(|index| entry(&PathBuf::from(format!(r"C:\f{index}\x.jpg")), 100 - index)).collect();
        assert_eq!(items(&entries, 10, |_| true).len(), 10);
        let unplugged = |folder: &Path| !folder.starts_with(r"C:\f0");
        let lines = items(&entries, 3, unplugged);
        let titles: Vec<&str> = lines.iter().map(|line| line.title.as_str()).collect();
        assert_eq!(titles, ["f1", "f2", "f3"], "a folder that is not there was listed, or the order was lost");
    }

    /// What a line passes is read back as the picture it was made for, so a
    /// removed line forgets the folder it stood for.
    #[test]
    fn a_line_gives_back_the_picture_it_passes() {
        for picture in [r"C:\Photos\a.jpg", r"D:\with spaces\и не только\кадр 1.heic", r"\\server\share\x.png"] {
            let picture = PathBuf::from(picture);
            assert_eq!(picture_of(&argument(&picture)), Some(picture.clone()), "{}", picture.display());
        }
        assert_eq!(picture_of("--peek"), None, "an argument that is not a picture was taken for one");
        assert_eq!(picture_of("\"\""), None);
    }

    /// The writer hears what the person removed, and leaves it off.
    #[test]
    fn a_removed_line_is_left_off_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (shoot, screens) = (dir.path().join("shoot"), dir.path().join("screens"));
        for folder in [&shoot, &screens] {
            std::fs::create_dir_all(folder).unwrap();
        }
        let file = dir.path().join("jump-list");
        std::fs::write(dir.path().join("jump-list.removed"), format!("{}\n", shoot.join("old.jpg").display())).unwrap();

        let entries = [entry(&shoot.join("new.jpg"), 2), entry(&screens.join("a.png"), 1)];
        let removed = record(&file, &entries).unwrap();
        assert_eq!(removed, vec![shoot.join("old.jpg")]);
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(!written.contains("shoot"), "a folder the person removed went back on the list:\n{written}");
        assert!(written.contains("screens"), "{written}");
        assert!(!dir.path().join("jump-list.removed").exists(), "a removal was kept to be reported twice");
    }

    /// The shell's own half, against the real shell, under an id of this
    /// test's so nobody's Jump List is touched — and taken away again.
    ///
    /// What the shell keeps is read back from where it keeps it: Windows has
    /// no call that reads a custom category, and its file names the lines in
    /// UTF-16, so the test looks for its own folder's name there.
    #[test]
    fn the_shell_takes_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let marker = format!("nitid-jump-list-test-{}", std::process::id());
        let folder = dir.path().join(&marker);
        std::fs::create_dir_all(&folder).unwrap();
        let picture = folder.join("a.png");
        std::fs::write(&picture, b"x").unwrap();
        let app_id = format!("lacodda.nitid.test{}", std::process::id());
        let viewer = std::env::current_exe().unwrap();

        let Some(store) = std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join(r"Microsoft\Windows\Recent\CustomDestinations")) else {
            return;
        };
        let started = std::time::SystemTime::now() - Duration::from_secs(2);
        let removed = commit(&app_id, &viewer, &[entry(&picture, 1)]).expect("the shell refused the list");
        assert!(removed.is_empty(), "a list nobody has seen has removals: {removed:?}");

        let wide: Vec<u8> = marker.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let holding = || -> Vec<PathBuf> {
            std::fs::read_dir(&store)
                .map(|entries| {
                    entries
                        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                        .filter(|path| std::fs::metadata(path).and_then(|data| data.modified()).is_ok_and(|when| when >= started))
                        .filter(|path| std::fs::read(path).is_ok_and(|bytes| bytes.windows(wide.len()).any(|window| window == wide)))
                        .collect()
                })
                .unwrap_or_default()
        };
        let kept = holding();

        // SAFETY: as in `commit`.
        let _com = crate::com::Com::begin();
        let list: ICustomDestinationList = unsafe { CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER) }.unwrap();
        unsafe { list.DeleteList(&HSTRING::from(app_id.as_str())) }.expect("taking the test's list away");

        // Recent items switched off is a list the shell keeps empty, and a
        // machine set that way has nothing to find.
        let tracking = windows_registry::CURRENT_USER
            .open(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced")
            .and_then(|key| key.get_u32("Start_TrackDocs"))
            .unwrap_or(1);
        if tracking == 0 {
            return;
        }
        assert!(!kept.is_empty(), "the shell keeps no list naming the folder written to it");
        assert!(holding().is_empty(), "the test's list outlived it");
    }
}

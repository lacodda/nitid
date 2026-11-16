//! Putting nitid on the machine it was run from, and taking it off again.
//!
//! Everything here writes under `HKEY_CURRENT_USER` and
//! `%LOCALAPPDATA%\Programs\nitid`, so installing needs no administrator and
//! touches nothing another user of the machine depends on.
//!
//! Windows deliberately does not let a program make itself the default handler
//! for a file type — that setting belongs to the user, and the API to force it
//! has been closed since Windows 8. What an installer *can* do is register the
//! application properly, so nitid appears in "Open with" and in Settings'
//! default-apps list; picking it there is one click and it sticks. Anything
//! that claims otherwise is fighting the shell and loses on the next update.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use windows_registry::{CURRENT_USER, Key, Type};

use crate::image_source::supported_extensions;

/// The ProgID under which nitid registers its file types.
///
/// The vendor prefix is the convention that keeps ProgIDs from colliding
/// across applications that happen to share a name.
const PROGID: &str = "lacodda.nitid.image";

/// Where `Open with` and the default-apps page read the application from.
///
/// Named after the windowed binary, because that is what the shell launches:
/// `nitid.exe` would flash a console before hiding it.
const APPLICATION_KEY: &str = r"Software\Classes\Applications\nitidw.exe";

/// The executables an install puts in place.
///
/// `nitid.exe` is the one to run from a terminal; `nitidw.exe` is what the
/// shell opens files with. See
/// `docs/adr/0004-two-binaries-console-and-windowed.md`.
pub const EXECUTABLES: [&str; 2] = ["nitid.exe", "nitidw.exe"];

/// The binary the shell launches for a file association.
const WINDOWED_EXE: &str = "nitidw.exe";

/// Application keys written by earlier versions.
///
/// v0.2.0 registered `nitid.exe` as the handler; v0.3.0 moved that to
/// `nitidw.exe` so the shell never creates a console. The old key is removed
/// on install as well as on uninstall, or "Open with" lists nitid twice and
/// one of the entries flashes a console window.
const LEGACY_APPLICATION_KEYS: [&str; 1] = [r"Software\Classes\Applications\nitid.exe"];

/// The registered-applications entry that puts nitid in Settings.
const CAPABILITIES_KEY: &str = r"Software\lacodda\nitid\Capabilities";

/// The verbs that put "Open with nitid" on a folder's menu: on a folder, and
/// on the empty space inside an open one, where the folder is the one being
/// looked at. `%1` is the folder clicked; `%V` is the folder the background
/// belongs to.
///
/// Registry verbs rather than a shell extension: they need no DLL loaded into
/// Explorer, and an `IExplorerCommand` in one reaches the top of Windows 11's
/// menu only from a signed package — without one it lands under "Show more
/// options" exactly where these do.
const FOLDER_VERBS: [FolderVerb; 2] = [
    FolderVerb {
        key: r"Software\Classes\Directory\shell\nitid",
        argument: "%1",
        single: true,
    },
    FolderVerb {
        key: r"Software\Classes\Directory\Background\shell\nitid",
        argument: "%V",
        single: false,
    },
];

/// One entry of a folder's menu.
struct FolderVerb {
    key: &'static str,
    argument: &'static str,
    /// Offered only when one folder is selected. The viewer walks one folder
    /// at a time, and five selected would start five launches with the last
    /// one winning.
    single: bool,
}

/// What the menu entry runs.
fn verb_command(exe: &str, argument: &str) -> String {
    format!("\"{exe}\" \"{argument}\"")
}

/// Where Windows reads what to start at sign-in, for this user.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The name nitid's entry carries there, and in Settings > Apps > Startup.
const RUN_VALUE: &str = "nitid";

/// What the sign-in entry runs: the resident, from the installed copy.
fn resident_command(exe: &str) -> String {
    format!("\"{exe}\" {}", crate::RESIDENT_ARGUMENT)
}

/// Where the per-user `PATH` lives.
///
/// The user hive, not the machine one: an install that needs no administrator
/// has no business editing the PATH every account on the machine shares.
const ENVIRONMENT_KEY: &str = "Environment";

/// Whether this process is under a test's instance tag.
///
/// Such a process has names of its own for everything else already (the pipe,
/// the resident's window), and the registry is the same: nothing it installs
/// reaches the real registration, the PATH, the desktop or the machine's
/// schedule. In the product rather than in each test, because a gate that
/// forgot would quietly rewrite the file types of whoever ran it.
fn sandboxed() -> bool {
    !crate::single::instance_tag().is_empty()
}

/// The registry everything here is written under: the user's, or a test's
/// own corner of it.
fn hive() -> Result<Key> {
    let tag = crate::single::instance_tag();
    let path = if tag.is_empty() {
        String::new()
    } else {
        format!(r"Software\lacodda\nitid-test\{tag}")
    };
    CURRENT_USER.create(path).context("opening the registry")
}

/// Who is asking for the registration, which decides what of it is written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Asked {
    /// A person installing: everything, including what they may have taken
    /// away since.
    Install,
    /// An update arriving on its own. It changes the program and leaves the
    /// person's choices alone: a shortcut deleted, a PATH entry removed or a
    /// sign-in entry taken out stays that way.
    Update,
}

/// Install this executable for the current user.
///
/// Returns the directory it was installed into.
pub fn install() -> Result<PathBuf> {
    let source = env::current_exe().context("locating the running executable")?;
    let source_dir = source.parent().context("the running executable has no directory")?;
    let target_dir = install_dir()?;

    fs::create_dir_all(&target_dir).with_context(|| format!("creating {}", target_dir.display()))?;
    let _turn = Turn::take()?;

    // A resident already running is running the copy about to be replaced,
    // and the one that should run afterwards is the new one: stopped first,
    // started again below.
    if let Err(error) = crate::resident::stop() {
        eprintln!("nitid: {error:#}");
    }

    if same_file(source_dir, &target_dir) {
        // Installing from the installed copy: the registry work below still
        // runs, which is how a re-register repairs a broken association.
        crate::console::say!("nitid is already installed in {}", target_dir.display());
    } else {
        put_in_place(source_dir, &target_dir)?;
        crate::console::say!("Copied nitid to {}", target_dir.display());
    }

    remove_legacy_keys()?;
    register(&target_dir.join(WINDOWED_EXE), Asked::Install)?;

    let extensions = supported_extensions();
    crate::console::say!("Registered {} file types: {}", extensions.len(), extensions.join(", "));
    crate::console::say!("Added \"Open with nitid\" to folders");

    // Reported, never fatal: a viewer the shell can open pictures with is
    // installed even if its directory could not reach the PATH.
    if leaves_path_alone(env::var_os("NITID_NO_PATH").as_deref()) {
        crate::console::say!("Left your PATH as it is, as NITID_NO_PATH asks");
    } else {
        match add_to_path(&target_dir) {
            Ok(true) => crate::console::say!("Added {} to your PATH", target_dir.display()),
            Ok(false) => {}
            Err(error) => eprintln!("nitid: could not add {} to your PATH: {error:#}", target_dir.display()),
        }
    }

    // Likewise: a desktop without a shortcut is an inconvenience, not a
    // failed install.
    match create_desktop_shortcut(&target_dir.join(WINDOWED_EXE)) {
        Ok(Some(path)) => crate::console::say!("Put a shortcut on your desktop: {}", path.display()),
        Ok(None) => {}
        Err(error) => eprintln!("nitid: could not create the desktop shortcut: {error:#}"),
    }

    // The resident is what answers Explorer's space bar; it also starts at
    // every sign-in from the entry `register` wrote. Not fatal either: the
    // viewer is installed whether or not the space bar is heard.
    match crate::resident::start(&target_dir) {
        Ok(()) => crate::console::say!("Press Space on a picture in Explorer to look at it; Space again puts it away"),
        Err(error) => eprintln!("nitid: could not start nitid in the background: {error:#}"),
    }

    crate::console::say!("Once a day nitid asks GitHub for a newer release and installs it; Settings > Updates switches that off");

    crate::console::say!();
    crate::console::say!("nitid is now offered in \"Open with\". Windows reserves the choice");
    crate::console::say!("of default application for you: right-click an image, choose");
    crate::console::say!("\"Open with\" > \"Choose another app\", pick nitid and tick");
    crate::console::say!("\"Always use this app\". Settings > Apps > Default apps works too.");

    Ok(target_dir)
}

/// Whether `NITID_NO_PATH` asks for the `PATH` to be left as it is.
///
/// The line's installers all hear `{APP}_NO_PATH`: whoever installs for a
/// check, or keeps their `PATH` by hand, says so once for every product. Set to
/// anything but nothing it means yes - the same reading `tools/install.ps1`
/// gives it, so the script and the program cannot disagree.
fn leaves_path_alone(asked: Option<&std::ffi::OsStr>) -> bool {
    asked.is_some_and(|value| !value.is_empty())
}

/// Replace the installed copy with this one, as an update.
///
/// Run by the new version from where it was unpacked. Whatever happens the
/// resident is started again if it was running, and what happened is written
/// down first, so the new one can say so.
pub fn update() -> Result<()> {
    let source = env::current_exe().context("locating the running executable")?;
    let source_dir = source.parent().context("the running executable has no directory")?;
    let target_dir = install_dir()?;
    if !target_dir.join(WINDOWED_EXE).exists() {
        bail!("nitid is not installed in {}", target_dir.display());
    }
    let _turn = Turn::take()?;

    // Someone who quit it and then asked for an update asked for an update,
    // and did not ask for it back.
    let was_running = match crate::resident::stop() {
        Ok(was_running) => was_running,
        Err(error) => {
            eprintln!("nitid: {error:#}");
            true
        }
    };

    let outcome = (|| {
        let changed = put_in_place(source_dir, &target_dir)?;
        remove_legacy_keys()?;
        register(&target_dir.join(WINDOWED_EXE), Asked::Update)?;
        Ok(changed)
    })();
    crate::update::note_outcome(&outcome);

    // Not a choice brought back: a shortcut taken away stays away, and one
    // still there is only given the name the windows now go by.
    if outcome.is_ok()
        && let Err(error) = name_desktop_shortcut(&target_dir.join(WINDOWED_EXE))
    {
        eprintln!("nitid: could not name the desktop shortcut: {error:#}");
    }

    if was_running && let Err(error) = crate::resident::start(&target_dir) {
        eprintln!("nitid: could not start nitid in the background: {error:#}");
    }

    outcome.map(|_| ())
}

/// One install or update at a time.
///
/// An update arriving just as a person runs the script would otherwise have
/// two processes retiring and placing the same two files.
struct Turn(windows::Win32::Foundation::HANDLE);

impl Turn {
    /// As long as an install takes, and then some.
    const PATIENCE_MS: u32 = 60_000;

    fn take() -> Result<Self> {
        use windows::Win32::Foundation::WAIT_TIMEOUT;
        use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
        use windows::core::HSTRING;

        let name = HSTRING::from(format!(r"Local\lacodda.nitid.install{}", crate::single::instance_tag()));
        // SAFETY: the handle is checked, and closed by `drop`.
        let lock = unsafe { CreateMutexW(None, false, &name) }.context("taking the installer's lock")?;
        // An abandoned lock — a process that died holding it — is a free one.
        if unsafe { WaitForSingleObject(lock, Self::PATIENCE_MS) } == WAIT_TIMEOUT {
            // SAFETY: as above.
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(lock);
            }
            bail!("another install of nitid has not finished");
        }
        Ok(Self(lock))
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        // SAFETY: taken by this thread in `take`, and released once.
        unsafe {
            let _ = windows::Win32::System::Threading::ReleaseMutex(self.0);
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Put this release's executables in `dir`, and see that they work.
///
/// Returns whether anything changed: both already byte for byte the ones
/// there is an update that has nothing to do, not a failure.
fn put_in_place(from: &Path, dir: &Path) -> Result<bool> {
    if same_file(from, dir) || identical(from, dir) {
        return Ok(false);
    }
    sweep_retired_in(dir);
    let swap = Swap::of(from, dir)?;
    if let Err(error) = works(from, dir) {
        swap.undo();
        return Err(error.context("the new copy did not work there, and the old one was put back"));
    }
    Ok(true)
}

/// Whether `dir` already holds exactly what `from` does.
fn identical(from: &Path, dir: &Path) -> bool {
    EXECUTABLES.iter().all(|name| match (fs::read(from.join(name)), fs::read(dir.join(name))) {
        (Ok(ours), Ok(theirs)) => ours == theirs,
        _ => false,
    })
}

/// Whether what was put in `dir` is what was meant, and runs from there.
///
/// Asked of the file in place rather than of the one it was copied from: a
/// machine's policy, or an antivirus deciding about a program it has not seen
/// before, says yes or no by where a thing is.
fn works(from: &Path, dir: &Path) -> Result<()> {
    if !identical(from, dir) {
        bail!("what landed in {} is not what was copied there", dir.display());
    }
    let said = version_said_by(&dir.join("nitid.exe"))?;
    let meant = format!("nitid {}", env!("CARGO_PKG_VERSION"));
    if said != meant {
        bail!("{} says `{said}` rather than `{meant}`", dir.display());
    }
    Ok(())
}

/// What an executable answers `--version` with.
///
/// With a limit, because a process that never answers must not leave the
/// resident stopped for good, and with no window, because this runs in the
/// background and the console binary would otherwise flash one.
pub fn version_said_by(exe: &Path) -> Result<String> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const PATIENCE: Duration = Duration::from_secs(30);

    let mut child = Command::new(exe)
        .arg("--version")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("running {}", exe.display()))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().with_context(|| format!("waiting for {}", exe.display()))? {
            break status;
        }
        if started.elapsed() > PATIENCE {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{} did not answer", exe.display());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !status.success() {
        bail!("{} stopped with {status}", exe.display());
    }
    let mut said = String::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut said).with_context(|| format!("reading what {} said", exe.display()))?;
    }
    Ok(said.trim().to_string())
}

/// Both executables of a release, or neither.
///
/// They are one program: the shell starts one and the terminal the other, and
/// the windowed one calls on the console one to decode. A failure halfway must
/// leave the pair that was there, because an update happens with nobody
/// watching and nobody to run the script again. So everything that can fail for
/// ordinary reasons - a full disk, an antivirus holding the file - fails while
/// copying beside the targets, before anything installed has been touched; the
/// rest are renames, each of which can be walked back.
#[derive(Debug)]
struct Swap {
    placed: Vec<Placed>,
}

#[derive(Debug)]
struct Placed {
    target: PathBuf,
    retired: Option<PathBuf>,
}

impl Swap {
    fn of(from: &Path, dir: &Path) -> Result<Self> {
        let mut incoming = Vec::new();
        let forget = |incoming: &[(PathBuf, PathBuf)]| {
            for (new, _) in incoming {
                let _ = fs::remove_file(new);
            }
        };
        for name in EXECUTABLES {
            let source = from.join(name);
            if !source.exists() {
                // A zip that carries only one of the pair is not something to
                // half-install: the association would point at a missing file.
                forget(&incoming);
                bail!("{} is missing from {}; both executables must be installed together", name, from.display());
            }
            let target = dir.join(name);
            let new = target.with_extension("exe.new");
            if let Err(error) = fs::copy(&source, &new) {
                let _ = fs::remove_file(&new);
                forget(&incoming);
                return Err(error).with_context(|| format!("copying to {}", new.display()));
            }
            incoming.push((new, target));
        }

        let mut swap = Self { placed: Vec::new() };
        for (new, target) in &incoming {
            if let Err(error) = swap.place(new, target) {
                swap.undo();
                forget(&incoming);
                return Err(error);
            }
        }
        Ok(swap)
    }

    fn place(&mut self, new: &Path, target: &Path) -> Result<()> {
        let retired = if target.exists() { Some(retire(target)?) } else { None };
        if let Err(error) = fs::rename(new, target) {
            if let Some(retired) = &retired {
                let _ = fs::rename(retired, target);
            }
            return Err(error).with_context(|| format!("putting {} in place", target.display()));
        }
        self.placed.push(Placed {
            target: target.to_path_buf(),
            retired,
        });
        Ok(())
    }

    /// Put back what was there.
    fn undo(&self) {
        for placed in self.placed.iter().rev() {
            // Retired rather than removed: something may have started it in
            // the moment it was there.
            if retire(&placed.target).is_ok()
                && let Some(retired) = &placed.retired
            {
                let _ = fs::rename(retired, &placed.target);
            }
        }
    }
}

/// Rename an executable out of its name, to make way for another.
///
/// Windows locks a running image against being overwritten or deleted, but not
/// against being renamed, which is what lets an update land while a viewer is
/// open. That viewer keeps the retired file locked for as long as it stays
/// open, across as many updates as arrive meanwhile, so the name retired to has
/// to be one that is free rather than one that was free last time.
///
/// A name already taken is passed over and never cleared: it may be the very
/// copy an undo is about to bring back. `sweep_retired_in` is what clears them,
/// before a swap begins.
fn retire(target: &Path) -> Result<PathBuf> {
    let name = target.file_name().context("an executable with no name")?.to_string_lossy().into_owned();
    for attempt in 0..1000 {
        let retired = target.with_file_name(if attempt == 0 {
            format!("{name}.old")
        } else {
            format!("{name}.{attempt}.old")
        });
        if retired.exists() {
            continue;
        }
        fs::rename(target, &retired).with_context(|| format!("replacing {}", target.display()))?;
        return Ok(retired);
    }
    bail!("{} has a thousand retired copies beside it", target.display());
}

/// Delete what earlier installs retired, and what one that died left half
/// done. Anything still running stays until it isn't.
fn sweep_retired_in(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ours = EXECUTABLES.iter().any(|executable| name.starts_with(executable));
        if ours && (name.ends_with(".old") || name.ends_with(".new")) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// The same, in the installed copy's directory.
pub fn sweep_retired() {
    if let Ok(dir) = install_dir() {
        sweep_retired_in(&dir);
    }
}

/// Whether this process is running from the installed copy.
pub fn is_installed_copy() -> bool {
    let running = env::current_exe().ok();
    match (running.as_deref().and_then(Path::parent), install_dir()) {
        (Some(here), Ok(installed)) => same_file(here, &installed),
        _ => false,
    }
}

/// The installed copy's console executable.
pub fn installed_executable() -> Result<PathBuf> {
    Ok(install_dir()?.join("nitid.exe"))
}

/// Remove the registration and the installed copy.
pub fn uninstall() -> Result<()> {
    // Before anything is taken away: a resident left running would answer the
    // space bar with a program that is no longer there.
    match crate::resident::stop() {
        Ok(true) => crate::console::say!("Stopped nitid in the background"),
        Ok(false) => {}
        Err(error) => eprintln!("nitid: {error:#}"),
    }

    unregister()?;
    crate::console::say!("Removed the file type registration");

    let dir = install_dir()?;
    match remove_from_path(&dir) {
        Ok(true) => crate::console::say!("Removed {} from your PATH", dir.display()),
        Ok(false) => {}
        Err(error) => eprintln!("nitid: could not remove {} from your PATH: {error:#}", dir.display()),
    }

    match remove_desktop_shortcut() {
        Ok(true) => crate::console::say!("Removed the desktop shortcut"),
        Ok(false) => {}
        Err(error) => eprintln!("nitid: could not remove the desktop shortcut: {error:#}"),
    }

    // The machine's own, not the instance's: a test uninstalling its sandbox
    // has no business with either of these.
    if !sandboxed() {
        // A schedule left behind would start a program that is no longer
        // there, every half hour, for as long as the machine lives.
        match crate::wallpaper::forget_schedule() {
            Ok(true) => crate::console::say!("Removed the desktop picture's schedule"),
            Ok(false) => {}
            Err(error) => eprintln!("nitid: could not remove the desktop picture's schedule: {error:#}"),
        }

        // The decoder's AppContainer profile is machine state nitid registered
        // on its first sandboxed decode; uninstalling takes it too. A machine
        // that never decoded a HEIC has nothing to remove, which is not an
        // error.
        if crate::sandbox::remove_container_profile().is_ok() {
            crate::console::say!("Removed the decoder's container profile");
        }
    }

    // What the updater keeps, and whatever it left unpacked.
    crate::update::remove_everything();

    let running = env::current_exe().unwrap_or_default();
    let mut removed = false;
    let mut deferred = false;

    for name in EXECUTABLES {
        let target = dir.join(name);
        if !target.exists() {
            continue;
        }

        if same_file(&running, &target) {
            // A running executable cannot delete itself; renaming it out of
            // the way lets the next install write a clean copy, and the next
            // one sweeps the stale name.
            retire(&target)?;
            deferred = true;
        } else {
            fs::remove_file(&target).with_context(|| format!("removing {}", target.display()))?;
            removed = true;
        }
    }

    if !removed && !deferred {
        crate::console::say!("Nothing installed in {}", dir.display());
        return Ok(());
    }

    if deferred {
        crate::console::say!("Marked the running executable for removal on exit");
    }
    sweep_retired_in(&dir);
    // Only succeeds once the directory is empty, which is the point.
    let _ = fs::remove_dir(&dir);
    crate::console::say!("Removed {}", dir.display());

    Ok(())
}

/// Put the install directory on the user's `PATH`, if it is not there already.
///
/// Without this `nitid --version` in a terminal finds nothing: the shell
/// association is registry work and says nothing about the command line, and
/// an installer that leaves a command you cannot run has only half installed
/// the program. Reported rather than fatal — a viewer that opens pictures from
/// Explorer is still installed, and a PATH that cannot be written is not a
/// reason to fail the whole command.
///
/// Returns whether the directory was added.
fn add_to_path(dir: &Path) -> Result<bool> {
    let environment = hive()?
        .options()
        .read()
        .write()
        .create()
        .open(ENVIRONMENT_KEY)
        .context("opening the user's environment")?;

    // A user who has never had a per-user PATH has no value here at all, which
    // is an empty PATH rather than an error.
    let current = environment.get_string("Path").unwrap_or_default();
    let Some(updated) = path_with(&current, dir) else {
        return Ok(false);
    };

    // The type has to be preserved. A PATH is nearly always `REG_EXPAND_SZ`
    // because entries like `%JAVA_HOME%\bin` are written unexpanded; storing
    // it back as a plain string would leave those entries as literal text and
    // silently break every one of them. Only a value that is already a plain
    // string — or absent — is written as one.
    match environment.get_type("Path") {
        Ok(Type::String) => environment.set_string("Path", &updated)?,
        // Absent, expandable, or something unexpected: the expandable form is
        // what Windows itself writes and is correct either way, since a string
        // with nothing to expand expands to itself.
        _ => environment.set_expand_string("Path", &updated)?,
    }

    announce_environment_change();
    Ok(true)
}

/// Take the install directory back off the user's `PATH`.
///
/// Silent about a PATH that never had it: uninstalling something that was
/// never there is not a failure.
fn remove_from_path(dir: &Path) -> Result<bool> {
    let Ok(environment) = hive()?.options().read().write().open(ENVIRONMENT_KEY) else {
        return Ok(false);
    };
    let Ok(current) = environment.get_string("Path") else {
        return Ok(false);
    };
    let Some(updated) = path_without(&current, dir) else {
        return Ok(false);
    };

    match environment.get_type("Path") {
        Ok(Type::String) => environment.set_string("Path", &updated)?,
        _ => environment.set_expand_string("Path", &updated)?,
    }

    announce_environment_change();
    Ok(true)
}

/// `path` with `dir` appended, or `None` if it is already there.
///
/// Kept apart from the registry so the rules that matter — how an entry is
/// matched, what happens to a trailing separator — are testable without
/// touching the machine's environment.
fn path_with(path: &str, dir: &Path) -> Option<String> {
    if path_contains(path, dir) {
        return None;
    }
    let trimmed = path.trim_end_matches(';');
    Some(if trimmed.is_empty() {
        dir.display().to_string()
    } else {
        format!("{trimmed};{}", dir.display())
    })
}

/// `path` with every entry naming `dir` removed, or `None` if there were none.
fn path_without(path: &str, dir: &Path) -> Option<String> {
    if !path_contains(path, dir) {
        return None;
    }
    let kept: Vec<&str> = path.split(';').filter(|entry| !entry.trim().is_empty() && !same_entry(entry, dir)).collect();
    Some(kept.join(";"))
}

/// Whether `path` already names `dir`.
fn path_contains(path: &str, dir: &Path) -> bool {
    path.split(';').any(|entry| same_entry(entry, dir))
}

/// Whether one `PATH` entry names this directory.
///
/// Compared case-insensitively and without a trailing separator, because
/// Windows treats `C:\Programs\nitid`, `c:\programs\nitid\` and the same path
/// with surrounding spaces as one directory — and adding a second spelling of
/// a directory already on the PATH is how a PATH grows a duplicate on every
/// upgrade.
fn same_entry(entry: &str, dir: &Path) -> bool {
    let normalise = |text: &str| text.trim().trim_end_matches(['\\', '/']).to_lowercase();
    !entry.trim().is_empty() && normalise(entry) == normalise(&dir.display().to_string())
}

/// Tell the system the environment changed.
///
/// Without this the new `PATH` reaches only processes started after the next
/// sign-in: Explorer caches the environment it hands to everything it
/// launches, and it re-reads it when this message arrives. Already-open
/// terminals keep the environment they started with either way — nothing can
/// change that from outside.
///
/// Best effort by construction: it is a courtesy to the shell, and a machine
/// where the broadcast fails still has the correct PATH in the registry.
fn announce_environment_change() {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE};

    if sandboxed() {
        return;
    }
    let environment = windows::core::w!("Environment");
    unsafe {
        // A timeout, and `ABORTIFHUNG`: broadcasting to every top-level window
        // means one wedged application must not hold up an install.
        SendMessageTimeoutW(
            HWND(HWND_BROADCAST.0),
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(environment.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            5000,
            None,
        );
    }
}

/// `%LOCALAPPDATA%\Programs\nitid` — where a per-user install belongs.
fn install_dir() -> Result<PathBuf> {
    let local = env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set; this command is for Windows")?;
    Ok(PathBuf::from(local).join("Programs").join("nitid"))
}

/// Whether two paths name the same file, comparing case-insensitively as
/// Windows does. Neither path needs to exist.
fn same_file(a: &Path, b: &Path) -> bool {
    let normalise = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().to_lowercase();
    normalise(a) == normalise(b)
}

/// Drop application keys left by an earlier version.
///
/// Silent: an upgrade that already worked must not fail because a key from a
/// previous release refuses to go.
fn remove_legacy_keys() -> Result<()> {
    let hive = hive()?;
    for key in LEGACY_APPLICATION_KEYS {
        if hive.open(key).is_ok() {
            let _ = hive.remove_tree(key);
        }
    }
    Ok(())
}

/// Writes string values, and remembers whether any of them was news.
///
/// Compared first, because an update runs this with nobody asking: a value
/// that already says the same is left unwritten, and a registration with
/// nothing new in it is not announced to the shell, which rereads its icons
/// when told, flicker and all.
struct Writer {
    hive: Key,
    changed: bool,
}

impl Writer {
    fn set(&mut self, key: &str, name: &str, value: &str) -> Result<()> {
        let key = self.hive.create(key).with_context(|| format!("creating {key}"))?;
        if key.get_string(name).is_ok_and(|current| current == value) {
            return Ok(());
        }
        key.set_string(name, value)?;
        self.changed = true;
        Ok(())
    }
}

/// Write every registry key the shell reads.
///
/// **Nothing here touches what a person chose.** The default program for a type
/// lives under `FileExts\.ext\UserChoiceLatest`, signed by Windows against the
/// ProgID's name; it depends on nothing below but that name, which never
/// changes, so choosing nitid survives every update. Other programs' entries in
/// `OpenWithProgids` are theirs. A gate compares the whole hive across an update
/// to hold this.
fn register(exe: &Path, asked: Asked) -> Result<()> {
    let exe = exe.to_string_lossy().to_string();
    let open_command = format!("\"{exe}\" \"%1\"");
    let icon = format!("\"{exe}\",0");
    let mut writer = Writer { hive: hive()?, changed: false };

    // The ProgID: what the shell shows and how it launches us.
    let progid = format!(r"Software\Classes\{PROGID}");
    writer.set(&progid, "", "Image")?;
    writer.set(&progid, "FriendlyTypeName", "Image")?;
    writer.set(&format!(r"{progid}\DefaultIcon"), "", &icon)?;
    writer.set(&format!(r"{progid}\shell\open\command"), "", &open_command)?;

    // The application entry: this is what "Open with" lists.
    writer.set(APPLICATION_KEY, "FriendlyAppName", "nitid")?;
    writer.set(&format!(r"{APPLICATION_KEY}\shell\open\command"), "", &open_command)?;

    // Capabilities: what puts nitid in Settings > Default apps.
    writer.set(CAPABILITIES_KEY, "ApplicationName", "nitid")?;
    writer.set(CAPABILITIES_KEY, "ApplicationDescription", "A fast image viewer with honest color")?;

    for extension in supported_extensions() {
        let dotted = format!(".{extension}");

        // Offer nitid for the type without seizing it: `OpenWithProgids` adds
        // an entry to the "Open with" list, whereas writing the key's default
        // value would claim the type outright.
        writer.set(&format!(r"Software\Classes\{dotted}\OpenWithProgids"), PROGID, "")?;

        writer.set(&format!(r"{CAPABILITIES_KEY}\FileAssociations"), &dotted, PROGID)?;
        // An empty string is the documented value here; the name is the point.
        writer.set(&format!(r"{APPLICATION_KEY}\SupportedTypes"), &dotted, "")?;
    }

    // Registering under this key is what makes Windows show the capabilities
    // above in the default-apps UI.
    writer.set(r"Software\RegisteredApplications", "nitid", CAPABILITIES_KEY)?;

    for verb in &FOLDER_VERBS {
        writer.set(verb.key, "MUIVerb", "Open with nitid")?;
        writer.set(verb.key, "Icon", &icon)?;
        if verb.single {
            writer.set(verb.key, "MultiSelectModel", "Single")?;
        }
        writer.set(&format!(r"{}\command", verb.key), "", &verb_command(&exe, verb.argument))?;
    }

    if asked == Asked::Install {
        writer.set(RUN_KEY, RUN_VALUE, &resident_command(&exe))?;
    }

    if writer.changed {
        announce_association_change();
    }
    Ok(())
}

/// Tell Explorer the registration changed.
///
/// Without it a window already open keeps the menu and the icons it read
/// before; with it they are read again at once. Best effort, like the PATH's
/// announcement: the registry is right either way.
fn announce_association_change() {
    use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    if !sandboxed() {
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }
}

/// Undo everything `register` wrote.
///
/// Failures are collected rather than propagated one at a time: a half-removed
/// registration is worse than a reported error, so every key is attempted.
fn unregister() -> Result<()> {
    let mut failures = Vec::new();
    let hive = hive()?;

    // An install upgraded from an older version may still carry its keys.
    remove_legacy_keys()?;

    for extension in supported_extensions() {
        let path = format!(r"Software\Classes\.{extension}\OpenWithProgids");
        // Removing a value needs write access; the plain `open` is read-only,
        // and asking for less than the operation needs fails with a bare
        // "access denied" that looks like a permissions problem.
        let Ok(key) = hive.options().read().write().open(&path) else {
            // No such key means nothing of ours is registered there.
            continue;
        };
        // A value that was never written is not a failure to report.
        if key.get_type(PROGID).is_ok()
            && let Err(error) = key.remove_value(PROGID)
        {
            failures.push(format!("{path}: {error}"));
        }
    }

    let trees = [
        format!(r"Software\Classes\{PROGID}"),
        APPLICATION_KEY.to_string(),
        r"Software\lacodda\nitid".to_string(),
        FOLDER_VERBS[0].key.to_string(),
        FOLDER_VERBS[1].key.to_string(),
    ];
    for tree in trees {
        if hive.open(&tree).is_ok()
            && let Err(error) = hive.remove_tree(&tree)
        {
            failures.push(format!("{tree}: {error}"));
        }
    }

    if let Ok(registered) = hive.options().read().write().open(r"Software\RegisteredApplications")
        && registered.get_string("nitid").is_ok()
        && let Err(error) = registered.remove_value("nitid")
    {
        failures.push(format!("RegisteredApplications: {error}"));
    }

    if let Ok(run) = hive.options().read().write().open(RUN_KEY)
        && run.get_string(RUN_VALUE).is_ok()
        && let Err(error) = run.remove_value(RUN_VALUE)
    {
        failures.push(format!("the sign-in entry: {error}"));
    }

    if !failures.is_empty() {
        bail!("some registry entries could not be removed:\n  {}", failures.join("\n  "));
    }

    announce_association_change();
    Ok(())
}

/// The name the shortcut carries on the desktop.
///
/// A constant because two functions must agree on it: one writes the file and
/// the other deletes it, and a rename in one place would leave the other
/// removing nothing while reporting success.
const SHORTCUT_NAME: &str = "nitid.lnk";

/// Where the shortcut goes.
///
/// The desktop is asked for rather than assembled from the profile directory:
/// it is a known folder and can be moved (OneDrive redirects it, and this
/// machine's is redirected), so `%USERPROFILE%\Desktop` is a guess that is
/// wrong exactly on the machines where it matters.
fn desktop_dir() -> Result<PathBuf> {
    use windows::Win32::UI::Shell::{FOLDERID_Desktop, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

    let wide = unsafe { SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None) }.context("asking Windows where the desktop is")?;
    let path = unsafe { wide.to_string() }.context("reading the desktop path")?;
    // The shell allocated this; the caller frees it.
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(wide.0 as *const _)) };
    Ok(PathBuf::from(path))
}

/// Put a shortcut to the viewer on the desktop.
///
/// `Ok(None)` when one is already there: an install that runs twice should not
/// leave "nitid (2)", and overwriting a shortcut the user may have renamed or
/// moved would undo a choice they made.
///
/// The windowed executable is the target, not the console one — a shortcut
/// that flashes a console window on every launch is a shortcut people delete
/// (ADR 0004).
fn create_desktop_shortcut(target: &Path) -> Result<Option<PathBuf>> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface};

    // The desktop has no corner for a test to keep to.
    if sandboxed() {
        return Ok(None);
    }
    let link_path = desktop_dir()?.join(SHORTCUT_NAME);
    if link_path.exists() {
        // Left where it is, but given the taskbar's name if it is ours and
        // was made before it had one (v0.42 and earlier).
        name_desktop_shortcut(target)?;
        return Ok(None);
    }

    // This runs in a short-lived command, so the apartment is entered and left
    // here rather than at startup. A failure means COM is already initialised
    // in a different mode, which the work below survives.
    let initialised = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let result = (|| -> Result<()> {
        let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }.context("creating the shortcut object")?;

        unsafe { link.SetPath(&HSTRING::from(target.as_os_str())) }.context("setting the shortcut's target")?;
        if let Some(dir) = target.parent() {
            unsafe { link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str())) }.context("setting the shortcut's working directory")?;
        }
        unsafe { link.SetDescription(&HSTRING::from("A fast image viewer with honest colour and HDR")) }.context("describing the shortcut")?;
        // Index 0 of the executable's own resources, which is the mark the
        // rest of the shell already shows for it.
        unsafe { link.SetIconLocation(&HSTRING::from(target.as_os_str()), 0) }.context("setting the shortcut's icon")?;
        // The name the viewer's windows go by, so the shortcut pinned to the
        // taskbar is the same button as the window it opens (`taskbar`).
        crate::taskbar::name_link(&link)?;

        let file: IPersistFile = link.cast().context("the shortcut cannot be saved")?;
        unsafe { file.Save(&HSTRING::from(link_path.as_os_str()), true) }.context("writing the shortcut")?;
        Ok(())
    })();

    if initialised {
        unsafe { CoUninitialize() };
    }
    result.map(|()| Some(link_path))
}

/// Give the desktop shortcut the taskbar's name, if it is there and points at
/// `target`.
///
/// For a shortcut made before v0.43, when the viewer had no name of its own:
/// pinned, it would be one button and the window it opens another. Nothing
/// else about it is touched — where it is and what it is called are the
/// person's — and one pointing somewhere else is not ours to change.
fn name_desktop_shortcut(target: &Path) -> Result<()> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READWRITE};
    use windows::Win32::UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink};
    use windows::core::{HSTRING, Interface};

    if sandboxed() {
        return Ok(());
    }
    let link_path = desktop_dir()?.join(SHORTCUT_NAME);
    if !link_path.exists() {
        return Ok(());
    }
    let _com = crate::com::Com::begin();
    // SAFETY: COM calls on an object created here, on this thread.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).context("creating the shortcut object")?;
        let file: IPersistFile = link.cast().context("the shortcut cannot be read")?;
        file.Load(&HSTRING::from(link_path.as_os_str()), STGM_READWRITE)
            .context("reading the desktop shortcut")?;
        let mut path = [0u16; 1024];
        link.GetPath(&mut path, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
            .context("reading where the shortcut points")?;
        let length = path.iter().position(|unit| *unit == 0).unwrap_or(path.len());
        let points_at = PathBuf::from(String::from_utf16_lossy(&path[..length]));
        if !same_file(&points_at, target) {
            return Ok(());
        }
        crate::taskbar::name_link(&link)?;
        // Saved back to the file it was read from.
        file.Save(windows::core::PCWSTR::null(), true).context("writing the desktop shortcut")?;
    }
    Ok(())
}

/// Take the shortcut away again.
///
/// Only the one this installed: a file of that name is removed, and a shortcut
/// the user made themselves somewhere else is theirs to keep.
fn remove_desktop_shortcut() -> Result<bool> {
    if sandboxed() {
        return Ok(false);
    }
    let link_path = desktop_dir()?.join(SHORTCUT_NAME);
    if !link_path.exists() {
        return Ok(false);
    }
    fs::remove_file(&link_path).with_context(|| format!("removing {}", link_path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_install_directory_is_per_user() {
        let dir = install_dir().expect("LOCALAPPDATA is set on Windows");
        assert!(dir.ends_with(r"Programs\nitid"), "{}", dir.display());
    }

    #[test]
    fn a_path_is_the_same_file_as_itself_whatever_its_case() {
        let exe = env::current_exe().unwrap();
        let shouted = PathBuf::from(exe.to_string_lossy().to_uppercase());
        assert!(same_file(&exe, &shouted));
    }

    #[test]
    fn different_paths_are_not_the_same_file() {
        assert!(!same_file(Path::new(r"C:\a\nitid.exe"), Path::new(r"C:\b\nitid.exe")));
    }

    /// The whole point of the patch: after an install the directory is on the
    /// PATH, so `nitid` is a command a terminal can find.
    #[test]
    fn the_install_directory_joins_the_path() {
        let dir = Path::new(r"C:\Users\a\AppData\Local\Programs\nitid");
        let updated = path_with(r"C:\Windows;C:\Windows\System32", dir).expect("the directory was not added");

        assert!(updated.ends_with(r"\nitid"), "{updated}");
        assert!(
            updated.starts_with(r"C:\Windows;C:\Windows\System32;"),
            "the existing PATH was disturbed: {updated}"
        );
    }

    /// An upgrade runs `install` again. Adding the directory a second time is
    /// how a PATH collects one copy of itself per release.
    #[test]
    fn installing_twice_does_not_add_the_directory_twice() {
        let dir = Path::new(r"C:\Programs\nitid");
        let once = path_with(r"C:\Windows", dir).expect("the first install added nothing");
        assert_eq!(path_with(&once, dir), None, "a second install added the directory again");
    }

    /// Windows treats these as one directory, so a PATH already carrying any
    /// spelling of it must not gain another.
    #[test]
    fn a_directory_already_on_the_path_is_recognised_however_it_is_spelt() {
        let dir = Path::new(r"C:\Programs\nitid");
        for spelling in [r"C:\Programs\nitid", r"c:\programs\NITID", r"C:\Programs\nitid\", r" C:\Programs\nitid "] {
            assert_eq!(
                path_with(&format!(r"C:\Windows;{spelling};C:\Other"), dir),
                None,
                "{spelling} was not recognised as the install directory",
            );
        }
    }

    /// Set means yes, whatever it is set to; unset or empty means the install
    /// a person expects.
    #[test]
    fn the_path_is_left_alone_only_when_asked() {
        use std::ffi::OsStr;
        assert!(!leaves_path_alone(None));
        assert!(!leaves_path_alone(Some(OsStr::new(""))));
        assert!(leaves_path_alone(Some(OsStr::new("1"))));
        assert!(leaves_path_alone(Some(OsStr::new("true"))));
    }

    /// A PATH is the user's, and most of it belongs to other programs. The
    /// unexpanded entry is the one that matters: it is why the value has to go
    /// back as `REG_EXPAND_SZ`, and it must survive the edit untouched.
    #[test]
    fn adding_and_removing_leave_every_other_entry_alone() {
        let dir = Path::new(r"C:\Programs\nitid");
        let original = r"C:\Windows;%JAVA_HOME%\bin;C:\Program Files\Git\cmd";

        let added = path_with(original, dir).expect("nothing was added");
        assert!(added.contains(r"%JAVA_HOME%\bin"), "an unexpanded entry was mangled: {added}");

        let removed = path_without(&added, dir).expect("nothing was removed");
        assert_eq!(removed, original, "a round trip through the PATH changed it");
    }

    /// Uninstalling takes back exactly what installing added, and nothing when
    /// there is nothing of ours there.
    #[test]
    fn uninstalling_removes_the_directory_and_only_it() {
        let dir = Path::new(r"C:\Programs\nitid");
        assert_eq!(path_without(r"C:\Windows;C:\Other", dir), None, "a PATH without us was rewritten anyway");

        // Every spelling goes, including a duplicate an earlier version left.
        let crowded = r"C:\Windows;C:\Programs\nitid;C:\Other;c:\programs\nitid\";
        assert_eq!(path_without(crowded, dir), Some(r"C:\Windows;C:\Other".to_string()));
    }

    /// A user who has never had a per-user PATH gets one with just this in it,
    /// rather than a leading separator and an empty first entry.
    #[test]
    fn an_empty_path_becomes_the_directory_alone() {
        let dir = Path::new(r"C:\Programs\nitid");
        assert_eq!(path_with("", dir), Some(r"C:\Programs\nitid".to_string()));
        assert_eq!(path_with(";", dir), Some(r"C:\Programs\nitid".to_string()));
    }

    /// A PATH ending in a separator is common and must not grow an empty entry.
    #[test]
    fn a_trailing_separator_does_not_become_an_empty_entry() {
        let dir = Path::new(r"C:\Programs\nitid");
        let updated = path_with(r"C:\Windows;", dir).expect("nothing was added");
        assert_eq!(updated, r"C:\Windows;C:\Programs\nitid");
        assert!(!updated.contains(";;"), "an empty entry crept in: {updated}");
    }

    /// The menu entries hand the viewer the folder, quoted: a folder named
    /// with a space is the ordinary case, and an unquoted one would arrive as
    /// two paths, neither of them the folder.
    #[test]
    fn the_folder_menu_hands_over_the_folder_quoted() {
        let exe = r"C:\Users\a b\AppData\Local\Programs\nitid\nitidw.exe";
        let [on_folder, inside] = &FOLDER_VERBS;
        assert_eq!(verb_command(exe, on_folder.argument), format!("\"{exe}\" \"%1\""));
        // The background of a folder has no item to name; `%V` is the folder
        // itself, where `%1` would be empty.
        assert_eq!(verb_command(exe, inside.argument), format!("\"{exe}\" \"%V\""));
        assert!(on_folder.key.ends_with(r"Directory\shell\nitid"));
        assert!(inside.key.ends_with(r"Directory\Background\shell\nitid"));
        assert!(on_folder.single && !inside.single);
    }

    /// The sign-in entry starts the resident, and only the resident: an
    /// entry that started the viewer would open a window at every sign-in.
    #[test]
    fn the_sign_in_entry_starts_the_resident() {
        let exe = r"C:\Users\a b\nitidw.exe";
        assert_eq!(resident_command(exe), format!("\"{exe}\" --resident"));
        assert!(matches!(
            crate::parse(["--resident"].into_iter().map(std::ffi::OsString::from)),
            crate::Command::Resident
        ));
    }

    /// The registration covers exactly what the build can open, so a version
    /// that adds a decoder registers it without anyone editing a second list.
    #[test]
    fn every_supported_extension_is_lowercase_and_undotted() {
        for extension in supported_extensions() {
            assert!(!extension.starts_with('.'), "{extension} carries a dot");
            assert_eq!(extension, extension.to_lowercase(), "{extension} is not lowercase");
        }
    }

    /// A pair of files standing in for the executables, each saying `word`.
    fn pair(dir: &Path, word: &str) {
        for name in EXECUTABLES {
            fs::write(dir.join(name), format!("{name} {word}")).unwrap();
        }
    }

    fn says(dir: &Path, name: &str) -> String {
        fs::read_to_string(dir.join(name)).unwrap_or_default()
    }

    /// A swap puts both in place and keeps what it replaced within reach.
    #[test]
    fn a_swap_replaces_both_and_undoing_it_brings_both_back() {
        let (new, installed) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        pair(new.path(), "new");
        pair(installed.path(), "old");

        let swap = Swap::of(new.path(), installed.path()).expect("a swap");
        for name in EXECUTABLES {
            assert_eq!(says(installed.path(), name), format!("{name} new"));
        }

        swap.undo();
        for name in EXECUTABLES {
            assert_eq!(says(installed.path(), name), format!("{name} old"), "undoing did not bring {name} back");
        }
    }

    /// A release short of one executable touches nothing: half a program is
    /// what the swap exists to rule out.
    #[test]
    fn a_release_missing_an_executable_changes_nothing() {
        let (new, installed) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::write(new.path().join(EXECUTABLES[0]), "only one").unwrap();
        pair(installed.path(), "old");

        assert!(Swap::of(new.path(), installed.path()).is_err());

        for name in EXECUTABLES {
            assert_eq!(says(installed.path(), name), format!("{name} old"));
        }
        let left: Vec<String> = fs::read_dir(installed.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), 2, "something was left beside them: {left:?}");
    }

    /// The second file failing to land is the case that would leave half a
    /// program, so the first one is walked back. A directory holding an open
    /// file sits where the second one goes, because it can be neither retired
    /// nor replaced.
    #[test]
    fn a_swap_that_fails_halfway_leaves_the_pair_that_was_there() {
        let (new, installed) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        pair(new.path(), "new");
        pair(installed.path(), "old");
        let second = installed.path().join(EXECUTABLES[1]);
        fs::remove_file(&second).unwrap();
        fs::create_dir(&second).unwrap();
        let held = second.join("held");
        let holding = fs::File::create(&held).unwrap();

        assert!(
            Swap::of(new.path(), installed.path()).is_err(),
            "a swap with its second place taken went through"
        );

        assert_eq!(
            says(installed.path(), EXECUTABLES[0]),
            format!("{} old", EXECUTABLES[0]),
            "the first file was left replaced"
        );
        assert!(
            !installed.path().join(format!("{}.new", EXECUTABLES[0])).exists(),
            "an incoming copy was left behind"
        );
        drop(holding);
    }

    /// A copy that lands and does not run is taken out again: a program
    /// nothing can start, put there with nobody watching, would stay until
    /// someone ran the script by hand. These files are not programs, which is
    /// all the check needs.
    #[test]
    fn a_copy_that_does_not_run_is_taken_out_again() {
        let (new, installed) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        pair(new.path(), "not a program");
        pair(installed.path(), "old");

        let error = put_in_place(new.path(), installed.path()).expect_err("a copy that cannot run was kept");

        assert!(format!("{error:#}").contains("put back"), "{error:#}");
        for name in EXECUTABLES {
            assert_eq!(says(installed.path(), name), format!("{name} old"), "{name} was not put back");
        }
    }

    /// The same bytes twice is an update with nothing to do. Reporting a
    /// change would announce a new version that is not one.
    #[test]
    fn the_same_copy_again_changes_nothing() {
        let (new, installed) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        pair(new.path(), "same");
        pair(installed.path(), "same");
        assert!(!put_in_place(new.path(), installed.path()).expect("nothing to do"));
    }

    /// A window left open across updates keeps its retired file locked, and
    /// the next update has to find another name. A file held open stands in
    /// for the image a running process holds.
    #[test]
    fn a_retired_file_still_held_does_not_stop_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nitid.exe");

        fs::write(&target, "first").unwrap();
        let first = retire(&target).expect("retiring the first");
        let holding = deny_deletion(&first);

        fs::write(&target, "second").unwrap();
        let second = retire(&target).expect("a held file stopped the next one from retiring");

        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(&second).unwrap(), "second");
        drop(holding);
    }

    /// A file opened the way the system holds an image: readable, and not
    /// deletable or replaceable until closed.
    fn deny_deletion(path: &Path) -> fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .expect("holding a file")
    }

    /// Everything retired goes when it can, in any spelling a version left,
    /// and nothing else in the directory is touched.
    #[test]
    fn sweeping_takes_what_was_retired_and_only_that() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "nitid.exe.old",
            "nitid.exe.7.old",
            "nitidw.exe.old",
            "nitidw.exe.new",
            "nitid.exe",
            "nitidw.exe",
            "mine.old",
            "notes.txt",
        ] {
            fs::write(dir.path().join(name), name).unwrap();
        }
        let held = dir.path().join("nitid.exe.2.old");
        fs::write(&held, "running").unwrap();
        let holding = deny_deletion(&held);

        sweep_retired_in(dir.path());

        let mut left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["mine.old", "nitid.exe", "nitid.exe.2.old", "nitidw.exe", "notes.txt"]);
        drop(holding);
    }

    /// `--version` is what a copy is believed by, so it must be answered by
    /// running the file and must not take a wedged one's word for ever.
    #[test]
    fn what_cannot_run_says_no_version() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("nitid.exe");
        fs::write(&fake, "not a program").unwrap();
        assert!(version_said_by(&fake).is_err());
        assert!(version_said_by(&dir.path().join("absent.exe")).is_err());
    }
}

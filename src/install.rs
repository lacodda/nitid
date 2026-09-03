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
use windows_registry::{CURRENT_USER, Type};

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
const EXECUTABLES: [&str; 2] = ["nitid.exe", "nitidw.exe"];

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

/// Where the per-user `PATH` lives.
///
/// The user hive, not the machine one: an install that needs no administrator
/// has no business editing the PATH every account on the machine shares.
const ENVIRONMENT_KEY: &str = "Environment";

/// Install this executable for the current user.
///
/// Returns the directory it was installed into.
pub fn install() -> Result<PathBuf> {
    let source = env::current_exe().context("locating the running executable")?;
    let source_dir = source.parent().context("the running executable has no directory")?;
    let target_dir = install_dir()?;

    fs::create_dir_all(&target_dir).with_context(|| format!("creating {}", target_dir.display()))?;

    if same_file(source_dir, &target_dir) {
        // Installing from the installed copy: the registry work below still
        // runs, which is how a re-register repairs a broken association.
        println!("nitid is already installed in {}", target_dir.display());
    } else {
        for name in EXECUTABLES {
            let from = source_dir.join(name);
            if !from.exists() {
                // A zip that carries only one of the pair is not something to
                // half-install: the association would point at a missing file.
                bail!("{} is missing from {}; both executables must be installed together", name, source_dir.display());
            }
            copy_over(&from, &target_dir.join(name))?;
        }
        println!("Copied nitid to {}", target_dir.display());
    }

    remove_legacy_keys();
    register(&target_dir.join(WINDOWED_EXE))?;

    let extensions = supported_extensions();
    println!("Registered {} file types: {}", extensions.len(), extensions.join(", "));

    // Reported, never fatal: a viewer the shell can open pictures with is
    // installed even if its directory could not reach the PATH.
    match add_to_path(&target_dir) {
        Ok(true) => println!("Added {} to your PATH", target_dir.display()),
        Ok(false) => {}
        Err(error) => eprintln!("nitid: could not add {} to your PATH: {error:#}", target_dir.display()),
    }

    // Likewise: a desktop without a shortcut is an inconvenience, not a
    // failed install.
    match create_desktop_shortcut(&target_dir.join(WINDOWED_EXE)) {
        Ok(Some(path)) => println!("Put a shortcut on your desktop: {}", path.display()),
        Ok(None) => {}
        Err(error) => eprintln!("nitid: could not create the desktop shortcut: {error:#}"),
    }

    println!();
    println!("nitid is now offered in \"Open with\". Windows reserves the choice");
    println!("of default application for you: right-click an image, choose");
    println!("\"Open with\" > \"Choose another app\", pick nitid and tick");
    println!("\"Always use this app\". Settings > Apps > Default apps works too.");

    Ok(target_dir)
}

/// Remove the registration and the installed copy.
pub fn uninstall() -> Result<()> {
    unregister()?;
    println!("Removed the file type registration");

    let dir = install_dir()?;
    match remove_from_path(&dir) {
        Ok(true) => println!("Removed {} from your PATH", dir.display()),
        Ok(false) => {}
        Err(error) => eprintln!("nitid: could not remove {} from your PATH: {error:#}", dir.display()),
    }

    match remove_desktop_shortcut() {
        Ok(true) => println!("Removed the desktop shortcut"),
        Ok(false) => {}
        Err(error) => eprintln!("nitid: could not remove the desktop shortcut: {error:#}"),
    }

    // The decoder's AppContainer profile is machine state nitid registered on
    // its first sandboxed decode; uninstalling takes it too. A machine that
    // never decoded a HEIC has nothing to remove, which is not an error.
    if crate::sandbox::remove_container_profile().is_ok() {
        println!("Removed the decoder's container profile");
    }

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
            // the way lets the next install write a clean copy, and Windows
            // removes the stale name once the process exits.
            let stale = target.with_extension("exe.old");
            let _ = fs::remove_file(&stale);
            fs::rename(&target, &stale).with_context(|| format!("retiring {}", target.display()))?;
            deferred = true;
        } else {
            fs::remove_file(&target).with_context(|| format!("removing {}", target.display()))?;
            let _ = fs::remove_file(target.with_extension("exe.old"));
            removed = true;
        }
    }

    if !removed && !deferred {
        println!("Nothing installed in {}", dir.display());
        return Ok(());
    }

    if deferred {
        println!("Marked the running executable for removal on exit");
    }
    // Only succeeds once the directory is empty, which is the point.
    let _ = fs::remove_dir(&dir);
    println!("Removed {}", dir.display());

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
    let environment = CURRENT_USER
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
    let Ok(environment) = CURRENT_USER.options().read().write().open(ENVIRONMENT_KEY) else {
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

/// Copy the executable, retiring a copy that is currently running.
fn copy_over(source: &Path, target: &Path) -> Result<()> {
    let stale = target.with_extension("exe.old");

    if target.exists() {
        // Windows locks a running image, so an in-place overwrite fails while
        // an older nitid is open. Renaming succeeds even then, which is what
        // lets an upgrade land without closing the viewer first.
        let _ = fs::remove_file(&stale);
        fs::rename(target, &stale).with_context(|| format!("replacing {}", target.display()))?;
    }

    fs::copy(source, target).with_context(|| format!("copying to {}", target.display()))?;

    // The retired copy is deletable once the process holding it has exited, so
    // this succeeds on the next install rather than the one that created it.
    let _ = fs::remove_file(&stale);
    Ok(())
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
fn remove_legacy_keys() {
    for key in LEGACY_APPLICATION_KEYS {
        if CURRENT_USER.open(key).is_ok() {
            let _ = CURRENT_USER.remove_tree(key);
        }
    }
}

/// Write every registry key the shell reads.
fn register(exe: &Path) -> Result<()> {
    let exe = exe.to_string_lossy().to_string();
    let open_command = format!("\"{exe}\" \"%1\"");

    // The ProgID: what the shell shows and how it launches us.
    let progid = CURRENT_USER.create(format!(r"Software\Classes\{PROGID}")).context("creating the ProgID key")?;
    progid.set_string("", "Image")?;
    progid.set_string("FriendlyTypeName", "Image")?;
    CURRENT_USER
        .create(format!(r"Software\Classes\{PROGID}\DefaultIcon"))?
        .set_string("", format!("\"{exe}\",0"))?;
    CURRENT_USER
        .create(format!(r"Software\Classes\{PROGID}\shell\open\command"))?
        .set_string("", &open_command)?;

    // The application entry: this is what "Open with" lists.
    let application = CURRENT_USER.create(APPLICATION_KEY).context("creating the application key")?;
    application.set_string("FriendlyAppName", "nitid")?;
    CURRENT_USER
        .create(format!(r"{APPLICATION_KEY}\shell\open\command"))?
        .set_string("", &open_command)?;

    // Capabilities: what puts nitid in Settings > Default apps.
    let capabilities = CURRENT_USER.create(CAPABILITIES_KEY).context("creating the capabilities key")?;
    capabilities.set_string("ApplicationName", "nitid")?;
    capabilities.set_string("ApplicationDescription", "A fast image viewer with honest color")?;

    let associations = CURRENT_USER.create(format!(r"{CAPABILITIES_KEY}\FileAssociations"))?;
    let supported = CURRENT_USER.create(format!(r"{APPLICATION_KEY}\SupportedTypes"))?;

    for extension in supported_extensions() {
        let dotted = format!(".{extension}");

        // Offer nitid for the type without seizing it: `OpenWithProgids` adds
        // an entry to the "Open with" list, whereas writing the key's default
        // value would claim the type outright.
        CURRENT_USER
            .create(format!(r"Software\Classes\{dotted}\OpenWithProgids"))
            .with_context(|| format!("registering {dotted}"))?
            .set_string(PROGID, "")?;

        associations.set_string(&dotted, PROGID)?;
        // An empty string is the documented value here; the name is the point.
        supported.set_string(&dotted, "")?;
    }

    // Registering under this key is what makes Windows show the capabilities
    // above in the default-apps UI.
    CURRENT_USER
        .create(r"Software\RegisteredApplications")
        .context("registering the application")?
        .set_string("nitid", CAPABILITIES_KEY)?;

    Ok(())
}

/// Undo everything `register` wrote.
///
/// Failures are collected rather than propagated one at a time: a half-removed
/// registration is worse than a reported error, so every key is attempted.
fn unregister() -> Result<()> {
    let mut failures = Vec::new();

    // An install upgraded from an older version may still carry its keys.
    remove_legacy_keys();

    for extension in supported_extensions() {
        let path = format!(r"Software\Classes\.{extension}\OpenWithProgids");
        // Removing a value needs write access; the plain `open` is read-only,
        // and asking for less than the operation needs fails with a bare
        // "access denied" that looks like a permissions problem.
        let Ok(key) = CURRENT_USER.options().read().write().open(&path) else {
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
    ];
    for tree in trees {
        if CURRENT_USER.open(&tree).is_ok()
            && let Err(error) = CURRENT_USER.remove_tree(&tree)
        {
            failures.push(format!("{tree}: {error}"));
        }
    }

    if let Ok(registered) = CURRENT_USER.options().read().write().open(r"Software\RegisteredApplications")
        && registered.get_string("nitid").is_ok()
        && let Err(error) = registered.remove_value("nitid")
    {
        failures.push(format!("RegisteredApplications: {error}"));
    }

    if !failures.is_empty() {
        bail!("some registry entries could not be removed:\n  {}", failures.join("\n  "));
    }

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

    let link_path = desktop_dir()?.join(SHORTCUT_NAME);
    if link_path.exists() {
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

        let file: IPersistFile = link.cast().context("the shortcut cannot be saved")?;
        unsafe { file.Save(&HSTRING::from(link_path.as_os_str()), true) }.context("writing the shortcut")?;
        Ok(())
    })();

    if initialised {
        unsafe { CoUninitialize() };
    }
    result.map(|()| Some(link_path))
}

/// Take the shortcut away again.
///
/// Only the one this installed: a file of that name is removed, and a shortcut
/// the user made themselves somewhere else is theirs to keep.
fn remove_desktop_shortcut() -> Result<bool> {
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

    /// The registration covers exactly what the build can open, so a version
    /// that adds a decoder registers it without anyone editing a second list.
    #[test]
    fn every_supported_extension_is_lowercase_and_undotted() {
        for extension in supported_extensions() {
            assert!(!extension.starts_with('.'), "{extension} carries a dot");
            assert_eq!(extension, extension.to_lowercase(), "{extension} is not lowercase");
        }
    }
}

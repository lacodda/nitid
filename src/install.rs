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
use windows_registry::CURRENT_USER;

use crate::image_source::SUPPORTED_EXTENSIONS;

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

/// The registered-applications entry that puts nitid in Settings.
const CAPABILITIES_KEY: &str = r"Software\lacodda\nitid\Capabilities";

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

    register(&target_dir.join(WINDOWED_EXE))?;

    println!("Registered {} file types: {}", SUPPORTED_EXTENSIONS.len(), SUPPORTED_EXTENSIONS.join(", "));
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

    for extension in SUPPORTED_EXTENSIONS {
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

    for extension in SUPPORTED_EXTENSIONS {
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

    /// The registration covers exactly what the build can open, so a version
    /// that adds a decoder registers it without anyone editing a second list.
    #[test]
    fn every_supported_extension_is_lowercase_and_undotted() {
        for extension in SUPPORTED_EXTENSIONS {
            assert!(!extension.starts_with('.'), "{extension} carries a dot");
            assert_eq!(*extension, extension.to_lowercase(), "{extension} is not lowercase");
        }
    }
}

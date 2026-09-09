//! Doing something to the file the picture came from: the recycle bin, a new
//! name, a folder it belongs in.
//!
//! A viewer that can only look is half a tool. Going through a shoot means
//! throwing some frames away, naming the keepers, and putting them where they
//! belong — and doing that in a file manager means leaving the picture to look
//! at a list of names, which is the one view that cannot answer "is this the
//! good one".
//!
//! **Everything here goes through the shell's own file operations**
//! (`IFileOperation`), never `std::fs`. Three things follow from that and none
//! of them could be had otherwise: a delete lands in the recycle bin instead
//! of being gone, the operation joins the shell's undo stack so `Ctrl+Z` in
//! Explorer takes it back, and a name that collides is resolved the way the
//! rest of Windows resolves it rather than by this viewer's guess. A viewer
//! that deleted with `fs::remove_file` would be a viewer that loses
//! photographs, whatever it said in a confirmation dialog.
//!
//! The operations are synchronous. They are single files on a local disk,
//! measured in milliseconds, and a background thread would buy nothing but a
//! window that has to be told what happened to a file it may already have
//! stepped away from.

use std::path::{Path, PathBuf};

use anyhow::Result;
#[cfg(windows)]
use anyhow::{Context, bail};

/// What became of the file.
///
/// The path matters to the caller: after a rename or a move the folder is
/// holding a name that is no longer there, and it has to be told what replaced
/// it — or, for a delete, that nothing did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The file is in the recycle bin; nothing stands where it was.
    Recycled,
    /// The file is still here under a new name, or in a new place.
    At(PathBuf),
    /// A copy was made and the original is untouched.
    Copied(PathBuf),
}

/// Send a file to the recycle bin.
///
/// The bin rather than oblivion, and not as a courtesy: pressing a key next to
/// the arrow keys while going quickly through a folder is a mistake everyone
/// makes, and the difference between a viewer you can cull with and one you
/// cannot is whether that mistake is recoverable. `FOFX_RECYCLEONDELETE` asks
/// for the bin, `FOF_ALLOWUNDO` puts it on the shell's undo stack.
///
/// A file the bin will not take — too large for it, or on a volume without one
/// — is **not** deleted instead. Windows would silently do exactly that, and a
/// viewer whose delete sometimes means "for ever" is worse than one that
/// cannot delete at all: `FOFX_EARLYFAILURE` turns that case into an error the
/// caller can report.
#[cfg(windows)]
pub fn recycle(path: &Path) -> Result<Outcome> {
    use windows::Win32::UI::Shell::{FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE};

    operate(
        |operation, shell| {
            unsafe { operation.SetOperationFlags(FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOFX_RECYCLEONDELETE | FOFX_EARLYFAILURE) }
                .context("asking for the recycle bin")?;
            unsafe { operation.DeleteItem(shell, None) }.context("queueing the delete")
        },
        path,
    )?;

    Ok(Outcome::Recycled)
}

/// Give the file a new name, in the folder it is already in.
///
/// The name only — a rename that could also move the file is a different
/// operation with a different risk, and typing a path into what looks like a
/// name box should not silently relocate a photograph. A name carrying a
/// separator is refused here rather than handed to the shell.
#[cfg(windows)]
pub fn rename(path: &Path, name: &str) -> Result<Outcome> {
    use windows::core::HSTRING;

    let name = name.trim();
    if name.is_empty() {
        bail!("a file needs a name");
    }
    if name.contains(['/', '\\', ':']) {
        bail!("a name cannot carry a path");
    }
    // The characters Windows will not have in a name. Refused with the list
    // rather than with "invalid", because the person is looking at what they
    // typed and has to know which part to fix.
    if let Some(bad) = name.chars().find(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*')) {
        bail!("a name cannot contain {bad}");
    }

    let parent = path.parent().unwrap_or(Path::new(""));
    let renamed = parent.join(name);
    if renamed == path {
        // Nothing to do, and the shell would report success for a no-op —
        // which the caller would then announce as though something happened.
        return Ok(Outcome::At(renamed));
    }
    if renamed.exists() {
        bail!("{name} is already there");
    }

    operate(
        |operation, shell| {
            // No `FOF_RENAMEONCOLLISION` here: the check above already said
            // the name is free, and letting the shell quietly land on
            // "photo (2).jpg" would rename the file to something other than
            // what was typed.
            unsafe { operation.RenameItem(shell, &HSTRING::from(name), None) }.context("queueing the rename")
        },
        path,
    )?;

    Ok(Outcome::At(renamed))
}

/// Move the file into `folder`.
///
/// The folder is made if it is not there: a destination configured once and
/// used months later should not fail because nothing has been put in it yet,
/// and the alternative is an error message about a folder the person thought
/// they had set up.
///
/// A name already taken in the destination is resolved by the shell, which
/// appends the same "(2)" the rest of Windows does — the file is not
/// overwritten, and the outcome names where it actually landed.
#[cfg(windows)]
pub fn move_to(path: &Path, folder: &Path) -> Result<Outcome> {
    let landed = transfer(path, folder, Transfer::Move)?;
    Ok(Outcome::At(landed))
}

/// Copy the file into `folder`, leaving the original where it is.
#[cfg(windows)]
pub fn copy_to(path: &Path, folder: &Path) -> Result<Outcome> {
    let landed = transfer(path, folder, Transfer::Copy)?;
    Ok(Outcome::Copied(landed))
}

#[cfg(windows)]
#[derive(Clone, Copy)]
enum Transfer {
    Move,
    Copy,
}

/// The half a move and a copy share: check the destination, run the operation,
/// and work out what the file ended up being called.
#[cfg(windows)]
fn transfer(path: &Path, folder: &Path, how: Transfer) -> Result<PathBuf> {
    use windows::Win32::UI::Shell::{FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_RENAMEONCOLLISION, IShellItem};
    use windows::core::HSTRING;

    if folder.as_os_str().is_empty() {
        bail!("no folder is set for that key");
    }
    // A destination inside itself, or the folder the file is already in.
    if path.parent() == Some(folder) {
        bail!("the file is already there");
    }
    std::fs::create_dir_all(folder).with_context(|| format!("making {}", folder.display()))?;

    let name = path.file_name().unwrap_or_default().to_os_string();
    // Whether the destination already holds this name, asked BEFORE the
    // operation. Afterwards the question is useless: the name is there either
    // way, and it belongs to whichever file got there first.
    let taken = folder.join(&name).exists();

    operate(
        |operation, shell| {
            // `FOF_RENAMEONCOLLISION` is what makes this safe to hold down: a
            // second frame of the same name lands beside the first as "(2)"
            // rather than replacing it. Culling a shoot must never be able to
            // overwrite a picture that was already sorted.
            unsafe { operation.SetOperationFlags(FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_RENAMEONCOLLISION) }.context("setting the operation's flags")?;
            let destination: IShellItem = unsafe { windows::Win32::UI::Shell::SHCreateItemFromParsingName(&HSTRING::from(folder.as_os_str()), None) }
                .with_context(|| format!("reaching {}", folder.display()))?;
            match how {
                // The fourth argument is a new name; `None` keeps the file's
                // own, which is what sorting into a folder means.
                Transfer::Move => unsafe { operation.MoveItem(shell, &destination, None, None) }.context("queueing the move"),
                Transfer::Copy => unsafe { operation.CopyItem(shell, &destination, None, None) }.context("queueing the copy"),
            }
        },
        path,
    )?;

    // Where it actually landed. With the name free, it is the name asked for.
    // With the name taken, the shell kept the file that was already there and
    // gave this one a name of its own — and asking "does the wanted name
    // exist" afterwards would answer yes about somebody else's file.
    let wanted = folder.join(&name);
    if !taken {
        return Ok(wanted);
    }
    Ok(collided(folder, Path::new(&name)).unwrap_or(wanted))
}

/// Find the file the shell renamed out of a collision.
///
/// **The name it picks is localised** — "photo (2).jpg" on an English Windows,
/// "photo — копия.jpg" on a Russian one — which is exactly why this searches
/// rather than predicting. Measured, not assumed: the first attempt here
/// looked for the file under the name it went in as, and found the picture
/// that was already sorted.
///
/// Best effort, and it is allowed to fail: this only decides what a message
/// says and what the folder puts its cursor on. The folder is read and the
/// newest file whose name starts with the same stem wins.
#[cfg(windows)]
fn collided(folder: &Path, name: &Path) -> Option<PathBuf> {
    let stem = name.file_stem()?.to_str()?;
    let extension = name.extension().and_then(|e| e.to_str()).unwrap_or_default();

    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(folder).ok()?.flatten() {
        let candidate = entry.path();
        let Some(candidate_stem) = candidate.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let same_extension = candidate.extension().and_then(|e| e.to_str()).unwrap_or_default() == extension;
        // "photo (2)" for a file that went in as "photo".
        if !same_extension || !candidate_stem.starts_with(stem) || candidate_stem == stem {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(seen, _)| modified > *seen) {
            best = Some((modified, candidate));
        }
    }
    best.map(|(_, path)| path)
}

/// Run one operation on one file, inside its own COM apartment.
///
/// The apartment is entered and left around each operation rather than held
/// for the life of the viewer: these happen at human speed, once in a while,
/// and an apartment entered at startup would be one more thing on the path to
/// the first pixel.
#[cfg(windows)]
fn operate<F>(queue: F, path: &Path) -> Result<()>
where
    F: FnOnce(&windows::Win32::UI::Shell::IFileOperation, &windows::Win32::UI::Shell::IShellItem) -> Result<()>,
{
    use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
    use windows::Win32::UI::Shell::{FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName};
    use windows::core::HSTRING;

    // The path has to be absolute: the shell resolves a relative one against
    // its own idea of the current directory, which is not this process's.
    let path = std::path::absolute(path).with_context(|| format!("resolving {}", path.display()))?;
    if !path.exists() {
        bail!("{} is not there any more", path.display());
    }

    let initialised = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let result = (|| -> Result<()> {
        let operation: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }.context("starting a file operation")?;
        let item: IShellItem =
            unsafe { SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None) }.with_context(|| format!("reaching {}", path.display()))?;

        queue(&operation, &item)?;
        unsafe { operation.PerformOperations() }.context("the operation did not go through")?;

        // A queued operation can be skipped without failing — the shell
        // reports that separately, and without this check a file that was
        // never touched would be announced as moved.
        if unsafe { operation.GetAnyOperationsAborted() }.unwrap_or_default().as_bool() {
            bail!("the operation was stopped");
        }
        Ok(())
    })();

    if initialised {
        unsafe { CoUninitialize() };
    }
    result
}

/// Elsewhere, these are not offered.
///
/// The same shape as the clipboard's counterpart, and for the same reason: the
/// Linux build exists to keep the core portable (winit and wgpu run there), and
/// it must compile rather than carry a hole where a caller names a function
/// that is not there. The message says the feature is a Windows one instead of
/// failing silently — this viewer's file operations are the shell's own, and
/// there is no shell here to lend them.
#[cfg(not(windows))]
mod elsewhere {
    use std::path::Path;

    use anyhow::{Result, bail};

    use super::Outcome;

    pub fn recycle(_path: &Path) -> Result<Outcome> {
        bail!("the recycle bin is a Windows feature")
    }

    pub fn rename(_path: &Path, _name: &str) -> Result<Outcome> {
        bail!("renaming goes through the Windows shell")
    }

    pub fn move_to(_path: &Path, _folder: &Path) -> Result<Outcome> {
        bail!("sorting goes through the Windows shell")
    }

    pub fn copy_to(_path: &Path, _folder: &Path) -> Result<Outcome> {
        bail!("sorting goes through the Windows shell")
    }
}

#[cfg(not(windows))]
pub use elsewhere::{copy_to, move_to, recycle, rename};

#[cfg(test)]
#[cfg(windows)]
mod tests {
    use super::*;

    /// A folder of this test's own, under the system's temporary directory.
    ///
    /// Named after the test, so a failure leaves evidence that says which one
    /// left it, and so two tests running at once cannot touch each other's
    /// files.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("nitid-file-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("making the scratch folder");
        dir
    }

    fn file(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).expect("writing a test file");
        path
    }

    /// Recycled means **in the recycle bin**, not merely gone.
    ///
    /// "The file is not where it was" is what a plain delete achieves too, so
    /// checking only that would pass for the one implementation this module
    /// exists to rule out. The bin is asked how many items it holds, before
    /// and after: a real delete would leave that number alone.
    ///
    /// The count rather than the name, because a file in the bin is stored
    /// under a generated name and its original path is metadata; the count is
    /// the cheap question that still cannot be satisfied by deleting.
    #[test]
    fn a_recycled_file_goes_to_the_recycle_bin() {
        let dir = scratch("recycle");
        let path = file(&dir, "gone.txt", "x");

        let before = bin_count();
        assert_eq!(recycle(&path).expect("recycling"), Outcome::Recycled);
        assert!(!path.exists(), "the file is still where it was");

        // `None` means the bin could not be asked — a locked-down machine, or
        // a shell that will not answer. The rest of the test still holds, and
        // failing here would be reporting on the environment rather than on
        // this code.
        if let (Some(before), Some(after)) = (before, bin_count()) {
            assert_eq!(after, before + 1, "the file did not reach the recycle bin: {before} -> {after}");
        }
    }

    /// How many items the recycle bin holds, or `None` if it cannot be asked.
    ///
    /// Through the shell's own namespace, which is the only thing that knows:
    /// the bin is not one directory, and reading `$Recycle.Bin` off the disk
    /// would be reading an implementation detail that differs per volume.
    fn bin_count() -> Option<usize> {
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(New-Object -ComObject Shell.Application).NameSpace(10).Items().Count",
            ])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }

    #[test]
    fn a_renamed_file_is_the_same_file_under_another_name() {
        let dir = scratch("rename");
        let path = file(&dir, "before.txt", "the contents");

        let outcome = rename(&path, "after.txt").expect("renaming");
        assert_eq!(outcome, Outcome::At(dir.join("after.txt")));
        assert!(!path.exists(), "the old name is still there");
        assert_eq!(std::fs::read_to_string(dir.join("after.txt")).unwrap(), "the contents");
    }

    /// The check that stops a rename box from being a way to move a file
    /// somewhere else by accident.
    ///
    /// Each attempt gets its own folder and its own file, and each is checked
    /// for both halves of the promise: the call fails, **and** the file is
    /// still where it was. Sharing one file across the attempts hid the thing
    /// this is about — measured with the guard removed, the first attempt
    /// moved the file out of the folder, so the other two "failed" only
    /// because there was nothing left to rename, and the test passed while
    /// the viewer was relocating photographs.
    #[test]
    fn a_name_carrying_a_path_is_refused() {
        for (index, attempt) in ["..\\elsewhere.txt", "sub\\here.txt", "sub/here.txt", "C:\\here.txt", "C:here.txt"]
            .into_iter()
            .enumerate()
        {
            // Two levels deep, so ".." from the file's folder lands inside
            // this attempt's own scratch rather than in the directory every
            // test shares — the check below would otherwise be reading
            // another test's leavings.
            let root = scratch(&format!("rename-path-{index}"));
            let dir = root.join("inner");
            std::fs::create_dir_all(&dir).expect("making the inner folder");
            let path = file(&dir, "here.txt", "the contents");

            assert!(rename(&path, attempt).is_err(), "{attempt:?} was accepted as a name");
            assert!(path.exists(), "{attempt:?} was refused, but the file left anyway");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "the contents",
                "{attempt:?} was refused, but the file changed",
            );
            // And nothing was created next to the folder, which is where
            // "..\\elsewhere.txt" would land.
            let outside = root.join("elsewhere.txt");
            assert!(!outside.exists(), "{attempt:?} wrote outside the folder: {}", outside.display());
        }
    }

    #[test]
    fn an_empty_name_is_refused() {
        let dir = scratch("rename-empty");
        let path = file(&dir, "here.txt", "x");
        assert!(rename(&path, "   ").is_err());
        assert!(path.exists());
    }

    /// Renaming onto a name that is taken must not swallow the file that was
    /// already there.
    #[test]
    fn a_rename_onto_an_existing_name_is_refused() {
        let dir = scratch("rename-collision");
        let path = file(&dir, "one.txt", "one");
        file(&dir, "two.txt", "two");

        assert!(rename(&path, "two.txt").is_err(), "the rename went through");
        assert_eq!(std::fs::read_to_string(dir.join("two.txt")).unwrap(), "two", "the other file was overwritten");
        assert!(path.exists());
    }

    #[test]
    fn a_moved_file_is_in_the_new_folder_and_not_the_old() {
        let dir = scratch("move");
        let destination = dir.join("keepers");
        let path = file(&dir, "photo.txt", "pixels");

        let outcome = move_to(&path, &destination).expect("moving");
        assert_eq!(outcome, Outcome::At(destination.join("photo.txt")));
        assert!(!path.exists(), "the original is still in place");
        assert_eq!(std::fs::read_to_string(destination.join("photo.txt")).unwrap(), "pixels");
    }

    /// A destination that does not exist yet is made, so a folder configured
    /// months ago and never used still works the first time.
    #[test]
    fn a_missing_destination_is_made() {
        let dir = scratch("move-makes");
        let destination = dir.join("deep").join("nested");
        let path = file(&dir, "photo.txt", "pixels");

        move_to(&path, &destination).expect("moving");
        assert!(destination.join("photo.txt").exists());
    }

    /// The property that makes this safe to hold down: a second file of the
    /// same name lands beside the first rather than on top of it.
    #[test]
    fn a_move_never_overwrites_what_is_already_sorted() {
        let dir = scratch("move-collision");
        let destination = dir.join("keepers");
        std::fs::create_dir_all(&destination).unwrap();
        file(&destination, "photo.txt", "the first one");

        let second = file(&dir, "photo.txt", "the second one");
        let outcome = move_to(&second, &destination).expect("moving");

        assert_eq!(
            std::fs::read_to_string(destination.join("photo.txt")).unwrap(),
            "the first one",
            "the sorted picture was overwritten",
        );
        let Outcome::At(landed) = outcome else {
            panic!("a move did not report where the file landed: {outcome:?}");
        };
        assert!(landed.exists(), "the outcome names a file that is not there: {}", landed.display());
        assert_eq!(std::fs::read_to_string(&landed).unwrap(), "the second one");
    }

    #[test]
    fn a_copy_leaves_the_original_alone() {
        let dir = scratch("copy");
        let destination = dir.join("keepers");
        let path = file(&dir, "photo.txt", "pixels");

        let outcome = copy_to(&path, &destination).expect("copying");
        assert_eq!(outcome, Outcome::Copied(destination.join("photo.txt")));
        assert!(path.exists(), "the original was moved rather than copied");
        assert_eq!(std::fs::read_to_string(destination.join("photo.txt")).unwrap(), "pixels");
    }

    #[test]
    fn a_folder_that_is_not_set_is_an_error_rather_than_a_move_to_nowhere() {
        let dir = scratch("move-unset");
        let path = file(&dir, "photo.txt", "pixels");

        assert!(move_to(&path, Path::new("")).is_err());
        assert!(path.exists());
    }

    #[test]
    fn moving_a_file_into_the_folder_it_is_in_says_so() {
        let dir = scratch("move-same");
        let path = file(&dir, "photo.txt", "pixels");

        assert!(move_to(&path, &dir).is_err(), "a move onto itself was attempted");
        assert!(path.exists());
    }

    #[test]
    fn an_operation_on_a_file_that_is_gone_is_an_error() {
        let dir = scratch("missing");
        let path = dir.join("never-existed.txt");

        assert!(recycle(&path).is_err());
        assert!(rename(&path, "other.txt").is_err());
    }
}

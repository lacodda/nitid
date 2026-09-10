//! Handing the picture on screen to another program.
//!
//! Two ways in, and the difference between them is who chooses the program.
//! `edit` asks Windows for the one already registered to edit this kind of
//! file — the same program the shell's own "Edit" offers — so the key works on
//! a viewer nobody has configured. `run` starts a program the person named in
//! the settings, for the nine keys that are theirs to assign.
//!
//! Nothing here waits for the program to finish. The viewer's event loop is
//! the thread that draws, and a photograph frozen behind a modal image editor
//! is a viewer that has stopped being one.

use std::path::Path;

#[cfg(windows)]
mod windows_shell {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use anyhow::{Context, Result, bail};
    use windows::Win32::UI::Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;

    /// A NUL-terminated UTF-16 string, which is what the shell reads.
    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Open the file in whatever program Windows has registered to edit it.
    ///
    /// The verb is `edit`, not `open`: `open` is nitid itself once the viewer
    /// is the default for the format, and a key that reopens the picture in
    /// the program it is already in does nothing anyone asked for.
    ///
    /// Not every file type has an `edit` verb registered. The shell answers
    /// that with an error rather than a guess, and the caller says so — a
    /// viewer that silently did nothing here would be pressed again, harder.
    pub fn edit(path: &Path) -> Result<()> {
        let file = wide(path.as_os_str());
        let verb = wide(std::ffi::OsStr::new("edit"));

        let mut info = SHELLEXECUTEINFOW {
            cbSize: u32::try_from(std::mem::size_of::<SHELLEXECUTEINFOW>()).expect("the struct fits in a u32"),
            // No error dialog of the shell's own: the viewer says what
            // happened in its own message, in its own window. `NOASYNC` keeps
            // the call from returning before the shell has finished with the
            // memory this stack frame owns.
            fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };

        // SAFETY: every pointer in `info` is to a buffer owned by this frame
        // and NUL-terminated above, and both outlive the call — `SEE_MASK_NOASYNC`
        // is what guarantees the shell is done with them when it returns.
        unsafe { ShellExecuteExW(&mut info) }.with_context(|| format!("no program is registered to edit {}", super::name_of(path)))
    }

    /// Start a named program on the file.
    ///
    /// `spawn`, never `status`: this returns to a viewer that has a frame to
    /// draw, and waiting for an image editor to be closed would hang the
    /// window until it was.
    pub fn run(program: &Path, path: &Path) -> Result<()> {
        if program.as_os_str().is_empty() {
            bail!("no program set");
        }
        std::process::Command::new(program)
            .arg(path)
            .spawn()
            .with_context(|| format!("could not start {}", super::name_of(program)))?;
        Ok(())
    }
}

/// The same shape as the file operations' counterpart, and for the same
/// reason: the Linux build exists to keep the core portable, and it must
/// compile rather than carry a hole where a caller names a function that is
/// not there.
///
/// `run` is not among these. Starting a named program is not a Windows idea —
/// it is `Command::spawn` either way — so the one implementation serves both
/// and only the shell's own verb needs a stand-in here.
#[cfg(not(windows))]
mod elsewhere {
    use std::path::Path;

    use anyhow::{Result, bail};

    pub fn edit(_path: &Path) -> Result<()> {
        bail!("the editor a file type is registered to is a Windows idea")
    }

    pub fn run(program: &Path, path: &Path) -> Result<()> {
        if program.as_os_str().is_empty() {
            anyhow::bail!("no program set");
        }
        std::process::Command::new(program)
            .arg(path)
            .spawn()
            .map_err(|error| anyhow::anyhow!("could not start {}: {error}", super::name_of(program)))?;
        Ok(())
    }
}

#[cfg(windows)]
pub use windows_shell::{edit, run};

#[cfg(not(windows))]
pub use elsewhere::{edit, run};

/// What to call a path in a message: its last component, or the whole path
/// when it has none.
fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program that is not set is refused before anything is started.
    ///
    /// The empty path is what eight of the nine keys hold on a normal
    /// installation, so this is the common case rather than an edge one. It
    /// must not reach `Command`, which on an empty program name fails with an
    /// operating system message that says nothing to the person who pressed a
    /// key they had never assigned.
    #[test]
    fn a_program_that_is_not_set_is_refused_by_name() {
        let error = run(Path::new(""), Path::new("picture.jpg")).expect_err("an empty program was started");
        assert!(
            error.to_string().contains("no program set"),
            "an unset program failed with {error:?}, which does not say that the key has nothing on it",
        );
    }

    /// A program that does not exist is reported by name, so the message names
    /// the thing to fix rather than the file that was on screen.
    #[test]
    fn a_program_that_is_missing_is_named_in_the_error() {
        let missing = Path::new("nitid-no-such-program-exists.exe");
        let error = run(missing, Path::new("picture.jpg")).expect_err("a missing program started");
        assert!(
            error.to_string().contains("nitid-no-such-program-exists.exe"),
            "the failure was reported as {error:?}, which does not name the program that could not start",
        );
    }

    /// The name in a message is the file's own, not the whole path: the
    /// settings hold `C:\Program Files\...\thing.exe` and a message that
    /// repeated it would be a message nobody reads to the end.
    ///
    /// Built from the platform's own separator rather than written out with
    /// backslashes. A backslash is an ordinary character in a Linux path, so a
    /// literal Windows path here would assert that the whole string is its own
    /// last component - true there, and nothing to do with the behaviour being
    /// checked. The Linux build exists to keep the core portable, and its tests
    /// have to be about the core.
    #[test]
    fn a_message_names_the_file_rather_than_its_whole_path() {
        let nested: std::path::PathBuf = ["a folder", "another one", "editor.exe"].iter().collect();
        assert_eq!(name_of(&nested), "editor.exe");
        assert_eq!(name_of(Path::new("editor.exe")), "editor.exe");
    }
}

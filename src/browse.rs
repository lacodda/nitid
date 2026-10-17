//! Choosing a picture with Windows' own dialog: `Ctrl+O`, and the start
//! screen's button.
//!
//! Until there was a start screen there was nowhere this was missed: the
//! viewer was opened on a file, by the shell. A viewer started from its icon,
//! with nothing on its list yet, needs a way to a picture that is not "go and
//! find it in Explorer".
//!
//! The dialog runs on a thread of its own and answers through the event loop.
//! It is modal to the viewer's window all the same — Windows disables an
//! owner on another thread just as well — but the window's own thread stays
//! free, so the viewer keeps drawing behind the dialog rather than freezing
//! into whatever frame it had when the dialog opened.

use std::path::{Path, PathBuf};

/// What the dialog filters on: every extension the viewer opens, the same
/// list the installer registers.
fn pattern() -> String {
    crate::image_source::supported_extensions()
        .iter()
        .map(|extension| format!("*.{extension}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// A folder as the shell's parser reads it: the same components, with the
/// platform's own separator between them.
fn parsing_name(folder: &Path) -> PathBuf {
    folder.components().collect()
}

/// Ask for pictures, on a thread of its own, starting in `folder` if there is
/// one; `chosen` is called there with what was picked — nothing, when the
/// dialog was cancelled or could not be shown, and the reason in the latter
/// case.
#[cfg(windows)]
pub fn choose(owner: &winit::window::Window, folder: Option<&Path>, chosen: impl FnOnce(Result<Vec<PathBuf>, String>) + Send + 'static) {
    // A window handle is a number to Windows and a pointer to Rust; it is
    // carried across as the number and rebuilt on the other side.
    let owner = crate::com::hwnd_of(owner).ok().map(|hwnd| hwnd.0 as isize);
    let folder = folder.map(Path::to_path_buf);
    std::thread::spawn(move || chosen(dialog::show(owner, folder.as_deref())));
}

#[cfg(windows)]
mod dialog {
    use std::path::{Path, PathBuf};

    use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
        SIGDN_FILESYSPATH,
    };
    use windows::core::{HSTRING, PCWSTR};

    use crate::com::Com;

    pub fn show(owner: Option<isize>, folder: Option<&Path>) -> Result<Vec<PathBuf>, String> {
        let _com = Com::begin();
        // SAFETY: COM calls on this thread's own apartment, begun above; every
        // string handed over outlives the call it is handed to.
        unsafe {
            let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(|error| error.message())?;
            let options = dialog.GetOptions().map_err(|error| error.message())?;
            // Several at once is a selection, the way several files handed
            // over by the shell are; only real files, because the viewer
            // reads paths and a library or a phone is not one.
            dialog
                .SetOptions(options | FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)
                .map_err(|error| error.message())?;

            let pictures = HSTRING::from("Pictures");
            let pattern = HSTRING::from(super::pattern());
            let everything = HSTRING::from("All files");
            let any = HSTRING::from("*.*");
            let kinds = [
                COMDLG_FILTERSPEC {
                    pszName: PCWSTR(pictures.as_ptr()),
                    pszSpec: PCWSTR(pattern.as_ptr()),
                },
                COMDLG_FILTERSPEC {
                    pszName: PCWSTR(everything.as_ptr()),
                    pszSpec: PCWSTR(any.as_ptr()),
                },
            ];
            dialog.SetFileTypes(&kinds).map_err(|error| error.message())?;
            let _ = dialog.SetTitle(&HSTRING::from("Open a picture"));

            // Where the picture on screen is, so the next one is a step away;
            // otherwise wherever Windows last left this dialog.
            //
            // Rebuilt from its components first: a path typed or handed over
            // with forward slashes is a good path to the file system and a
            // name the shell's parser cannot read — the dialog then opened in
            // Documents, which is what a live run found.
            if let Some(folder) = folder
                && let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(super::parsing_name(folder).as_os_str()), None)
            {
                let _ = dialog.SetFolder(&item);
            }

            let owner = owner.map(|handle| HWND(handle as *mut core::ffi::c_void));
            if let Err(error) = dialog.Show(owner) {
                // Cancelled is an answer, not a failure: nothing was chosen.
                if error.code() == ERROR_CANCELLED.to_hresult() {
                    return Ok(Vec::new());
                }
                return Err(error.message());
            }

            let results = dialog.GetResults().map_err(|error| error.message())?;
            let count = results.GetCount().map_err(|error| error.message())?;
            let mut paths = Vec::with_capacity(count as usize);
            for index in 0..count {
                let Ok(item) = results.GetItemAt(index) else {
                    continue;
                };
                let Ok(name) = item.GetDisplayName(SIGDN_FILESYSPATH) else {
                    continue;
                };
                if let Ok(path) = name.to_string() {
                    paths.push(PathBuf::from(path));
                }
                CoTaskMemFree(Some(name.0 as *const core::ffi::c_void));
            }
            Ok(paths)
        }
    }
}

#[cfg(test)]
mod tests {
    /// The dialog offers exactly what the viewer opens: a format the viewer
    /// learns and the dialog does not would be a picture nobody could pick.
    #[test]
    fn the_filter_names_every_extension_the_viewer_opens() {
        let pattern = super::pattern();
        for extension in crate::image_source::supported_extensions() {
            assert!(
                pattern.split(';').any(|part| part == format!("*.{extension}")),
                "*.{extension} is missing from {pattern}"
            );
        }
        assert!(!pattern.contains(' '), "a space in the pattern splits it for the dialog: {pattern}");
    }

    /// A folder named with forward slashes — typed on a command line, handed
    /// over by a script — reaches the shell with its own separators, or the
    /// dialog opens somewhere else entirely.
    #[cfg(windows)]
    #[test]
    fn a_folder_reaches_the_shell_with_its_own_separators() {
        let named = super::parsing_name(std::path::Path::new(r"C:/Users/someone\Pictures/Holiday 2026"));
        assert_eq!(named.as_os_str(), r"C:\Users\someone\Pictures\Holiday 2026");
    }
}

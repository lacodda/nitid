//! Keeping the console out of the way of the viewer.
//!
//! nitid is linked as a console application, which is what makes `install`,
//! `--version` and error messages work when it is run from a terminal: a GUI
//! subsystem binary starts with no standard handles, and Rust binds `println!`
//! to them before `main` runs, so attaching a console afterwards is too late —
//! output vanishes even when redirected to a file.
//!
//! The cost of a console binary is a console window when the shell launches
//! it by double-click. That is paid off here: the viewer hides the window it
//! was given, but only when the window it owns is the one the user asked for.

/// Write `text` to standard output as it is, and carry on if nobody reads it.
///
/// What every command prints goes through here rather than `println!`, which
/// panics when the reader has gone: `nitid install | Select-Object -First 2`
/// closed the pipe after two lines, and the panic stopped the install between
/// writing the registry and starting the resident (v0.43). A report is for the
/// person reading it; once they stop reading, the work it reports on goes on.
pub fn put(text: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}

/// `println!` that never stops the command: see `put`.
macro_rules! say {
    () => {
        $crate::console::put("\n")
    };
    ($($arg:tt)*) => {{
        let mut line = format!($($arg)*);
        line.push('\n');
        $crate::console::put(&line)
    }};
}
pub(crate) use say;

/// Hide the console window this process was given, if it owns one.
///
/// Called on the viewing path only. A process launched from a terminal shares
/// that terminal's window and must leave it alone — hiding it would take the
/// user's shell down with it, so ownership is checked first.
pub fn hide_if_ours() {
    #[cfg(windows)]
    windows_impl::hide_if_ours();
}

#[cfg(windows)]
mod windows_impl {
    use windows::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

    pub fn hide_if_ours() {
        // SAFETY: both calls are reads of process-global console state and
        // report absence through their return values.
        let window = unsafe { GetConsoleWindow() };
        if window.is_invalid() {
            return;
        }

        // A console created for this process alone lists exactly one process:
        // us. Anything more means we joined someone else's terminal.
        let mut processes = [0u32; 2];
        let count = unsafe { GetConsoleProcessList(&mut processes) };
        if count != 1 {
            return;
        }

        // SAFETY: the handle came from `GetConsoleWindow` and was checked.
        let _ = unsafe { ShowWindow(window, SW_HIDE) };
    }
}

#[cfg(test)]
mod tests {
    /// Every report goes through `say` and `put`: one `println!` in a command
    /// is the panic of v0.42 back, at whichever line of it a reader stops.
    #[test]
    fn nothing_prints_past_the_console() {
        let mut found = Vec::new();
        let mut folders = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).expect("reading the source tree") {
                let path = entry.expect("a source entry").path();
                if path.is_dir() {
                    folders.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("reading a source file");
                for (number, line) in text.lines().enumerate() {
                    let code = line.trim_start();
                    if code.starts_with("//") {
                        continue;
                    }
                    // Spelt in halves, or this line would be found by itself.
                    for printer in [concat!("print", "ln!("), concat!("print", "!(")] {
                        let printed = code
                            .match_indices(printer)
                            .any(|(at, _)| code[..at].chars().next_back().is_none_or(|before| !before.is_alphanumeric() && before != '_'));
                        if printed {
                            found.push(format!("{}:{}: {}", path.display(), number + 1, code));
                        }
                    }
                }
            }
        }
        assert!(found.is_empty(), "printing that panics when the reader goes away:\n{}", found.join("\n"));
    }
}

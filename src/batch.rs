//! One command over many pictures: which files a command line names, and
//! working through them.
//!
//! `nitid info` and `nitid convert` take what a person types at a prompt, and
//! on Windows that includes `*.heic` exactly as typed: neither cmd nor
//! PowerShell expands a pattern for the program it starts. Git Bash does, and
//! then the names arrive already expanded and pass through untouched. So the
//! expansion happens here, by the rules the viewer already walks a folder by:
//!
//! - a file is itself, whatever it is called — a file named on purpose and
//!   turning out not to be a picture is answered by its decoder, not skipped;
//! - a folder is the pictures in it, the list the viewer would walk, without
//!   its subfolders;
//! - a name with `*` or `?` in it is a pattern over the pictures of its
//!   folder, matched the way the folder filter matches (see `pattern`).
//!
//! A file named twice — `a.jpg` and `*.jpg` — is worked on once.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::pattern::Pattern;

/// What a command line names.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Named {
    /// The files, in the order they were named; a folder's and a pattern's in
    /// the order of their names.
    pub files: Vec<PathBuf>,
    /// Every argument that named nothing, and why.
    pub missing: Vec<(PathBuf, String)>,
}

/// A failure the command has already said, file by file, where it happened.
///
/// It sets the exit code and adds nothing: a summary of "3 failed" after
/// three lines that each say which file and why would only repeat them.
#[derive(Debug)]
pub struct Reported;

impl std::fmt::Display for Reported {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("some files could not be worked on")
    }
}

impl std::error::Error for Reported {}

/// Turn the arguments into the files they name.
pub fn named(args: &[PathBuf]) -> Named {
    let mut named = Named::default();
    let mut seen = HashSet::new();
    let mut add = |named: &mut Named, path: PathBuf| {
        // Compared as the file system resolves them, so `a.jpg` and `.\a.jpg`
        // are one file; a path that cannot be resolved is its own key.
        let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if seen.insert(key) {
            named.files.push(path);
        }
    };

    for arg in args {
        let name = arg.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        if name.contains(['*', '?']) {
            let parent = arg.parent().unwrap_or(Path::new(""));
            if parent.to_string_lossy().contains(['*', '?']) {
                named.missing.push((arg.clone(), "a pattern names files, not folders".to_string()));
                continue;
            }
            let Some(pattern) = Pattern::parse(&name) else {
                named.missing.push((arg.clone(), "names nothing".to_string()));
                continue;
            };
            let matched = pictures_in(parent).map(|pictures| pictures.into_iter().filter(|picture| pattern.admits(picture)).collect::<Vec<_>>());
            match matched {
                Ok(matched) if !matched.is_empty() => matched.into_iter().for_each(|path| add(&mut named, path)),
                Ok(_) => named.missing.push((arg.clone(), "no picture matches".to_string())),
                Err(error) => named.missing.push((arg.clone(), format!("{error:#}"))),
            }
        } else if arg.is_dir() {
            match pictures_in(arg) {
                Ok(pictures) if !pictures.is_empty() => pictures.into_iter().for_each(|path| add(&mut named, path)),
                Ok(_) => named.missing.push((arg.clone(), "no pictures in this folder".to_string())),
                Err(error) => named.missing.push((arg.clone(), format!("{error:#}"))),
            }
        } else if arg.is_file() {
            add(&mut named, arg.clone());
        } else {
            named.missing.push((arg.clone(), "no such file or folder".to_string()));
        }
    }

    named
}

/// The pictures of a folder, each named under the folder as it was typed.
///
/// As typed rather than as listed: `*.heic` should come back as `a.heic`, not
/// `.\a.heic`, because what is printed beside each file is the name the person
/// would type for it.
fn pictures_in(folder: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let listed = if folder.as_os_str().is_empty() { Path::new(".") } else { folder };
    Ok(crate::folder::pictures(listed)?
        .into_iter()
        .filter_map(|(path, _)| path.file_name().map(|name| folder.join(name)))
        .collect())
}

/// How many pictures are worked on at once.
///
/// Four at most, whatever the processor: a decoded 48-megapixel photograph is
/// close to 400 MB at sixteen bits, and a machine with twenty cores converting
/// a phone's camera roll should not try to hold twenty of them.
const MOST_AT_ONCE: usize = 4;

/// Do `work` to every file, several at a time, and hand each result to
/// `report` in the order the files came in.
///
/// In order because what reads the output is a person scrolling a terminal or
/// a script pairing lines with names, and neither should depend on which
/// decode happened to finish first. A result waits only for the ones before
/// it, so the first lines appear while the rest are still being worked on.
pub fn each<T: Send>(files: &[PathBuf], work: impl Fn(&Path) -> T + Sync, mut report: impl FnMut(&Path, T)) {
    let workers = std::thread::available_parallelism()
        .map_or(1, |count| count.get())
        .min(MOST_AT_ONCE)
        .min(files.len());
    if workers <= 1 {
        for file in files {
            report(file, work(file));
        }
        return;
    }

    let next = AtomicUsize::new(0);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let sender = sender.clone();
            let (next, work) = (&next, &work);
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(file) = files.get(index) else {
                        break;
                    };
                    if sender.send((index, work(file))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);

        let mut waiting = BTreeMap::new();
        let mut due = 0;
        for (index, result) in receiver {
            waiting.insert(index, result);
            while let Some(result) = waiting.remove(&due) {
                report(&files[due], result);
                due += 1;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder with three pictures, a text file and a subfolder holding a
    /// fourth picture.
    fn folder() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temporary folder");
        for name in ["b.png", "a.jpg", "c.HEIC", "notes.txt"] {
            std::fs::write(dir.path().join(name), b"not really a picture").expect("writing a file");
        }
        std::fs::create_dir(dir.path().join("inner")).expect("a subfolder");
        std::fs::write(dir.path().join("inner").join("d.png"), b"x").expect("writing a file");
        dir
    }

    fn names(named: &Named) -> Vec<String> {
        named
            .files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    /// A folder is its pictures, by name, and not its other files or its
    /// subfolders.
    #[test]
    fn a_folder_names_its_pictures() {
        let dir = folder();
        let named = named(&[dir.path().to_path_buf()]);
        assert_eq!(names(&named), ["a.jpg", "b.png", "c.HEIC"]);
        assert!(named.missing.is_empty(), "{:?}", named.missing);
    }

    /// The pattern a shell did not expand is expanded here, ignoring case as
    /// the file system does, and the names keep the folder as typed.
    #[test]
    fn a_pattern_is_expanded_over_its_folder() {
        let dir = folder();
        let named = named(&[dir.path().join("*.heic")]);
        assert_eq!(named.files, [dir.path().join("c.HEIC")]);

        let several = super::named(&[dir.path().join("*.png;*.jpg")]);
        assert_eq!(names(&several), ["a.jpg", "b.png"]);
    }

    /// A file is itself even when it is not a picture: named on purpose, it is
    /// for the decoder to say what it is.
    #[test]
    fn a_file_is_itself_whatever_it_is_called() {
        let dir = folder();
        let named = named(&[dir.path().join("notes.txt")]);
        assert_eq!(names(&named), ["notes.txt"]);
    }

    /// Something that names nothing is said, and the rest still goes ahead.
    #[test]
    fn what_names_nothing_is_said_and_the_rest_kept() {
        let dir = folder();
        let named = named(&[
            dir.path().join("gone.jpg"),
            dir.path().join("*.gif"),
            dir.path().join("*").join("*.png"),
            dir.path().join("a.jpg"),
        ]);
        assert_eq!(names(&named), ["a.jpg"]);
        assert_eq!(named.missing.len(), 3, "{:?}", named.missing);
        assert_eq!(named.missing[0], (dir.path().join("gone.jpg"), "no such file or folder".to_string()));
        assert_eq!(named.missing[1].1, "no picture matches");
        assert_eq!(named.missing[2].1, "a pattern names files, not folders");
    }

    /// A file reached two ways is one file.
    #[test]
    fn a_file_named_twice_is_named_once() {
        let dir = folder();
        let named = named(&[dir.path().join("a.jpg"), dir.path().join("*.jpg"), dir.path().to_path_buf()]);
        assert_eq!(names(&named), ["a.jpg", "b.png", "c.HEIC"]);
    }

    /// Results come back in the order the files went in, even when the later
    /// ones finish first.
    #[test]
    fn results_are_reported_in_the_order_named() {
        let files: Vec<PathBuf> = (0..12).map(|index| PathBuf::from(format!("{index}"))).collect();
        let mut seen = Vec::new();
        each(
            &files,
            |file| {
                let index: u64 = file.to_string_lossy().parse().unwrap();
                // The first files take longest, so a report in finishing
                // order would come out reversed.
                std::thread::sleep(std::time::Duration::from_millis((12 - index) * 5));
                index
            },
            |file, index| seen.push((file.to_path_buf(), index)),
        );
        let expected: Vec<(PathBuf, u64)> = (0..12).map(|index| (PathBuf::from(format!("{index}")), index)).collect();
        assert_eq!(seen, expected);
    }
}

//! The pictures looked at lately, one to a folder.
//!
//! A viewer started from its icon, with no file, used to open an empty window
//! — the one launch that had nothing to show. What a person starting it that
//! way wants is to go back to something: the folder of the shoot they were
//! halfway through, the screenshots from this morning. So the viewer keeps a
//! short list of where it has been, and the start screen draws it.
//!
//! **One entry to a folder**, holding the picture last on screen there. Not a
//! list of folders beside a list of files: a folder alone does not say where
//! to start, and a file alone is something nitid never opens anyway — one
//! file browses its folder. The entry is both, and opening it carries on from
//! the picture it shows.
//!
//! The list lives in `%LOCALAPPDATA%\lacodda\nitid\recent`: local rather than
//! roaming, because the paths are this machine's. Plain lines, newest first,
//! each the moment and the path with a tab between them — a path holds spaces
//! and never a tab or a line break, and a person reading the file can tell what
//! it is. Every failure to read or write it is silent, for the reason the
//! settings' are: a viewer that will not open because it cannot remember where
//! it has been has its priorities backwards.

pub mod pictures;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many folders are remembered.
///
/// More than a window shows at once, so a folder pushed off the end of the
/// screen by a busy afternoon is still there to scroll to; few enough that the
/// file stays a glance long and the start screen is a choice, not a search.
pub const KEPT: usize = 24;

/// One folder, as it was last left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The picture that was on screen when the folder was last looked at.
    pub path: PathBuf,
    /// When, in seconds since the Unix epoch.
    pub when: u64,
}

impl Entry {
    /// The folder this entry stands for.
    pub fn folder(&self) -> Option<&Path> {
        self.path.parent()
    }
}

/// The remembered folders, newest first.
#[derive(Clone, Debug, Default)]
pub struct Recent {
    entries: Vec<Entry>,
    /// Folders let go of in this session.
    ///
    /// Kept so that writing the list, which reads what is on disk first, does
    /// not bring back a folder the person has just taken off it: the copy on
    /// disk still names it until this write replaces it.
    forgotten: Vec<PathBuf>,
    /// Whether the list has moved since it was last written.
    dirty: bool,
}

impl Recent {
    /// The list as it was last written, or an empty one.
    pub fn load() -> Self {
        Self {
            entries: path().map(|path| read(&path)).unwrap_or_default(),
            ..Self::default()
        }
    }

    /// Every folder remembered, newest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The ones that can be gone back to now.
    ///
    /// A folder on a drive that is not plugged in is left out of the screen
    /// but not out of the list: the stick goes back in tomorrow, and the
    /// folder is there again. It leaves the list the ordinary way, by being
    /// pushed off its end.
    pub fn reachable(&self) -> Vec<Entry> {
        self.entries.iter().filter(|entry| entry.folder().is_some_and(Path::is_dir)).cloned().collect()
    }

    /// Note that `path` is on screen now.
    ///
    /// Returns whether a folder came to the front that was not there before,
    /// which is when the list is worth writing at once: a step to the next
    /// picture in the same folder changes only which picture the entry holds,
    /// and writing the file on every arrow key would be a disk write per
    /// keystroke for a detail the window's closing writes anyway.
    pub fn record(&mut self, path: &Path, when: u64) -> bool {
        let Some(folder) = path.parent() else {
            return false;
        };
        let moved = self.entries.first().and_then(Entry::folder).is_none_or(|first| !same_path(first, folder));

        self.entries.retain(|entry| entry.folder().is_none_or(|other| !same_path(other, folder)));
        self.entries.insert(
            0,
            Entry {
                path: path.to_path_buf(),
                when,
            },
        );
        self.entries.truncate(KEPT);
        self.forgotten.retain(|gone| !same_path(gone, folder));
        self.dirty = true;
        moved
    }

    /// Take the folder `path` is in off the list.
    pub fn forget(&mut self, path: &Path) {
        let Some(folder) = path.parent() else {
            return;
        };
        self.entries.retain(|entry| entry.folder().is_none_or(|other| !same_path(other, folder)));
        self.forgotten.push(folder.to_path_buf());
        self.dirty = true;
    }

    /// Write the list, if it moved since it was last written.
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = path() else {
            return;
        };
        self.save_to(&path);
    }

    /// Write the list to `file`, joined with what is already there.
    ///
    /// Read first, because this window may not be the only one: with sharing
    /// turned off two viewers each keep a list, and the second to close would
    /// otherwise wipe out every folder the first one went to. A folder both
    /// know about keeps whichever visit was later.
    fn save_to(&mut self, file: &Path) {
        let on_disk = read(file);
        self.merge(on_disk);
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(file, render(&self.entries)).is_ok() {
            self.dirty = false;
        }
    }

    /// Take in what another window wrote, keeping this one's own choices.
    fn merge(&mut self, others: Vec<Entry>) {
        for other in others {
            let Some(folder) = other.folder() else {
                continue;
            };
            if self.forgotten.iter().any(|gone| same_path(gone, folder)) {
                continue;
            }
            match self.entries.iter_mut().find(|entry| entry.folder().is_some_and(|mine| same_path(mine, folder))) {
                Some(mine) if mine.when >= other.when => {}
                Some(mine) => *mine = other,
                None => self.entries.push(other),
            }
        }
        // Newest first again, with the folder that came last in on top of a
        // tie: the sort is stable, and this window's own entries came first.
        self.entries.sort_by_key(|entry| std::cmp::Reverse(entry.when));
        self.entries.truncate(KEPT);
    }
}

/// Seconds since the Unix epoch, now.
pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// How long ago `when` was, the way a person would say it at a glance.
///
/// Relative rather than a date: "2 h ago" answers "is this the one from this
/// morning" without arithmetic, and that is the only question a start screen
/// asks of a time.
pub fn ago(now: u64, when: u64) -> String {
    let seconds = now.saturating_sub(when);
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    match () {
        _ if minutes < 1 => "just now".to_string(),
        _ if hours < 1 => format!("{minutes} min ago"),
        _ if days < 1 => format!("{hours} h ago"),
        _ if days == 1 => "yesterday".to_string(),
        _ if days < 14 => format!("{days} days ago"),
        _ if days < 60 => format!("{} weeks ago", days / 7),
        _ => format!("{} months ago", days / 30),
    }
}

/// The newest picture in any of `folders`, by when it was last written.
///
/// What the hotkey brings up. Not the picture last looked at — that one is
/// already on screen — but the one most lately made: the screenshot just
/// taken, the frame a generator just wrote, the photographs just imported.
/// Read with the viewer's own listing rules, so it never names a file the
/// viewer would not show.
///
/// Every folder is read afresh, which on a network share can take seconds:
/// the caller runs this off the event loop.
pub fn newest(folders: &[PathBuf]) -> Option<PathBuf> {
    folders
        .iter()
        .filter_map(|folder| crate::folder::pictures(folder).ok())
        .flatten()
        .max_by_key(|(_, modified)| *modified)
        .map(|(path, _)| path)
}

/// Whether two paths name the same place.
///
/// Compared component by component, and without regard to case on Windows,
/// where the file system makes no distinction: `C:\Photos` reached from a
/// shortcut and `c:\photos` typed into a dialog are one folder, and keeping
/// both would put the same folder on the screen twice.
fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        let lower = |path: &Path| {
            path.components()
                .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
                .collect::<Vec<_>>()
        };
        lower(a) == lower(b)
    } else {
        a.components().eq(b.components())
    }
}

fn read(file: &Path) -> Vec<Entry> {
    std::fs::read_to_string(file).map(|text| parse(&text)).unwrap_or_default()
}

fn parse(text: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = text
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let (when, path) = line.split_once('\t')?;
            let path = path.trim_end_matches('\r');
            let when = when.trim().parse().ok()?;
            (!path.is_empty()).then(|| Entry {
                path: PathBuf::from(path),
                when,
            })
        })
        .collect();
    // A file edited by hand, or written by a build with a longer list, is
    // still read as the list this one keeps: newest first, a folder once.
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.when));
    let mut seen: Vec<PathBuf> = Vec::new();
    entries.retain(|entry| {
        let Some(folder) = entry.folder() else {
            return false;
        };
        if seen.iter().any(|other| same_path(other, folder)) {
            return false;
        }
        seen.push(folder.to_path_buf());
        true
    });
    entries.truncate(KEPT);
    entries
}

fn render(entries: &[Entry]) -> String {
    let mut out = String::from("# nitid: the pictures looked at lately, one to a folder. Written by nitid.\n");
    for entry in entries {
        out.push_str(&format!("{}\t{}\n", entry.when, entry.path.display()));
    }
    out
}

/// `%LOCALAPPDATA%\lacodda\nitid\recent`.
fn path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("lacodda").join("nitid").join("recent"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    fn folders(recent: &Recent) -> Vec<String> {
        recent
            .entries()
            .iter()
            .map(|entry| entry.folder().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    /// The property the screen is built on: a folder appears once, holding
    /// the picture last looked at in it, however many were stepped through.
    #[test]
    fn a_folder_is_one_entry_holding_the_last_picture() {
        let mut recent = Recent::default();
        recent.record(&at("/shoot/one.jpg"), 10);
        recent.record(&at("/shoot/two.jpg"), 11);
        recent.record(&at("/shoot/three.jpg"), 12);

        assert_eq!(recent.entries().len(), 1, "{:?}", recent.entries());
        assert_eq!(recent.entries()[0].path, at("/shoot/three.jpg"));
        assert_eq!(recent.entries()[0].when, 12);
    }

    #[test]
    fn the_folder_last_visited_comes_first() {
        let mut recent = Recent::default();
        recent.record(&at("/a/1.jpg"), 1);
        recent.record(&at("/b/1.jpg"), 2);
        recent.record(&at("/a/2.jpg"), 3);

        assert_eq!(folders(&recent), vec!["/a", "/b"]);
        assert_eq!(recent.entries()[0].path, at("/a/2.jpg"));
    }

    /// Writing on every arrow key would be a disk write per keystroke; the
    /// answer says when a write is worth it.
    #[test]
    fn only_a_folder_coming_to_the_front_asks_for_a_write() {
        let mut recent = Recent::default();
        assert!(recent.record(&at("/a/1.jpg"), 1), "the first folder of all came to the front");
        assert!(!recent.record(&at("/a/2.jpg"), 2), "a step inside the folder asked for a write");
        assert!(recent.record(&at("/b/1.jpg"), 3), "another folder came to the front unannounced");
        assert!(recent.record(&at("/a/3.jpg"), 4), "going back to the first folder went unannounced");
    }

    #[test]
    fn the_list_is_held_to_its_length() {
        let mut recent = Recent::default();
        for index in 0..KEPT + 5 {
            recent.record(&at(&format!("/folder{index}/x.jpg")), index as u64);
        }
        assert_eq!(recent.entries().len(), KEPT);
        // The ones pushed off are the oldest.
        assert_eq!(recent.entries()[0].path, at(&format!("/folder{}/x.jpg", KEPT + 4)));
        assert!(!folders(&recent).contains(&"/folder0".to_string()));
    }

    #[test]
    fn forgetting_takes_the_folder_off() {
        let mut recent = Recent::default();
        recent.record(&at("/a/1.jpg"), 1);
        recent.record(&at("/b/1.jpg"), 2);
        recent.forget(&at("/a/other.jpg"));
        assert_eq!(folders(&recent), vec!["/b"]);
    }

    /// The case-insensitive file system: one folder reached two ways is still
    /// one folder on the screen.
    #[cfg(windows)]
    #[test]
    fn a_folder_spelled_in_another_case_is_the_same_folder() {
        let mut recent = Recent::default();
        recent.record(&at(r"C:\Photos\a.jpg"), 1);
        recent.record(&at(r"c:\photos\b.jpg"), 2);
        assert_eq!(recent.entries().len(), 1, "{:?}", recent.entries());
    }

    #[test]
    fn the_file_survives_the_round_trip() {
        let entries = vec![
            Entry {
                path: at("/with spaces/и не только/кадр 1.jpg"),
                when: 1_760_000_000,
            },
            Entry {
                path: at("/b/x.png"),
                when: 1_750_000_000,
            },
        ];
        assert_eq!(parse(&render(&entries)), entries);
    }

    /// A file edited by hand is read as the list this build keeps rather than
    /// trusted to already be one.
    #[test]
    fn a_hand_edited_file_is_put_back_in_order() {
        let text = "# comment\n5\t/a/old.jpg\n9\t/b/x.jpg\nnot a line\n7\t/a/new.jpg\n\t/c/no-time.jpg\n";
        let entries = parse(text);
        let paths: Vec<&Path> = entries.iter().map(|entry| entry.path.as_path()).collect();
        assert_eq!(paths, vec![Path::new("/b/x.jpg"), Path::new("/a/new.jpg")]);
    }

    /// A second window's visits survive this one writing the file, and a
    /// folder both went to keeps the later visit.
    #[test]
    fn writing_joins_what_another_window_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("recent");

        let mut other = Recent::default();
        other.record(&at("/theirs/a.jpg"), 5);
        other.record(&at("/shared/later.jpg"), 9);
        other.save_to(&file);

        let mut mine = Recent::default();
        mine.record(&at("/shared/earlier.jpg"), 7);
        mine.record(&at("/mine/b.jpg"), 8);
        mine.save_to(&file);

        let written = read(&file);
        let paths: Vec<&Path> = written.iter().map(|entry| entry.path.as_path()).collect();
        assert_eq!(
            paths,
            vec![Path::new("/shared/later.jpg"), Path::new("/mine/b.jpg"), Path::new("/theirs/a.jpg")]
        );
    }

    /// Joining what is on disk must not undo a forget: the file still names
    /// the folder until this very write replaces it.
    #[test]
    fn a_forgotten_folder_does_not_come_back_from_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("recent");

        let mut recent = Recent::default();
        recent.record(&at("/a/1.jpg"), 1);
        recent.record(&at("/b/1.jpg"), 2);
        recent.save_to(&file);

        recent.forget(&at("/a/1.jpg"));
        recent.save_to(&file);

        let written = read(&file);
        assert_eq!(written.len(), 1, "{written:?}");
        assert_eq!(written[0].path, at("/b/1.jpg"));
    }

    /// Going back to a folder after forgetting it is a new visit, and the
    /// forget does not stand in its way.
    #[test]
    fn a_folder_forgotten_and_visited_again_is_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("recent");

        let mut recent = Recent::default();
        recent.record(&at("/a/1.jpg"), 1);
        recent.forget(&at("/a/1.jpg"));
        recent.record(&at("/a/2.jpg"), 2);
        recent.save_to(&file);

        assert_eq!(read(&file).len(), 1);
    }

    #[test]
    fn nothing_moved_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("recent");
        let mut recent = Recent::default();
        recent.save_to(&file);
        // `save_to` writes regardless; it is `save` that asks first, and that
        // is what a clean list must not reach.
        assert!(!recent.dirty);
    }

    #[test]
    fn a_missing_folder_is_left_off_the_screen_but_kept() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("present.jpg");

        let mut recent = Recent::default();
        recent.record(Path::new("/no/such/folder/anywhere/x.jpg"), 1);
        recent.record(&present, 2);

        let reachable = recent.reachable();
        assert_eq!(reachable.len(), 1, "{reachable:?}");
        assert_eq!(reachable[0].path, present);
        assert_eq!(recent.entries().len(), 2, "the unreachable folder was dropped from the list itself");
    }

    #[test]
    fn the_newest_picture_is_found_across_folders() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let old = first.path().join("old.png");
        let new = second.path().join("new.png");
        let not_a_picture = second.path().join("notes.txt");
        for path in [&old, &new, &not_a_picture] {
            std::fs::write(path, b"x").unwrap();
        }
        let set = |path: &Path, seconds: u64| {
            let file = std::fs::File::options().write(true).open(path).unwrap();
            file.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(seconds)).unwrap();
        };
        set(&old, 1_700_000_000);
        set(&new, 1_700_000_100);
        // The newest file of all is not a picture, and is not the answer.
        set(&not_a_picture, 1_700_000_200);

        let found = newest(&[first.path().to_path_buf(), second.path().to_path_buf()]);
        assert_eq!(found, Some(new));
    }

    #[test]
    fn no_folders_have_no_newest_picture() {
        assert_eq!(newest(&[]), None);
        assert_eq!(newest(&[PathBuf::from("/no/such/folder/anywhere")]), None);
    }

    #[test]
    fn a_time_is_said_the_way_a_person_says_it() {
        let now = 1_760_000_000;
        assert_eq!(ago(now, now - 20), "just now");
        assert_eq!(ago(now, now - 5 * 60), "5 min ago");
        assert_eq!(ago(now, now - 3 * 3600), "3 h ago");
        assert_eq!(ago(now, now - 30 * 3600), "yesterday");
        assert_eq!(ago(now, now - 5 * 86_400), "5 days ago");
        assert_eq!(ago(now, now - 21 * 86_400), "3 weeks ago");
        assert_eq!(ago(now, now - 90 * 86_400), "3 months ago");
        // A clock that went backwards is not a visit from the future.
        assert_eq!(ago(now, now + 500), "just now");
    }
}

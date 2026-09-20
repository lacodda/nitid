//! The list of images sitting next to the one that was opened.
//!
//! Opening a file means opening its folder: arrow keys move through the
//! neighbours in the order the shell shows them, and v0.2.0 will prefetch
//! them. The listing is taken once, when the file opens — a folder that
//! changes underneath is rescanned only on an explicit reload.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::Order;
use crate::image_source;

/// The images of one folder plus a cursor onto the current file.
pub struct Folder {
    entries: Vec<PathBuf>,
    current: usize,
    /// Whether a step past either end comes round to the other.
    ///
    /// On by default. Off, the ends of the folder are ends: the arrow key
    /// stops rather than starting again, which is what someone working
    /// through a shoot in order expects.
    wrap: bool,
    /// Which entries the arrow keys are allowed to land on.
    ///
    /// `None` — the ordinary state — is the whole folder. A filter narrows
    /// what is walked **without** narrowing what is held: the listing stays
    /// whole, so lifting the filter is free and a file that loses its mark
    /// while the filter is up does not vanish from the folder, only from the
    /// walk. A second list would be a second answer to "what am I browsing".
    ///
    /// The indices are into `entries` and are kept sorted, which is what lets
    /// stepping be a search rather than a scan.
    shown: Option<Vec<usize>>,
}

impl Folder {
    /// Scan the folder containing `path` and place the cursor on `path`.
    ///
    /// A file whose folder cannot be read still opens: the listing falls back
    /// to that single entry, because failing to browse is not a reason to
    /// refuse to show the picture that was double-clicked.
    pub fn open(path: &Path, order: Order, wrap: bool) -> Result<Self> {
        let path = absolute(path)?;
        let entries = match path.parent() {
            Some(parent) => scan(parent, order).unwrap_or_else(|_| vec![path.clone()]),
            None => vec![path.clone()],
        };

        let entries = if entries.is_empty() { vec![path.clone()] } else { entries };

        let current = entries.iter().position(|entry| entry == &path).unwrap_or(0);

        Ok(Self {
            entries,
            current,
            wrap,
            shown: None,
        })
    }

    /// Browse an explicit selection rather than a folder.
    ///
    /// Selecting five files in the shell and pressing Enter should give one
    /// window that browses those five, not the hundreds that happen to sit
    /// beside them. The cursor starts on the first.
    ///
    /// `None` when nothing selected is an image this build can open, which
    /// leaves the caller showing whatever it already had.
    /// The selection keeps the order it was given: the person picked these
    /// files in this order, and re-sorting them would answer a question they
    /// did not ask. `wrap` still applies — it is about the ends of a list,
    /// whichever list it is.
    pub fn of_selection(paths: &[PathBuf], wrap: bool) -> Option<Self> {
        let entries: Vec<PathBuf> = paths
            .iter()
            .filter(|path| image_source::is_supported(path))
            .filter_map(|path| absolute(path).ok())
            .collect();

        (!entries.is_empty()).then_some(Self {
            entries,
            current: 0,
            wrap,
            shown: None,
        })
    }

    /// Add files to what is being browsed, and move the cursor to the first
    /// of them.
    ///
    /// This is a hand-over from another instance arriving while the window is
    /// already open. Files already in the list are not duplicated; the cursor
    /// lands on the first of the new ones either way, because the person who
    /// double-clicked it expects to be looking at it.
    ///
    /// `None` when nothing worth showing arrived.
    ///
    /// Windows-only, like the hand-over that is its only caller: elsewhere
    /// this would be a method nobody calls, which the build denies.
    #[cfg(windows)]
    pub fn extend(&mut self, paths: &[PathBuf]) -> Option<&Path> {
        let arriving: Vec<PathBuf> = paths
            .iter()
            .filter(|path| image_source::is_supported(path))
            .filter_map(|path| absolute(path).ok())
            .collect();

        let first = arriving.first()?.clone();
        for path in arriving {
            if !self.entries.contains(&path) {
                self.entries.push(path);
            }
        }

        self.current = self.entries.iter().position(|entry| entry == &first)?;
        Some(self.current())
    }

    /// The file the viewer is showing.
    pub fn current(&self) -> &Path {
        &self.entries[self.current]
    }

    /// How many images the arrow keys walk through.
    ///
    /// The filtered count while a filter is up, because this is what the
    /// status line counts against: "3 of 7" has to agree with what pressing
    /// the arrow key seven times does.
    pub fn len(&self) -> usize {
        match &self.shown {
            Some(shown) => shown.len(),
            None => self.entries.len(),
        }
    }

    /// Zero-based position of the current file among the ones being walked.
    ///
    /// While a filter is up and the current file is not in it — which happens
    /// when the filter is switched on with an unmarked picture on screen — the
    /// position is where it *would* fall, so the count never reads as being
    /// past the end.
    pub fn position(&self) -> usize {
        match &self.shown {
            Some(shown) => shown.partition_point(|&index| index < self.current),
            None => self.current,
        }
    }

    /// Walk only the entries a predicate admits.
    ///
    /// The cursor does not move: switching the filter on should not change the
    /// picture on screen, even when that picture is one the filter excludes.
    /// A person marks a frame as rejected, turns the filter on to see what is
    /// left, and expects to still be looking at the frame they just judged —
    /// the *next* arrow key is what takes them into the filtered set.
    ///
    /// Returns how many entries the filter admits. Zero is a real answer and
    /// the caller has to say so rather than presenting an empty walk: with
    /// nothing admitted, the arrow keys have nowhere to go, and a filter that
    /// silently does nothing is worse than one that says it found nothing.
    pub fn show_only(&mut self, admit: impl Fn(&Path) -> bool) -> usize {
        let shown: Vec<usize> = (0..self.entries.len()).filter(|&index| admit(&self.entries[index])).collect();
        let count = shown.len();
        self.shown = Some(shown);
        count
    }

    /// Walk the whole folder again.
    pub fn show_all(&mut self) {
        self.shown = None;
    }

    /// Whether a filter is up.
    pub fn is_filtered(&self) -> bool {
        self.shown.is_some()
    }

    /// Re-run the filter that is up, if one is.
    ///
    /// Marking the picture on screen changes whether it belongs in the walk,
    /// and the walk has to be told: otherwise the arrow key still lands on a
    /// frame the filter now excludes, which reads as the filter not working.
    ///
    /// Recomputed rather than patched at one index, because a mark can be
    /// written to a file that is not the current one — by another program,
    /// between one press and the next — and a patch would keep the stale
    /// answer for every entry but the one just touched.
    pub fn refilter(&mut self, admit: impl Fn(&Path) -> bool) -> usize {
        match self.shown {
            Some(_) => self.show_only(admit),
            None => self.entries.len(),
        }
    }

    /// The current image and `radius` neighbours either side of it.
    ///
    /// Wraps like the navigation does, so the last image of a folder counts
    /// the first as its neighbour — an arrow key there is instant too. Never
    /// repeats a path, however small the folder.
    pub fn neighbourhood(&self, radius: usize) -> Vec<PathBuf> {
        let len = self.entries.len();
        let span = (radius * 2 + 1).min(len);

        (0..span)
            .map(|step| {
                let offset = self.current + len + step - radius.min(len);
                self.entries[offset % len].clone()
            })
            .collect()
    }

    /// Move to the next image, wrapping at the end of the folder.
    ///
    /// Returns `None` when the folder holds a single image, so the caller can
    /// skip a redundant reload.
    pub fn next(&mut self) -> Option<&Path> {
        self.step(1)
    }

    /// Move to the previous image, wrapping at the start of the folder.
    pub fn previous(&mut self) -> Option<&Path> {
        self.step(-1)
    }

    /// Move to the first image being walked.
    pub fn first(&mut self) -> Option<&Path> {
        let index = match &self.shown {
            Some(shown) => *shown.first()?,
            None => 0,
        };
        self.jump(index)
    }

    /// Move to the last image being walked.
    pub fn last(&mut self) -> Option<&Path> {
        let index = match &self.shown {
            Some(shown) => *shown.last()?,
            None => self.entries.len() - 1,
        };
        self.jump(index)
    }

    /// Take the current file out of the listing, because it is not there any
    /// more, and say what to show instead.
    ///
    /// The **next** picture, not the previous one: going through a folder
    /// deleting as you go should carry on forwards, and landing on the frame
    /// you just judged would mean judging it twice. At the end of the folder
    /// there is no next one, so the cursor steps back onto what is now the
    /// last picture.
    ///
    /// `None` when the folder is empty afterwards — the viewer keeps showing
    /// what is on screen, which is a picture of a file that is gone rather
    /// than a blank window, and says so in a message.
    ///
    /// This does not rescan. The listing was taken when the folder opened and
    /// a rescan here would pull in every file that has appeared since, which
    /// is a different folder from the one being worked through.
    pub fn remove_current(&mut self) -> Option<&Path> {
        if self.entries.len() <= 1 {
            self.entries.clear();
            self.shown = None;
            self.current = 0;
            return None;
        }

        self.entries.remove(self.current);
        // Every admitted index above the hole now points one file too far
        // along. Left alone they would keep their old values and the filtered
        // walk would land on the wrong pictures — silently, because a stale
        // index is still a valid one.
        let removed = self.current;
        if let Some(shown) = self.shown.as_mut() {
            shown.retain(|&index| index != removed);
            for index in shown.iter_mut() {
                if *index > removed {
                    *index -= 1;
                }
            }
        }
        // The removal already moved the next picture into this index; only at
        // the very end is there nothing there to move.
        if self.current >= self.entries.len() {
            self.current = self.entries.len() - 1;
        }
        Some(&self.entries[self.current])
    }

    /// The current file is still here, under another name or in another place.
    ///
    /// A rename keeps the cursor where it is: the picture on screen has not
    /// changed, only what it is called. A move takes the file out of this
    /// folder, which is [`remove_current`](Self::remove_current) instead.
    ///
    /// The listing keeps its order rather than being re-sorted around the new
    /// name. Re-sorting would move the picture on screen to somewhere else in
    /// the folder mid-session, so the next arrow key would land somewhere the
    /// person did not expect; the order is settled when the folder opens.
    pub fn rename_current(&mut self, path: PathBuf) {
        if let Some(entry) = self.entries.get_mut(self.current) {
            *entry = path;
        }
    }

    fn step(&mut self, delta: isize) -> Option<&Path> {
        let next = match &self.shown {
            Some(shown) => Self::stepped_within(shown, self.current, delta, self.wrap)?,
            None => Self::stepped(self.entries.len(), self.current, delta, self.wrap)?,
        };
        self.jump(next)
    }

    /// Where a step lands in an unfiltered folder.
    fn stepped(len: usize, current: usize, delta: isize, wrap: bool) -> Option<usize> {
        if len < 2 {
            return None;
        }
        let next = current as isize + delta;
        if wrap {
            Some(next.rem_euclid(len as isize) as usize)
        } else if (0..len as isize).contains(&next) {
            Some(next as usize)
        } else {
            // At the end and told not to come round: staying put is the
            // answer, and `jump` reports "nothing changed" for it.
            None
        }
    }

    /// Where a step lands among the entries a filter admits.
    ///
    /// The cursor is an index into the whole listing and may sit *outside*
    /// `shown` — the filter is switched on without moving the picture, so the
    /// frame on screen is often one the filter excludes. So the step is a
    /// search rather than an addition: find where the cursor falls among the
    /// admitted indices, and move from there.
    ///
    /// `partition_point` gives the number of admitted entries before the
    /// cursor, which is both the cursor's own position when it is admitted and
    /// the position of the next one along when it is not. That single value
    /// serves both directions, which is what keeps this from being two rules
    /// that have to agree.
    fn stepped_within(shown: &[usize], current: usize, delta: isize, wrap: bool) -> Option<usize> {
        if shown.is_empty() {
            return None;
        }

        let before = shown.partition_point(|&index| index < current);
        let inside = shown.get(before) == Some(&current);

        // Going forward from a cursor that is not admitted, the entry at
        // `before` is already the next one along, so the step has been taken
        // by arriving. Everywhere else the offset is the plain one.
        let target = if inside || delta < 0 {
            before as isize + delta
        } else {
            before as isize + delta - 1
        };

        let len = shown.len() as isize;
        let target = if wrap {
            target.rem_euclid(len)
        } else if (0..len).contains(&target) {
            target
        } else {
            return None;
        };

        let landed = shown[target as usize];
        // A folder of one admitted entry, with the cursor already on it, is
        // the "nothing to step to" case the unfiltered path reports as `None`.
        (landed != current).then_some(landed)
    }

    fn jump(&mut self, index: usize) -> Option<&Path> {
        if index == self.current {
            return None;
        }
        self.current = index;
        Some(&self.entries[self.current])
    }
}

/// List the decodable images of a folder, in the order the settings ask for.
///
/// By name is the default and the shell's own order: case-insensitive, so
/// `IMG_2.jpg` precedes `IMG_10.jpg` only when the names say so. Natural
/// numeric ordering, where `IMG_10` follows `IMG_9`, is not offered yet — it
/// is a third way to read a name rather than a fourth thing to sort on, and
/// it belongs beside the name option when it arrives.
fn scan(folder: &Path, order: Order) -> Result<Vec<PathBuf>> {
    let read = folder.read_dir().with_context(|| format!("listing {}", folder.display()))?;

    let mut entries: Vec<PathBuf> = read
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && image_source::is_supported(path))
        .collect();

    sort(&mut entries, order);
    Ok(entries)
}

/// Put the listing in the order the settings ask for.
///
/// Name is the shell's own order and the default. The other two answer a
/// different question — what did I shoot last, what is the big one — and both
/// fall back to the name so that files sharing a date or a size keep a stable
/// order rather than whatever the filesystem happened to hand over.
fn sort(entries: &mut [PathBuf], order: Order) {
    match order {
        Order::Name => entries.sort_by_key(|path| sort_key(path)),
        Order::Modified => entries.sort_by(|a, b| modified(b).cmp(&modified(a)).then_with(|| sort_key(a).cmp(&sort_key(b)))),
        Order::Size => entries.sort_by(|a, b| size(b).cmp(&size(a)).then_with(|| sort_key(a).cmp(&sort_key(b)))),
    }
}

fn sort_key(path: &Path) -> String {
    path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_lowercase()
}

/// When the file was last written, or the epoch for one that will not say.
///
/// A file whose metadata cannot be read sorts as the oldest rather than
/// dropping out of the listing: it is still a picture, and refusing to show
/// it because its timestamp is unreadable would be the wrong trade.
fn modified(path: &Path) -> std::time::SystemTime {
    path.metadata().and_then(|data| data.modified()).unwrap_or(std::time::UNIX_EPOCH)
}

fn size(path: &Path) -> u64 {
    path.metadata().map(|data| data.len()).unwrap_or(0)
}

/// Resolve a path against the working directory without requiring it to exist
/// on disk in a canonical form — `canonicalize` on Windows returns a `\\?\`
/// prefix that would never match the entries produced by `read_dir`.
fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().context("resolving the working directory")?;
    Ok(cwd.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder_with(names: &[&str]) -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().expect("creating a temporary folder");
        let paths = names
            .iter()
            .map(|name| {
                let path = dir.path().join(name);
                std::fs::write(&path, b"placeholder").expect("writing a temporary file");
                path
            })
            .collect();
        (dir, paths)
    }

    #[test]
    fn lists_only_decodable_files_sorted_by_name() {
        let (dir, _) = folder_with(&["b.png", "a.JPG", "notes.txt", "c.gif"]);
        let folder = Folder::open(&dir.path().join("a.JPG"), Order::Name, true).unwrap();

        assert_eq!(folder.len(), 3);
        assert_eq!(folder.position(), 0);
        assert_eq!(folder.current().file_name().unwrap(), "a.JPG");
    }

    #[test]
    fn navigation_wraps_in_both_directions() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        assert_eq!(folder.next().unwrap().file_name().unwrap(), "b.png");
        assert_eq!(folder.next().unwrap().file_name().unwrap(), "c.png");
        assert_eq!(folder.next().unwrap().file_name().unwrap(), "a.png");
        assert_eq!(folder.previous().unwrap().file_name().unwrap(), "c.png");
    }

    /// With wrapping off the ends are ends: the arrow key stops rather than
    /// starting the folder again, which is what someone working through a
    /// shoot in order expects.
    #[test]
    fn navigation_stops_at_the_ends_when_it_is_told_not_to_wrap() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, false).unwrap();

        // Backwards from the first goes nowhere, and leaves the cursor put.
        assert!(folder.previous().is_none());
        assert_eq!(folder.current().file_name().unwrap(), "a.png");

        assert_eq!(folder.next().unwrap().file_name().unwrap(), "b.png");
        assert_eq!(folder.next().unwrap().file_name().unwrap(), "c.png");
        assert!(folder.next().is_none(), "the last image stepped past the end");
        assert_eq!(folder.current().file_name().unwrap(), "c.png");
    }

    /// Date and size order answer a different question from name order, and
    /// both put the most recent or the largest first.
    #[test]
    fn the_order_setting_decides_the_listing() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        // Written biggest-first so that size order is not the order they were
        // created in: a test that agreed with the filesystem by accident
        // would pass with the sort deleted.
        for (name, bytes) in [("a.png", 300), ("b.png", 100), ("c.png", 200)] {
            std::fs::write(dir.path().join(name), vec![0u8; bytes]).expect("writing a temporary file");
        }

        let by_name = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();
        assert_eq!(names(&by_name), ["a.png", "b.png", "c.png"]);

        let by_size = Folder::open(&dir.path().join("a.png"), Order::Size, true).unwrap();
        assert_eq!(names(&by_size), ["a.png", "c.png", "b.png"], "the largest file did not come first");
        // The cursor stays on the file that was opened, wherever the order
        // put it — opening a picture must always show that picture.
        assert_eq!(by_size.current().file_name().unwrap(), "a.png");
    }

    /// A hand-picked selection keeps the order it was given: the person
    /// picked these files in this order.
    #[test]
    fn a_selection_is_not_re_sorted() {
        let (_dir, paths) = folder_with(&["c.png", "a.png", "b.png"]);
        let folder = Folder::of_selection(&paths, true).unwrap();

        assert_eq!(names(&folder), ["c.png", "a.png", "b.png"]);
    }

    /// The file names of a listing, in the order it holds them.
    fn names(folder: &Folder) -> Vec<String> {
        folder
            .entries
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn first_and_last_jump_across_the_folder() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("b.png"), Order::Name, true).unwrap();

        assert_eq!(folder.last().unwrap().file_name().unwrap(), "c.png");
        assert_eq!(folder.first().unwrap().file_name().unwrap(), "a.png");
        // Already there — no reload is asked for.
        assert!(folder.first().is_none());
    }

    #[test]
    fn a_lone_image_reports_no_movement() {
        let (dir, _) = folder_with(&["only.png"]);
        let mut folder = Folder::open(&dir.path().join("only.png"), Order::Name, true).unwrap();

        assert_eq!(folder.len(), 1);
        assert!(folder.next().is_none());
        assert!(folder.previous().is_none());
    }

    /// Deleting carries on forwards: the next picture, not the one already
    /// judged. Going through a folder culling as you go must not put the
    /// frame you just threw away's neighbour behind you.
    #[test]
    fn removing_the_current_file_lands_on_the_next_one() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("b.png"), Order::Name, true).unwrap();

        let landed = folder.remove_current().expect("nothing to show").to_path_buf();
        assert_eq!(landed.file_name().unwrap(), "c.png");
        assert_eq!(folder.len(), 2);
        assert_eq!(folder.current().file_name().unwrap(), "c.png");
    }

    /// At the end there is no next one, so the cursor steps back onto what is
    /// now the last picture rather than off the end of the listing.
    #[test]
    fn removing_the_last_file_steps_back() {
        let (dir, _) = folder_with(&["a.png", "b.png"]);
        let mut folder = Folder::open(&dir.path().join("b.png"), Order::Name, true).unwrap();

        let landed = folder.remove_current().expect("nothing to show").to_path_buf();
        assert_eq!(landed.file_name().unwrap(), "a.png");
        assert_eq!(folder.position(), 0);
        assert_eq!(folder.len(), 1);
    }

    /// The last picture of the folder leaves nothing to show, and that is
    /// said rather than answered with a picture that is not there.
    #[test]
    fn removing_the_only_file_leaves_nothing() {
        let (dir, _) = folder_with(&["only.png"]);
        let mut folder = Folder::open(&dir.path().join("only.png"), Order::Name, true).unwrap();

        assert!(folder.remove_current().is_none());
        assert_eq!(folder.len(), 0);
    }

    /// Removing repeatedly walks the folder to its end without ever naming a
    /// file that has been taken out — the sequence a person culling a whole
    /// folder actually performs.
    #[test]
    fn removing_repeatedly_never_names_a_file_that_is_gone() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        let mut removed = Vec::new();
        loop {
            removed.push(folder.current().to_path_buf());
            let Some(landed) = folder.remove_current().map(Path::to_path_buf) else {
                break;
            };
            assert!(!removed.contains(&landed), "landed back on {} after removing it", landed.display());
        }
        assert_eq!(removed.len(), 3, "the walk did not cover the folder: {removed:?}");
        assert_eq!(folder.len(), 0);
    }

    /// A rename keeps the cursor on the same picture: it is the same picture,
    /// and only its name changed.
    #[test]
    fn renaming_keeps_the_cursor_on_the_picture() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("b.png"), Order::Name, true).unwrap();

        let renamed = dir.path().join("zzz.png");
        folder.rename_current(renamed.clone());

        assert_eq!(folder.current(), renamed, "the cursor left the picture");
        assert_eq!(folder.position(), 1, "the listing was re-sorted around the new name");
        assert_eq!(folder.len(), 3);
        // And stepping still goes where the order says, not where the name
        // would put it if the listing had been sorted again.
        assert_eq!(folder.next().unwrap().file_name().unwrap(), "c.png");
    }

    #[test]
    fn a_file_missing_from_its_folder_still_opens() {
        let (dir, _) = folder_with(&["a.png"]);
        let folder = Folder::open(&dir.path().join("gone.png"), Order::Name, true).unwrap();

        // The cursor falls back to the start rather than refusing to open.
        assert_eq!(folder.position(), 0);
    }

    /// Whether a name is one the filter admits, for tests that do not want a
    /// real mark in a real file: the filter takes a predicate precisely so
    /// that what "marked" means lives in one place and not in this module.
    fn named(names: &[&str]) -> impl Fn(&Path) -> bool + use<> {
        let names: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
        move |path: &Path| names.iter().any(|name| path.file_name().is_some_and(|actual| actual == name.as_str()))
    }

    /// The filter narrows what the arrow keys walk, and the count with it.
    #[test]
    fn a_filter_walks_only_what_it_admits() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png", "d.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        assert_eq!(folder.show_only(named(&["b.png", "d.png"])), 2);
        assert_eq!(folder.len(), 2, "the count did not follow the filter");

        assert_eq!(folder.next().unwrap().file_name().unwrap(), "b.png");
        assert_eq!(folder.next().unwrap().file_name().unwrap(), "d.png");
        assert_eq!(folder.next().unwrap().file_name().unwrap(), "b.png", "the filtered walk did not wrap");
        assert_eq!(folder.previous().unwrap().file_name().unwrap(), "d.png");
    }

    /// Switching the filter on does not move the picture, even when the
    /// picture on screen is one the filter excludes — which is the ordinary
    /// case, since the frame just judged is usually still up.
    #[test]
    fn switching_the_filter_on_leaves_the_picture_alone() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        folder.show_only(named(&["b.png", "c.png"]));

        assert_eq!(
            folder.current().file_name().unwrap(),
            "a.png",
            "the filter moved the picture out from under the viewer"
        );
    }

    /// Stepping away from a picture the filter excludes goes to the nearest
    /// admitted one in that direction, and does not skip it.
    ///
    /// This is the case a plain offset gets wrong: the cursor is not in the
    /// filtered set, so "one along from where I am" has no obvious meaning,
    /// and an implementation that adds one to a searched position steps over
    /// the very next picture.
    #[test]
    fn stepping_off_an_excluded_picture_lands_on_the_neighbour() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png", "d.png", "e.png"]);

        // The cursor sits on c, which the filter excludes; b is behind it and
        // d ahead of it.
        let mut folder = Folder::open(&dir.path().join("c.png"), Order::Name, true).unwrap();
        folder.show_only(named(&["b.png", "d.png"]));
        assert_eq!(
            folder.next().unwrap().file_name().unwrap(),
            "d.png",
            "stepping forward skipped the next admitted picture"
        );

        let mut folder = Folder::open(&dir.path().join("c.png"), Order::Name, true).unwrap();
        folder.show_only(named(&["b.png", "d.png"]));
        assert_eq!(
            folder.previous().unwrap().file_name().unwrap(),
            "b.png",
            "stepping back skipped the previous admitted picture"
        );
    }

    /// The ends of the filtered walk are the ends of the filtered set, not of
    /// the folder.
    #[test]
    fn the_ends_of_a_filtered_walk_are_its_own() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png", "d.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();
        folder.show_only(named(&["b.png", "c.png"]));

        assert_eq!(folder.first().unwrap().file_name().unwrap(), "b.png");
        assert_eq!(folder.last().unwrap().file_name().unwrap(), "c.png");
    }

    /// Without wrapping, a filtered walk stops at its own last admitted
    /// picture rather than at the folder's.
    #[test]
    fn a_filtered_walk_that_does_not_wrap_stops_at_its_own_end() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png", "d.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, false).unwrap();
        folder.show_only(named(&["a.png", "b.png"]));

        assert_eq!(folder.next().unwrap().file_name().unwrap(), "b.png");
        assert!(folder.next().is_none(), "the walk ran past the last admitted picture");
    }

    /// A filter admitting nothing is a real answer: there is nowhere to step.
    #[test]
    fn a_filter_that_admits_nothing_has_nowhere_to_go() {
        let (dir, _) = folder_with(&["a.png", "b.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        assert_eq!(folder.show_only(named(&[])), 0);
        assert_eq!(folder.len(), 0);
        assert!(folder.next().is_none());
        assert!(folder.previous().is_none());
        assert!(folder.first().is_none());
        assert!(folder.last().is_none());
        assert_eq!(folder.current().file_name().unwrap(), "a.png", "an empty filter took the picture away");
    }

    /// A filter admitting exactly the picture on screen has nowhere to step
    /// either — the same answer the unfiltered folder gives for one file.
    #[test]
    fn a_filter_admitting_one_picture_reports_no_step() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("b.png"), Order::Name, true).unwrap();
        folder.show_only(named(&["b.png"]));

        assert!(folder.next().is_none(), "stepping onto the picture already shown was reported as a move");
        assert!(folder.previous().is_none());
    }

    /// Lifting the filter gives the whole folder back, from where the cursor
    /// stands.
    #[test]
    fn lifting_the_filter_gives_the_folder_back() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        folder.show_only(named(&["c.png"]));
        assert!(folder.is_filtered());
        folder.show_all();

        assert!(!folder.is_filtered());
        assert_eq!(folder.len(), 3);
        assert_eq!(folder.next().unwrap().file_name().unwrap(), "b.png");
    }

    /// The position reads against the filtered walk, so the status line's
    /// "n of m" agrees with what the arrow key does.
    #[test]
    fn the_position_counts_within_the_filtered_walk() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png", "d.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();
        folder.show_only(named(&["b.png", "d.png"]));

        folder.next();
        assert_eq!(
            (folder.position(), folder.len()),
            (0, 2),
            "the first admitted picture did not read as the first"
        );
        folder.next();
        assert_eq!((folder.position(), folder.len()), (1, 2));
    }

    /// Re-running the filter after a mark changes picks the change up: a
    /// picture that stops being admitted stops being walked.
    #[test]
    fn refiltering_picks_up_a_mark_that_changed() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();
        folder.show_only(named(&["b.png", "c.png"]));

        // b loses its mark; only c is left to walk.
        assert_eq!(folder.refilter(named(&["c.png"])), 1);
        assert_eq!(folder.next().unwrap().file_name().unwrap(), "c.png");
    }

    /// With no filter up, re-running one does not start filtering.
    #[test]
    fn refiltering_an_unfiltered_folder_leaves_it_unfiltered() {
        let (dir, _) = folder_with(&["a.png", "b.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        assert_eq!(folder.refilter(named(&[])), 2);
        assert!(!folder.is_filtered(), "re-running a filter that was not up switched one on");
        assert_eq!(folder.len(), 2);
    }

    /// Taking a file out of the folder while a filter is up keeps the
    /// admitted indices pointing at the pictures they named.
    ///
    /// Every index above the hole shifts by one. Left alone they stay valid
    /// numbers pointing at the wrong files, which is a walk that lands
    /// somewhere unrelated and never says why.
    #[test]
    fn removing_a_file_keeps_the_filtered_walk_pointing_at_the_right_pictures() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png", "d.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();
        folder.show_only(named(&["c.png", "d.png"]));

        // Take out a.png, which sits below both admitted entries.
        folder.remove_current();

        assert_eq!(folder.len(), 2, "the filtered count changed for a file that was not in it");
        assert_eq!(folder.first().unwrap().file_name().unwrap(), "c.png", "the walk followed a stale index");
        assert_eq!(folder.last().unwrap().file_name().unwrap(), "d.png");
    }

    /// Removing a file that the filter *did* admit takes it out of the walk
    /// as well, rather than leaving an index onto whatever slid into its
    /// place.
    #[test]
    fn removing_an_admitted_file_takes_it_out_of_the_walk() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("b.png"), Order::Name, true).unwrap();
        folder.show_only(named(&["b.png", "c.png"]));

        folder.remove_current();

        assert_eq!(folder.len(), 1, "the removed picture is still being walked");
        // The removal already landed on c.png, which is the one picture the
        // walk still admits — so there is nowhere to step, which this module
        // reports as `None` everywhere (`first` on the first file does the
        // same). What matters is *which* picture the walk is standing on.
        assert_eq!(folder.current().file_name().unwrap(), "c.png");
        assert_eq!(folder.position(), 0, "the surviving picture did not read as the first of the walk");
        assert!(folder.next().is_none(), "a walk of one picture reported somewhere to go");
    }
}

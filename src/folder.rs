//! The list of images sitting next to the one that was opened.
//!
//! Opening a file means opening its folder: arrow keys move through the
//! neighbours in the order the shell shows them, and the ones either side are
//! decoded ahead. The listing follows the folder while it is open (ADR 0034):
//! a picture written into it joins the walk where the order puts it, one taken
//! away leaves it, and one written again is told apart from one that stayed as
//! it was. What the listing never does is re-sort what it already holds — the
//! order is settled when the folder opens, so mid-session the arrow keys go
//! where the person expects them to.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::config::Order;
use crate::image_source;
use crate::pattern::Pattern;

/// What a file looked like on disk when it was last seen.
///
/// The two things writing a file changes. A picture whose stamp moved was
/// written again — a generator overwriting its output, an editor saving over
/// the original — and one whose stamp stayed is the picture already on screen,
/// however many times the folder is read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Stamp {
    modified: SystemTime,
    size: u64,
}

impl Stamp {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            size: metadata.len(),
        }
    }

    /// The stamp of a file asked for by path.
    ///
    /// A file whose metadata cannot be read gets the oldest possible stamp
    /// rather than dropping out of the listing: it is still a picture, and
    /// refusing to show it because its timestamp is unreadable would be the
    /// wrong trade.
    fn read(path: &Path) -> Self {
        path.metadata().map(|metadata| Self::of(&metadata)).unwrap_or(Self {
            modified: SystemTime::UNIX_EPOCH,
            size: 0,
        })
    }
}

/// One picture of the listing.
#[derive(Clone, Debug)]
struct Entry {
    path: PathBuf,
    stamp: Stamp,
}

/// Decides whether a picture carries a mark, for the filter that walks only
/// marked pictures.
///
/// Handed in rather than known here: what "marked" means lives in `cull`, and
/// the listing only has to ask.
pub type Admit = Box<dyn Fn(&Path) -> bool>;

/// What reading the folder again found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Pictures that were not in the listing and now are.
    pub added: Vec<PathBuf>,
    /// Pictures still here that were written again.
    pub rewritten: Vec<PathBuf>,
    /// Pictures that went.
    pub removed: Vec<PathBuf>,
    /// The picture on screen is still here under this name: it went, and a
    /// file with exactly its stamp arrived in the same breath, which is what a
    /// rename in Explorer looks like from outside.
    pub renamed: Option<PathBuf>,
    /// The picture on screen went and did not come back. The cursor is on
    /// the next one now — or on nothing, when it was the last.
    pub current_gone: bool,
    /// A file that arrived or changed is still being written. It is left out
    /// until it is finished, and the folder should be read again shortly even
    /// if nothing else is said about it.
    pub unsettled: bool,
}

impl Changes {
    /// Whether nothing at all changed in what the listing holds — the folder
    /// was read and found as it was, or with only an unfinished file in it.
    pub fn is_quiet(&self) -> bool {
        self.added.is_empty() && self.rewritten.is_empty() && self.removed.is_empty() && self.renamed.is_none() && !self.current_gone
    }
}

/// The images of one folder plus a cursor onto the current file.
pub struct Folder {
    /// The folder the listing was read from. `None` for an explicit selection,
    /// which is a list of files rather than a place, and has nothing to follow.
    dir: Option<PathBuf>,
    entries: Vec<Entry>,
    /// Index into `entries` of the picture on screen. Meaningless while the
    /// listing is empty, which every reader checks first.
    current: usize,
    /// Whether a step past either end comes round to the other.
    ///
    /// On by default. Off, the ends of the folder are ends: the arrow key
    /// stops rather than starting again, which is what someone working
    /// through a shoot in order expects.
    wrap: bool,
    /// The order the listing was put in, kept so a picture arriving later goes
    /// where that order would have put it.
    order: Order,
    /// The name pattern the walk is narrowed to, if any.
    pattern: Option<Pattern>,
    /// Whether only marked pictures are walked, and how to tell.
    marked: Option<Admit>,
    /// Which entries the arrow keys are allowed to land on.
    ///
    /// `None` — the ordinary state — is the whole folder. The two filters, a
    /// name pattern and the marks, narrow what is walked **without** narrowing
    /// what is held: the listing stays whole, so lifting a filter is free and
    /// a file that loses its mark while the filter is up does not vanish from
    /// the folder, only from the walk. Both filters answer into this one list,
    /// because a second list would be a second answer to "what am I browsing".
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
    /// refuse to show the picture that was double-clicked. Such a listing is
    /// not followed — there is no folder it could be read again from.
    pub fn open(path: &Path, order: Order, wrap: bool) -> Result<Self> {
        let path = absolute(path)?;
        let scanned = path
            .parent()
            .and_then(|parent| scan(parent, order).ok().map(|entries| (parent.to_path_buf(), entries)));

        let (dir, entries) = match scanned {
            Some((dir, entries)) if !entries.is_empty() => (Some(dir), entries),
            Some((dir, _)) => (Some(dir), vec![Entry::read(path.clone())]),
            None => (None, vec![Entry::read(path.clone())]),
        };

        let current = entries.iter().position(|entry| entry.path == path).unwrap_or(0);

        Ok(Self {
            dir,
            entries,
            current,
            wrap,
            order,
            pattern: None,
            marked: None,
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
        let entries: Vec<Entry> = paths
            .iter()
            .filter(|path| image_source::is_supported(path))
            .filter_map(|path| absolute(path).ok())
            .map(Entry::read)
            .collect();

        (!entries.is_empty()).then_some(Self {
            dir: None,
            entries,
            current: 0,
            wrap,
            order: Order::Name,
            pattern: None,
            marked: None,
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
            if !self.entries.iter().any(|entry| entry.path == path) {
                self.entries.push(Entry::read(path));
            }
        }

        self.current = self.entries.iter().position(|entry| entry.path == first)?;
        self.narrow();
        self.current()
    }

    /// The folder this listing follows. `None` for a selection.
    pub fn watched(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Move to a picture already in the listing, without stepping through
    /// the ones between — back to the pinned picture when a comparison ends,
    /// on to the newest when the folder is followed.
    ///
    /// False when it is not listed any more: removed or renamed behind the
    /// viewer's back, in which case the cursor stays where it was. A filter
    /// does not stand in the way, for the reason switching one on does not
    /// move the picture: the cursor may rest on a picture the filter leaves
    /// out, and the next step goes into the selection.
    pub fn go_to(&mut self, path: &Path) -> bool {
        match self.entries.iter().position(|entry| entry.path == path) {
            Some(index) => {
                self.current = index;
                true
            }
            None => false,
        }
    }

    /// The file the viewer is showing. `None` once the listing is empty —
    /// every picture deleted, moved, or taken away from outside.
    pub fn current(&self) -> Option<&Path> {
        self.entries.get(self.current).map(|entry| entry.path.as_path())
    }

    /// Whether nothing is left in the listing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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

    /// Walk only marked pictures, or stop.
    ///
    /// The cursor does not move: switching the filter on should not change the
    /// picture on screen, even when that picture is one the filter excludes.
    /// A person marks a frame as rejected, turns the filter on to see what is
    /// left, and expects to still be looking at the frame they just judged —
    /// the *next* arrow key is what takes them into the filtered set.
    ///
    /// Returns how many entries the filters now admit together. Zero is a real
    /// answer and the caller has to say so rather than presenting an empty
    /// walk: with nothing admitted, the arrow keys have nowhere to go, and a
    /// filter that silently does nothing is worse than one that says it found
    /// nothing.
    pub fn set_marked(&mut self, admit: Option<Admit>) -> usize {
        self.marked = admit;
        self.narrow()
    }

    /// Walk only the pictures whose names match, or every name again.
    ///
    /// Leaves the cursor where it is for the reason `set_marked` does, and
    /// answers with the count in the same way. The marks still apply when
    /// both are up: `*.png` and "marked" walk the marked PNGs.
    pub fn set_pattern(&mut self, pattern: Option<Pattern>) -> usize {
        self.pattern = pattern;
        self.narrow()
    }

    /// Whether any filter is up.
    #[cfg(test)]
    fn is_filtered(&self) -> bool {
        self.shown.is_some()
    }

    /// Whether only marked pictures are walked.
    pub fn marked_only(&self) -> bool {
        self.marked.is_some()
    }

    /// The name pattern the walk is narrowed to.
    pub fn pattern(&self) -> Option<&Pattern> {
        self.pattern.as_ref()
    }

    /// Re-run the filters that are up, if any are.
    ///
    /// Marking the picture on screen changes whether it belongs in the walk,
    /// and the walk has to be told: otherwise the arrow key still lands on a
    /// frame the filter now excludes, which reads as the filter not working.
    ///
    /// Recomputed rather than patched at one index, because a mark can be
    /// written to a file that is not the current one — by another program,
    /// between one press and the next — and a patch would keep the stale
    /// answer for every entry but the one just touched.
    pub fn refilter(&mut self) -> usize {
        self.narrow()
    }

    fn narrow(&mut self) -> usize {
        if self.pattern.is_none() && self.marked.is_none() {
            self.shown = None;
            return self.entries.len();
        }

        let shown: Vec<usize> = (0..self.entries.len())
            .filter(|&index| {
                let path = &self.entries[index].path;
                // The pattern first: it is a look at a name, where the mark is
                // a read of the file.
                self.pattern.as_ref().is_none_or(|pattern| pattern.admits(path)) && self.marked.as_ref().is_none_or(|marked| marked(path))
            })
            .collect();
        let count = shown.len();
        self.shown = Some(shown);
        count
    }

    /// The pictures the arrow keys walk, in the folder's order: the whole
    /// listing, or what the filters admit of it.
    ///
    /// What a shuffled slideshow deals a round from, so that a show started
    /// with a filter up shows the filtered pictures and nothing else — the
    /// same answer to "what am I browsing" the arrow keys give.
    pub fn walked(&self) -> Vec<PathBuf> {
        self.walked_indices().map(|index| self.entries[index].path.clone()).collect()
    }

    fn walked_indices(&self) -> Box<dyn Iterator<Item = usize> + '_> {
        match &self.shown {
            Some(shown) => Box::new(shown.iter().copied()),
            None => Box::new(0..self.entries.len()),
        }
    }

    /// Whether the arrow keys could land on `path` now: still listed, and
    /// admitted by the filters if any are up.
    ///
    /// Asked of a picture a shuffle dealt some time ago, which may have been
    /// deleted, moved or filtered out since.
    pub fn walks(&self, path: &Path) -> bool {
        let Some(index) = self.entries.iter().position(|entry| entry.path == path) else {
            return false;
        };
        match &self.shown {
            Some(shown) => shown.binary_search(&index).is_ok(),
            None => true,
        }
    }

    /// The most recently written of the pictures being walked.
    ///
    /// Whatever order the folder is shown in: "the newest" is a question about
    /// when, and a folder sorted by name answers it from anywhere in the
    /// listing.
    pub fn newest(&self) -> Option<&Path> {
        self.walked_indices()
            .max_by_key(|&index| self.entries[index].stamp.modified)
            .map(|index| self.entries[index].path.as_path())
    }

    /// The newest of `paths` that the arrow keys walk — what following the
    /// folder moves to when several pictures arrive at once.
    pub fn newest_of<'a>(&self, paths: &'a [PathBuf]) -> Option<&'a Path> {
        paths
            .iter()
            .filter(|path| self.walks(path))
            .filter_map(|path| self.entries.iter().find(|entry| &entry.path == path).map(|entry| (path, entry.stamp.modified)))
            .max_by_key(|(_, modified)| *modified)
            .map(|(path, _)| path.as_path())
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
                self.entries[offset % len].path.clone()
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
            None if self.entries.is_empty() => return None,
            None => 0,
        };
        self.jump(index)
    }

    /// Move to the last image being walked.
    pub fn last(&mut self) -> Option<&Path> {
        let index = match &self.shown {
            Some(shown) => *shown.last()?,
            None => self.entries.len().checked_sub(1)?,
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
    pub fn remove_current(&mut self) -> Option<&Path> {
        if self.entries.len() <= 1 {
            self.entries.clear();
            self.current = 0;
            self.narrow();
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
        self.current()
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
            entry.path = path;
        }
        // A name pattern may think differently of the new name.
        self.narrow();
    }

    /// A file this viewer itself just wrote into the folder: a crop, a copy,
    /// a picture saved as another format.
    ///
    /// It joins the listing now, where the order puts it, rather than when the
    /// folder is next read — which would then call it an arrival and announce
    /// it a second time, after the message that already named it. Nothing
    /// happens for a file in another folder, or one already listed.
    pub fn adopt(&mut self, path: &Path) {
        let inside = self.dir.as_deref().is_some_and(|dir| path.parent() == Some(dir));
        if !inside || !image_source::is_supported(path) || self.entries.iter().any(|entry| entry.path == path) {
            return;
        }
        self.insert(Entry::read(path.to_path_buf()));
        self.narrow();
    }

    /// A file this viewer itself just wrote to: a mark, a turn saved into it.
    ///
    /// Its stamp is taken again now, so reading the folder afterwards does not
    /// mistake the viewer's own write for someone else's and load the picture
    /// a second time.
    pub fn restamp(&mut self, path: &Path) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.path == path) {
            entry.stamp = Stamp::read(path);
        }
    }

    /// Read the folder again and bring the listing into line with it.
    ///
    /// A file that is new or written again is only taken once nobody is
    /// writing it any more (see `settled`); until then it is left out, or kept
    /// at its old stamp, and the answer says the folder is still settling.
    pub fn rescan(&mut self) -> Result<Changes> {
        let Some(dir) = self.dir.clone() else {
            return Ok(Changes::default());
        };
        let mut found = scan(&dir, self.order)?;

        let held: HashMap<&Path, Stamp> = self.entries.iter().map(|entry| (entry.path.as_path(), entry.stamp)).collect();
        let mut unsettled = false;
        found.retain_mut(|entry| match held.get(entry.path.as_path()) {
            Some(stamp) if *stamp == entry.stamp => true,
            Some(stamp) => {
                if !settled(&entry.path) {
                    entry.stamp = *stamp;
                    unsettled = true;
                }
                true
            }
            // An empty file is one that has just been created and not yet
            // written; it is not a picture of anything yet.
            None if entry.stamp.size == 0 || !settled(&entry.path) => {
                unsettled = true;
                false
            }
            None => true,
        });

        let mut changes = self.sync(found);
        changes.unsettled = unsettled;
        Ok(changes)
    }

    /// Bring the listing into line with what the folder holds now.
    ///
    /// Separate from reading the disk so the bookkeeping — which index the
    /// cursor ends on, where an arrival goes, what counts as a rename — can be
    /// held to account without a real folder changing under a test.
    fn sync(&mut self, found: Vec<Entry>) -> Changes {
        let mut changes = Changes::default();
        let found_stamps: HashMap<PathBuf, Stamp> = found.iter().map(|entry| (entry.path.clone(), entry.stamp)).collect();
        let on_screen = self.entries.get(self.current).cloned();

        for entry in &mut self.entries {
            if let Some(stamp) = found_stamps.get(&entry.path)
                && *stamp != entry.stamp
            {
                entry.stamp = *stamp;
                changes.rewritten.push(entry.path.clone());
            }
        }

        let held: HashSet<PathBuf> = self.entries.iter().map(|entry| entry.path.clone()).collect();
        let mut arrived: Vec<Entry> = found.into_iter().filter(|entry| !held.contains(&entry.path)).collect();
        // Only what lives in this folder can have left it. A file handed over
        // from somewhere else is in the listing because someone opened it,
        // and this folder's contents say nothing about it.
        let dir = self.dir.as_deref();
        let mut gone: HashSet<PathBuf> = self
            .entries
            .iter()
            .filter(|entry| entry.path.parent() == dir && !found_stamps.contains_key(&entry.path))
            .map(|entry| entry.path.clone())
            .collect();

        if let Some(on_screen) = &on_screen
            && gone.contains(&on_screen.path)
        {
            let twins: Vec<usize> = arrived
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.stamp == on_screen.stamp)
                .map(|(index, _)| index)
                .collect();
            // Exactly one: two files with the same size and the same time to
            // the tenth of a microsecond are a copy and its original, and
            // guessing which one is "the picture" would be a coin toss.
            if let [only] = twins[..] {
                let renamed = arrived.remove(only);
                gone.remove(&on_screen.path);
                self.entries[self.current].path = renamed.path.clone();
                changes.renamed = Some(renamed.path);
            }
        }

        if !gone.is_empty() {
            let was = self.current;
            let mut kept = Vec::with_capacity(self.entries.len());
            let mut cursor = None;
            for (index, entry) in std::mem::take(&mut self.entries).into_iter().enumerate() {
                if gone.contains(&entry.path) {
                    changes.removed.push(entry.path);
                    continue;
                }
                // The picture on screen if it stayed; otherwise the first one
                // after it — the rule a delete in the viewer follows.
                if cursor.is_none() && index >= was {
                    cursor = Some(kept.len());
                }
                kept.push(entry);
            }
            changes.current_gone = on_screen.as_ref().is_some_and(|entry| gone.contains(&entry.path));
            // Nothing after it survived: the last of what is left.
            self.current = cursor.unwrap_or(kept.len().saturating_sub(1));
            self.entries = kept;
        }

        arrived.sort_by(|a, b| compare(self.order, a, b));
        for entry in arrived {
            changes.added.push(entry.path.clone());
            self.insert(entry);
        }

        self.narrow();
        changes
    }

    /// Put an entry where the folder's order says, keeping the cursor on the
    /// picture it was on.
    fn insert(&mut self, entry: Entry) {
        let at = self
            .entries
            .partition_point(|held| compare(self.order, held, &entry) == std::cmp::Ordering::Less);
        if !self.entries.is_empty() && at <= self.current {
            self.current += 1;
        }
        self.entries.insert(at, entry);
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
        self.current()
    }
}

impl Entry {
    fn read(path: PathBuf) -> Self {
        let stamp = Stamp::read(&path);
        Self { path, stamp }
    }
}

/// List the decodable images of a folder, in the order the settings ask for.
///
/// By name is the default and the shell's own order: case-insensitive, so
/// `IMG_2.jpg` precedes `IMG_10.jpg` only when the names say so. Natural
/// numeric ordering, where `IMG_10` follows `IMG_9`, is not offered yet — it
/// is a third way to read a name rather than a fourth thing to sort on, and
/// it belongs beside the name option when it arrives.
///
/// The stamp comes from the listing itself: on Windows a directory listing
/// already carries every file's size and time, so asking the entry costs no
/// second trip to the disk. A link is asked through, for what it points at.
fn scan(folder: &Path, order: Order) -> Result<Vec<Entry>> {
    let read = folder.read_dir().with_context(|| format!("listing {}", folder.display()))?;

    let mut entries: Vec<Entry> = read
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            // The extension first: it is free, and most of a folder that is
            // not pictures never needs its metadata asked for.
            if !image_source::is_supported(&path) {
                return None;
            }
            let metadata = match entry.file_type() {
                Ok(kind) if kind.is_symlink() => path.metadata().ok()?,
                _ => entry.metadata().ok()?,
            };
            metadata.is_file().then(|| Entry {
                stamp: Stamp::of(&metadata),
                path,
            })
        })
        .collect();

    entries.sort_by(|a, b| compare(order, a, b));
    Ok(entries)
}

/// The pictures of a folder and when each was last written, by name.
///
/// The listing rules the viewer walks by — which files count as pictures, how
/// a link is followed — for anything else that chooses from a folder: the
/// desktop picture does, and choosing by other rules would let it pick a file
/// the viewer would not show.
pub fn pictures(dir: &Path) -> Result<Vec<(PathBuf, SystemTime)>> {
    Ok(scan(dir, Order::Name)?.into_iter().map(|entry| (entry.path, entry.stamp.modified)).collect())
}

/// The order the settings ask for, as a comparison of two entries.
///
/// Name is the shell's own order and the default. The other two answer a
/// different question — what did I shoot last, what is the big one — and both
/// fall back to the name so that files sharing a date or a size keep a stable
/// order rather than whatever the filesystem happened to hand over. One
/// function for sorting the listing and for placing an arrival in it, so the
/// two cannot disagree about where a picture belongs.
fn compare(order: Order, a: &Entry, b: &Entry) -> std::cmp::Ordering {
    let by_name = || sort_key(&a.path).cmp(&sort_key(&b.path));
    match order {
        Order::Name => by_name(),
        Order::Modified => b.stamp.modified.cmp(&a.stamp.modified).then_with(by_name),
        Order::Size => b.stamp.size.cmp(&a.stamp.size).then_with(by_name),
    }
}

fn sort_key(path: &Path) -> String {
    path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_lowercase()
}

/// Whether nobody is still writing the file.
///
/// Asked by opening it while refusing to share it with a writer: a program
/// still writing holds it open for writing, and Windows answers such an open
/// with a sharing violation. Without this a generator writing a picture in
/// pieces would be caught half-way, and the half would be decoded and shown as
/// a broken file.
#[cfg(windows)]
fn settled(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path).is_ok()
}

/// Elsewhere there is no share mode to ask with, and nothing that reads the
/// folder again, so every file counts as finished.
#[cfg(not(windows))]
fn settled(_path: &Path) -> bool {
    true
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
    use std::time::Duration;

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

    fn name_of(path: Option<&Path>) -> String {
        path.and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    #[test]
    fn lists_only_decodable_files_sorted_by_name() {
        let (dir, _) = folder_with(&["b.png", "a.JPG", "notes.txt", "c.gif"]);
        let folder = Folder::open(&dir.path().join("a.JPG"), Order::Name, true).unwrap();

        assert_eq!(folder.len(), 3);
        assert_eq!(folder.position(), 0);
        assert_eq!(name_of(folder.current()), "a.JPG");
        assert_eq!(folder.watched(), Some(dir.path()), "a folder that was read is followed");
    }

    #[test]
    fn navigation_wraps_in_both_directions() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        assert_eq!(name_of(folder.next()), "b.png");
        assert_eq!(name_of(folder.next()), "c.png");
        assert_eq!(name_of(folder.next()), "a.png");
        assert_eq!(name_of(folder.previous()), "c.png");
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
        assert_eq!(name_of(folder.current()), "a.png");

        assert_eq!(name_of(folder.next()), "b.png");
        assert_eq!(name_of(folder.next()), "c.png");
        assert!(folder.next().is_none(), "the last image stepped past the end");
        assert_eq!(name_of(folder.current()), "c.png");
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
        assert_eq!(name_of(by_size.current()), "a.png");
    }

    /// A hand-picked selection keeps the order it was given: the person
    /// picked these files in this order.
    #[test]
    fn a_selection_is_not_re_sorted() {
        let (_dir, paths) = folder_with(&["c.png", "a.png", "b.png"]);
        let folder = Folder::of_selection(&paths, true).unwrap();

        assert_eq!(names(&folder), ["c.png", "a.png", "b.png"]);
        assert_eq!(folder.watched(), None, "a selection is not a place to follow");
    }

    /// The file names of a listing, in the order it holds them.
    fn names(folder: &Folder) -> Vec<String> {
        folder
            .entries
            .iter()
            .map(|entry| entry.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn first_and_last_jump_across_the_folder() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("b.png"), Order::Name, true).unwrap();

        assert_eq!(name_of(folder.last()), "c.png");
        assert_eq!(name_of(folder.first()), "a.png");
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

        let landed = name_of(folder.remove_current());
        assert_eq!(landed, "c.png");
        assert_eq!(folder.len(), 2);
        assert_eq!(name_of(folder.current()), "c.png");
    }

    /// At the end there is no next one, so the cursor steps back onto what is
    /// now the last picture rather than off the end of the listing.
    #[test]
    fn removing_the_last_file_steps_back() {
        let (dir, _) = folder_with(&["a.png", "b.png"]);
        let mut folder = Folder::open(&dir.path().join("b.png"), Order::Name, true).unwrap();

        let landed = name_of(folder.remove_current());
        assert_eq!(landed, "a.png");
        assert_eq!(folder.position(), 0);
        assert_eq!(folder.len(), 1);
    }

    /// The last picture of the folder leaves nothing to show, and that is
    /// said rather than answered with a picture that is not there — and
    /// asking an empty listing anything does not bring the viewer down.
    #[test]
    fn removing_the_only_file_leaves_nothing() {
        let (dir, _) = folder_with(&["only.png"]);
        let mut folder = Folder::open(&dir.path().join("only.png"), Order::Name, true).unwrap();

        assert!(folder.remove_current().is_none());
        assert_eq!(folder.len(), 0);
        assert!(folder.is_empty());
        // Each of these panicked on an empty listing before v0.37: the title
        // asked for the current name after the last picture was deleted.
        assert_eq!(folder.current(), None);
        assert_eq!(folder.first(), None);
        assert_eq!(folder.last(), None);
        assert_eq!(folder.next(), None);
        assert_eq!(folder.newest(), None);
        assert!(folder.neighbourhood(2).is_empty());
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
            removed.push(folder.current().unwrap().to_path_buf());
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

        assert_eq!(folder.current(), Some(renamed.as_path()), "the cursor left the picture");
        assert_eq!(folder.position(), 1, "the listing was re-sorted around the new name");
        assert_eq!(folder.len(), 3);
        // And stepping still goes where the order says, not where the name
        // would put it if the listing had been sorted again.
        assert_eq!(name_of(folder.next()), "c.png");
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
    fn named(names: &[&str]) -> Option<Admit> {
        let names: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
        Some(Box::new(move |path: &Path| {
            names.iter().any(|name| path.file_name().is_some_and(|actual| actual == name.as_str()))
        }))
    }

    /// A comparison ends on the pinned picture, however far the walk went:
    /// the cursor goes straight there and the next step carries on from it.
    #[test]
    fn going_to_a_listed_picture_moves_the_cursor_there() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png", "d.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, false).unwrap();
        let pinned = folder.current().unwrap().to_path_buf();
        folder.next();
        folder.next();

        assert!(folder.go_to(&pinned));
        assert_eq!(folder.current(), Some(pinned.as_path()));
        assert_eq!(name_of(folder.next()), "b.png", "the walk did not carry on from it");

        let gone = dir.path().join("gone.png");
        let before = folder.current().unwrap().to_path_buf();
        assert!(!folder.go_to(&gone), "a picture that is not listed was gone to");
        assert_eq!(folder.current(), Some(before.as_path()), "a refused move moved the cursor");
    }

    /// The filter narrows what the arrow keys walk, and the count with it.
    #[test]
    fn a_filter_walks_only_what_it_admits() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png", "d.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        assert_eq!(folder.set_marked(named(&["b.png", "d.png"])), 2);
        assert_eq!(folder.len(), 2, "the count did not follow the filter");

        assert_eq!(name_of(folder.next()), "b.png");
        assert_eq!(name_of(folder.next()), "d.png");
        assert_eq!(name_of(folder.next()), "b.png", "the filtered walk did not wrap");
        assert_eq!(name_of(folder.previous()), "d.png");
    }

    /// Switching the filter on does not move the picture, even when the
    /// picture on screen is one the filter excludes — which is the ordinary
    /// case, since the frame just judged is usually still up.
    #[test]
    fn switching_the_filter_on_leaves_the_picture_alone() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        folder.set_marked(named(&["b.png", "c.png"]));

        assert_eq!(name_of(folder.current()), "a.png", "the filter moved the picture out from under the viewer");
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
        folder.set_marked(named(&["b.png", "d.png"]));
        assert_eq!(name_of(folder.next()), "d.png", "stepping forward skipped the next admitted picture");

        let mut folder = Folder::open(&dir.path().join("c.png"), Order::Name, true).unwrap();
        folder.set_marked(named(&["b.png", "d.png"]));
        assert_eq!(name_of(folder.previous()), "b.png", "stepping back skipped the previous admitted picture");
    }

    /// The ends of the filtered walk are the ends of the filtered set, not of
    /// the folder.
    #[test]
    fn the_ends_of_a_filtered_walk_are_its_own() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png", "d.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();
        folder.set_marked(named(&["b.png", "c.png"]));

        assert_eq!(name_of(folder.first()), "b.png");
        assert_eq!(name_of(folder.last()), "c.png");
    }

    /// Without wrapping, a filtered walk stops at its own last admitted
    /// picture rather than at the folder's.
    #[test]
    fn a_filtered_walk_that_does_not_wrap_stops_at_its_own_end() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png", "d.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, false).unwrap();
        folder.set_marked(named(&["a.png", "b.png"]));

        assert_eq!(name_of(folder.next()), "b.png");
        assert!(folder.next().is_none(), "the walk ran past the last admitted picture");
    }

    /// A filter admitting nothing is a real answer: there is nowhere to step.
    #[test]
    fn a_filter_that_admits_nothing_has_nowhere_to_go() {
        let (dir, _) = folder_with(&["a.png", "b.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        assert_eq!(folder.set_marked(named(&[])), 0);
        assert_eq!(folder.len(), 0);
        assert!(folder.next().is_none());
        assert!(folder.previous().is_none());
        assert!(folder.first().is_none());
        assert!(folder.last().is_none());
        assert_eq!(name_of(folder.current()), "a.png", "an empty filter took the picture away");
    }

    /// A filter admitting exactly the picture on screen has nowhere to step
    /// either — the same answer the unfiltered folder gives for one file.
    #[test]
    fn a_filter_admitting_one_picture_reports_no_step() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("b.png"), Order::Name, true).unwrap();
        folder.set_marked(named(&["b.png"]));

        assert!(folder.next().is_none(), "stepping onto the picture already shown was reported as a move");
        assert!(folder.previous().is_none());
    }

    /// Lifting the filter gives the whole folder back, from where the cursor
    /// stands.
    #[test]
    fn lifting_the_filter_gives_the_folder_back() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        folder.set_marked(named(&["c.png"]));
        assert!(folder.is_filtered());
        folder.set_marked(None);

        assert!(!folder.is_filtered());
        assert_eq!(folder.len(), 3);
        assert_eq!(name_of(folder.next()), "b.png");
    }

    /// The position reads against the filtered walk, so the status line's
    /// "n of m" agrees with what the arrow key does.
    #[test]
    fn the_position_counts_within_the_filtered_walk() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png", "d.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();
        folder.set_marked(named(&["b.png", "d.png"]));

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
        let marked = std::rc::Rc::new(std::cell::RefCell::new(vec!["b.png".to_string(), "c.png".to_string()]));
        let reading = marked.clone();
        folder.set_marked(Some(Box::new(move |path: &Path| {
            reading
                .borrow()
                .iter()
                .any(|name| path.file_name().is_some_and(|actual| actual == name.as_str()))
        })));

        // b loses its mark; only c is left to walk.
        marked.borrow_mut().retain(|name| name != "b.png");
        assert_eq!(folder.refilter(), 1);
        assert_eq!(name_of(folder.next()), "c.png");
    }

    /// With no filter up, re-running one does not start filtering.
    #[test]
    fn refiltering_an_unfiltered_folder_leaves_it_unfiltered() {
        let (dir, _) = folder_with(&["a.png", "b.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        assert_eq!(folder.refilter(), 2);
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
        folder.set_marked(named(&["c.png", "d.png"]));

        // Take out a.png, which sits below both admitted entries.
        folder.remove_current();

        assert_eq!(folder.len(), 2, "the filtered count changed for a file that was not in it");
        assert_eq!(name_of(folder.first()), "c.png", "the walk followed a stale index");
        assert_eq!(name_of(folder.last()), "d.png");
    }

    /// Removing a file that the filter *did* admit takes it out of the walk
    /// as well, rather than leaving an index onto whatever slid into its
    /// place.
    #[test]
    fn removing_an_admitted_file_takes_it_out_of_the_walk() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("b.png"), Order::Name, true).unwrap();
        folder.set_marked(named(&["b.png", "c.png"]));

        folder.remove_current();

        assert_eq!(folder.len(), 1, "the removed picture is still being walked");
        // The removal already landed on c.png, which is the one picture the
        // walk still admits — so there is nowhere to step, which this module
        // reports as `None` everywhere (`first` on the first file does the
        // same). What matters is *which* picture the walk is standing on.
        assert_eq!(name_of(folder.current()), "c.png");
        assert_eq!(folder.position(), 0, "the surviving picture did not read as the first of the walk");
        assert!(folder.next().is_none(), "a walk of one picture reported somewhere to go");
    }

    /// A shuffle deals from what the arrow keys walk and lands only where
    /// they could: a filter narrows both, and a file that went is in neither.
    #[test]
    fn a_shuffle_is_told_what_the_arrow_keys_walk() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png", "d.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();
        let name = |path: &PathBuf| path.file_name().unwrap().to_string_lossy().into_owned();

        assert_eq!(folder.walked().iter().map(name).collect::<Vec<_>>(), ["a.png", "b.png", "c.png", "d.png"]);
        assert!(folder.walks(&dir.path().join("c.png")));

        folder.set_marked(named(&["b.png", "d.png"]));
        assert_eq!(folder.walked().iter().map(name).collect::<Vec<_>>(), ["b.png", "d.png"]);
        assert!(!folder.walks(&dir.path().join("c.png")), "a picture the filter leaves out was walkable");
        assert!(folder.walks(&dir.path().join("d.png")));

        folder.set_marked(None);
        folder.go_to(&dir.path().join("c.png"));
        folder.remove_current();
        assert!(!folder.walks(&dir.path().join("c.png")), "a removed picture was walkable");
        assert!(!folder.walks(&dir.path().join("never-there.png")));
    }

    /// A name pattern narrows the walk like the marks do, and the two
    /// together walk what both admit.
    #[test]
    fn a_name_pattern_narrows_the_walk_and_composes_with_the_marks() {
        let (dir, _) = folder_with(&["a.png", "a.jpg", "b.png", "b.jpg", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("a.jpg"), Order::Name, true).unwrap();
        let name = |path: &PathBuf| path.file_name().unwrap().to_string_lossy().into_owned();

        assert_eq!(folder.set_pattern(Pattern::parse("*.png")), 3);
        assert_eq!(folder.walked().iter().map(name).collect::<Vec<_>>(), ["a.png", "b.png", "c.png"]);
        assert_eq!(name_of(folder.current()), "a.jpg", "the pattern moved the picture on screen");

        assert_eq!(folder.set_marked(named(&["b.png", "b.jpg"])), 1, "the two filters did not compose");
        assert_eq!(folder.walked().iter().map(name).collect::<Vec<_>>(), ["b.png"]);

        // Lifting the pattern leaves the marks up, and the other way round.
        assert_eq!(folder.set_pattern(None), 2);
        assert!(folder.is_filtered());
        assert_eq!(folder.set_marked(None), 5);
        assert!(!folder.is_filtered());
    }

    /// "The newest" is a question about when, whatever the order: a folder
    /// sorted by name answers it from the middle of the listing.
    #[test]
    fn the_newest_is_decided_by_time_and_only_among_the_walked() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.jpg"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        for (entry, age) in folder.entries.iter_mut().zip([30, 0, 10]) {
            entry.stamp.modified = base - Duration::from_secs(age);
        }

        assert_eq!(name_of(folder.newest()), "b.png");
        folder.set_pattern(Pattern::parse("*.jpg"));
        assert_eq!(name_of(folder.newest()), "c.jpg", "the newest ignored the filter");
    }

    fn entry(dir: &Path, name: &str, seconds: u64, size: u64) -> Entry {
        Entry {
            path: dir.join(name),
            stamp: Stamp {
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
                size,
            },
        }
    }

    /// A listing over a folder that only exists as entries, so the
    /// bookkeeping can be watched without a disk changing under the test.
    fn listing(dir: &Path, order: Order, entries: Vec<Entry>, current: usize) -> Folder {
        Folder {
            dir: Some(dir.to_path_buf()),
            entries,
            current,
            wrap: true,
            order,
            pattern: None,
            marked: None,
            shown: None,
        }
    }

    /// A picture written into the folder joins the walk where the order puts
    /// it, and the cursor stays on the picture it was on.
    #[test]
    fn an_arrival_goes_where_the_order_puts_it() {
        let dir = Path::new("pictures");
        let mut folder = listing(dir, Order::Name, vec![entry(dir, "a.png", 1, 1), entry(dir, "c.png", 1, 1)], 1);

        let changes = folder.sync(vec![entry(dir, "a.png", 1, 1), entry(dir, "b.png", 2, 1), entry(dir, "c.png", 1, 1)]);

        assert_eq!(changes.added, vec![dir.join("b.png")]);
        assert_eq!(names(&folder), ["a.png", "b.png", "c.png"]);
        assert_eq!(name_of(folder.current()), "c.png", "an arrival before the cursor moved the picture on screen");
        assert_eq!(folder.position(), 2);
    }

    /// Newest first is where a picture that has just been written belongs.
    #[test]
    fn in_date_order_an_arrival_goes_first() {
        let dir = Path::new("pictures");
        let mut folder = listing(dir, Order::Modified, vec![entry(dir, "old.png", 20, 1), entry(dir, "older.png", 10, 1)], 0);

        folder.sync(vec![entry(dir, "old.png", 20, 1), entry(dir, "older.png", 10, 1), entry(dir, "zzz.png", 30, 1)]);

        assert_eq!(names(&folder), ["zzz.png", "old.png", "older.png"]);
        assert_eq!(name_of(folder.current()), "old.png");
    }

    /// A picture taken away from outside leaves the walk; the one on screen
    /// stays on screen.
    #[test]
    fn a_picture_taken_away_leaves_the_walk() {
        let dir = Path::new("pictures");
        let mut folder = listing(
            dir,
            Order::Name,
            vec![entry(dir, "a.png", 1, 1), entry(dir, "b.png", 1, 1), entry(dir, "c.png", 1, 1)],
            2,
        );

        let changes = folder.sync(vec![entry(dir, "b.png", 1, 1), entry(dir, "c.png", 1, 1)]);

        assert_eq!(changes.removed, vec![dir.join("a.png")]);
        assert!(!changes.current_gone);
        assert_eq!(name_of(folder.current()), "c.png");
        assert_eq!(folder.position(), 1);
    }

    /// The picture on screen taken away from outside goes the way a delete
    /// in the viewer goes: on to the next, or back at the end.
    #[test]
    fn the_picture_on_screen_taken_away_moves_the_cursor_on() {
        let dir = Path::new("pictures");
        let mut folder = listing(
            dir,
            Order::Name,
            vec![entry(dir, "a.png", 1, 1), entry(dir, "b.png", 1, 1), entry(dir, "c.png", 1, 1)],
            1,
        );

        let changes = folder.sync(vec![entry(dir, "a.png", 1, 1), entry(dir, "c.png", 1, 1)]);
        assert!(changes.current_gone);
        assert_eq!(name_of(folder.current()), "c.png", "the cursor did not move on to the next picture");

        let changes = folder.sync(vec![entry(dir, "a.png", 1, 1)]);
        assert!(changes.current_gone);
        assert_eq!(name_of(folder.current()), "a.png", "at the end the cursor did not step back");

        let changes = folder.sync(Vec::new());
        assert!(changes.current_gone);
        assert_eq!(folder.current(), None);

        // And an emptied folder that fills again has a picture to show.
        let changes = folder.sync(vec![entry(dir, "new.png", 5, 1)]);
        assert_eq!(changes.added, vec![dir.join("new.png")]);
        assert_eq!(name_of(folder.current()), "new.png");
    }

    /// The picture on screen renamed in Explorer is the same picture: it went
    /// and a file with exactly its stamp came, so the cursor stays on it
    /// under the new name instead of jumping to the next one.
    #[test]
    fn the_picture_on_screen_renamed_outside_keeps_the_cursor() {
        let dir = Path::new("pictures");
        let mut folder = listing(
            dir,
            Order::Name,
            vec![entry(dir, "a.png", 1, 1), entry(dir, "b.png", 7, 77), entry(dir, "c.png", 1, 1)],
            1,
        );

        let changes = folder.sync(vec![entry(dir, "a.png", 1, 1), entry(dir, "c.png", 1, 1), entry(dir, "zebra.png", 7, 77)]);

        assert_eq!(changes.renamed, Some(dir.join("zebra.png")));
        assert!(!changes.current_gone);
        assert!(
            changes.added.is_empty() && changes.removed.is_empty(),
            "a rename read as a removal and an arrival: {changes:?}"
        );
        assert_eq!(name_of(folder.current()), "zebra.png");
        assert_eq!(folder.position(), 1, "the renamed picture was re-sorted");
    }

    /// Two arrivals with the stamp of the picture that went are a copy and
    /// its original, and which one is "the picture" is not guessed.
    #[test]
    fn a_rename_is_not_guessed_between_two_twins() {
        let dir = Path::new("pictures");
        let mut folder = listing(dir, Order::Name, vec![entry(dir, "a.png", 1, 1), entry(dir, "b.png", 7, 77)], 1);

        let changes = folder.sync(vec![entry(dir, "a.png", 1, 1), entry(dir, "x.png", 7, 77), entry(dir, "y.png", 7, 77)]);

        assert_eq!(changes.renamed, None);
        assert!(changes.current_gone);
        assert_eq!(changes.added.len(), 2);
    }

    /// A picture written again is told apart from one that stayed as it was.
    #[test]
    fn a_picture_written_again_is_reported_as_written_again() {
        let dir = Path::new("pictures");
        let mut folder = listing(dir, Order::Name, vec![entry(dir, "a.png", 1, 1), entry(dir, "b.png", 1, 1)], 0);

        let changes = folder.sync(vec![entry(dir, "a.png", 1, 1), entry(dir, "b.png", 2, 5)]);
        assert_eq!(changes.rewritten, vec![dir.join("b.png")]);
        assert!(changes.added.is_empty() && changes.removed.is_empty());

        // Read again with nothing changed, nothing is reported.
        let changes = folder.sync(vec![entry(dir, "a.png", 1, 1), entry(dir, "b.png", 2, 5)]);
        assert_eq!(changes, Changes::default());
    }

    /// A file handed over from another folder is not this folder's to take
    /// away when this folder is read again.
    #[test]
    fn a_picture_from_elsewhere_is_not_taken_away() {
        let dir = Path::new("pictures");
        let elsewhere = Path::new("downloads");
        let mut folder = listing(dir, Order::Name, vec![entry(dir, "a.png", 1, 1), entry(elsewhere, "z.png", 1, 1)], 1);

        let changes = folder.sync(vec![entry(dir, "a.png", 1, 1)]);

        assert!(changes.removed.is_empty());
        assert_eq!(name_of(folder.current()), "z.png");
    }

    /// An arrival goes through the filters that are up, and the walk's count
    /// follows.
    #[test]
    fn an_arrival_is_filtered_like_the_rest() {
        let dir = Path::new("pictures");
        let mut folder = listing(dir, Order::Name, vec![entry(dir, "a.png", 1, 1), entry(dir, "b.jpg", 1, 1)], 0);
        folder.set_pattern(Pattern::parse("*.png"));
        assert_eq!(folder.len(), 1);

        folder.sync(vec![
            entry(dir, "a.png", 1, 1),
            entry(dir, "b.jpg", 1, 1),
            entry(dir, "c.jpg", 2, 1),
            entry(dir, "d.png", 2, 1),
        ]);

        assert_eq!(folder.len(), 2, "the arrivals were not filtered");
        assert!(folder.walks(&dir.join("d.png")));
        assert!(!folder.walks(&dir.join("c.jpg")));
        assert_eq!(folder.newest_of(&[dir.join("c.jpg"), dir.join("d.png")]), Some(dir.join("d.png").as_path()));
    }

    /// The viewer's own copy joins the listing at once, and reading the
    /// folder afterwards does not announce it again.
    #[test]
    fn the_viewers_own_copy_is_adopted_not_announced() {
        let (dir, _) = folder_with(&["a.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("c.png"), Order::Name, true).unwrap();

        let copy = dir.path().join("b.png");
        std::fs::write(&copy, b"placeholder").unwrap();
        folder.adopt(&copy);
        assert_eq!(names(&folder), ["a.png", "b.png", "c.png"]);
        assert_eq!(name_of(folder.current()), "c.png");

        let changes = folder.rescan().unwrap();
        assert!(changes.added.is_empty(), "the viewer's own copy was announced as an arrival: {changes:?}");

        // A file in another folder, or one already listed, is left alone.
        folder.adopt(Path::new("elsewhere").join("x.png").as_path());
        folder.adopt(&copy);
        assert_eq!(folder.len(), 3);
    }

    /// Reading a real folder: an arrival, a file written again and a file
    /// taken away, all in one pass.
    #[test]
    fn reading_the_folder_again_finds_what_changed_on_disk() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        std::fs::write(dir.path().join("d.png"), b"a new picture").unwrap();
        std::fs::write(dir.path().join("b.png"), b"written again, and longer").unwrap();
        std::fs::remove_file(dir.path().join("c.png")).unwrap();

        let changes = folder.rescan().unwrap();
        assert_eq!(changes.added, vec![dir.path().join("d.png")]);
        assert_eq!(changes.rewritten, vec![dir.path().join("b.png")]);
        assert_eq!(changes.removed, vec![dir.path().join("c.png")]);
        assert!(!changes.unsettled);
        assert_eq!(names(&folder), ["a.png", "b.png", "d.png"]);
    }

    /// A file that has only just been created, with nothing in it yet, is not
    /// a picture; it is taken once it has been written.
    #[test]
    fn an_empty_new_file_waits_until_it_has_been_written() {
        let (dir, _) = folder_with(&["a.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        std::fs::write(dir.path().join("b.png"), b"").unwrap();
        let changes = folder.rescan().unwrap();
        assert!(changes.added.is_empty());
        assert!(changes.unsettled, "an unfinished file did not ask for another look");

        std::fs::write(dir.path().join("b.png"), b"now it is a picture").unwrap();
        let changes = folder.rescan().unwrap();
        assert_eq!(changes.added, vec![dir.path().join("b.png")]);
    }

    /// A file a program still holds open for writing is caught half-way if it
    /// is taken; it waits until the program lets go.
    #[cfg(windows)]
    #[test]
    fn a_file_still_being_written_waits_until_it_is_finished() {
        use std::io::Write;

        let (dir, _) = folder_with(&["a.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png"), Order::Name, true).unwrap();

        let mut writing = std::fs::File::create(dir.path().join("b.png")).unwrap();
        writing.write_all(b"the first half").unwrap();
        writing.flush().unwrap();
        let changes = folder.rescan().unwrap();
        assert!(changes.added.is_empty(), "a file still open for writing was taken");
        assert!(changes.unsettled);

        drop(writing);
        let changes = folder.rescan().unwrap();
        assert_eq!(changes.added, vec![dir.path().join("b.png")]);
    }
}

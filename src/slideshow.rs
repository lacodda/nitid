//! A slideshow: the clock that moves it on, and the order it moves in.
//!
//! Like the animation player and the blink, nothing here owns a window. The
//! event loop asks when to wake and what comes next, which keeps the rules
//! testable as plain values and keeps a paused show — or no show at all — free
//! of wakeups.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The intervals the arrow keys step through, in seconds.
///
/// Uneven on purpose. The difference between two seconds and three is felt;
/// the difference between two minutes and two minutes and a second is not. So
/// the steps grow with the interval, and the long end reaches a picture frame
/// on a wall, which is what a television in a living room turns into.
pub const INTERVALS: [u32; 17] = [1, 2, 3, 4, 5, 7, 10, 15, 20, 30, 45, 60, 90, 120, 180, 300, 600];

/// How long each picture stays up, in seconds, before anyone has said.
///
/// Long enough to take a photograph in, short enough that a folder of a
/// hundred moves.
pub const DEFAULT_INTERVAL: u32 = 5;

/// The shortest and the longest interval the settings accept.
pub const MIN_INTERVAL: u32 = INTERVALS[0];
pub const MAX_INTERVAL: u32 = INTERVALS[INTERVALS.len() - 1];

/// The longest an animation may keep its slide up past the interval.
///
/// An animation is shown through one whole cycle rather than cut off in the
/// middle, which is what showing it means. A cycle of ten minutes is possible
/// in a file, though, and a show that stopped on it would look stuck; past
/// this the interval wins, and the arrow key is always there.
pub const MAX_HOLD: Duration = Duration::from_secs(60);

/// The next interval up from `seconds`.
pub fn longer(seconds: u32) -> u32 {
    INTERVALS.iter().copied().find(|&step| step > seconds).unwrap_or(MAX_INTERVAL)
}

/// The next interval down from `seconds`.
pub fn shorter(seconds: u32) -> u32 {
    INTERVALS.iter().rev().copied().find(|&step| step < seconds).unwrap_or(MIN_INTERVAL)
}

/// An interval the way a person says it: "5 s", "2 min", "1 min 30 s".
pub fn interval_text(seconds: u32) -> String {
    match (seconds / 60, seconds % 60) {
        (0, seconds) => format!("{seconds} s"),
        (minutes, 0) => format!("{minutes} min"),
        (minutes, seconds) => format!("{minutes} min {seconds} s"),
    }
}

/// When the show moves on.
///
/// The interval is counted from the moment the picture is **whole** on
/// screen, not from the moment it was asked for. A sixty-megapixel file can
/// take a second to decode, and a clock that started on the key would show it
/// for four seconds of five while another one, already prefetched, got all
/// five. Counting from the arrival gives every picture the time the setting
/// promises — which is the setting's whole meaning.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    interval: Duration,
    state: State,
    /// Stopped by a person. Kept apart from `state` because a pause can begin
    /// before the picture has arrived, and the arrival still has to be noted.
    paused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// The picture has been asked for and is not whole on screen yet.
    Waiting,
    /// It is up, since this moment, and asks to stay at least this long.
    Showing { since: Instant, hold: Duration },
}

impl Clock {
    /// A clock waiting for its first picture.
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            state: State::Waiting,
            paused: false,
        }
    }

    /// The picture is whole on screen.
    ///
    /// `hold` is how long the picture itself asks for — one cycle of an
    /// animation — and it only ever lengthens the slide, up to [`MAX_HOLD`].
    /// A second arrival for a slide already showing changes nothing: the same
    /// picture can be uploaded again, and that is not a new slide.
    pub fn arrived(&mut self, now: Instant, hold: Duration) {
        if self.state == State::Waiting {
            self.state = State::Showing {
                since: now,
                hold: hold.min(MAX_HOLD),
            };
        }
    }

    /// Another picture has been asked for; the count waits for it.
    pub fn stepped(&mut self) {
        self.state = State::Waiting;
    }

    /// Stop the show, or let it go on. Says whether it is now paused.
    ///
    /// Going on gives the picture on screen a whole interval again: a person
    /// who paused was looking at it, and moving on a moment after they let go
    /// would take it away before they had looked back at the room.
    pub fn toggle_paused(&mut self, now: Instant) -> bool {
        self.paused = !self.paused;
        if !self.paused {
            self.hold_still(now);
        }
        self.paused
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Start the count for the picture on screen again from `now`.
    ///
    /// What a box over the picture does for as long as it is up: a person
    /// typing a new name for this photograph must not have it changed under
    /// the name.
    pub fn hold_still(&mut self, now: Instant) {
        if let State::Showing { since, .. } = &mut self.state {
            *since = now;
        }
    }

    /// Take a new interval, for the picture on screen as well as the ones
    /// after it: someone who shortens the interval because the show is
    /// dragging wants the next picture sooner, not one picture later.
    pub fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
    }

    /// When the show moves on, or `None` while it is paused or waiting for
    /// the picture — the two states in which nothing is due, and the loop
    /// sleeps.
    pub fn due(&self) -> Option<Instant> {
        if self.paused {
            return None;
        }
        match self.state {
            State::Waiting => None,
            State::Showing { since, hold } => Some(since + self.interval.max(hold)),
        }
    }

    /// Whether the show should move on at `now`.
    pub fn is_due(&self, now: Instant) -> bool {
        self.due().is_some_and(|due| now >= due)
    }
}

/// How many pictures a shuffled show remembers having shown, for the left
/// arrow to walk back through.
///
/// The current round is kept whatever its size — it is what stops a picture
/// coming up twice before the others have had their turn — so this bounds only
/// the rounds before it, for a show left running for days.
const HISTORY: usize = 1000;

/// A random order through a folder: every picture once, then again in another
/// order.
///
/// Not a random pick at every step. Drawing each picture independently shows
/// some of them three times before others once, which in a folder of thirty
/// photographs from one holiday is what everyone in the room notices. A
/// shuffled deck shows each one once per round, and the left arrow goes back
/// through what was actually shown.
pub struct Shuffle {
    /// What has been shown, oldest first.
    history: Vec<PathBuf>,
    /// Where in `history` the show is. The last entry, unless the left arrow
    /// has been pressed.
    at: usize,
    /// Where in `history` this round began.
    round: usize,
    /// The rest of this round, the next one last.
    ahead: Vec<PathBuf>,
    rng: Rng,
}

impl Shuffle {
    /// A shuffle that starts on `current` and deals the rest of `walked`
    /// after it.
    ///
    /// The picture on screen is the first of the show, because it is the one
    /// the person was looking at when they asked for it; the round then
    /// deals everything else, so it is not shown twice in the first round.
    pub fn new(current: &Path, walked: Vec<PathBuf>, seed: u64) -> Self {
        let mut shuffle = Self {
            history: vec![current.to_path_buf()],
            at: 0,
            round: 0,
            ahead: Vec::new(),
            rng: Rng(seed),
        };
        shuffle.deal(walked);
        shuffle
    }

    /// Move on, and say where to.
    ///
    /// `walks` says whether a picture can still be landed on — it may have
    /// been deleted, renamed or filtered out since it was dealt — and those
    /// that cannot are passed over. `walked` lists what can be landed on now,
    /// asked for only when a round runs out.
    ///
    /// `None` when nothing else can be shown: a folder of one, or one that
    /// emptied under the show.
    pub fn next(&mut self, walks: impl Fn(&Path) -> bool, walked: impl FnOnce() -> Vec<PathBuf>) -> Option<PathBuf> {
        // Forward through what was shown before, after the left arrow went
        // back: the right arrow retraces the same steps rather than dealing
        // new ones.
        if let Some(index) = (self.at + 1..self.history.len()).find(|&index| walks(&self.history[index])) {
            self.at = index;
            return Some(self.history[index].clone());
        }

        let next = self.draw(&walks).or_else(|| {
            // The round is used up: deal the next one and draw again.
            self.round = self.history.len();
            self.deal(walked());
            self.draw(&walks)
        })?;
        // A round of one is the picture already up. Moving "on" to it would
        // restart its count and call that a slide.
        if next == self.history[self.at] {
            return None;
        }

        // Moving on from a picture the left arrow went back to drops what came
        // after it, the way a browser does: two different futures from one
        // place would make the right arrow ambiguous.
        self.history.truncate(self.at + 1);
        self.round = self.round.min(self.history.len());
        self.history.push(next.clone());
        self.at = self.history.len() - 1;
        self.forget_old_rounds();
        Some(next)
    }

    /// Step back to the picture shown before this one, passing over any that
    /// cannot be landed on any more. `None` at the start of what is
    /// remembered.
    pub fn previous(&mut self, walks: impl Fn(&Path) -> bool) -> Option<PathBuf> {
        let index = (0..self.at).rev().find(|&index| walks(&self.history[index]))?;
        self.at = index;
        Some(self.history[index].clone())
    }

    /// The picture [`next`](Self::next) will move to, if it can be known
    /// without moving: the one worth decoding ahead of time.
    ///
    /// `None` at the very end of a round, where the next one is not dealt yet;
    /// the prefetch misses once per round rather than dealing early.
    pub fn upcoming(&self, walks: impl Fn(&Path) -> bool) -> Option<&Path> {
        if let Some(path) = self.history[self.at + 1..].iter().find(|path| walks(path)) {
            return Some(path);
        }
        self.ahead.iter().rev().find(|path| walks(path)).map(PathBuf::as_path)
    }

    /// The picture shown before this one, if there is one to go back to.
    pub fn before(&self, walks: impl Fn(&Path) -> bool) -> Option<&Path> {
        self.history[..self.at].iter().rev().find(|path| walks(path)).map(PathBuf::as_path)
    }

    /// Take the next undealt picture that can still be landed on.
    fn draw(&mut self, walks: &impl Fn(&Path) -> bool) -> Option<PathBuf> {
        while let Some(path) = self.ahead.pop() {
            if walks(&path) {
                return Some(path);
            }
        }
        None
    }

    /// Deal a round: everything in `walked` that this round has not shown
    /// yet, in a fresh order.
    ///
    /// A new round must not open with the picture the last one closed on —
    /// to the person watching, that is the same photograph twice in a row,
    /// and a shuffle that does it looks broken however fair it is.
    fn deal(&mut self, walked: Vec<PathBuf>) {
        let shown: HashSet<&Path> = self.history[self.round..].iter().map(PathBuf::as_path).collect();
        let mut round: Vec<PathBuf> = walked.into_iter().filter(|path| !shown.contains(path.as_path())).collect();
        fisher_yates(&mut round, &mut self.rng);
        // The next picture is the last of `ahead`.
        let showing = &self.history[self.at];
        if round.len() > 1 && round.last() == Some(showing) {
            let other = self.rng.below(round.len() - 1);
            let end = round.len() - 1;
            round.swap(other, end);
        }
        self.ahead = round;
    }

    /// Keep the history to a bounded length, never cutting into this round
    /// or behind where the left arrow has taken the show.
    fn forget_old_rounds(&mut self) {
        if self.history.len() <= HISTORY {
            return;
        }
        let cut = (self.history.len() - HISTORY).min(self.round).min(self.at);
        self.history.drain(..cut);
        self.at -= cut;
        self.round -= cut;
    }
}

/// A seed that differs from one show to the next.
///
/// The clock, the process and an address on the stack: none of it is secret
/// and none of it needs to be — a slideshow wants an order nobody has seen
/// before, not one nobody could predict.
pub fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos() as u64);
    let place = &nanos as *const u64 as u64;
    nanos ^ u64::from(std::process::id()).rotate_left(32) ^ place
}

/// SplitMix64: a few lines of arithmetic with a good spread, which is all a
/// shuffle of photographs needs. A crate for it would be a dependency for one
/// function.
#[derive(Clone, Copy, Debug)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// A number in `0..bound`. The bias of the remainder is a few parts in
    /// 2^64 for any folder that fits on a disk.
    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

/// Fisher–Yates: every order equally likely.
fn fisher_yates<T>(items: &mut [T], rng: &mut Rng) {
    for index in (1..items.len()).rev() {
        let other = rng.below(index + 1);
        items.swap(index, other);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(count: usize) -> Vec<PathBuf> {
        (0..count).map(|index| PathBuf::from(format!("picture-{index:02}.jpg"))).collect()
    }

    fn everything(_: &Path) -> bool {
        true
    }

    /// Walk a shuffle `steps` times, collecting where it went.
    fn walk(shuffle: &mut Shuffle, all: &[PathBuf], steps: usize) -> Vec<PathBuf> {
        (0..steps)
            .map(|_| shuffle.next(everything, || all.to_vec()).expect("a folder of several always has a next"))
            .collect()
    }

    /// The point of a shuffle over a random pick: every picture comes up once
    /// before any comes up twice — the picture the show started on included.
    #[test]
    fn every_picture_comes_up_once_a_round() {
        let all = folder(12);
        for seed in 0..50 {
            let mut shuffle = Shuffle::new(&all[3], all.clone(), seed);
            let mut first: Vec<PathBuf> = vec![all[3].clone()];
            first.extend(walk(&mut shuffle, &all, 11));
            first.sort();
            assert_eq!(first, all, "seed {seed}: the first round was not every picture once");

            // And the round after it, dealt afresh, is every picture once too.
            let mut second = walk(&mut shuffle, &all, 12);
            second.sort();
            assert_eq!(second, all, "seed {seed}: the second round was not every picture once");
        }
    }

    /// The seam between two rounds is where a fair shuffle can still show the
    /// same photograph twice in a row, and the room sees it.
    #[test]
    fn a_new_round_never_opens_on_the_picture_the_last_one_closed_on() {
        let all = folder(3);
        for seed in 0..200 {
            let mut shuffle = Shuffle::new(&all[0], all.clone(), seed);
            let mut previous = all[0].clone();
            for step in 0..30 {
                let next = shuffle.next(everything, || all.clone()).expect("three pictures always have a next");
                assert_ne!(next, previous, "seed {seed}, step {step}: the same picture twice in a row");
                previous = next;
            }
        }
    }

    /// Two pictures still alternate, and one has nowhere to go.
    #[test]
    fn the_smallest_folders_do_what_they_can() {
        let two = folder(2);
        let mut shuffle = Shuffle::new(&two[0], two.clone(), 7);
        let steps = walk(&mut shuffle, &two, 6);
        for pair in steps.windows(2) {
            assert_ne!(pair[0], pair[1], "two pictures did not alternate: {steps:?}");
        }

        let one = folder(1);
        let mut alone = Shuffle::new(&one[0], one.clone(), 7);
        assert_eq!(alone.next(everything, || one.clone()), None, "a folder of one moved somewhere");
    }

    /// The left arrow goes back through what was actually shown, and the right
    /// arrow then retraces it rather than dealing something new.
    #[test]
    fn going_back_retraces_what_was_shown() {
        let all = folder(10);
        let mut shuffle = Shuffle::new(&all[0], all.clone(), 11);
        let shown = walk(&mut shuffle, &all, 4);

        assert_eq!(shuffle.previous(everything).as_ref(), Some(&shown[2]));
        assert_eq!(shuffle.previous(everything).as_ref(), Some(&shown[1]));
        assert_eq!(
            shuffle.next(everything, || all.clone()).as_ref(),
            Some(&shown[2]),
            "forward again went somewhere new"
        );
        assert_eq!(shuffle.next(everything, || all.clone()).as_ref(), Some(&shown[3]));

        // Back at the start of the show there is nowhere further back to go.
        let mut fresh = Shuffle::new(&all[0], all.clone(), 11);
        assert_eq!(fresh.previous(everything), None);
    }

    /// A picture deleted or filtered out since it was dealt is passed over,
    /// forwards and backwards.
    #[test]
    fn a_picture_that_has_gone_is_passed_over() {
        let all = folder(8);
        let mut shuffle = Shuffle::new(&all[0], all.clone(), 5);
        let gone = shuffle.upcoming(everything).expect("a next picture").to_path_buf();
        let walks = |path: &Path| path != gone;

        let next = shuffle.next(walks, || all.clone()).expect("seven pictures left to show");
        assert_ne!(next, gone, "a picture that has gone was shown");

        let mut seen: Vec<PathBuf> = vec![all[0].clone(), next];
        for _ in 0..5 {
            seen.push(
                shuffle
                    .next(walks, || all.iter().filter(|path| **path != gone).cloned().collect())
                    .expect("a next picture"),
            );
        }
        assert!(!seen.contains(&gone), "the gone picture came up: {seen:?}");
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 7, "the round skipped more than the one that went: {seen:?}");
    }

    /// What the prefetch is told comes next is what comes next.
    #[test]
    fn the_upcoming_picture_is_the_next_one() {
        let all = folder(9);
        let mut shuffle = Shuffle::new(&all[4], all.clone(), 99);
        for _ in 0..30 {
            let expected = shuffle.upcoming(everything).map(Path::to_path_buf);
            let actual = shuffle.next(everything, || all.clone());
            // The end of a round is the one place the next is not known yet.
            if let Some(expected) = expected {
                assert_eq!(Some(expected), actual, "the prefetch was told about a different picture");
            }
        }
    }

    /// The same seed gives the same show, and another gives another — a check
    /// that the order comes from the generator at all.
    #[test]
    fn the_order_comes_from_the_seed() {
        let all = folder(20);
        let mut one = Shuffle::new(&all[0], all.clone(), 1);
        let mut same = Shuffle::new(&all[0], all.clone(), 1);
        let mut other = Shuffle::new(&all[0], all.clone(), 2);
        let first = walk(&mut one, &all, 19);
        assert_eq!(first, walk(&mut same, &all, 19));
        assert_ne!(first, walk(&mut other, &all, 19));
        // And it is not simply the folder's own order.
        assert_ne!(first, all[1..].to_vec());
    }

    /// A show left running for days keeps a bounded memory, and going back
    /// still works after the trim.
    #[test]
    fn a_long_show_forgets_old_rounds_but_not_this_one() {
        let all = folder(30);
        let mut shuffle = Shuffle::new(&all[0], all.clone(), 3);
        walk(&mut shuffle, &all, HISTORY * 3);
        assert!(shuffle.history.len() <= HISTORY + all.len(), "the history grew to {}", shuffle.history.len());
        assert!(shuffle.previous(everything).is_some(), "the trim took away the way back");
    }

    #[test]
    fn the_clock_counts_from_the_arrival_not_from_the_request() {
        let start = Instant::now();
        let mut clock = Clock::new(Duration::from_secs(5));
        assert_eq!(clock.due(), None, "a clock waiting for its picture named a deadline");

        // The decode took two seconds: the picture still gets its five.
        clock.arrived(start + Duration::from_secs(2), Duration::ZERO);
        assert_eq!(clock.due(), Some(start + Duration::from_secs(7)));
        assert!(!clock.is_due(start + Duration::from_secs(6)));
        assert!(clock.is_due(start + Duration::from_secs(7)));
    }

    /// The same picture uploaded again is not a new slide, and must not buy
    /// itself more time.
    #[test]
    fn a_second_arrival_for_the_same_slide_changes_nothing() {
        let start = Instant::now();
        let mut clock = Clock::new(Duration::from_secs(5));
        clock.arrived(start, Duration::ZERO);
        clock.arrived(start + Duration::from_secs(3), Duration::ZERO);
        assert_eq!(clock.due(), Some(start + Duration::from_secs(5)));

        // A step is what starts the count over.
        clock.stepped();
        assert_eq!(clock.due(), None);
        clock.arrived(start + Duration::from_secs(4), Duration::ZERO);
        assert_eq!(clock.due(), Some(start + Duration::from_secs(9)));
    }

    #[test]
    fn a_paused_show_names_no_deadline_and_resumes_with_a_whole_interval() {
        let start = Instant::now();
        let mut clock = Clock::new(Duration::from_secs(5));
        clock.arrived(start, Duration::ZERO);

        assert!(clock.toggle_paused(start + Duration::from_secs(4)));
        assert_eq!(clock.due(), None, "a paused show would still wake the loop");
        assert!(!clock.is_due(start + Duration::from_secs(60)));

        assert!(!clock.toggle_paused(start + Duration::from_secs(60)));
        assert_eq!(
            clock.due(),
            Some(start + Duration::from_secs(65)),
            "resuming did not give the picture its interval back"
        );
    }

    /// A pause that begins before the picture arrives still notes the arrival,
    /// so resuming does not leave the show waiting for a picture already up.
    #[test]
    fn a_picture_arriving_during_a_pause_is_noted() {
        let start = Instant::now();
        let mut clock = Clock::new(Duration::from_secs(5));
        clock.toggle_paused(start);
        clock.arrived(start + Duration::from_secs(1), Duration::ZERO);
        clock.toggle_paused(start + Duration::from_secs(10));
        assert_eq!(clock.due(), Some(start + Duration::from_secs(15)));
    }

    /// An animation is shown through one cycle, up to the cap.
    #[test]
    fn an_animation_holds_its_slide_for_one_cycle_up_to_the_cap() {
        let start = Instant::now();
        let mut clock = Clock::new(Duration::from_secs(5));
        clock.arrived(start, Duration::from_secs(8));
        assert_eq!(clock.due(), Some(start + Duration::from_secs(8)));

        clock.stepped();
        clock.arrived(start, Duration::from_secs(2));
        assert_eq!(clock.due(), Some(start + Duration::from_secs(5)), "a short animation cut the interval");

        clock.stepped();
        clock.arrived(start, Duration::from_secs(3600));
        assert_eq!(clock.due(), Some(start + MAX_HOLD), "a very long animation held the show past the cap");
    }

    #[test]
    fn holding_still_restarts_the_count_and_a_new_interval_applies_at_once() {
        let start = Instant::now();
        let mut clock = Clock::new(Duration::from_secs(10));
        clock.arrived(start, Duration::ZERO);
        clock.hold_still(start + Duration::from_secs(8));
        assert_eq!(clock.due(), Some(start + Duration::from_secs(18)));

        clock.set_interval(Duration::from_secs(2));
        assert_eq!(
            clock.due(),
            Some(start + Duration::from_secs(10)),
            "a shorter interval waited for the next picture"
        );
    }

    #[test]
    fn the_arrow_keys_walk_the_intervals_and_stop_at_the_ends() {
        assert_eq!(longer(5), 7);
        assert_eq!(shorter(5), 4);
        // A value typed into the settings between two steps goes to the
        // neighbouring step in the direction asked.
        assert_eq!(longer(6), 7);
        assert_eq!(shorter(6), 5);
        assert_eq!(longer(MAX_INTERVAL), MAX_INTERVAL);
        assert_eq!(shorter(MIN_INTERVAL), MIN_INTERVAL);
        // Every step is reachable from its neighbour both ways.
        for pair in INTERVALS.windows(2) {
            assert_eq!(longer(pair[0]), pair[1]);
            assert_eq!(shorter(pair[1]), pair[0]);
        }
        assert!(INTERVALS.contains(&DEFAULT_INTERVAL));
    }

    #[test]
    fn an_interval_is_written_the_way_it_is_said() {
        assert_eq!(interval_text(5), "5 s");
        assert_eq!(interval_text(59), "59 s");
        assert_eq!(interval_text(60), "1 min");
        assert_eq!(interval_text(90), "1 min 30 s");
        assert_eq!(interval_text(600), "10 min");
    }
}

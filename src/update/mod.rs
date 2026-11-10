//! Keeping the installed copy current.
//!
//! Once a day the resident asks GitHub what the newest published release is,
//! and when it is newer than this program, fetches it and has it install
//! itself. `nitid update` does the same on request. The viewer takes no part:
//! it starts without asking anybody anything, and it reaches no network.
//!
//! **What has to stay true across versions.** A copy already out there cannot
//! be corrected, so three things are a promise to every copy that has ever
//! run: what [`feed`] reads (the manifest's address and its keys), the zip
//! carrying both executables, and `APPLY_UPDATE_ARGUMENT`, which the old copy passes
//! to the new one. Everything else about installing belongs to the new version,
//! which runs its own code to do it. The release workflow installs each release
//! over the one before with the one before's own updater, so a break here is
//! found before it can strand anybody.
//!
//! What is fetched is believed in a fixed order: the digest and the size the
//! manifest gave, then the zip's own checksums, then the program itself saying
//! which version it is. Nothing is run or put in place before the digest
//! matches, and whatever goes wrong after that the copy already installed is
//! left working (`install::update`).
//!
//! See `docs/adr/0043-a-release-installs-itself-from-the-resident.md`.

mod feed;
mod http;
mod state;
mod unzip;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};

use crate::APPLY_UPDATE_ARGUMENT;
use crate::config::{Config, Updates};
use feed::{Feed, Release, Version};
use state::State;

/// How far apart GitHub is asked.
const DAY: u64 = 24 * 60 * 60;

/// How often the clock is looked at. A machine asleep through a whole day
/// asks within this of waking, and one with no connection tries again this
/// soon.
const WAKE: Duration = Duration::from_secs(60 * 60);

/// How long after sign-in the first question waits: the connection may not be
/// up yet, and sign-in has enough to do.
const SETTLE: Duration = Duration::from_secs(120);

/// The longest a manifest may be. It is four short lines.
const LONGEST_MANIFEST: u64 = 16 * 1024;

/// What `nitid update` was asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Asked {
    /// Fetch and install what is newer, if anything is.
    Install,
    /// Only say.
    Check,
}

impl Asked {
    pub fn parse(arguments: &[String]) -> Result<Self> {
        match arguments {
            [] => Ok(Self::Install),
            [only] if only == "--check" => Ok(Self::Check),
            [other, ..] => bail!("`nitid update` takes --check and nothing else, not `{other}`"),
        }
    }
}

/// `nitid update`.
pub fn command(asked: Asked) -> Result<()> {
    let current = Version::current();
    // Before anything is asked: the answer would be no however the network
    // answers, and installing over the installed copy from some other copy
    // is `nitid install`.
    if asked == Asked::Install && !crate::install::is_installed_copy() {
        bail!(
            "this copy of nitid is not the installed one, so there is nothing here to update - run {}",
            crate::install::installed_executable()?.display()
        );
    }

    let feed = Feed::new()?;
    let release = newest(&feed).context("asking GitHub for the newest release")?;
    if release.version <= current {
        crate::console::say!("nitid {current} is the latest.");
        return Ok(());
    }
    if asked == Asked::Check {
        crate::console::say!("nitid {} is out; this is {current}. `nitid update` installs it.", release.version);
        return Ok(());
    }

    crate::console::say!("Downloading nitid {}", release.version);
    let stage = fetch(&feed, &release).inspect_err(note_failure)?;
    // The console binary, whose report lands in this terminal.
    let status = std::process::Command::new(stage.join("nitid.exe"))
        .arg(APPLY_UPDATE_ARGUMENT)
        .status()
        .with_context(|| format!("starting {}", stage.display()));
    let _ = std::fs::remove_dir_all(&stage);
    if !status?.success() {
        bail!("nitid {} did not install; {current} is still in place", release.version);
    }
    crate::console::say!(
        "nitid {} is installed. A window already open keeps its version until it closes.",
        release.version
    );
    Ok(())
}

/// Be the new version, installing itself. See `APPLY_UPDATE_ARGUMENT`.
pub fn apply() -> Result<()> {
    crate::install::update()
}

/// How an install went, for the resident and the settings to read.
///
/// Called by `install::update` before it starts the resident again, so the new
/// one finds it.
pub fn note_outcome(outcome: &Result<bool>) {
    let mut state = State::load();
    match outcome {
        Ok(changed) => {
            state.failed = None;
            if *changed {
                state.news = Some(Version::built());
            }
        }
        Err(error) => state.failed = Some(format!("{error:#}")),
    }
    state.save();
}

fn note_failure(error: &anyhow::Error) {
    let mut state = State::load();
    state.failed = Some(format!("{error:#}"));
    state.save();
}

/// What the newest published release says about itself.
///
/// Stamps the state when something answers, whatever it answered: a release
/// that cannot be read is no reason to ask every hour rather than every day.
fn newest(feed: &Feed) -> Result<Release> {
    let bytes = http::get(&feed.latest(), LONGEST_MANIFEST)?;
    let mut state = State::load();
    state.checked = Some(now());
    let release = std::str::from_utf8(&bytes)
        .context("the manifest is not text")
        .and_then(Release::parse)
        .with_context(|| format!("reading {}", feed::MANIFEST));
    if let Ok(release) = &release {
        state.latest = Some(release.version);
    }
    state.save();
    release
}

/// Fetch a release and unpack it into a directory of its own, ready to run.
///
/// The directory is gone again if any of it fails.
fn fetch(feed: &Feed, release: &Release) -> Result<PathBuf> {
    tidy();
    let dir = state::dir().context("LOCALAPPDATA is not set")?;
    let stage = dir.join(format!("v{}-{}", release.version, std::process::id()));
    std::fs::create_dir_all(&stage).with_context(|| format!("creating {}", stage.display()))?;

    let unpacked = (|| {
        let archive = http::get(&feed.archive(release), release.size).with_context(|| format!("downloading {}", release.archive))?;
        believe(&archive, release)?;
        unzip::unpack(&archive, &crate::install::EXECUTABLES, &stage)?;
        // Run before anything is replaced, as the one question that shows the
        // program runs here at all and is the one that was promised. A
        // mislabelled release would otherwise be fetched again every day.
        let said = crate::install::version_said_by(&stage.join("nitid.exe"))?;
        if said != format!("nitid {}", release.version) {
            bail!("the release says it is {} and its program says `{said}`", release.version);
        }
        Ok(())
    })();
    match unpacked {
        Ok(()) => Ok(stage),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&stage);
            Err(error)
        }
    }
}

/// Whether these are the bytes the manifest promised.
fn believe(archive: &[u8], release: &Release) -> Result<()> {
    if archive.len() as u64 != release.size {
        bail!("{} is {} bytes and should be {}", release.archive, archive.len(), release.size);
    }
    if sha256(archive)? != release.sha256 {
        bail!("{} does not match the digest its release published", release.archive);
    }
    Ok(())
}

/// The digest, from the hashing Windows carries: a viewer has no other use
/// for one, and this is not the place for one written by hand.
pub fn sha256(bytes: &[u8]) -> Result<[u8; 32]> {
    let mut digest = [0u8; 32];
    // SAFETY: the pseudo-handle needs no opening, and the slices are the
    // lengths the call is told.
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, bytes, &mut digest) }
        .ok()
        .context("hashing the download")?;
    Ok(digest)
}

/// Take away what earlier attempts left: directories they were unpacked in,
/// and copies retired while something was running them.
///
/// Whatever is still in use stays for next time.
pub fn tidy() {
    if let Some(dir) = state::dir()
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    crate::install::sweep_retired();
}

/// Everything the updater keeps, for `nitid uninstall`.
pub fn remove_everything() {
    if let Some(dir) = state::dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// What came of asking, for whoever pressed the button.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Said {
    /// This is the newest there is.
    Latest(Version),
    /// A newer one is installing itself, and will say so when it has.
    Installing,
    /// GitHub could not be reached.
    NoAnswer,
    /// Something answered and it went wrong.
    Failed(String),
}

impl Said {
    /// The title and the text of the notification, or `None` when the new
    /// version will say it itself.
    pub fn words(&self) -> Option<(String, String)> {
        match self {
            Self::Latest(version) => Some(("nitid is up to date".to_string(), format!("v{version} is the latest release."))),
            Self::Installing => None,
            Self::NoAnswer => Some(("nitid could not ask".to_string(), "No answer from GitHub. It will ask again later.".to_string())),
            Self::Failed(why) => Some(("nitid did not update".to_string(), why.clone())),
        }
    }
}

/// One go: ask, and if there is something newer, set it installing.
///
/// Returns once the new version is running on its own. That process stops the
/// resident this runs in, so nothing after it here matters.
fn attempt(current: Version) -> Said {
    let feed = match Feed::new() {
        Ok(feed) => feed,
        Err(error) => return Said::Failed(format!("{error:#}")),
    };
    let release = match newest(&feed) {
        Ok(release) => release,
        Err(error) if matches!(error.downcast_ref::<http::Failure>(), Some(http::Failure::NoAnswer(_))) => return Said::NoAnswer,
        Err(error) => {
            note_failure(&error);
            return Said::Failed(format!("{error:#}"));
        }
    };
    if release.version <= current {
        // The trouble it was reporting is behind it, or has stopped mattering.
        let mut state = State::load();
        state.failed = None;
        state.save();
        return Said::Latest(current);
    }
    match fetch(&feed, &release).and_then(|stage| start_installing(&stage)) {
        Ok(()) => Said::Installing,
        Err(error) => {
            note_failure(&error);
            Said::Failed(format!("{error:#}"))
        }
    }
}

/// Start the new version's windowed executable on its way, on its own: no
/// console to flash, and nothing of this process it needs to outlive.
fn start_installing(stage: &Path) -> Result<()> {
    crate::resident::detached(&stage.join("nitidw.exe"), &[APPLY_UPDATE_ARGUMENT])
}

/// Whether the daily question is due.
///
/// A clock that has moved back past the last question is treated as due, or a
/// machine whose date was wrong once would go quiet until that date came
/// round.
fn due(updates: Updates, checked: Option<u64>, now: u64) -> bool {
    updates == Updates::Automatic && checked.is_none_or(|at| at > now || now - at >= DAY)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// Ask on a thread of its own from now on, for as long as this process lives.
///
/// Its own thread because an answer can take a minute and the resident's window
/// has a space bar to answer. Sending on what comes back asks at once, whatever
/// the clock and the setting say, and hands the answer to `tell`: that is a
/// person pressing a button.
pub fn watch(tell: impl Fn(Said) + Send + 'static) -> Sender<()> {
    let (asking, asked) = mpsc::channel();
    std::thread::spawn(move || {
        // A test standing in for GitHub is not waiting for a sign-in to settle.
        let mut wait = if Feed::stood_in_for() { Duration::ZERO } else { SETTLE };
        loop {
            let person_asked = match asked.recv_timeout(wait) {
                Ok(()) => true,
                Err(RecvTimeoutError::Timeout) => false,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            wait = WAKE;
            // The process that installed cannot take away the directory it
            // ran from, and the new resident may look before it has gone.
            tidy();
            // Read each time, so the switch in the settings needs no message
            // to reach here and no restart to take effect.
            if person_asked {
                tell(attempt(Version::current()));
            } else if due(Config::load().updates, State::load().checked, now()) {
                let _ = attempt(Version::current());
            }
        }
    });
    asking
}

/// A version installed that nobody has been told about, once.
pub fn take_news() -> Option<String> {
    let mut state = State::load();
    let news = state.news.take()?;
    state.save();
    Some(news.to_string())
}

/// Where to read about a release, for the notification that says it arrived.
pub fn notes(version: &str) -> Option<String> {
    Version::parse(version).map(Feed::notes)
}

/// What the settings say about the state of things.
pub fn status() -> String {
    describe(&State::load(), Version::current(), now())
}

fn describe(state: &State, current: Version, now: u64) -> String {
    if let Some(failed) = &state.failed {
        return format!("This is v{current}. The last update did not go through: {failed}");
    }
    if let Some(latest) = state.latest.filter(|latest| *latest > current) {
        return format!("This is v{current}; v{latest} is out.");
    }
    match state.checked {
        Some(checked) => format!("This is v{current}, the latest {}.", ago(now.saturating_sub(checked))),
        None => format!("This is v{current}. nitid has not asked yet."),
    }
}

/// How long ago, as a person says it.
fn ago(seconds: u64) -> String {
    let (count, unit) = match seconds {
        0..60 => return "a moment ago".to_string(),
        60..3600 => (seconds / 60, "minute"),
        3600..86_400 => (seconds / 3600, "hour"),
        _ => (seconds / 86_400, "day"),
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).expect("a version")
    }

    fn release_of(bytes: &[u8]) -> Release {
        Release {
            version: version("0.43.0"),
            archive: "a.zip".to_string(),
            sha256: sha256(bytes).expect("a digest"),
            size: bytes.len() as u64,
        }
    }

    /// The published value for "abc", from FIPS 180: the digest in use is the
    /// standard's rather than something that merely agrees with itself.
    #[test]
    fn the_digest_is_sha_256() {
        let digest = sha256(b"abc").expect("a digest");
        let text: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(text, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn only_the_promised_bytes_are_believed() {
        let release = release_of(b"the release");
        assert!(believe(b"the release", &release).is_ok());
        let tampered = believe(b"the relea5e", &release).expect_err("different bytes of the same length were believed");
        assert!(format!("{tampered:#}").contains("digest"), "{tampered:#}");
        assert!(believe(b"the release and more", &release).is_err(), "more bytes than promised were believed");
        assert!(believe(b"", &release).is_err(), "nothing was believed");
    }

    #[test]
    fn the_question_is_due_once_a_day() {
        let at = 1_790_000_000;
        assert!(due(Updates::Automatic, None, at), "a copy that has never asked did not ask");
        assert!(!due(Updates::Automatic, Some(at), at));
        assert!(!due(Updates::Automatic, Some(at - DAY + 1), at), "it asked within the day");
        assert!(due(Updates::Automatic, Some(at - DAY), at), "it did not ask after a day");
    }

    /// Switched off means never, however long it has been, which is the point
    /// of having the switch.
    #[test]
    fn switched_off_it_is_never_due() {
        let at = 1_790_000_000;
        assert!(!due(Updates::Off, None, at));
        assert!(!due(Updates::Off, Some(0), at));
    }

    /// A date set wrong once must not silence a machine until that date.
    #[test]
    fn a_clock_moved_back_does_not_stop_the_question() {
        let at = 1_790_000_000;
        assert!(due(Updates::Automatic, Some(at + 1_000_000), at));
    }

    #[test]
    fn the_command_takes_check_and_nothing_else() {
        assert_eq!(Asked::parse(&[]).expect("no arguments"), Asked::Install);
        assert_eq!(Asked::parse(&["--check".to_string()]).expect("--check"), Asked::Check);
        assert!(Asked::parse(&["--force".to_string()]).is_err());
        assert!(Asked::parse(&["--check".to_string(), "--check".to_string()]).is_err());
    }

    /// Every unit's edges: a step that leaves a gap or says "60 minutes" is
    /// wrong for exactly one minute a day, which is why it survives.
    #[test]
    fn how_long_ago_reads_at_every_edge() {
        for (seconds, said) in [
            (0, "a moment ago"),
            (59, "a moment ago"),
            (60, "1 minute ago"),
            (119, "1 minute ago"),
            (120, "2 minutes ago"),
            (3599, "59 minutes ago"),
            (3600, "1 hour ago"),
            (86_399, "23 hours ago"),
            (86_400, "1 day ago"),
            (172_800, "2 days ago"),
        ] {
            assert_eq!(ago(seconds), said, "{seconds} seconds");
        }
    }

    #[test]
    fn the_settings_say_how_things_stand() {
        let current = version("0.42.0");
        let now = 1_790_000_000;
        assert_eq!(describe(&State::default(), current, now), "This is v0.42.0. nitid has not asked yet.");

        let asked = State {
            checked: Some(now - 7200),
            latest: Some(current),
            ..State::default()
        };
        assert_eq!(describe(&asked, current, now), "This is v0.42.0, the latest 2 hours ago.");

        let behind = State {
            latest: Some(version("0.43.0")),
            ..asked.clone()
        };
        assert_eq!(describe(&behind, current, now), "This is v0.42.0; v0.43.0 is out.");

        // A release older than this copy, withdrawn or not yet caught up, is
        // not news.
        let ahead = State {
            latest: Some(version("0.41.0")),
            ..asked.clone()
        };
        assert_eq!(describe(&ahead, current, now), "This is v0.42.0, the latest 2 hours ago.");

        // The reason outranks everything: it is what someone opened this for.
        let failed = State {
            failed: Some("the disk is full".to_string()),
            ..behind
        };
        assert_eq!(
            describe(&failed, current, now),
            "This is v0.42.0. The last update did not go through: the disk is full"
        );
    }

    /// A person who pressed a button is told something every time, except
    /// when the new version is about to say it itself.
    #[test]
    fn every_answer_but_the_one_on_its_way_is_said() {
        assert!(Said::Latest(version("0.42.0")).words().is_some_and(|(_, text)| text.contains("v0.42.0")));
        assert!(Said::NoAnswer.words().is_some());
        assert!(Said::Failed("why".to_string()).words().is_some_and(|(_, text)| text == "why"));
        assert_eq!(Said::Installing.words(), None);
    }
}

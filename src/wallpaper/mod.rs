//! The desktop's picture, from nitid: one picture now (`W`), or a folder on a
//! schedule (`Shift+W`, `nitid wallpaper`).
//!
//! The schedule is a task in the Windows Task Scheduler that runs
//! `nitidw.exe wallpaper next` (ADR 0035). Nothing of nitid stays running
//! between changes, and nothing has to be installed beside it: Windows already
//! knows how to start a program every half hour, after sleep, on battery.
//!
//! Each change deals from the folder the way a shuffled slideshow does —
//! every picture once a round, the seam between rounds without a repeat —
//! except that a picture which arrived in the folder since the last change
//! goes first, newest first. That is what makes the folder the source rather
//! than a snapshot of it: drop a picture in, and it is the next one up.
//!
//! The desktop is given a picture made for it rather than the file itself.
//! Windows reads only a few formats, does not manage the desktop's colour and
//! ignores a photograph's orientation tag; so the picture is converted to sRGB
//! (as an export with the colour baked in is), turned upright, made no larger
//! than the largest display needs, and written as a JPEG.
//!
//! Where things live: the settings in `%APPDATA%\lacodda\nitid\wallpaper.conf`,
//! written the moment they change — not in `settings.conf`, which an open
//! viewer writes whole when it closes and would undo a change made meanwhile
//! from the command line. What has been on the desktop, and the pictures made
//! for it, in `%LOCALAPPDATA%\lacodda\nitid\wallpaper`.

mod desktop;
mod schedule;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};

use crate::fit::Fit;
use crate::pattern::Pattern;
use crate::slideshow::Rng;

/// How often the desktop's picture changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Every {
    minutes: u32,
}

impl Every {
    /// What the settings offer, shortest first.
    pub const LADDER: [u32; 10] = [5, 10, 15, 30, 60, 120, 180, 360, 720, 1440];

    /// Half an hour: what Windows' own slideshow starts at, and often enough
    /// that the desktop is noticed changing without being watched doing it.
    pub const DEFAULT: Self = Self { minutes: 30 };

    /// The Task Scheduler repeats no more often than once a minute and no
    /// less often than once in 31 days.
    const MIN: u32 = 1;
    const MAX: u32 = 31 * 24 * 60;

    pub fn minutes(minutes: u32) -> Self {
        Self {
            minutes: minutes.clamp(Self::MIN, Self::MAX),
        }
    }

    pub fn in_minutes(self) -> u32 {
        self.minutes
    }

    /// An interval as a person types it: `30m`, `2h`, `1d`, `1h30m`, or a bare
    /// number of minutes. `None` for anything else, or for zero.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_ascii_lowercase();
        if let Ok(minutes) = text.parse::<u32>() {
            return (minutes > 0).then(|| Self::minutes(minutes));
        }

        let mut total: u64 = 0;
        let mut number = String::new();
        for character in text.chars() {
            match character {
                '0'..='9' => number.push(character),
                'd' | 'h' | 'm' => {
                    let value: u64 = number.parse().ok()?;
                    number.clear();
                    total += value
                        * match character {
                            'd' => 24 * 60,
                            'h' => 60,
                            _ => 1,
                        };
                }
                ' ' => {}
                _ => return None,
            }
        }
        if !number.is_empty() || total == 0 {
            return None;
        }
        Some(Self::minutes(u32::try_from(total).unwrap_or(u32::MAX)))
    }

    /// Short, for the settings file and the command line: `30m`, `2h`, `1d`,
    /// `1h30m` — what `parse` reads back.
    pub fn key(self) -> String {
        let (days, hours, minutes) = self.split();
        let mut out = String::new();
        if days > 0 {
            out.push_str(&format!("{days}d"));
        }
        if hours > 0 {
            out.push_str(&format!("{hours}h"));
        }
        if minutes > 0 {
            out.push_str(&format!("{minutes}m"));
        }
        out
    }

    /// In words, for the interface: `30 min`, `1 h`, `1 h 30 min`, `1 day`.
    pub fn text(self) -> String {
        let (days, hours, minutes) = self.split();
        let mut parts = Vec::new();
        match days {
            0 => {}
            1 => parts.push("1 day".to_string()),
            many => parts.push(format!("{many} days")),
        }
        if hours > 0 {
            parts.push(format!("{hours} h"));
        }
        if minutes > 0 {
            parts.push(format!("{minutes} min"));
        }
        parts.join(" ")
    }

    /// As the Task Scheduler writes a duration: `PT30M`, `PT2H`, `P1D`,
    /// `P1DT2H30M`.
    fn duration(self) -> String {
        let (days, hours, minutes) = self.split();
        let mut out = String::from("P");
        if days > 0 {
            out.push_str(&format!("{days}D"));
        }
        if hours > 0 || minutes > 0 {
            out.push('T');
            if hours > 0 {
                out.push_str(&format!("{hours}H"));
            }
            if minutes > 0 {
                out.push_str(&format!("{minutes}M"));
            }
        }
        out
    }

    fn split(self) -> (u32, u32, u32) {
        (self.minutes / (24 * 60), self.minutes / 60 % 24, self.minutes % 60)
    }
}

impl Default for Every {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The desktop's words for how the picture meets a display.
impl Fit {
    fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "fill" => Some(Self::Fill),
            "fit" => Some(Self::Fit),
            _ => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Fit => "fit",
        }
    }

    /// In words, for a sentence about the desktop.
    pub fn text(self) -> &'static str {
        match self {
            Self::Fill => "filling the screen",
            Self::Fit => "whole on the screen",
        }
    }
}

/// Where the desktop's picture comes from, and how.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Settings {
    /// The folder a schedule deals from. `None` until one has been chosen.
    pub folder: Option<PathBuf>,
    /// Only the names this admits, when set.
    pub only: Option<Pattern>,
    pub every: Every,
    pub fit: Fit,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            folder: None,
            only: None,
            every: Every::default(),
            // Filling: a desktop with bars at its edges looks broken rather
            // than careful.
            fit: Fit::Fill,
        }
    }
}

impl Settings {
    /// The settings as the file says, or the defaults for anything it does
    /// not — the same silent fallback the viewer's own settings use.
    pub fn load() -> Self {
        settings_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// Write the settings, now. A failure is said: the schedule would
    /// otherwise go on with settings the person was told had changed.
    pub fn save(&self) -> Result<()> {
        let path = settings_path().context("there is no APPDATA folder to keep the settings in")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&path, self.render()).with_context(|| format!("writing {}", path.display()))
    }

    fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                // A path holds spaces; it is taken as it stands.
                "folder" if !value.is_empty() => settings.folder = Some(PathBuf::from(value)),
                "only" => settings.only = Pattern::parse(value),
                "every" => settings.every = Every::parse(value).unwrap_or_default(),
                "fit" => settings.fit = Fit::parse(value).unwrap_or(Fit::Fill),
                _ => {}
            }
        }
        settings
    }

    fn render(&self) -> String {
        let mut out = String::from("# nitid: where the desktop's picture comes from (nitid wallpaper, or Shift+W in the viewer)\n");
        if let Some(folder) = &self.folder {
            out.push_str(&format!("folder = {}\n", folder.display()));
        }
        if let Some(only) = &self.only {
            out.push_str(&format!("only = {}\n", only.text()));
        }
        out.push_str(&format!("every = {}\n", self.every.key()));
        out.push_str(&format!("fit = {}\n", self.fit.key()));
        out
    }

    /// The folder in a sentence: `Walls`, or `Walls (*.png)` with a pattern.
    fn source(&self) -> String {
        let folder = self.folder.as_deref().map(folder_name).unwrap_or_default();
        match &self.only {
            Some(only) => format!("{folder} ({})", only.text()),
            None => folder,
        }
    }
}

/// What a press of `W` or `Shift+W`, or the settings dialog, asks for. Done on
/// a worker thread by [`carry_out`]: preparing a sixty-megapixel photograph and
/// talking to the Task Scheduler both take long enough to stall a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// This picture on the desktop, now, and the schedule off if it was on —
    /// a person who chose one picture wants it to stay.
    Picture(PathBuf),
    /// This folder on a schedule, starting with this picture; or, when the
    /// schedule is already dealing from this very folder and pattern, stop it.
    Folder { first: PathBuf, only: Option<Pattern> },
    /// The settings changed in the dialog: write them, and let a running
    /// schedule take the new interval and the desktop the new fit.
    Settings(Settings),
    /// Stop changing the picture.
    Stop,
}

/// Do what was asked, and say what happened in a sentence for a toast.
pub fn carry_out(request: Request) -> Result<String> {
    match request {
        Request::Picture(path) => {
            let settings = Settings::load();
            let mut deck = Deck::load();
            put_up(&path, &settings, &mut deck)?;
            deck.save()?;
            let stopped = schedule::remove()?;
            let name = file_name(&path);
            Ok(match stopped {
                true => format!("{name} is on the desktop, and stays: the folder's schedule is off"),
                false => format!("{name} is on the desktop"),
            })
        }
        Request::Folder { first, only } => {
            let folder = first.parent().map(Path::to_path_buf).context("this picture is in no folder")?;
            let mut settings = Settings::load();
            let same = settings.folder.as_deref() == Some(folder.as_path()) && settings.only == only;
            if same && schedule::registered()? {
                schedule::remove()?;
                return Ok("the desktop keeps the picture it has: the folder's schedule is off".to_string());
            }

            settings.folder = Some(folder);
            settings.only = only;
            settings.save()?;
            // From the deck there is, not from nothing: a fresh start keeps
            // which of the two made pictures is on the desktop, or the first
            // one up would be written over the one being shown.
            let mut deck = Deck::load();
            deck.start(&settings, &listing(&settings)?, Some(&file_name(&first)));
            put_up(&first, &settings, &mut deck)?;
            schedule::register(settings.every, &scheduled_program()?)?;
            deck.registered = Some(settings.every);
            deck.save()?;
            Ok(format!("the desktop changes every {} from {}", settings.every.text(), settings.source()))
        }
        Request::Settings(settings) => {
            let before = Settings::load();
            settings.save()?;
            let mut said = Vec::new();
            if settings.fit != before.fit {
                desktop::set_fit(settings.fit)?;
                said.push(format!("the desktop's picture is {} now", settings.fit.text()));
            }
            if settings.every != before.every && schedule::registered()? {
                schedule::register(settings.every, &scheduled_program()?)?;
                let mut deck = Deck::load();
                deck.registered = Some(settings.every);
                deck.save()?;
                said.push(format!("the desktop changes every {} now", settings.every.text()));
            }
            Ok(said.join("; "))
        }
        Request::Stop => Ok(match schedule::remove()? {
            true => "the desktop keeps the picture it has: the folder's schedule is off".to_string(),
            false => "the desktop's picture was not changing on its own".to_string(),
        }),
    }
}

/// Whether a schedule is running, and from where — for the settings dialog.
/// `None` when nothing changes the desktop on its own.
pub fn scheduled() -> Option<String> {
    let settings = Settings::load();
    match schedule::registered() {
        Ok(true) => Some(format!(
            "Changing every {} from {}, {}.",
            settings.every.text(),
            settings.source(),
            settings.fit.text()
        )),
        _ => None,
    }
}

/// What the command line asked of `nitid wallpaper`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    /// Say what the desktop is doing.
    Status,
    /// Change to the next picture now. What the scheduled task runs.
    Next,
    /// Stop changing it.
    Off,
    /// Change it from a folder on a schedule, or change how.
    Set {
        folder: Option<PathBuf>,
        every: Option<Every>,
        /// `Some("")` takes the pattern off.
        only: Option<String>,
        fit: Option<Fit>,
    },
}

impl Asked {
    /// Read what followed `nitid wallpaper`.
    pub fn parse(args: &[String]) -> Result<Self> {
        match args {
            [] => return Ok(Self::Status),
            [only] if only == "next" => return Ok(Self::Next),
            [only] if only == "off" => return Ok(Self::Off),
            _ => {}
        }

        let (mut folder, mut every, mut only, mut fit) = (None, None, None, None);
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let mut value = |flag: &str| args.next().cloned().with_context(|| format!("{flag} needs a value"));
            match arg.as_str() {
                "--every" => {
                    let text = value("--every")?;
                    every = Some(Every::parse(&text).with_context(|| format!("{text} is not an interval: try 30m, 2h or 1d"))?);
                }
                "--only" => only = Some(value("--only")?),
                "--fit" => {
                    let text = value("--fit")?;
                    fit = Some(Fit::parse(&text).with_context(|| format!("{text} is not a fit: it is fill or fit"))?);
                }
                flag if flag.starts_with("--") => bail!("{flag} is not something nitid wallpaper takes"),
                path if folder.is_none() => folder = Some(PathBuf::from(path)),
                extra => bail!("one folder at a time: {extra} is one too many"),
            }
        }
        Ok(Self::Set { folder, every, only, fit })
    }
}

/// Carry out `nitid wallpaper …`, printing what was done.
///
/// `Next` prints nothing on success: it is what the scheduled task runs, from
/// the windowed binary, where there is nowhere to print. A failure is kept in
/// the deck for `nitid wallpaper` to report, as well as returned.
pub fn command(asked: Asked) -> Result<()> {
    match asked {
        Asked::Status => {
            println!("{}", status());
            Ok(())
        }
        Asked::Next => next(),
        Asked::Off => {
            println!("{}", carry_out(Request::Stop)?);
            Ok(())
        }
        Asked::Set { folder, every, only, fit } => {
            let before = Settings::load();
            let mut settings = before.clone();
            let named_a_folder = folder.is_some();
            if let Some(folder) = folder {
                let folder = std::path::absolute(&folder).with_context(|| format!("resolving {}", folder.display()))?;
                if !folder.is_dir() {
                    bail!("{} is not a folder", folder.display());
                }
                settings.folder = Some(folder);
            }
            if let Some(every) = every {
                settings.every = every;
            }
            if let Some(only) = only {
                settings.only = Pattern::parse(&only);
            }
            if let Some(fit) = fit {
                settings.fit = fit;
            }
            if settings.folder.is_none() {
                bail!("name the folder the pictures come from: nitid wallpaper FOLDER");
            }

            // Only how a running schedule works was asked about — how often,
            // how the picture fits: that changes, and the picture on the
            // desktop does not. Naming the folder, or another pattern, deals
            // afresh below.
            let same_source = settings.folder == before.folder && settings.only == before.only;
            if !named_a_folder && same_source && schedule::registered()? {
                carry_out(Request::Settings(settings.clone()))?;
                println!(
                    "The desktop changes every {} from {}, {}.",
                    settings.every.text(),
                    settings.source(),
                    settings.fit.text()
                );
                return Ok(());
            }

            // Everything that can be wrong with the folder is found out before
            // anything is registered: a schedule that fails every half hour
            // is a worse answer than a refusal now.
            let pictures = listing(&settings)?;
            settings.save()?;
            let mut deck = Deck::load().for_settings(&settings);
            let picked = deck
                .deal(&pictures, &mut Rng::new(crate::slideshow::seed()))
                .context("there is no picture to deal")?;
            let path = settings.folder.as_deref().map(|folder| folder.join(&picked)).context("no folder")?;
            put_up(&path, &settings, &mut deck)?;
            schedule::register(settings.every, &scheduled_program()?)?;
            deck.registered = Some(settings.every);
            deck.save()?;
            println!(
                "The desktop changes every {} from {}, {}. On it now: {picked}",
                settings.every.text(),
                settings.source(),
                settings.fit.text()
            );
            Ok(())
        }
    }
}

/// The next picture, put up now.
fn next() -> Result<()> {
    let settings = Settings::load();
    let mut deck = Deck::load();
    let outcome = (|| -> Result<()> {
        let folder = settings.folder.clone().context("no folder is set: nitid wallpaper FOLDER sets one")?;
        deck = std::mem::take(&mut deck).for_settings(&settings);
        let pictures = listing(&settings)?;
        let picked = deck
            .deal(&pictures, &mut Rng::new(crate::slideshow::seed()))
            .context("there is no picture to deal")?;
        put_up(&folder.join(picked), &settings, &mut deck)?;
        // The interval was changed by hand in the settings file since the task
        // was made: the task takes it, so the file stays the one truth.
        if deck.registered != Some(settings.every) && schedule::registered()? {
            schedule::register(settings.every, &scheduled_program()?)?;
            deck.registered = Some(settings.every);
        }
        Ok(())
    })();
    deck.error = outcome.as_ref().err().map(|error| format!("{error:#}"));
    deck.save()?;
    outcome
}

/// What `nitid wallpaper` says.
fn status() -> String {
    let settings = Settings::load();
    let deck = Deck::load();
    let mut lines = Vec::new();
    let running = schedule::registered();
    match &running {
        Ok(true) => {
            let folder = settings.folder.as_deref().map(|folder| folder.display().to_string()).unwrap_or_default();
            let only = settings
                .only
                .as_ref()
                .map(|only| format!(", names matching {}", only.text()))
                .unwrap_or_default();
            lines.push(format!(
                "The desktop changes every {} from {folder}{only}, {}.",
                settings.every.text(),
                settings.fit.text()
            ));
            if let Ok(Some(next)) = schedule::next_run() {
                lines.push(format!("Next change: {next}"));
            }
        }
        Ok(false) => lines.push("The desktop's picture does not change on its own. nitid wallpaper FOLDER starts changing it.".to_string()),
        Err(error) => lines.push(format!("The schedule could not be read: {error:#}")),
    }
    if let Some(last) = &deck.last {
        lines.push(format!("Last put up: {last}"));
    }
    // About a schedule that is running: once it is off, an old failure is
    // news about nothing.
    if let (Some(error), Ok(true)) = (&deck.error, &running) {
        lines.push(format!("The last change failed: {error}"));
    }
    lines.join("\n")
}

/// The pictures a schedule deals from: the folder's, by the viewer's listing
/// rules, narrowed to the pattern.
fn listing(settings: &Settings) -> Result<Vec<Candidate>> {
    let folder = settings.folder.as_deref().context("no folder is set")?;
    let pictures: Vec<Candidate> = crate::folder::pictures(folder)?
        .into_iter()
        .filter(|(path, _)| settings.only.as_ref().is_none_or(|only| only.admits(path)))
        .map(|(path, modified)| Candidate {
            name: file_name(&path),
            modified,
        })
        .collect();
    if pictures.is_empty() {
        match &settings.only {
            Some(only) => bail!("nothing in {} is named {}", folder.display(), only.text()),
            None => bail!("there is no picture in {}", folder.display()),
        }
    }
    Ok(pictures)
}

/// Make the desktop's picture from `source` and put it up.
fn put_up(source: &Path, settings: &Settings, deck: &mut Deck) -> Result<()> {
    let dir = data_dir().context("there is no LOCALAPPDATA folder to keep the desktop's picture in")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    // Two files, taken in turn: the one on the desktop now is never the one
    // being written, so a change that fails half-way leaves the desktop as it
    // was rather than pointing at a broken file.
    let slot = 1 - deck.slot.min(1);
    let made = dir.join(format!("picture-{slot}.jpg"));

    let screens = desktop::screens();
    let bytes = prepare(source, &screens, settings.fit)?;
    std::fs::write(&made, bytes).with_context(|| format!("writing {}", made.display()))?;
    desktop::show(&made, settings.fit)?;

    deck.slot = slot;
    deck.last = Some(file_name(source));
    deck.error = None;
    Ok(())
}

/// The desktop's picture, as JPEG bytes: the picture as an sRGB-only
/// destination receives it (`export::upright_srgb`), as large as the largest
/// display needs. A vector picture is drawn at the size it is needed at.
pub(crate) fn prepare(source: &Path, screens: &[(u32, u32)], fit: Fit) -> Result<Vec<u8>> {
    let bytes = std::fs::read(source).with_context(|| format!("reading {}", source.display()))?;
    let loaded = crate::image_source::decode(&bytes).with_context(|| format!("opening {}", source.display()))?;

    let shown = loaded.display_size();
    let vector = loaded.vector.is_some();
    let wanted = needed_size(shown, screens, fit, vector);

    let (image, orientation) = match &loaded.vector {
        Some(vector) => (vector.rasterise(wanted.0, wanted.1)?, crate::image_source::Orientation::Normal),
        None => (loaded.image.clone(), loaded.orientation),
    };
    let upright = crate::export::upright_srgb(&image, loaded.profile.as_ref(), orientation, wanted.0.max(wanted.1))?;

    crate::export::encode(&crate::export::Request {
        image: &upright,
        // Baked already: the numbers are sRGB, and saying nothing about them
        // is how a JPEG says sRGB.
        profile: None,
        target: crate::export::Target::Jpeg,
        colour: crate::export::Colour::KeepProfile,
        quality: 95,
        caveat: None,
    })
}

/// How big the desktop's picture needs to be, shown the right way up.
///
/// Big enough that no display has to enlarge it: filling, its shorter side
/// against the display's; fitting, its longer. The largest answer over every
/// display wins, because one picture goes on all of them. A photograph is
/// never made bigger than it is — that would be inventing detail — but a
/// vector is drawn at whatever size is needed, which is what it is for. With
/// no display known the picture keeps its size.
fn needed_size(shown: (u32, u32), screens: &[(u32, u32)], fit: Fit, vector: bool) -> (u32, u32) {
    let (width, height) = (f64::from(shown.0.max(1)), f64::from(shown.1.max(1)));
    let scale = screens
        .iter()
        .map(|&(screen_width, screen_height)| fit.scale((width, height), (f64::from(screen_width), f64::from(screen_height))))
        .fold(None, |most: Option<f64>, scale| Some(most.map_or(scale, |most| most.max(scale))));
    let Some(scale) = scale else {
        return shown;
    };
    let scale = if vector { scale } else { scale.min(1.0) };
    (((width * scale).round() as u32).max(1), ((height * scale).round() as u32).max(1))
}

/// A picture a schedule can deal: its name in the folder, and when it was last
/// written.
#[derive(Clone, Debug)]
struct Candidate {
    name: String,
    modified: SystemTime,
}

/// What has been on the desktop: enough to deal the next picture without a
/// repeat, and to put a picture that has just arrived first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Deck {
    /// The folder and pattern it was dealt from. Settings that name another
    /// start a fresh deck — what was shown from one folder says nothing about
    /// another.
    folder: Option<PathBuf>,
    only: Option<String>,
    /// Every name the folder held at the last deal. A name not among them
    /// next time arrived in between.
    known: BTreeSet<String>,
    /// What has been up this round.
    shown: BTreeSet<String>,
    /// Arrivals not yet put up, newest first.
    fresh: Vec<String>,
    /// What went up last: never the first of the next round.
    last: Option<String>,
    /// Which of the two made pictures the desktop shows.
    slot: u8,
    /// The interval the scheduled task was made with, so a change to the
    /// settings by hand reaches the task on its next run.
    registered: Option<Every>,
    /// Why the last change failed, for `nitid wallpaper` to say.
    error: Option<String>,
}

impl Deck {
    fn deals_from(&self, settings: &Settings) -> bool {
        self.folder == settings.folder && self.only == settings.only.as_ref().map(|only| only.text().to_string())
    }

    /// This deck if it was dealt from these settings, or a fresh one that
    /// says it is — so the next run knows it and does not start over again.
    /// What the desktop shows and how the task was made carry over either way:
    /// they are about the machine, not the folder.
    fn for_settings(self, settings: &Settings) -> Self {
        if self.deals_from(settings) {
            return self;
        }
        Self {
            folder: settings.folder.clone(),
            only: settings.only.as_ref().map(|only| only.text().to_string()),
            slot: self.slot,
            registered: self.registered,
            ..Self::default()
        }
    }

    /// A fresh deck for these settings, with `first` already put up.
    fn start(&mut self, settings: &Settings, pictures: &[Candidate], first: Option<&str>) {
        *self = Self {
            folder: settings.folder.clone(),
            only: settings.only.as_ref().map(|only| only.text().to_string()),
            known: pictures.iter().map(|picture| picture.name.clone()).collect(),
            slot: self.slot,
            registered: self.registered,
            ..Self::default()
        };
        if let Some(first) = first {
            self.shown.insert(first.to_string());
            self.last = Some(first.to_string());
        }
    }

    /// The next picture's name.
    ///
    /// The very first deal is the newest picture — a start has to start
    /// somewhere, and the latest is what a person who just pointed the desktop
    /// at a folder most likely wants to see. After that: an arrival if there
    /// is one, newest first; otherwise one not yet up this round, at random;
    /// and when the round is done, a new one that does not open with the
    /// picture that closed the last.
    fn deal(&mut self, pictures: &[Candidate], rng: &mut Rng) -> Option<String> {
        if pictures.is_empty() {
            return None;
        }
        let present: BTreeSet<String> = pictures.iter().map(|picture| picture.name.clone()).collect();
        let modified = |name: &str| pictures.iter().find(|picture| picture.name == name).map(|picture| picture.modified);

        let picked = if self.known.is_empty() && self.last.is_none() {
            pictures.iter().max_by_key(|picture| picture.modified).map(|picture| picture.name.clone())?
        } else {
            for arrival in present.difference(&self.known) {
                if !self.fresh.contains(arrival) {
                    self.fresh.push(arrival.clone());
                }
            }
            self.fresh.retain(|name| present.contains(name));
            self.fresh.sort_by_key(|name| std::cmp::Reverse(modified(name)));

            match self.fresh.first() {
                Some(newest) => newest.clone(),
                None => {
                    let mut left: Vec<&String> = present.difference(&self.shown).collect();
                    if left.is_empty() {
                        self.shown.clear();
                        left = present.iter().filter(|name| present.len() < 2 || Some(*name) != self.last.as_ref()).collect();
                    }
                    left[rng.below(left.len())].clone()
                }
            }
        };

        self.fresh.retain(|name| *name != picked);
        self.shown.insert(picked.clone());
        self.known = present;
        self.last = Some(picked.clone());
        Some(picked)
    }

    fn load() -> Self {
        deck_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    fn save(&self) -> Result<()> {
        let path = deck_path().context("there is no LOCALAPPDATA folder to keep the deck in")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&path, self.render()).with_context(|| format!("writing {}", path.display()))
    }

    /// Plain lines, like the settings: a few `key = value` lines, then the
    /// names under the list they belong to. A name is one line because a
    /// file name cannot hold a line break.
    fn parse(text: &str) -> Self {
        let mut deck = Self::default();
        let mut list: Option<&str> = None;
        for line in text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
                list = Some(match name {
                    "known" => "known",
                    "shown" => "shown",
                    "fresh" => "fresh",
                    _ => "",
                });
                continue;
            }
            match list {
                Some("known") => {
                    deck.known.insert(line.to_string());
                }
                Some("shown") => {
                    deck.shown.insert(line.to_string());
                }
                Some("fresh") => deck.fresh.push(line.to_string()),
                Some(_) => {}
                None => {
                    let Some((key, value)) = line.split_once('=') else {
                        continue;
                    };
                    let value = value.trim();
                    match key.trim() {
                        "folder" if !value.is_empty() => deck.folder = Some(PathBuf::from(value)),
                        "only" if !value.is_empty() => deck.only = Some(value.to_string()),
                        "last" if !value.is_empty() => deck.last = Some(value.to_string()),
                        "slot" => deck.slot = value.parse().unwrap_or(0),
                        "registered" => deck.registered = Every::parse(value),
                        "error" if !value.is_empty() => deck.error = Some(value.to_string()),
                        _ => {}
                    }
                }
            }
        }
        deck
    }

    fn render(&self) -> String {
        let mut out = String::from("# nitid: what has been on the desktop. Written by nitid; the settings are in wallpaper.conf.\n");
        let mut line = |key: &str, value: Option<String>| {
            if let Some(value) = value {
                out.push_str(&format!("{key} = {value}\n"));
            }
        };
        line("folder", self.folder.as_ref().map(|folder| folder.display().to_string()));
        line("only", self.only.clone());
        line("last", self.last.clone());
        line("slot", Some(self.slot.to_string()));
        line("registered", self.registered.map(Every::key));
        // One line: an error is a sentence, and a line break in it would read
        // back as the start of a list.
        line("error", self.error.as_ref().map(|error| error.replace(['\r', '\n'], " ")));
        for (name, list) in [
            ("known", self.known.iter().collect::<Vec<_>>()),
            ("shown", self.shown.iter().collect()),
            ("fresh", self.fresh.iter().collect()),
        ] {
            out.push_str(&format!("[{name}]\n"));
            for entry in list {
                out.push_str(entry);
                out.push('\n');
            }
        }
        out
    }
}

/// The program the scheduled task runs: the windowed binary beside this one,
/// which opens no console window when the task starts it.
fn scheduled_program() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating the running executable")?;
    let program = exe.with_file_name(if cfg!(windows) { "nitidw.exe" } else { "nitidw" });
    if !program.exists() {
        bail!("{} is missing; the schedule runs it", program.display());
    }
    Ok(program)
}

/// `%APPDATA%\lacodda\nitid\wallpaper.conf`, beside the viewer's settings.
fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("lacodda").join("nitid").join("wallpaper.conf"))
}

/// `%LOCALAPPDATA%\lacodda\nitid\wallpaper`: the deck and the pictures made
/// for the desktop. Local, not roaming: both are about this machine's folders
/// and this machine's displays.
fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("lacodda").join("nitid").join("wallpaper"))
}

fn deck_path() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join("deck"))
}

/// Take the schedule away, for `nitid uninstall`: a task left behind would run
/// a program that is not there any more, every half hour, for ever.
#[cfg(windows)]
pub fn forget_schedule() -> Result<bool> {
    schedule::remove()
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn an_interval_reads_the_way_a_person_types_it() {
        assert_eq!(Every::parse("30m").map(Every::in_minutes), Some(30));
        assert_eq!(Every::parse("2h").map(Every::in_minutes), Some(120));
        assert_eq!(Every::parse("1d").map(Every::in_minutes), Some(1440));
        assert_eq!(Every::parse("1h30m").map(Every::in_minutes), Some(90));
        assert_eq!(Every::parse("1H 30M").map(Every::in_minutes), Some(90));
        assert_eq!(Every::parse("45").map(Every::in_minutes), Some(45), "a bare number is minutes");
        for nonsense in ["", "0", "0m", "half an hour", "30x", "h", "30m5"] {
            assert_eq!(Every::parse(nonsense), None, "{nonsense:?} read as an interval");
        }
    }

    /// The Task Scheduler takes once a minute to once in 31 days; anything
    /// outside is held to the nearest end rather than refused by it later.
    #[test]
    fn an_interval_is_held_to_what_the_scheduler_takes() {
        assert_eq!(Every::parse("60d").map(Every::in_minutes), Some(31 * 24 * 60));
        assert_eq!(Every::minutes(0).in_minutes(), 1);
    }

    #[test]
    fn an_interval_is_written_back_the_way_it_is_read() {
        for minutes in [1, 5, 30, 60, 90, 120, 1440, 1530, 31 * 24 * 60] {
            let every = Every::minutes(minutes);
            assert_eq!(Every::parse(&every.key()), Some(every), "{} did not read back", every.key());
        }
        assert_eq!(Every::minutes(90).text(), "1 h 30 min");
        assert_eq!(Every::minutes(1440).text(), "1 day");
        assert_eq!(Every::minutes(2 * 1440 + 30).text(), "2 days 30 min");
    }

    #[test]
    fn an_interval_is_a_duration_the_task_scheduler_reads() {
        assert_eq!(Every::minutes(30).duration(), "PT30M");
        assert_eq!(Every::minutes(120).duration(), "PT2H");
        assert_eq!(Every::minutes(1440).duration(), "P1D");
        assert_eq!(Every::minutes(1440 + 150).duration(), "P1DT2H30M");
    }

    #[test]
    fn the_settings_survive_being_written_and_read() {
        let settings = Settings {
            folder: Some(PathBuf::from(r"D:\Pictures\Walls and more")),
            only: Pattern::parse("*.jpg;*.png"),
            every: Every::minutes(90),
            fit: Fit::Fit,
        };
        assert_eq!(Settings::parse(&settings.render()), settings);
        assert_eq!(Settings::parse(""), Settings::default());
        // A file edited into nonsense falls back key by key.
        let edited = Settings::parse("every = soon\nfit = stretch\nfolder = \n");
        assert_eq!(edited, Settings::default());
    }

    fn pictures(names: &[(&str, u64)]) -> Vec<Candidate> {
        names
            .iter()
            .map(|(name, seconds)| Candidate {
                name: (*name).to_string(),
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(*seconds),
            })
            .collect()
    }

    /// Every picture once a round, and the seam between rounds without the
    /// same picture twice in a row.
    #[test]
    fn a_round_shows_every_picture_once_and_the_seam_does_not_repeat() {
        let folder = pictures(&[("a", 1), ("b", 2), ("c", 3), ("d", 4)]);
        let mut deck = Deck::default();
        let mut rng = Rng::new(7);
        let mut dealt = Vec::new();
        for _ in 0..40 {
            dealt.push(deck.deal(&folder, &mut rng).unwrap());
        }
        assert_eq!(dealt[0], "d", "the first deal is not the newest picture");
        for round in dealt.chunks(4) {
            let distinct: BTreeSet<&String> = round.iter().collect();
            assert_eq!(distinct.len(), 4, "a round repeated a picture: {round:?}");
        }
        for pair in dealt.windows(2) {
            assert_ne!(pair[0], pair[1], "the same picture went up twice in a row: {dealt:?}");
        }
    }

    /// A picture that arrived since the last change goes up next, newest
    /// first, ahead of the rest of the round.
    #[test]
    fn an_arrival_goes_up_next_and_the_newest_first() {
        let mut deck = Deck::default();
        let mut rng = Rng::new(3);
        let before = pictures(&[("a", 1), ("b", 2), ("c", 3)]);
        deck.deal(&before, &mut rng);
        deck.deal(&before, &mut rng);

        let after = pictures(&[("a", 1), ("b", 2), ("c", 3), ("new", 10), ("newer", 20)]);
        assert_eq!(deck.deal(&after, &mut rng).as_deref(), Some("newer"));
        assert_eq!(deck.deal(&after, &mut rng).as_deref(), Some("new"));
        // And then the round goes on with what is left of it.
        let next = deck.deal(&after, &mut rng).unwrap();
        assert!(["a", "b", "c"].contains(&next.as_str()));
    }

    /// An arrival that went away again before its turn is not dealt.
    #[test]
    fn an_arrival_that_went_again_is_not_dealt() {
        let mut deck = Deck::default();
        let mut rng = Rng::new(5);
        deck.deal(&pictures(&[("a", 1), ("b", 2)]), &mut rng);
        deck.known.clear();
        deck.known.insert("a".into());
        deck.known.insert("b".into());
        deck.fresh = vec!["ghost".into()];
        let dealt = deck.deal(&pictures(&[("a", 1), ("b", 2)]), &mut rng).unwrap();
        assert_ne!(dealt, "ghost");
    }

    /// A folder of one picture deals it every time rather than nothing.
    #[test]
    fn a_folder_of_one_picture_deals_it_every_time() {
        let mut deck = Deck::default();
        let mut rng = Rng::new(1);
        let only = pictures(&[("only", 1)]);
        for _ in 0..3 {
            assert_eq!(deck.deal(&only, &mut rng).as_deref(), Some("only"));
        }
        assert_eq!(deck.deal(&[], &mut rng), None);
    }

    /// Started from a picture already put up, the deck does not put it up
    /// again in the same round.
    #[test]
    fn a_deck_started_from_a_picture_does_not_deal_it_again_this_round() {
        let settings = Settings {
            folder: Some(PathBuf::from("walls")),
            ..Settings::default()
        };
        let folder = pictures(&[("a", 1), ("b", 2), ("c", 3)]);
        let mut deck = Deck { slot: 1, ..Deck::default() };
        deck.start(&settings, &folder, Some("b"));
        // Which made picture is on the desktop is about the machine, and a new
        // start must not forget it — the next one up would be written over it.
        assert_eq!(deck.slot, 1);
        let mut rng = Rng::new(11);
        let next_two = [deck.deal(&folder, &mut rng).unwrap(), deck.deal(&folder, &mut rng).unwrap()];
        assert!(
            !next_two.contains(&"b".to_string()),
            "the first picture came round again too soon: {next_two:?}"
        );
        assert!(deck.deals_from(&settings));
        assert!(!deck.deals_from(&Settings::default()));
    }

    /// A deck started for a folder says so, so the next run carries on with
    /// it instead of starting over — which would put the newest picture up
    /// every single time.
    #[test]
    fn a_deck_remembers_what_it_deals_from_and_carries_on() {
        let settings = Settings {
            folder: Some(PathBuf::from("walls")),
            only: Pattern::parse("*.png"),
            ..Settings::default()
        };
        let folder = pictures(&[("a.png", 1), ("b.png", 2), ("c.png", 3)]);
        let mut rng = Rng::new(9);

        let mut deck = Deck::default().for_settings(&settings);
        assert!(deck.deals_from(&settings));
        let first = deck.deal(&folder, &mut rng).unwrap();

        // Written and read back between runs, as the schedule does.
        let mut deck = Deck::parse(&deck.render()).for_settings(&settings);
        let second = deck.deal(&folder, &mut rng).unwrap();
        assert_ne!(first, second, "the second run started the deck over");

        // Another folder is another deck.
        let elsewhere = Settings {
            folder: Some(PathBuf::from("other")),
            ..Settings::default()
        };
        let other = deck.clone().for_settings(&elsewhere);
        assert!(other.shown.is_empty() && other.deals_from(&elsewhere));
        assert_eq!(other.slot, deck.slot, "the desktop's picture file was forgotten with the folder");
    }

    #[test]
    fn the_deck_survives_being_written_and_read() {
        let mut deck = Deck {
            folder: Some(PathBuf::from(r"D:\Walls")),
            only: Some("*.jpg".into()),
            last: Some("b.jpg".into()),
            slot: 1,
            registered: Some(Every::minutes(60)),
            error: Some("a line\nbreak".into()),
            ..Deck::default()
        };
        deck.known.extend(["a.jpg".to_string(), "b.jpg".to_string(), "c d.jpg".to_string()]);
        deck.shown.insert("b.jpg".into());
        deck.fresh.push("c d.jpg".into());

        let read = Deck::parse(&deck.render());
        deck.error = Some("a line break".into());
        assert_eq!(read, deck);
    }

    #[test]
    fn the_command_line_is_read() {
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_string()).collect::<Vec<_>>();
        assert_eq!(Asked::parse(&args(&[])).unwrap(), Asked::Status);
        assert_eq!(Asked::parse(&args(&["next"])).unwrap(), Asked::Next);
        assert_eq!(Asked::parse(&args(&["off"])).unwrap(), Asked::Off);
        assert_eq!(
            Asked::parse(&args(&["D:\\Walls", "--every", "2h", "--only", "*.png", "--fit", "fit"])).unwrap(),
            Asked::Set {
                folder: Some(PathBuf::from("D:\\Walls")),
                every: Every::parse("2h"),
                only: Some("*.png".into()),
                fit: Some(Fit::Fit),
            }
        );
        assert_eq!(
            Asked::parse(&args(&["--every", "1d"])).unwrap(),
            Asked::Set {
                folder: None,
                every: Every::parse("1d"),
                only: None,
                fit: None
            }
        );
        // A folder that happens to be called "next" is reached by its path.
        assert!(matches!(Asked::parse(&args(&[".\\next"])).unwrap(), Asked::Set { folder: Some(_), .. }));
        for wrong in [&["--every"][..], &["--every", "soon"], &["--fit", "stretch"], &["--loud"], &["a", "b"]] {
            assert!(Asked::parse(&args(wrong)).is_err(), "{wrong:?} was accepted");
        }
    }

    /// Big enough that no display enlarges it, and a photograph never
    /// enlarged to get there.
    #[test]
    fn the_picture_is_made_as_big_as_the_largest_display_needs() {
        let laptop = (1920, 1200);
        let television = (3840, 2160);
        // A 3:2 photograph filling a 16:9 television: it is the narrower of the
        // two, so its width meets the screen's and the top and bottom overhang.
        assert_eq!(needed_size((6000, 4000), &[laptop, television], Fit::Fill, false), (3840, 2560));
        // Fitting, its height meets the screen's and the sides are left bare.
        assert_eq!(needed_size((6000, 4000), &[television], Fit::Fit, false), (3240, 2160));
        // A small picture stays small.
        assert_eq!(needed_size((800, 600), &[television], Fit::Fill, false), (800, 600));
        // A vector is drawn at the size it is needed at.
        assert_eq!(needed_size((100, 50), &[laptop], Fit::Fill, true), (2400, 1200));
        // Nothing known about the displays: the picture keeps its size.
        assert_eq!(needed_size((6000, 4000), &[], Fit::Fill, false), (6000, 4000));
    }

    /// The desktop's picture is made upright and in sRGB from a file of any
    /// kind: here a PNG written by a third-party encoder, shrunk to a small
    /// display.
    #[test]
    fn a_picture_is_prepared_for_the_desktop_as_a_jpeg_of_the_size_needed() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("wide.png");
        let mut image = image::RgbaImage::new(400, 200);
        for (x, _, pixel) in image.enumerate_pixels_mut() {
            *pixel = image::Rgba([if x < 200 { 255 } else { 0 }, 0, 0, 255]);
        }
        image.save(&source).unwrap();

        let bytes = prepare(&source, &[(100, 100)], Fit::Fill).unwrap();
        let made = image::load_from_memory(&bytes).unwrap();
        assert_eq!(image::guess_format(&bytes).unwrap(), image::ImageFormat::Jpeg);
        assert_eq!((made.width(), made.height()), (200, 100), "the picture was not made the size the display needs");
        let left = made.to_rgb8().get_pixel(10, 50).0;
        assert!(left[0] > 200 && left[1] < 60, "the colour did not survive: {left:?}");
    }
}

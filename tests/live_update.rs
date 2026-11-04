//! Updating, with the real binaries against a server of the test's own.
//!
//! Each test installs a copy into a scratch `%LOCALAPPDATA%`, under an instance
//! tag that keeps its registry entries in a corner of their own and its
//! resident out of everyone's way. The installed copy is this build with a few
//! bytes on the end, which runs the same and hashes differently; the release
//! the server offers is this build as it is. `NITID_UPDATE_CURRENT` has the old
//! one believe it is older, since both are really one version.
//!
//! Nothing here touches the real install, the real registration or GitHub. The
//! real thing is run on every release by `.github/workflows/install.yml`.

#![cfg(windows)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};
use windows::core::HSTRING;
use windows_registry::CURRENT_USER;

/// How long an update is given, on a slow test machine.
const PATIENCE: Duration = Duration::from_secs(60);

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn binary(name: &str) -> PathBuf {
    let mut dir = std::env::current_exe().expect("the test's own path");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    let exe = dir.join(name);
    assert!(exe.exists(), "{} is not built", exe.display());
    exe
}

/// Both executables with `extra` on the end of each. A program does not read
/// past its own image, so it runs the same and is a different file.
fn pair_with(extra: &[u8]) -> [(&'static str, Vec<u8>); 2] {
    ["nitid.exe", "nitidw.exe"].map(|name| {
        let mut bytes = fs::read(binary(name)).expect("reading the build");
        bytes.extend_from_slice(extra);
        (name, bytes)
    })
}

/// A zip that stores its files, in the shape the updater reads.
fn zip_of(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let offset = out.len() as u32;
        let size = (data.len() as u32).to_le_bytes();

        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&crc.sum().to_le_bytes());
        out.extend_from_slice(&size);
        out.extend_from_slice(&size);
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend_from_slice(&crc.sum().to_le_bytes());
        central.extend_from_slice(&size);
        central.extend_from_slice(&size);
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let start = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A stand-in for GitHub's release addresses: what it serves, and who asked.
struct Feed {
    port: u16,
    files: Arc<Mutex<BTreeMap<String, Arc<Vec<u8>>>>>,
    manifest_asks: Arc<AtomicUsize>,
    archive_asks: Arc<AtomicUsize>,
    closing: Arc<AtomicBool>,
}

impl Feed {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("its address").port();
        let feed = Self {
            port,
            files: Arc::default(),
            manifest_asks: Arc::default(),
            archive_asks: Arc::default(),
            closing: Arc::default(),
        };
        let (files, manifest_asks, archive_asks, closing) = (feed.files.clone(), feed.manifest_asks.clone(), feed.archive_asks.clone(), feed.closing.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if closing.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(stream) = stream else {
                    continue;
                };
                let (files, manifest_asks, archive_asks) = (files.clone(), manifest_asks.clone(), archive_asks.clone());
                std::thread::spawn(move || answer(stream, &files, &manifest_asks, &archive_asks));
            }
        });
        feed
    }

    fn address(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Offer `archive` as the newest release, under `version`.
    fn offer(&self, version: &str, archive: Vec<u8>) {
        let digest = hex(&nitid::testing::sha256(&archive).expect("a digest"));
        self.offer_saying(version, archive, &digest);
    }

    /// The same, with a digest of the caller's choosing.
    fn offer_saying(&self, version: &str, archive: Vec<u8>, digest: &str) {
        let name = format!("nitid-v{version}-x86_64-pc-windows-msvc.zip");
        let manifest = format!("version = {version}\narchive = {name}\nsha256 = {digest}\nsize = {}\n", archive.len());
        let mut files = self.files.lock().expect("the files");
        files.insert("/latest/download/update.txt".to_string(), Arc::new(manifest.into_bytes()));
        files.insert(format!("/download/v{version}/{name}"), Arc::new(archive));
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        // Wakes the accept the thread is sitting in.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn answer(mut stream: TcpStream, files: &Mutex<BTreeMap<String, Arc<Vec<u8>>>>, manifest_asks: &AtomicUsize, archive_asks: &AtomicUsize) {
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(read) => request.extend_from_slice(&chunk[..read]),
        }
    }
    let line = String::from_utf8_lossy(&request);
    let path = line.split_whitespace().nth(1).unwrap_or_default().to_string();
    if path.ends_with("update.txt") {
        manifest_asks.fetch_add(1, Ordering::SeqCst);
    } else {
        archive_asks.fetch_add(1, Ordering::SeqCst);
    }
    let found = files.lock().expect("the files").get(&path).cloned();
    let (status, body) = match found {
        Some(body) => ("200 OK", body),
        None => ("404 Not Found", Arc::new(Vec::new())),
    };
    let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
}

/// One scratch machine: an install directory, a corner of the registry, a
/// resident of its own and a feed. Taken apart however the test ends.
struct Stand {
    dir: TempDir,
    tag: String,
    feed: Feed,
    _turn: std::sync::MutexGuard<'static, ()>,
}

/// One stand at a time. An update holds the two executables in memory several
/// times over - in the feed, the download and the unpacking - and side by side
/// these would ask more of a build machine than the test is worth.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// A release this test has no intention of letting anyone install: for the
/// paths that stop before reading it, or at its first check.
fn no_real_archive() -> Vec<u8> {
    b"not an archive".to_vec()
}

impl Stand {
    fn new(name: &str) -> Self {
        Self {
            _turn: ONE_AT_A_TIME.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
            dir: tempfile::tempdir().expect("a scratch directory"),
            tag: format!("update{}{name}", std::process::id()),
            feed: Feed::start(),
        }
    }

    fn installed(&self) -> PathBuf {
        self.dir.path().join("Programs").join("nitid")
    }

    /// Everything a process of this stand needs told: where its machine is,
    /// and that it is not really up to date.
    fn command(&self, exe: &Path) -> Command {
        let mut command = Command::new(exe);
        common::keep_data_in(&mut command, self.dir.path())
            .env("NITID_INSTANCE_ID", &self.tag)
            .env("NITID_UPDATE_FEED", self.feed.address())
            .env("NITID_UPDATE_CURRENT", "0.0.1");
        command
    }

    fn settings(&self, text: &str) {
        let dir = self.dir.path().join("lacodda").join("nitid");
        fs::create_dir_all(&dir).expect("the settings' directory");
        fs::write(dir.join("settings.conf"), text).expect("writing the settings");
    }

    /// Install the old copy: this build with a mark on the end, from a
    /// directory of its own, by the same `nitid install` a person runs.
    fn install_old(&self) {
        let from = self.dir.path().join("old");
        fs::create_dir_all(&from).expect("a directory to install from");
        for (name, bytes) in pair_with(b"the old copy") {
            fs::write(from.join(name), bytes).expect("writing the old copy");
        }
        let mut command = self.command(&from.join("nitid.exe"));
        command.arg("install");
        let output = finished(command);
        assert!(output.status.success(), "install failed: {}", said(&output));
    }

    /// Install this build as it is, for a test where no update is meant to
    /// land. Far cheaper than the old copy: the executables are written once
    /// rather than twice, and an antivirus has one new file to read per name.
    fn install_this_build(&self) {
        let mut command = self.command(&binary("nitid.exe"));
        command.arg("install");
        let output = finished(command);
        assert!(output.status.success(), "install failed: {}", said(&output));
    }

    fn update(&self) -> Output {
        self.installed_copy(&["update"])
    }

    /// The installed console binary, run to its end.
    fn installed_copy(&self, arguments: &[&str]) -> Output {
        let mut command = self.command(&self.installed().join("nitid.exe"));
        command.args(arguments);
        finished(command)
    }

    fn hive(&self) -> String {
        format!(r"Software\lacodda\nitid-test\{}", self.tag)
    }

    fn resident_running(&self) -> bool {
        unsafe { FindWindowW(&HSTRING::from(format!("lacodda.nitid.resident{}", self.tag)), None) }.is_ok()
    }

    /// The installed pair, byte for byte.
    fn on_disk(&self) -> Vec<Vec<u8>> {
        ["nitid.exe", "nitidw.exe"]
            .map(|name| fs::read(self.installed().join(name)).expect("an installed file"))
            .to_vec()
    }

    fn state(&self) -> String {
        fs::read_to_string(self.dir.path().join("lacodda").join("nitid").join("update").join("state")).unwrap_or_default()
    }

    fn stop_resident(&self) {
        let class = HSTRING::from(format!("lacodda.nitid.resident{}", self.tag));
        if let Ok(window) = unsafe { FindWindowW(&class, None) } {
            let _ = unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.resident_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        // Its window goes a moment before its image is let go of, and the
        // scratch directory cannot be removed around a running executable.
        std::thread::sleep(Duration::from_millis(300));
    }
}

impl Drop for Stand {
    fn drop(&mut self) {
        self.stop_resident();
        let _ = CURRENT_USER.remove_tree(self.hive());
    }
}

/// Run a command to its end and take what it said.
///
/// With a limit, and on a thread of its own: reading a process's output ends
/// when everything holding the far end lets go, so a resident that inherited it
/// is a read that never finishes. That has happened, and it is a failure to
/// report rather than a gate to hang.
fn finished(mut command: Command) -> Output {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(command.stdin(Stdio::null()).output());
    });
    finished
        .recv_timeout(PATIENCE)
        .expect("the command has not let go of its output: something it started is still holding it")
        .expect("running the command")
}

fn said(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

fn until(what: &str, mut holds: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !holds() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Every key and value under `path`, as text.
fn dump(path: &str) -> BTreeMap<String, String> {
    fn walk(key: &windows_registry::Key, path: &str, out: &mut BTreeMap<String, String>) {
        if let Ok(values) = key.values() {
            for (name, value) in values {
                out.insert(format!(r"{path}\\{name}"), format!("{value:?}"));
            }
        }
        if let Ok(names) = key.keys() {
            for name in names {
                if let Ok(inner) = key.open(&name) {
                    out.insert(format!(r"{path}\{name}"), String::new());
                    walk(&inner, &format!(r"{path}\{name}"), out);
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    if let Ok(key) = CURRENT_USER.open(path) {
        walk(&key, "", &mut out);
    }
    out
}

/// What belongs to a person and to other programs, planted where Windows and
/// they keep it, as it sits before nitid is ever installed.
fn plant_what_is_not_ours(hive: &str) {
    let choice = CURRENT_USER
        .create(format!(
            r"{hive}\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.jpg\UserChoiceLatest\ProgId"
        ))
        .expect("a choice");
    choice.set_string("ProgId", "lacodda.nitid.image").expect("the choice");
    choice.set_string("Hash", "signed by windows").expect("its signature");
    CURRENT_USER
        .create(format!(r"{hive}\Software\Classes\.jpg\OpenWithProgids"))
        .expect("the type's list")
        .set_string("Another.Program", "")
        .expect("another program's entry");
    CURRENT_USER
        .create(format!(r"{hive}\Software\Classes\.jpg"))
        .expect("the type")
        .set_string("", "Another.Default")
        .expect("the type's default");
}

/// The whole of it, through the command: the release is believed, the old copy
/// goes, the new one runs, and nothing but the program has changed. A person's
/// default program, another program's entries and every key of nitid's own come
/// through byte for byte, which is what "associations survive an update" is.
#[test]
fn an_update_replaces_the_program_and_nothing_else() {
    let stand = Stand::new("whole");
    stand.settings("updates = off\n");
    plant_what_is_not_ours(&stand.hive());
    stand.install_old();
    assert!(stand.resident_running(), "install did not start the resident");

    let new = pair_with(b"");
    stand.feed.offer(VERSION, zip_of(&new));
    let before = dump(&stand.hive());
    assert!(
        before.keys().any(|key| key.contains("UserChoiceLatest")),
        "the choice was not planted: {before:?}"
    );

    let output = stand.update();
    assert!(output.status.success(), "update failed: {}", said(&output));

    assert_eq!(
        stand.on_disk(),
        new.iter().map(|(_, bytes)| bytes.clone()).collect::<Vec<_>>(),
        "the installed pair is not the release's"
    );
    assert_eq!(dump(&stand.hive()), before, "the update changed the registry");
    until("the resident did not come back", || stand.resident_running());
    assert!(!stand.state().contains("failed"), "{}", stand.state());

    // And again, from the new copy: the release is now what is installed.
    let asks = stand.feed.archive_asks.load(Ordering::SeqCst);
    let mut again = stand.command(&stand.installed().join("nitid.exe"));
    again.env("NITID_UPDATE_CURRENT", VERSION).arg("update");
    let again = finished(again);
    assert!(again.status.success(), "{}", said(&again));
    assert!(said(&again).contains("is the latest"), "{}", said(&again));
    assert_eq!(stand.feed.archive_asks.load(Ordering::SeqCst), asks, "an up-to-date copy downloaded something");
}

/// What a person took away stays away. An install puts these back because
/// someone ran it; an update runs with nobody asking. Quitting the resident is
/// one of them: someone who quit it and then asked for an update asked for an
/// update.
#[test]
fn an_update_does_not_bring_back_what_was_taken_away() {
    let stand = Stand::new("away");
    stand.settings("updates = off\n");
    stand.install_old();

    let hive = stand.hive();
    let run = CURRENT_USER
        .options()
        .read()
        .write()
        .open(format!(r"{hive}\Software\Microsoft\Windows\CurrentVersion\Run"))
        .expect("the sign-in entries");
    assert!(run.get_string("nitid").is_ok(), "install wrote no sign-in entry");
    run.remove_value("nitid").expect("taking it out");
    let environment = CURRENT_USER
        .options()
        .read()
        .write()
        .open(format!(r"{hive}\Environment"))
        .expect("the environment");
    environment.remove_value("Path").expect("taking the PATH out");
    stand.stop_resident();

    stand.feed.offer(VERSION, zip_of(&pair_with(b"")));
    let output = stand.update();
    assert!(output.status.success(), "update failed: {}", said(&output));

    assert!(run.get_string("nitid").is_err(), "the update wrote the sign-in entry back");
    assert!(environment.get_string("Path").is_err(), "the update wrote the PATH back");
    std::thread::sleep(Duration::from_secs(2));
    assert!(!stand.resident_running(), "the update started a resident that had been quit");
}

/// The whole of it again with nobody asking: the resident notices, the new
/// version installs itself and starts the new resident, which is told.
#[test]
fn the_resident_keeps_the_copy_current_on_its_own() {
    let stand = Stand::new("alone");
    let new = pair_with(b"");
    stand.feed.offer(VERSION, zip_of(&new));

    stand.install_old();

    let wanted: Vec<Vec<u8>> = new.iter().map(|(_, bytes)| bytes.clone()).collect();
    until("the resident did not install the release", || stand.on_disk() == wanted);
    until("the new resident did not start", || stand.resident_running());
    // The new resident takes the news once it is up. Its window comes first,
    // so this waits: the point is that it is taken and not left to be told
    // again at every sign-in.
    until("the news was not taken", || {
        let state = stand.state();
        state.contains("checked") && !state.contains("news") && !state.contains("failed")
    });
    assert!(stand.feed.archive_asks.load(Ordering::SeqCst) >= 1);

    // At the next sign-in nothing is left of the old copy or the download: the
    // process that retired them has gone, so nothing can be holding them.
    stand.stop_resident();
    let _resident = Parked(
        stand
            .command(&stand.installed().join("nitidw.exe"))
            .arg("--resident")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("starting the resident"),
    );
    until("the retired copies were not swept", || {
        let mut names: Vec<String> = fs::read_dir(stand.installed())
            .expect("the directory")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names == ["nitid.exe", "nitidw.exe"]
    });
    until("what was unpacked was not taken away", || {
        let update = stand.dir.path().join("lacodda").join("nitid").join("update");
        fs::read_dir(update)
            .expect("the updater's directory")
            .flatten()
            .all(|entry| !entry.path().is_dir())
    });
}

/// Switched off, nothing at all reaches the network: the switch is the reason
/// a person can keep this on a machine that must not call out.
#[test]
fn switched_off_nothing_is_asked() {
    let stand = Stand::new("off");
    stand.settings("updates = off\n");
    stand.feed.offer(VERSION, no_real_archive());

    stand.install_this_build();
    let before = stand.on_disk();

    // Long past the moment a resident that was going to ask would have: with a
    // feed standing in, it asks as soon as it starts.
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(stand.feed.manifest_asks.load(Ordering::SeqCst), 0, "the resident asked while switched off");
    assert_eq!(stand.on_disk(), before);
    assert!(stand.resident_running());
}

/// A copy that is already the newest asks, and stops there.
#[test]
fn an_up_to_date_resident_downloads_nothing() {
    let stand = Stand::new("current");
    stand.settings("updates = off\n");
    stand.feed.offer(VERSION, no_real_archive());
    stand.install_this_build();
    let before = stand.on_disk();
    stand.stop_resident();

    // Switched on and started as a sign-in starts it, knowing its real version.
    stand.settings("");
    let _resident = Parked(
        stand
            .command(&stand.installed().join("nitidw.exe"))
            .env("NITID_UPDATE_CURRENT", VERSION)
            .arg("--resident")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("starting the resident"),
    );

    until("the resident did not ask", || stand.feed.manifest_asks.load(Ordering::SeqCst) >= 1);
    until("the answer was not noted", || stand.state().contains("checked"));
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(stand.feed.archive_asks.load(Ordering::SeqCst), 0, "an up-to-date copy downloaded the release");
    assert_eq!(stand.on_disk(), before);
}

/// A resident run from a build asks nobody anything: it has nothing to replace,
/// and anyone running one has not asked to have the installed copy replaced.
#[test]
fn a_resident_that_is_not_the_installed_copy_asks_nothing() {
    let stand = Stand::new("buildresident");
    stand.feed.offer(VERSION, no_real_archive());

    let _resident = Parked(
        stand
            .command(&binary("nitidw.exe"))
            .arg("--resident")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("starting the resident"),
    );
    until("the resident did not start", || stand.resident_running());

    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(stand.feed.manifest_asks.load(Ordering::SeqCst), 0, "a build's resident asked");
}

/// A decoder started by a window open across an update may not understand the
/// request. It says so rather than dying: silence reads as a file that crashed
/// it, and the person would blame the picture.
#[test]
fn a_decoder_that_cannot_read_the_request_answers() {
    let mut decoder = Command::new(binary("nitid.exe"))
        .arg("--decode-stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("starting the decoder");
    decoder
        .stdin
        .take()
        .expect("its input")
        .write_all(b"not a request in any version")
        .expect("writing to it");

    let output = decoder.wait_with_output().expect("waiting for it");

    assert!(!output.stdout.is_empty(), "the decoder went away without answering");
    assert!(output.status.success(), "the decoder failed instead of answering: {}", output.status);
}

/// `--check` says and does nothing else.
#[test]
fn checking_only_says() {
    let stand = Stand::new("check");
    stand.settings("updates = off\n");
    stand.install_this_build();
    stand.feed.offer(VERSION, no_real_archive());
    let before = stand.on_disk();

    let output = stand.installed_copy(&["update", "--check"]);

    assert!(output.status.success(), "{}", said(&output));
    assert!(said(&output).contains(&format!("nitid {VERSION} is out")), "{}", said(&output));
    assert_eq!(stand.feed.archive_asks.load(Ordering::SeqCst), 0, "--check downloaded the release");
    assert_eq!(stand.on_disk(), before, "--check changed the install");
}

/// A release whose bytes are not the ones published never gets as far as
/// running, and leaves the copy that is there exactly as it was, working.
#[test]
fn a_release_that_does_not_match_its_digest_is_not_run() {
    let stand = Stand::new("digest");
    stand.settings("updates = off\n");
    stand.install_this_build();
    let before = stand.on_disk();

    stand.feed.offer_saying(VERSION, no_real_archive(), &"0".repeat(64));
    let output = stand.update();

    assert!(!output.status.success(), "a release with the wrong digest was installed");
    assert!(said(&output).contains("digest"), "{}", said(&output));
    assert_eq!(stand.on_disk(), before, "the install changed");
    assert!(stand.state().contains("failed"), "the settings will not be able to say why: {}", stand.state());
    assert!(
        stand.resident_running(),
        "the resident was stopped for a release that was never going to install"
    );
}

/// A release whose program says it is another version is not the release it
/// claims to be. Taken, it would be fetched again every day, for ever.
#[test]
fn a_release_that_is_not_the_version_it_says_is_refused() {
    let stand = Stand::new("label");
    stand.settings("updates = off\n");
    stand.install_old();
    let before = stand.on_disk();

    stand.feed.offer("999.0.0", zip_of(&pair_with(b"")));
    let output = stand.update();

    assert!(!output.status.success(), "a mislabelled release was installed");
    assert!(said(&output).contains("999.0.0"), "{}", said(&output));
    assert_eq!(stand.on_disk(), before, "the install changed");
}

/// Older than what is here is not an update, whatever the manifest says: a
/// release withdrawn sends `latest` back to the one before it.
#[test]
fn an_older_release_is_left_alone() {
    let stand = Stand::new("older");
    stand.settings("updates = off\n");
    stand.install_this_build();
    let before = stand.on_disk();

    stand.feed.offer("0.0.1", no_real_archive());
    let output = stand.update();

    assert!(output.status.success(), "{}", said(&output));
    assert!(said(&output).contains("is the latest"), "{}", said(&output));
    assert_eq!(
        stand.feed.archive_asks.load(Ordering::SeqCst),
        0,
        "it downloaded a release it was never going to take"
    );
    assert_eq!(stand.on_disk(), before);
}

/// A process this test started, killed however the test ends.
struct Parked(Child);

impl Drop for Parked {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A window left open across two updates: it holds the file the first one
/// retired for as long as it stays open, and the second must find another name
/// for the one it retires. Before this version the second update failed here,
/// and with nobody watching would have kept failing until the window closed.
#[test]
fn two_updates_land_while_something_runs_the_oldest_copy() {
    let stand = Stand::new("twice");
    stand.settings("updates = off\n");
    stand.install_old();

    // The decoder waits for its request on stdin, so with the pipe held open
    // this is a process of the installed executable sitting there as long as
    // the test likes: all a window is, to the file system.
    let parked = Parked(
        stand
            .command(&stand.installed().join("nitid.exe"))
            .arg("--decode-stdin")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("running the installed copy"),
    );

    stand.feed.offer(VERSION, zip_of(&pair_with(b"first")));
    let first = stand.update();
    assert!(first.status.success(), "the first update failed: {}", said(&first));

    let second_release = pair_with(b"second");
    stand.feed.offer(VERSION, zip_of(&second_release));
    let second = stand.update();
    assert!(
        second.status.success(),
        "the second update failed with the oldest copy running: {}",
        said(&second)
    );

    assert_eq!(stand.on_disk(), second_release.iter().map(|(_, bytes)| bytes.clone()).collect::<Vec<_>>());
    drop(parked);
}

/// Replacing the installed copy from a build run anywhere else is what
/// `nitid install` is for, and nobody running a build means it by accident.
#[test]
fn a_copy_that_is_not_installed_does_not_update() {
    let stand = Stand::new("build");
    stand.settings("updates = off\n");
    stand.install_this_build();
    stand.feed.offer(VERSION, no_real_archive());
    let before = stand.on_disk();

    let mut command = stand.command(&binary("nitid.exe"));
    command.arg("update");
    let output = finished(command);

    assert!(!output.status.success(), "a build updated the installed copy");
    assert!(said(&output).contains("not the installed one"), "{}", said(&output));
    assert_eq!(stand.feed.manifest_asks.load(Ordering::SeqCst), 0, "it asked before refusing");
    assert_eq!(stand.on_disk(), before);
}

/// Installing under a test's tag reaches nothing of the real registration:
/// without this every gate run would rewrite the file types of whoever ran it.
/// Asked of the same keys the code writes, in the place it would write them.
#[test]
fn a_tagged_install_stays_in_its_corner() {
    let stand = Stand::new("corner");
    stand.settings("updates = off\n");

    let real = r"Software\Classes\lacodda.nitid.image\shell\open\command";
    let before = CURRENT_USER.open(real).and_then(|key| key.get_string(""));

    stand.install_this_build();

    let after = CURRENT_USER.open(real).and_then(|key| key.get_string(""));
    assert_eq!(after.ok(), before.ok(), "a tagged install wrote the real registration");
    assert!(
        CURRENT_USER
            .open(format!(r"{}\{real}", stand.hive()))
            .and_then(|key| key.get_string(""))
            .is_ok_and(|command| command.contains(&stand.installed().display().to_string())),
        "the tagged install wrote nothing where it should have"
    );
}

/// Uninstalling takes the updater's own leftovers with it.
#[test]
fn uninstalling_leaves_nothing_of_the_updater() {
    let stand = Stand::new("gone");
    stand.settings("updates = off\n");
    stand.install_this_build();
    stand.feed.offer(VERSION, no_real_archive());
    let output = stand.installed_copy(&["update", "--check"]);
    assert!(output.status.success(), "{}", said(&output));
    let state = stand.dir.path().join("lacodda").join("nitid").join("update");
    assert!(state.exists(), "the update wrote no state");

    // From a copy outside the install, which can delete all of it.
    let mut command = stand.command(&binary("nitid.exe"));
    command.arg("uninstall");
    let output = finished(command);
    assert!(output.status.success(), "{}", said(&output));

    assert!(!state.exists(), "the updater's directory survived uninstalling");
    assert!(!stand.installed().join("nitid.exe").exists());
    until("the resident outlived the uninstall", || !stand.resident_running());
}

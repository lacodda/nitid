//! The release gate for Explorer's thumbnails: **the DLL itself, loaded the
//! way Explorer's host loads it**, beside the real `nitid.exe`.
//!
//! What makes a thumbnail is held by unit tests (`src/thumbs`). What is held
//! here is what they cannot reach: the DLL is compiled by the build script and
//! its COM is written out by hand, so only a program that loads it and calls it
//! can say that it answers; and that the picture is read in the sandbox is a
//! statement about processes, which only processes can show.
//!
//! Then the registration, in a corner of the registry of the test's own.
//! Explorer is not asked here, since that takes the real registration:
//! `tools/check-thumbnails.ps1` asks it, on the machines the release workflow
//! installs to and on every push to CI.

#![cfg(windows)]

mod common;

use std::ffi::{CString, c_char, c_void};
use std::io::{Cursor, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_NOINTERFACE, E_UNEXPECTED, ERROR_ALREADY_INITIALIZED, S_FALSE, S_OK,
};
use windows::Win32::Graphics::Gdi::{BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DIBSECTION, DeleteObject, GetDC, GetDIBits, GetObjectW, HBITMAP, ReleaseDC};
use windows::Win32::System::Com::{IClassFactory, IStream};
use windows::Win32::UI::Shell::PropertiesSystem::IInitializeWithStream;
use windows::Win32::UI::Shell::{IThumbnailProvider, SHCreateMemStream, WTS_ALPHATYPE, WTSAT_ARGB};
use windows::core::{GUID, HRESULT, IUnknown, Interface};
use windows_registry::CURRENT_USER;

const NITID: &str = env!("CARGO_BIN_EXE_nitid");

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}

type GetClassObject = unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;
type CanUnloadNow = unsafe extern "system" fn() -> HRESULT;

/// One at a time: what the decoder is told comes through the environment, which
/// is the process's and not a test's.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// The DLL in a directory of its own, loaded.
struct Dll {
    module: *mut c_void,
    get_class_object: GetClassObject,
    can_unload_now: CanUnloadNow,
    class: GUID,
    dir: TempDir,
    _turn: MutexGuard<'static, ()>,
}

impl Dll {
    /// Loaded beside this build's `nitid.exe`, as an install leaves the two.
    fn beside_nitid() -> Self {
        let dll = Self::alone();
        let beside = dll.dir.path().join("nitid.exe");
        if std::fs::hard_link(NITID, &beside).is_err() {
            std::fs::copy(NITID, &beside).expect("copying nitid.exe");
        }
        dll
    }

    fn alone() -> Self {
        let turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = tempfile::tempdir().expect("a scratch directory");
        let (name, bytes, class) = nitid::testing::thumbnail_provider();
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).expect("writing the DLL");

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: the names are terminated, the module is checked, and the two
        // functions have the types every COM server's have.
        unsafe {
            let module = LoadLibraryW(wide.as_ptr());
            assert!(!module.is_null(), "the DLL does not load: {}", std::io::Error::last_os_error());
            let export = |name: &str| {
                let found = GetProcAddress(module, CString::new(name).expect("a name").as_ptr());
                assert!(!found.is_null(), "the DLL exports no {name}");
                found
            };
            Self {
                module,
                get_class_object: std::mem::transmute::<*mut c_void, GetClassObject>(export("DllGetClassObject")),
                can_unload_now: std::mem::transmute::<*mut c_void, CanUnloadNow>(export("DllCanUnloadNow")),
                class: GUID::from_u128(class),
                dir,
                _turn: turn,
            }
        }
    }

    fn factory(&self) -> IClassFactory {
        let mut out = std::ptr::null_mut();
        // SAFETY: the pointers are to locals, and what comes back is a
        // reference this takes over.
        unsafe {
            (self.get_class_object)(&self.class, &IClassFactory::IID, &mut out).ok().expect("a factory");
            IClassFactory::from_raw(out)
        }
    }

    /// Everything a host does for one file.
    fn thumbnail(&self, picture: &[u8], side: u32) -> windows::core::Result<Bitmap> {
        // SAFETY: COM calls on objects made here, on this thread.
        unsafe {
            let initialize: IInitializeWithStream = self.factory().CreateInstance(None)?;
            initialize.Initialize(&stream_of(picture), 0)?;
            ask(&initialize.cast::<IThumbnailProvider>()?, side)
        }
    }

    fn can_unload(&self) -> bool {
        // SAFETY: the DLL is loaded for as long as this is.
        match unsafe { (self.can_unload_now)() } {
            S_OK => true,
            S_FALSE => false,
            other => panic!("DllCanUnloadNow answered {other:?}"),
        }
    }
}

impl Drop for Dll {
    fn drop(&mut self) {
        // SAFETY: loaded once, and nothing of it is used after this.
        unsafe { FreeLibrary(self.module) };
    }
}

fn stream_of(bytes: &[u8]) -> IStream {
    // SAFETY: the stream copies the bytes.
    unsafe { SHCreateMemStream(Some(bytes)) }.expect("a stream")
}

/// What a host is handed back.
struct Bitmap {
    width: i32,
    height: i32,
    /// Blue, green, red, alpha; the top row first.
    pixels: Vec<u8>,
}

impl Bitmap {
    fn at(&self, x: i32, y: i32) -> [u8; 4] {
        let start = ((y * self.width + x) * 4) as usize;
        self.pixels[start..start + 4].try_into().expect("a pixel")
    }
}

unsafe fn ask(provider: &IThumbnailProvider, side: u32) -> windows::core::Result<Bitmap> {
    let mut handle = HBITMAP::default();
    let mut alpha = WTS_ALPHATYPE::default();
    // SAFETY: the out-parameters are locals; the bitmap is this one's to
    // delete, and is read before it is.
    unsafe {
        provider.GetThumbnail(side, &mut handle, &mut alpha)?;
        assert_eq!(alpha, WTSAT_ARGB, "the bitmap's alpha is not declared");
        let mut section = DIBSECTION::default();
        let size = size_of::<DIBSECTION>() as i32;
        assert_eq!(
            GetObjectW(handle.into(), size, Some((&raw mut section).cast())),
            size,
            "it is not a DIB section"
        );
        assert_eq!(section.dsBm.bmBitsPixel, 32);
        let (width, height) = (section.dsBm.bmWidth, section.dsBm.bmHeight);
        // Through GDI, which is told the top row comes first and turns over a
        // bitmap that was made the other way up. The memory would read the same.
        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let screen = GetDC(None);
        let rows = GetDIBits(screen, handle, 0, height as u32, Some(pixels.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, screen);
        assert_eq!(rows, height, "the bitmap cannot be read");
        let _ = DeleteObject(handle.into());
        Ok(Bitmap { width, height, pixels })
    }
}

/// Run `body` with these set for whatever it starts.
fn told<T>(variables: &[(&str, &str)], body: impl FnOnce() -> T) -> T {
    // SAFETY: every caller holds `ONE_AT_A_TIME`, through its `Dll`.
    unsafe { variables.iter().for_each(|(name, value)| std::env::set_var(name, value)) };
    let outcome = body();
    // SAFETY: as above.
    unsafe { variables.iter().for_each(|(name, _)| std::env::remove_var(name)) };
    outcome
}

fn png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(width, height, |x, y| image::Rgba(pixel(x, y)));
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encoding a picture");
    out.into_inner()
}

/// Every format nitid draws for Windows, each a file another program wrote,
/// through the DLL, the program and the sandbox to a bitmap of that picture.
#[test]
fn a_picture_of_each_format_comes_back_as_a_bitmap() {
    let dll = Dll::beside_nitid();
    let mut pictures = nitid::testing::thumbnail_samples();
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><rect width="64" height="64" fill="#2878c8"/></svg>"##;
    pictures.push(("svg", svg.to_vec()));
    assert_eq!(pictures.len(), 4);

    for (extension, picture) in pictures {
        let bitmap = dll.thumbnail(&picture, 256).unwrap_or_else(|error| panic!("{extension}: {error}"));
        // Shapes are drawn at the size asked for; a picture is not enlarged.
        let side = if extension == "svg" { 256 } else { 64 };
        assert_eq!((bitmap.width, bitmap.height), (side, side), "{extension}");
        let [blue, green, red, alpha] = bitmap.at(side / 2, side / 2);
        for (got, meant) in [(red, 40u8), (green, 120), (blue, 200)] {
            assert!(got.abs_diff(meant) <= 6, "{extension}: {red},{green},{blue} is not 40,120,200");
        }
        assert_eq!(alpha, 255, "{extension}");
    }
    assert!(dll.can_unload(), "something was left out");
}

/// The size is the one asked for, the first row is the top one, and what is
/// transparent is.
#[test]
fn a_bitmap_is_the_size_asked_for_and_the_right_way_up() {
    let dll = Dll::beside_nitid();
    let picture = png(1000, 500, |x, y| match (x < 500, y < 250) {
        (true, true) => [255, 0, 0, 255],
        (false, true) => [0, 255, 0, 255],
        (true, false) => [0, 0, 255, 255],
        (false, false) => [255, 255, 255, 0],
    });
    let bitmap = dll.thumbnail(&picture, 96).expect("a thumbnail");
    assert_eq!((bitmap.width, bitmap.height), (96, 48));
    assert_eq!(bitmap.at(10, 10), [0, 0, 255, 255], "the top left is not red");
    assert_eq!(bitmap.at(85, 10), [0, 255, 0, 255], "the top right is not green");
    assert_eq!(bitmap.at(10, 38), [255, 0, 0, 255], "the bottom left is not blue");
    // Multiplied by its alpha, white that is not there is nothing at all.
    assert_eq!(bitmap.at(85, 38), [0, 0, 0, 0], "the bottom right is not transparent");
}

/// A file nothing can be made of is a failure, at once, and leaves nothing
/// behind.
#[test]
fn a_file_that_will_not_decode_has_no_thumbnail() {
    let dll = Dll::beside_nitid();
    let whole = png(300, 300, |x, y| [x as u8, y as u8, 7, 255]);
    for broken in [b"not a picture at all".to_vec(), Vec::new(), whole[..whole.len() / 3].to_vec()] {
        let started = Instant::now();
        let error = dll.thumbnail(&broken, 256).err().expect("a thumbnail was made of it");
        assert_eq!(error.code(), E_FAIL);
        assert!(started.elapsed() < Duration::from_secs(20), "it took {:?}", started.elapsed());
    }
    assert!(dll.can_unload());
}

/// A decoder that never answers costs an icon in place of a thumbnail, and not
/// the host that waits for it.
#[test]
fn a_decoder_that_hangs_is_ended() {
    let dll = Dll::beside_nitid();
    let picture = png(30, 30, |_, _| [1, 2, 3, 255]);
    let started = Instant::now();
    let outcome = told(&[("NITID_DECODER_HANGS", "1"), ("NITID_DECODE_TIMEOUT_MS", "700")], || {
        dll.thumbnail(&picture, 256)
    });
    assert_eq!(outcome.err().expect("a hung decoder answered").code(), E_FAIL);
    assert!(started.elapsed() < Duration::from_secs(20), "it took {:?}", started.elapsed());
    assert!(dll.can_unload());
}

/// With nothing beside it to ask, it says so. A DLL left behind by a program
/// that is gone is a folder of plain icons.
#[test]
fn without_the_program_there_is_no_thumbnail() {
    let dll = Dll::alone();
    let error = dll
        .thumbnail(&png(30, 30, |_, _| [1, 2, 3, 255]), 256)
        .err()
        .expect("a thumbnail out of nothing");
    assert_eq!(error.code(), E_FAIL);
    assert!(dll.can_unload());
}

/// What COM holds a server to. The tables are written by hand, so nothing but
/// this would notice a wrong one before Explorer's host did.
#[test]
fn the_dll_keeps_coms_rules() {
    let dll = Dll::beside_nitid();
    // SAFETY: COM calls on objects made here, on this thread, with pointers
    // to locals.
    unsafe {
        let mut out = std::ptr::null_mut();
        assert_eq!(
            (dll.get_class_object)(&GUID::from_u128(1), &IClassFactory::IID, &mut out),
            CLASS_E_CLASSNOTAVAILABLE
        );
        assert!(out.is_null());
        assert_eq!((dll.get_class_object)(&dll.class, &IThumbnailProvider::IID, &mut out), E_NOINTERFACE);

        assert!(dll.can_unload());
        let factory = dll.factory();
        assert!(!dll.can_unload(), "the factory is out and the DLL would be unloaded");

        let aggregated = factory.CreateInstance::<_, IUnknown>(&factory);
        assert_eq!(aggregated.err().expect("it was aggregated").code(), CLASS_E_NOAGGREGATION);

        let provider: IThumbnailProvider = factory.CreateInstance(None).expect("a provider");
        let initialize: IInitializeWithStream = provider.cast().expect("its other interface");
        assert_eq!(
            provider.cast::<IUnknown>().expect("its identity").as_raw(),
            initialize.cast::<IUnknown>().expect("its identity").as_raw(),
            "the two interfaces are two objects"
        );
        assert_eq!(provider.cast::<IClassFactory>().err().expect("a factory").code(), E_NOINTERFACE);

        assert_eq!(ask(&provider, 256).err().expect("it had no picture").code(), E_UNEXPECTED);
        let stream = stream_of(&png(40, 20, |_, _| [9, 8, 7, 255]));
        initialize.Initialize(&stream, 0).expect("a stream");
        let again = initialize.Initialize(&stream, 0);
        assert_eq!(again.err().expect("it took a second one").code(), ERROR_ALREADY_INITIALIZED.to_hresult());

        // Asked twice it answers twice, whoever moved the stream meanwhile.
        for _ in 0..2 {
            let bitmap = ask(&provider, 256).expect("a thumbnail");
            assert_eq!((bitmap.width, bitmap.height), (40, 20));
        }

        // One interface outlives the other, and the factory.
        drop(factory);
        drop(initialize);
        assert!(ask(&provider, 10).is_ok(), "the object went with its first interface");
        assert!(!dll.can_unload());
        drop(provider);
        assert!(dll.can_unload(), "something was left out");
        factory_locks(&dll);
    }
}

fn factory_locks(dll: &Dll) {
    let factory = dll.factory();
    // SAFETY: COM calls on an object made here.
    unsafe {
        factory.LockServer(true).expect("locking");
        drop(factory);
        assert!(!dll.can_unload(), "locked, it would be unloaded");
        dll.factory().LockServer(false).expect("unlocking");
    }
    assert!(dll.can_unload());
}

/// The program, asked the way the DLL asks it.
fn broker(picture: &[u8], variables: &[(&str, &str)]) -> Output {
    let mut child = Command::new(NITID)
        .args(["--decode-stdin", "--thumb", "64"])
        .envs(variables.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("starting nitid");
    // A program that has gone already is a failure the output will show.
    let _ = child.stdin.take().expect("its input").write_all(picture);
    child.wait_with_output().expect("its answer")
}

/// The sandbox is the only place the picture is read. Two halves of that: the
/// process that is given the picture is the confined one, and when there is no
/// such process there is no thumbnail - the one that set it up does not do the
/// reading instead, of any format.
#[test]
fn the_picture_is_read_in_the_sandbox_and_nowhere_else() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let picture = png(30, 30, |_, _| [1, 2, 3, 255]);

    let answered = broker(&picture, &[]);
    assert!(answered.status.success(), "{}", String::from_utf8_lossy(&answered.stderr));
    assert!(answered.stdout.starts_with(b"NTTH"));

    // A decoder that says where it is instead of decoding.
    let probed = broker(&picture, &[("NITID_DECODER_PROBES_NETWORK", "127.0.0.1:9")]);
    let said = String::from_utf8_lossy(&probed.stderr);
    assert!(said.contains("container=true"), "the decoder is not in the container: {said}");
    assert!(probed.stdout.is_empty());

    // A decoder that is not one. `where.exe` starts, and knows nothing of this.
    let system = PathBuf::from(std::env::var_os("SystemRoot").expect("a Windows")).join(r"System32\where.exe");
    let elsewhere = broker(&picture, &[("NITID_DECODER", system.to_str().expect("a path"))]);
    assert!(!elsewhere.status.success(), "a thumbnail was made without the decoder");
    assert!(elsewhere.stdout.is_empty(), "something was answered without the decoder");
}

/// A machine of the test's own, as far as installing is concerned.
struct Machine {
    dir: TempDir,
    tag: String,
}

const HANDLER: &str = "{E357FCCD-A995-4576-B01F-234630154E96}";
const ANOTHERS: &str = "{0D0D0D0D-0000-4000-8000-000000000001}";
const GONE: &str = "{0D0D0D0D-0000-4000-8000-000000000002}";

impl Machine {
    fn new(name: &str) -> Self {
        Self {
            dir: tempfile::tempdir().expect("a scratch directory"),
            tag: format!("thumbs{}{name}", std::process::id()),
        }
    }

    /// `nitid install` or `nitid uninstall`, on a machine where Windows draws
    /// what `windows_draws` says.
    fn run(&self, command: &str, windows_draws: &str) {
        let mut nitid = Command::new(NITID);
        let output = common::keep_data_in(&mut nitid, self.dir.path())
            .arg(command)
            .env("NITID_INSTANCE_ID", &self.tag)
            .env("NITID_UPDATE_FEED", "http://127.0.0.1:9")
            .env("NITID_WINDOWS_DRAWS", windows_draws)
            .stdin(Stdio::null())
            .output()
            .expect("running nitid");
        assert!(output.status.success(), "{command} failed: {}", String::from_utf8_lossy(&output.stderr));
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("thumbnails"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn classes(&self) -> String {
        format!(r"Software\lacodda\nitid-test\{}\Software\Classes", self.tag)
    }

    fn value(&self, path: &str, name: &str) -> Option<String> {
        CURRENT_USER
            .open(format!(r"{}\{path}", self.classes()))
            .and_then(|key| key.get_string(name))
            .ok()
    }

    fn handler(&self, extension: &str) -> Option<String> {
        self.value(&format!(r".{extension}\ShellEx\{HANDLER}"), "")
    }

    fn plant(&self, path: &str, value: &str) {
        CURRENT_USER
            .create(format!(r"{}\{path}", self.classes()))
            .and_then(|key| key.set_string("", value))
            .expect("planting a value");
    }

    fn dll(&self) -> PathBuf {
        self.dir.path().join(r"Programs\nitid\nitid_thumbs.dll")
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        // Stops the resident the install started, whatever the test came to.
        let mut nitid = Command::new(NITID);
        let _ = common::keep_data_in(&mut nitid, self.dir.path())
            .arg("uninstall")
            .env("NITID_INSTANCE_ID", &self.tag)
            .stdin(Stdio::null())
            .output();
        let _ = CURRENT_USER.remove_tree(format!(r"Software\lacodda\nitid-test\{}", self.tag));
    }
}

fn ours() -> String {
    let (_, _, class) = nitid::testing::thumbnail_provider();
    format!("{{{:?}}}", GUID::from_u128(class))
}

/// The formats Windows does not draw, and only those; another program's
/// handler stays another program's; and all of it follows the machine, and
/// goes when nitid does.
#[test]
fn explorer_comes_to_nitid_only_for_what_nobody_else_draws() {
    let machine = Machine::new("only");
    // A handler of a program that is there, and one of a program that is not
    // any more.
    machine.plant(&format!(r".svg\ShellEx\{HANDLER}"), ANOTHERS);
    machine.plant(&format!(r"CLSID\{ANOTHERS}"), "Another program's thumbnails");
    machine.plant(&format!(r".heif\ShellEx\{HANDLER}"), GONE);

    machine.run("install", "avif");
    let (_, bytes, _) = nitid::testing::thumbnail_provider();
    assert!(
        std::fs::read(machine.dll()).is_ok_and(|there| there == bytes),
        "the DLL is not where it was registered"
    );
    let server = format!(r"CLSID\{}\InprocServer32", ours());
    assert_eq!(machine.value(&server, "").map(PathBuf::from), Some(machine.dll()));
    assert_eq!(machine.value(&server, "ThreadingModel").as_deref(), Some("Apartment"));
    for extension in ["jxl", "heic", "heif", "hif"] {
        assert_eq!(machine.handler(extension), Some(ours()), "{extension}");
    }
    assert_eq!(machine.handler("avif"), None, "Windows draws it here");
    assert_eq!(machine.handler("svg").as_deref(), Some(ANOTHERS), "another program draws it");
    for extension in ["jpg", "png", "webp", "gif", "bmp", "tif"] {
        assert_eq!(machine.handler(extension), None, "{extension} was taken over");
    }

    // The machine gains one codec and loses another.
    machine.run("install", "jxl");
    assert_eq!(machine.handler("jxl"), None, "Windows draws it now and it was not given back");
    assert_eq!(machine.handler("avif"), Some(ours()));
    assert!(
        machine.value(r".jxl\OpenWithProgids", "lacodda.nitid.image").is_some(),
        "giving the thumbnails back took more with it"
    );

    machine.run("uninstall", "jxl");
    for extension in ["jxl", "avif", "heic", "heif", "hif"] {
        assert_eq!(machine.handler(extension), None, "{extension} was left behind");
    }
    assert_eq!(machine.handler("svg").as_deref(), Some(ANOTHERS), "uninstalling took another program's handler");
    assert_eq!(machine.value(&server, ""), None, "the class was left behind");
    assert!(!machine.dll().exists(), "the DLL was left behind");
}

/// A DLL in use - a host had a folder open a moment ago - stops neither an
/// install nor an uninstall.
#[test]
fn a_dll_in_use_is_stepped_around() {
    let machine = Machine::new("held");
    machine.run("install", "");

    // The one before, held the way a host holds one.
    let held = |path: &Path| {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: the name is terminated; the module is freed below.
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        assert!(!module.is_null(), "{}", std::io::Error::last_os_error());
        module
    };
    let (_, bytes, _) = nitid::testing::thumbnail_provider();
    let mut older = bytes.to_vec();
    older.extend_from_slice(b"an older build");
    std::fs::write(machine.dll(), &older).expect("an older DLL");
    let older = held(&machine.dll());
    machine.run("install", "");
    assert!(
        std::fs::read(machine.dll()).is_ok_and(|there| there == bytes),
        "the DLL in use kept the new one out"
    );

    let current = held(&machine.dll());
    machine.run("uninstall", "");
    assert!(!machine.dll().exists(), "the DLL is still under its name");
    // SAFETY: both were loaded above and nothing of them is used.
    unsafe {
        FreeLibrary(older);
        FreeLibrary(current);
    }
}

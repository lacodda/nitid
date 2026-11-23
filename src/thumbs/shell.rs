//! Putting the thumbnail DLL where Windows finds it, and taking it away.
//!
//! **Only where nobody else draws the format.** Each extension is left to
//! whichever other program has registered a handler for it, and to Windows
//! when Windows can decode the format. A registered handler does not mean it
//! can: Windows lists its own for `.jxl` and `.heic` on machines that have no
//! codec for either, and draws nothing there. So Windows is asked the only
//! question that answers itself - here is a picture, decode it.
//!
//! Asked at every install and every update, so a machine that has gained a
//! codec since gets its own drawing back at the next one, and a machine that
//! has lost one gets nitid's.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use windows_registry::{Key, LOCAL_MACHINE};

use super::{samples, wire};
use crate::format::Format;
use crate::install::{self, Writer};

/// The DLL's name, beside the executables.
pub const FILE: &str = "nitid_thumbs.dll";

/// The DLL itself, as `build.rs` compiled it.
pub const PROVIDER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/nitid_thumbs.dll"));

/// `IThumbnailProvider`, the name a type's thumbnail handler is listed under.
const HANDLER: &str = "{E357FCCD-A995-4576-B01F-234630154E96}";

/// Windows' own handler for pictures. It draws whatever the machine has codecs
/// for, and is listed for formats it has none for.
const WINDOWS_OWN: &str = "{C7657C4A-9F68-40FA-A4DF-96BC08EB3551}";

/// Put the DLL in `dir` and register it. Returns the extensions whose
/// thumbnails nitid now draws.
pub fn install(dir: &Path) -> Result<Vec<&'static str>> {
    place(dir)?;
    register(&dir.join(FILE))
}

/// Write the DLL, unless it is there already.
///
/// The one before it may be loaded in Explorer's host, where it can be renamed
/// and not replaced - the arrangement the executables have, and the same sweep
/// clears up after it.
fn place(dir: &Path) -> Result<()> {
    let target = dir.join(FILE);
    if fs::read(&target).is_ok_and(|there| there == PROVIDER) {
        return Ok(());
    }
    let new = dir.join(format!("{FILE}.new"));
    fs::write(&new, PROVIDER).with_context(|| format!("writing {}", new.display()))?;
    if target.exists() {
        install::retire(&target)?;
    }
    fs::rename(&new, &target).with_context(|| format!("putting {} in place", target.display()))
}

/// Take the registration away, and the DLL.
///
/// Every extension nitid opens rather than the ones it draws today: what an
/// earlier version registered goes too.
pub fn uninstall(dir: &Path) -> Result<()> {
    let hive = install::hive()?;
    for extension in crate::image_source::supported_extensions() {
        give_back(&hive, extension)?;
    }
    let class = format!(r"Software\Classes\CLSID\{}", wire::class_text());
    if hive.open(&class).is_ok() {
        hive.remove_tree(&class).context("removing the thumbnail provider's class")?;
    }
    install::announce_association_change();

    let target = dir.join(FILE);
    if target.exists() && fs::remove_file(&target).is_err() {
        // Explorer's host still has it open. Renamed, it answers to nothing,
        // and it is gone with the directory once the host is.
        install::retire(&target)?;
    }
    Ok(())
}

fn register(dll: &Path) -> Result<Vec<&'static str>> {
    let hive = install::hive()?;
    let mut writer = Writer::new()?;

    let class = wire::class_text();
    let key = format!(r"Software\Classes\CLSID\{class}");
    writer.set(&key, "", "nitid thumbnails")?;
    let server = format!(r"{key}\InprocServer32");
    writer.set(&server, "", &dll.to_string_lossy())?;
    writer.set(&server, "ThreadingModel", "Apartment")?;

    let mut drawn = Vec::new();
    let mut changed = false;
    for format in Format::ALL.iter().filter(|format| format.explorer_may_not_draw()) {
        let windows = windows_draws(*format);
        for extension in format.extensions() {
            if windows || others_draw(&hive, extension) {
                changed |= give_back(&hive, extension)?;
            } else {
                writer.set(&handler_key(&format!(".{extension}")), "", &class)?;
                drawn.push(*extension);
            }
        }
    }

    if writer.changed || changed {
        install::announce_association_change();
    }
    Ok(drawn)
}

/// Where a type's thumbnail handler is listed.
fn handler_key(kind: &str) -> String {
    format!(r"Software\Classes\{kind}\ShellEx\{HANDLER}")
}

/// Let go of an extension, if it is nitid that holds it.
///
/// Another program's handler in the same place is that program's.
fn give_back(hive: &Key, extension: &str) -> Result<bool> {
    let path = handler_key(&format!(".{extension}"));
    let ours = hive
        .open(&path)
        .and_then(|key| key.get_string(""))
        .is_ok_and(|class| class.eq_ignore_ascii_case(&wire::class_text()));
    if !ours {
        return Ok(false);
    }
    hive.remove_tree(&path).with_context(|| format!("removing {path}"))?;
    // The key it was in, when it was the only thing there.
    let parent = format!(r"Software\Classes\.{extension}\ShellEx");
    let empty = hive.open(&parent).is_ok_and(|key| {
        let keys = key.keys().map(|mut keys| keys.next().is_none()).unwrap_or(false);
        let values = key.values().map(|mut values| values.next().is_none()).unwrap_or(false);
        keys && values
    });
    if empty {
        let _ = hive.remove_tree(&parent);
    }
    Ok(true)
}

/// Whether another program draws this extension's thumbnails.
///
/// Looked for in all four places a handler for an extension can be: under the
/// extension and under the system's list for it, this person's and the
/// machine's. Not only the first that answers - nitid's own entry would answer
/// for a program installed after it. A handler of a program that has since been
/// uninstalled is listed and draws nothing, so its class has to exist.
fn others_draw(hive: &Key, extension: &str) -> bool {
    let kinds = [format!(".{extension}"), format!(r"SystemFileAssociations\.{extension}")];
    [hive, LOCAL_MACHINE]
        .iter()
        .flat_map(|root| {
            kinds
                .iter()
                .filter_map(|kind| root.open(handler_key(kind)).and_then(|key| key.get_string("")).ok())
        })
        .any(|class| {
            let known = [wire::class_text().as_str(), WINDOWS_OWN].iter().any(|known| class.eq_ignore_ascii_case(known));
            !known && class_exists(hive, &class)
        })
}

fn class_exists(hive: &Key, class: &str) -> bool {
    let path = format!(r"Software\Classes\CLSID\{class}");
    hive.open(&path).is_ok() || LOCAL_MACHINE.open(&path).is_ok()
}

/// Whether Windows can decode this format on this machine.
///
/// A format there is no sample of is one Windows has no decoder for.
fn windows_draws(format: Format) -> bool {
    // The lever the tests pull, so that what they hold does not depend on
    // which codecs the machine running them has. Nothing in normal use sets
    // this.
    if let Ok(said) = std::env::var("NITID_WINDOWS_DRAWS") {
        return said.split(',').any(|extension| format.extensions().contains(&extension.trim()));
    }
    samples::of(format).is_some_and(|sample| decodes(&sample).is_ok())
}

/// Have Windows decode a picture, the way its own handler would: every pixel,
/// because some codecs are found missing only when the first one is asked for.
fn decodes(picture: &[u8]) -> Result<()> {
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory, WICConvertBitmapSource, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};

    let _com = crate::com::Com::begin();
    // SAFETY: COM calls on objects made here, on this thread; the stream reads
    // from `picture`, which outlives it, and the buffer is the size the copy
    // is told.
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).context("reaching WIC")?;
        let stream = wic.CreateStream()?;
        stream.InitializeFromMemory(picture)?;
        let decoder = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let (mut width, mut height) = (0u32, 0u32);
        frame.GetSize(&mut width, &mut height)?;
        let pixels = WICConvertBitmapSource(&GUID_WICPixelFormat32bppBGRA, &frame)?;
        let mut out = vec![0u8; width as usize * height as usize * 4];
        pixels.CopyPixels(std::ptr::null(), width * 4, &mut out)?;
    }
    Ok(())
}

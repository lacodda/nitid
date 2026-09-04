//! The window's own icon.
//!
//! The executable carries the mark as a Windows resource (`build.rs`), which
//! is what Explorer and a pinned shortcut show. A window does not inherit it:
//! `winit` creates its class without one, so the titlebar and the taskbar
//! button fall back to the system default — a grey-green placeholder that is
//! nobody's brand. So the same `.ico` is embedded a second time, as bytes, and
//! the window is told about it explicitly.
//!
//! Reading it out of the `.ico` rather than keeping separate PNGs beside it is
//! deliberate: two files that must agree are two files that can disagree, and
//! the one that shows up wrong is the one nobody looks at. There is one source
//! of the mark here, and the sizes are pulled from it.
//!
//! Two sizes are pulled, because Windows asks for two. The titlebar and the
//! Alt+Tab list use the small icon, the taskbar button and the task switcher
//! the large one; handing the same 256px image to both leaves the titlebar
//! downscaling a full mark into sixteen pixels of mush.

/// The mark, as the linker also gets it.
const ICO: &[u8] = include_bytes!("../assets/icon.ico");

/// What the titlebar and the Alt+Tab list draw.
///
/// 32 rather than 16: Windows scales this one down when it needs 16, and a
/// display at 150% asks for 24. Starting from the larger of the small sizes
/// leaves something to scale from.
const SMALL: u32 = 32;

/// What the taskbar button and the task switcher draw.
///
/// Windows-only: it is the only platform that asks for a second, larger icon
/// (`ICON_BIG`) alongside the titlebar's. Elsewhere `with_window_icon` is the
/// whole story, and a constant nothing reads is dead code.
#[cfg(windows)]
const LARGE: u32 = 256;

/// One image inside an `.ico`, as the directory describes it.
///
/// The container is a header, a table of these, and the payloads. Both fields
/// are what the caller needs; the rest of each entry (planes, bit depth, the
/// palette count) says nothing a PNG payload does not already say.
#[derive(Debug, PartialEq)]
pub struct Entry {
    pub size: u32,
    pub offset: usize,
    pub length: usize,
}

/// Read the directory of an `.ico`.
///
/// Its own function so the arithmetic can be tested without a window: an
/// off-by-one in the entry stride reads a valid-looking but wrong slice, and
/// the failure that follows is "the icon looks odd", which nobody debugs.
///
/// A malformed container gives an empty list rather than an error. This one is
/// compiled in, so a failure here means the build embedded something broken —
/// worth a blank icon rather than a panic in front of the picture.
pub fn entries(ico: &[u8]) -> Vec<Entry> {
    // Header: reserved (2), type (2), count (2). Then 16 bytes per entry.
    let Some(count) = ico.get(4..6).map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]) as usize) else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let at = 6 + index * 16;
        let Some(entry) = ico.get(at..at + 16) else {
            break;
        };
        // A zero width means 256: the field is one byte and 256 does not fit.
        let size = if entry[0] == 0 { 256 } else { entry[0] as u32 };
        let length = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as usize;
        let offset = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as usize;
        if ico.len() < offset + length {
            continue;
        }
        out.push(Entry { size, offset, length });
    }
    out
}

/// The payload of the image closest to `wanted`, if the container has one.
///
/// Closest rather than exact: the `.ico` is built by the asset exporter and
/// its list of sizes is that script's decision, not this module's. Asking for
/// an exact match would leave the window bare the day the list changes.
pub fn image_near(ico: &[u8], wanted: u32) -> Option<&[u8]> {
    let entries = entries(ico);
    let best = entries.iter().min_by_key(|entry| entry.size.abs_diff(wanted))?;
    ico.get(best.offset..best.offset + best.length)
}

/// Decode one of the embedded images into an icon `winit` can take.
fn icon_at(wanted: u32) -> Option<winit::window::Icon> {
    let payload = image_near(ICO, wanted)?;
    // The payloads are PNG, which this build already decodes for pictures.
    let decoded = image::load_from_memory_with_format(payload, image::ImageFormat::Png).ok()?;
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    winit::window::Icon::from_rgba(rgba.into_raw(), width, height).ok()
}

/// The icon for the titlebar and the Alt+Tab list.
pub fn small() -> Option<winit::window::Icon> {
    icon_at(SMALL)
}

/// The icon for the taskbar button.
///
/// Windows-only, like the attribute that takes it: see `LARGE`.
#[cfg(windows)]
pub fn large() -> Option<winit::window::Icon> {
    icon_at(LARGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded container is readable at all. Everything else here rests
    /// on this, and it is the one thing a bad build would break silently.
    #[test]
    fn the_embedded_icon_has_images() {
        let entries = entries(ICO);
        assert!(!entries.is_empty(), "the embedded .ico gave no images");
        for entry in &entries {
            assert!(entry.length > 0, "an image of {}px is empty", entry.size);
            assert!(ICO.len() >= entry.offset + entry.length, "an image of {}px runs past the file", entry.size);
        }
    }

    /// The two sizes the window asks for have to be there, and they have to be
    /// different images — handing the same picture to both is the smeared
    /// titlebar this module exists to avoid.
    #[test]
    fn the_window_gets_two_different_sizes() {
        let small = image_near(ICO, SMALL).expect("no small image");
        // 256 spelled out rather than `LARGE`, which only exists on Windows:
        // the two images being different is a fact about the container and
        // holds wherever the container is read.
        let large = image_near(ICO, 256).expect("no large image");
        assert_ne!(small, large, "the titlebar and the taskbar were handed the same image");
        assert!(large.len() > small.len(), "the large icon is not the larger image");
    }

    /// Asking for a size returns the nearest one rather than nothing, so a
    /// change to the exporter's list cannot leave the window bare.
    #[test]
    fn the_nearest_size_is_taken() {
        let sizes: Vec<u32> = entries(ICO).iter().map(|entry| entry.size).collect();
        assert!(sizes.contains(&256), "the 256px image is missing: {sizes:?}");

        // A size nothing matches exactly still gives the closest image.
        let odd = image_near(ICO, 200).expect("nothing came back for 200px");
        let exact = image_near(ICO, 256).expect("nothing came back for 256px");
        assert_eq!(odd, exact, "200px did not resolve to the nearest image");
    }

    /// A container that is not one is answered with nothing rather than a
    /// panic in front of the photograph.
    #[test]
    fn a_broken_container_is_empty_rather_than_a_panic() {
        assert!(entries(&[]).is_empty());
        assert!(entries(&[0, 0, 1, 0]).is_empty(), "a header with no count was read anyway");
        // A count that lies: two entries promised, none supplied.
        assert!(entries(&[0, 0, 1, 0, 2, 0]).is_empty(), "entries were read past the end of the file");
        // An entry pointing past the end is skipped, not sliced.
        let mut lying = vec![0, 0, 1, 0, 1, 0];
        lying.extend_from_slice(&[32, 32, 0, 0, 1, 0, 32, 0]);
        lying.extend_from_slice(&9999u32.to_le_bytes()); // length
        lying.extend_from_slice(&22u32.to_le_bytes()); // offset
        assert!(entries(&lying).is_empty(), "an image running past the file was accepted");
        assert!(image_near(&lying, 32).is_none());
    }

    /// The level rule of the line, held against the actual pixels.
    ///
    /// This is the test the patch exists for. The exporter used to take the S
    /// tile — a hexagon filled with the brand colour — for *every* size, so a
    /// 256px icon was a flat cyan blob and the taskbar showed a coloured
    /// lozenge instead of the mark. Nothing held it, and it shipped that way
    /// for a month until the owner noticed.
    ///
    /// The rule is: S at 27px and below (the filled tile is all that survives
    /// there), the plated mark above it. The plate is near-black, the filled
    /// tile is the brand colour, so one pixel inside the hexagon and away from
    /// the code tells them apart — which is exactly what the eye does at a
    /// glance, and what nobody was doing.
    #[test]
    fn every_size_carries_the_level_that_reads_at_it() {
        for entry in entries(ICO) {
            let payload = &ICO[entry.offset..entry.offset + entry.length];
            let decoded = image::load_from_memory_with_format(payload, image::ImageFormat::Png).expect("an embedded image is not a PNG");
            let rgba = decoded.to_rgba8();
            let (width, height) = rgba.dimensions();
            assert_eq!(width, entry.size, "the {}px entry holds a {width}px image", entry.size);

            // A quarter of the way across, vertically centred: inside the
            // hexagon, clear of the code and of the metaphor beneath it.
            let sample = rgba.get_pixel(width / 4, height / 2).0;
            assert!(sample[3] > 40, "the {}px image is transparent where the tile should be", entry.size);
            let brightness = u32::from(sample[0]) + u32::from(sample[1]) + u32::from(sample[2]);
            let filled = brightness > 180;

            if entry.size <= 27 {
                assert!(
                    filled,
                    "the {}px image is not the filled tile; below 28px the outline collapses into noise",
                    entry.size
                );
            } else {
                assert!(
                    !filled,
                    "the {}px image is the filled S tile (sample {sample:?}), not the plated mark — the level rule of the line puts S at 27px and below",
                    entry.size,
                );
            }
        }
    }

    /// Largest first.
    ///
    /// Windows picks by closest size and ignores order, but readers that take
    /// the first entry verbatim exist — kilna's titlebar was stretched from a
    /// 16px entry for exactly this reason. Cheap to hold, expensive to notice.
    #[test]
    fn the_largest_image_comes_first() {
        let sizes: Vec<u32> = entries(ICO).iter().map(|entry| entry.size).collect();
        let mut sorted = sizes.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(sizes, sorted, "the images are not ordered largest first");
    }

    /// The mark the window shows is the mark the executable carries.
    ///
    /// They come from one file, and this says so: if someone reintroduces
    /// separate PNGs beside the `.ico`, the two can drift and the window ends
    /// up showing a mark the shell does not.
    #[test]
    fn the_window_and_the_executable_share_one_source() {
        let on_disk = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon.ico")).expect("assets/icon.ico is missing");
        assert_eq!(ICO, on_disk.as_slice(), "the embedded icon is not the file build.rs gives the linker");
    }
}

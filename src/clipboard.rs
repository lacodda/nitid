//! The Windows clipboard, for pictures and for paths.
//!
//! `egui` carries text and nothing else, which is enough for the Info panel's
//! copyable rows and not for a viewer: the thing a person most wants to take
//! out of an image viewer is the image. So this talks to the clipboard
//! directly, in the format every Windows application understands.
//!
//! The bitmap that goes out carries the file's own pixels, unconverted — the
//! decision ADR 0019 records, applied here: the numbers on the clipboard are
//! the numbers the eyedropper reports and the histogram counts. The clipboard
//! has no way to say what a number means (there is no profile on a `CF_DIB`),
//! so a wide-gamut picture pasted into an application that assumes sRGB will
//! look flatter there than it does here. That is a true statement about the
//! clipboard rather than something a viewer can fix by quietly rewriting the
//! pixels on the way out.
//!
//! The conversions between a decoded image and a DIB are plain arithmetic and
//! live apart from the Windows calls, so the parts that are easy to get wrong
//! — rows running bottom-up, each padded to four bytes — are tested without a
//! clipboard.

use crate::image_source::{DecodedImage, Depth};

/// The header of a `CF_DIB`, which is a `BITMAPINFOHEADER` followed by pixels.
///
/// Written out rather than taken from the `windows` crate so the arithmetic
/// below can be tested on any platform, and so the layout this module depends
/// on is stated where it is used.
const HEADER_SIZE: usize = 40;

/// How wide a DIB row is once padded.
///
/// Every row of a DIB starts on a four-byte boundary. At 24 bits per pixel a
/// row of three pixels is nine bytes and pads to twelve — the single most
/// common way to get a bitmap subtly sheared.
fn row_stride(width: u32) -> usize {
    (width as usize * 3).next_multiple_of(4)
}

/// Turn a decoded image into a `CF_DIB` payload.
///
/// Twenty-four bits, bottom-up, which is the form every Windows application
/// reads without argument. Alpha is dropped: `CF_DIB` has no dependable way to
/// carry it — applications disagree about whether the fourth byte means
/// anything — so a transparent pixel is composited onto white here rather than
/// pasted as whatever the receiving application decides black-or-garbage means.
pub fn to_dib(image: &DecodedImage) -> Vec<u8> {
    let width = image.width.max(1);
    let height = image.height.max(1);
    let stride = row_stride(width);
    let mut out = vec![0u8; HEADER_SIZE + stride * height as usize];

    // BITMAPINFOHEADER.
    out[0..4].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
    out[4..8].copy_from_slice(&(width as i32).to_le_bytes());
    // Positive height means bottom-up, which is the DIB default and what the
    // row order below writes.
    out[8..12].copy_from_slice(&(height as i32).to_le_bytes());
    out[12..14].copy_from_slice(&1u16.to_le_bytes()); // planes
    out[14..16].copy_from_slice(&24u16.to_le_bytes()); // bits per pixel
    // biCompression BI_RGB = 0, biSizeImage may be zero for BI_RGB, and the
    // resolution and palette fields are zero. Left as the zeros above.

    let bytes_per_pixel = 4 * image.depth.bytes() as usize;
    for y in 0..height as usize {
        // Bottom-up: the first row written is the image's last.
        let source_row = height as usize - 1 - y;
        let row_start = HEADER_SIZE + y * stride;

        for x in 0..width as usize {
            let index = source_row * width as usize + x;
            let offset = index * bytes_per_pixel;
            let Some(pixel) = image.pixels.get(offset..offset + bytes_per_pixel) else {
                // A truncated image is drawn as far as it goes rather than
                // refused: the picture on screen is whatever decoded, and the
                // clipboard should carry the same thing.
                continue;
            };

            let (r, g, b, a) = match image.depth {
                Depth::Eight => (pixel[0], pixel[1], pixel[2], pixel[3]),
                Depth::Sixteen => {
                    let sample = |channel: usize| u16::from_ne_bytes([pixel[channel * 2], pixel[channel * 2 + 1]]);
                    ((sample(0) >> 8) as u8, (sample(1) >> 8) as u8, (sample(2) >> 8) as u8, (sample(3) >> 8) as u8)
                }
            };

            // Composited onto white: a cut-out pasted into a document is
            // nearly always going onto a white page, and white is the one
            // background that does not turn a soft edge into a dark halo.
            let over_white = |channel: u8| {
                let alpha = u32::from(a);
                ((u32::from(channel) * alpha + 255 * (255 - alpha)) / 255) as u8
            };

            // A DIB row is blue, green, red.
            let at = row_start + x * 3;
            out[at] = over_white(b);
            out[at + 1] = over_white(g);
            out[at + 2] = over_white(r);
        }
    }

    out
}

/// Read a `CF_DIB` payload back into a decoded image.
///
/// Handles the two header sizes in circulation — the 40-byte
/// `BITMAPINFOHEADER` and the 124-byte `BITMAPV5HEADER` that applications
/// which care about alpha write — and both row orders. Anything else is
/// declined rather than guessed at: a wrong guess here is a sheared or
/// upside-down picture presented as though it were the real thing.
pub fn from_dib(bytes: &[u8]) -> Option<DecodedImage> {
    let header_size = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
    // 40 is `BITMAPINFOHEADER`; 108 and 124 are the V4 and V5 headers, whose
    // first forty bytes are the same fields in the same places.
    if !matches!(header_size, 40 | 108 | 124) {
        return None;
    }

    let width = i32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?);
    let raw_height = i32::from_le_bytes(bytes.get(8..12)?.try_into().ok()?);
    let bits = u16::from_le_bytes(bytes.get(14..16)?.try_into().ok()?);
    let compression = u32::from_le_bytes(bytes.get(16..20)?.try_into().ok()?);

    // Only uncompressed bitmaps. `BI_BITFIELDS` (3) is common at 32 bits and
    // carries channel masks that would have to be honoured; declining is
    // better than reading it as if the masks were the usual ones.
    if compression != 0 {
        return None;
    }
    if width <= 0 || raw_height == 0 {
        return None;
    }
    if !matches!(bits, 24 | 32) {
        return None;
    }

    let width = width as u32;
    // A negative height means the rows are stored top-down.
    let top_down = raw_height < 0;
    let height = raw_height.unsigned_abs();

    // A palette would sit between the header and the pixels; at 24 and 32 bits
    // there is none, so the pixels start immediately after.
    let pixels_start = header_size;
    let stride = match bits {
        24 => row_stride(width),
        _ => width as usize * 4,
    };

    let mut out = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        // The row of the *image* this stored row belongs to.
        let target_row = if top_down { y } else { height as usize - 1 - y };
        let row_start = pixels_start + y * stride;

        for x in 0..width as usize {
            let at = row_start + x * (bits as usize / 8);
            let pixel = bytes.get(at..at + bits as usize / 8)?;
            let out_at = (target_row * width as usize + x) * 4;

            // Stored blue, green, red — and at 32 bits a fourth byte that may
            // be alpha or may be padding. It is taken as opaque either way:
            // an application that wrote zeros there meant padding, and reading
            // them as alpha would make the whole picture invisible.
            out[out_at] = pixel[2];
            out[out_at + 1] = pixel[1];
            out[out_at + 2] = pixel[0];
            out[out_at + 3] = 255;
        }
    }

    Some(DecodedImage {
        width,
        height,
        pixels: out,
        depth: Depth::Eight,
    })
}

/// A path as a terminal would want it: quoted when it needs to be.
///
/// A path with a space in it, pasted unquoted into a shell, is two arguments
/// and an error message. Quoting only when necessary keeps the common case
/// clean — most paths need nothing, and quotes a person has to delete are as
/// much of a nuisance as quotes they have to add.
pub fn quote_for_shell(path: &str) -> String {
    // The quote itself belongs in this list: a path containing one is not
    // safe to paste bare, and leaving it out was caught by the test below
    // rather than by reading the list.
    let needs_quotes = path.is_empty() || path.contains([' ', '\t', '&', '(', ')', ';', ',', '^', '=', '\'', '`', '"']);
    if needs_quotes {
        // Doubled inner quotes, which is how both `cmd` and PowerShell read a
        // literal quote inside a quoted string.
        format!("\"{}\"", path.replace('"', "\"\""))
    } else {
        path.to_string()
    }
}

#[cfg(windows)]
mod windows_clipboard {
    use anyhow::{Context, Result, bail};
    use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GHND, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5};

    /// The clipboard, opened and closed as one thing.
    ///
    /// Windows lets exactly one process hold the clipboard at a time, and a
    /// process that forgets to close it locks it for everyone until it exits.
    /// A guard makes that impossible to forget on an error path.
    struct Clipboard;

    impl Clipboard {
        fn open() -> Result<Self> {
            // The clipboard is often held for a moment by whichever
            // application the user just copied from, so a single failure is
            // not a real one. A few tries over a few tens of milliseconds is
            // what every clipboard-using application does.
            let mut last = None;
            for attempt in 0..8 {
                match unsafe { OpenClipboard(Some(HWND::default())) } {
                    Ok(()) => return Ok(Self),
                    Err(error) => {
                        last = Some(error);
                        std::thread::sleep(std::time::Duration::from_millis(5 * (attempt + 1)));
                    }
                }
            }
            Err(last.expect("the loop ran at least once")).context("opening the clipboard")
        }
    }

    impl Drop for Clipboard {
        fn drop(&mut self) {
            let _ = unsafe { CloseClipboard() };
        }
    }

    /// Put a `CF_DIB` payload on the clipboard.
    pub fn set_dib(payload: &[u8]) -> Result<()> {
        let clipboard = Clipboard::open()?;
        unsafe { EmptyClipboard() }.context("emptying the clipboard")?;

        // The clipboard takes ownership of this block, so it is deliberately
        // not freed here: freeing it would leave the clipboard pointing at
        // memory that no longer exists.
        let handle = unsafe { GlobalAlloc(GHND, payload.len()) }.context("allocating for the clipboard")?;
        let pointer = unsafe { GlobalLock(handle) };
        if pointer.is_null() {
            bail!("locking the clipboard's memory");
        }
        unsafe {
            std::ptr::copy_nonoverlapping(payload.as_ptr(), pointer.cast::<u8>(), payload.len());
            let _ = GlobalUnlock(handle);
        }

        unsafe { SetClipboardData(CF_DIB.0.into(), Some(HANDLE(handle.0))) }.context("writing to the clipboard")?;
        // Ownership passed to the clipboard; dropping the guard closes it.
        drop(clipboard);
        Ok(())
    }

    /// Take a bitmap off the clipboard, if there is one.
    ///
    /// `None` rather than an error when the clipboard holds something else —
    /// pasting when there is no picture is a thing that did not happen, not a
    /// failure to report.
    pub fn get_dib() -> Result<Option<Vec<u8>>> {
        let available = unsafe { IsClipboardFormatAvailable(CF_DIB.0.into()) }.is_ok() || unsafe { IsClipboardFormatAvailable(CF_DIBV5.0.into()) }.is_ok();
        if !available {
            return Ok(None);
        }

        let _clipboard = Clipboard::open()?;
        // V5 first: an application that wrote both wrote the richer one for a
        // reason, and the reader below understands its header.
        let handle = unsafe { GetClipboardData(CF_DIBV5.0.into()) }.or_else(|_| unsafe { GetClipboardData(CF_DIB.0.into()) });
        let Ok(handle) = handle else {
            return Ok(None);
        };

        let global = HGLOBAL(handle.0);
        let size = unsafe { GlobalSize(global) };
        if size == 0 {
            return Ok(None);
        }

        let pointer = unsafe { GlobalLock(global) };
        if pointer.is_null() {
            bail!("locking the clipboard's memory");
        }
        let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), size) }.to_vec();
        unsafe {
            let _ = GlobalUnlock(global);
        }

        Ok(Some(bytes))
    }
}

#[cfg(windows)]
pub use windows_clipboard::{get_dib, set_dib};

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, pixels: Vec<u8>) -> DecodedImage {
        DecodedImage {
            width,
            height,
            pixels,
            depth: Depth::Eight,
        }
    }

    /// The header says what the pixels are, and every field of it matters to
    /// the application on the other end.
    #[test]
    fn the_header_describes_the_bitmap() {
        let dib = to_dib(&image(3, 2, vec![0; 3 * 2 * 4]));

        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40, "header size");
        assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 3, "width");
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2, "height");
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 24, "bits per pixel");
        assert_eq!(u32::from_le_bytes(dib[16..20].try_into().unwrap()), 0, "compression");
    }

    /// Every row starts on a four-byte boundary. Getting this wrong shears the
    /// picture diagonally, which is the classic DIB bug.
    #[test]
    fn rows_are_padded_to_four_bytes() {
        // Three pixels is nine bytes, which pads to twelve.
        assert_eq!(row_stride(3), 12);
        assert_eq!(row_stride(4), 12, "four pixels is exactly twelve and needs no padding");
        assert_eq!(row_stride(1), 4);

        let dib = to_dib(&image(3, 2, vec![0; 3 * 2 * 4]));
        assert_eq!(dib.len(), 40 + 12 * 2, "the payload is not header plus padded rows");
    }

    /// A DIB is stored bottom-up, so the first row of the payload is the last
    /// row of the picture. Upside-down is the other classic DIB bug.
    #[test]
    fn the_rows_are_written_bottom_up() {
        // Top row red, bottom row blue.
        let mut pixels = Vec::new();
        pixels.extend_from_slice(&[255, 0, 0, 255]);
        pixels.extend_from_slice(&[0, 0, 255, 255]);
        let dib = to_dib(&image(1, 2, pixels));

        // First stored row is the picture's bottom: blue, in BGR.
        assert_eq!(&dib[40..43], &[255, 0, 0], "the first stored row is not the picture's bottom");
        // Second stored row is the top: red.
        assert_eq!(&dib[44..47], &[0, 0, 255], "the second stored row is not the picture's top");
    }

    /// A round trip through the two conversions has to give the picture back.
    /// This is what a person actually does: copy from nitid, paste into nitid.
    #[test]
    fn a_picture_survives_a_round_trip() {
        let mut pixels = Vec::new();
        for y in 0..3u8 {
            for x in 0..5u8 {
                pixels.extend_from_slice(&[x * 40, y * 60, 128, 255]);
            }
        }
        let original = image(5, 3, pixels);

        let back = from_dib(&to_dib(&original)).expect("a bitmap nitid wrote is one it can read");

        assert_eq!((back.width, back.height), (5, 3));
        assert_eq!(back.pixels, original.pixels, "the picture changed on the way through the clipboard");
    }

    /// A top-down bitmap — negative height — is what several applications
    /// write, and reading it as bottom-up turns the picture over.
    #[test]
    fn a_top_down_bitmap_is_read_the_right_way_up() {
        let mut bottom_up = to_dib(&image(1, 2, vec![255, 0, 0, 255, 0, 0, 255, 255]));

        // The same pixels, declared top-down and with the rows reversed.
        let mut top_down = bottom_up.clone();
        top_down[8..12].copy_from_slice(&(-2i32).to_le_bytes());
        let stride = row_stride(1);
        let (first, second) = (bottom_up[40..40 + stride].to_vec(), bottom_up[40 + stride..40 + 2 * stride].to_vec());
        top_down[40..40 + stride].copy_from_slice(&second);
        top_down[40 + stride..40 + 2 * stride].copy_from_slice(&first);

        let from_bottom = from_dib(&bottom_up).expect("bottom-up reads");
        let from_top = from_dib(&top_down).expect("top-down reads");
        assert_eq!(from_bottom.pixels, from_top.pixels, "the two row orders gave different pictures");

        // And the picture is the right way up: the first pixel is the red one.
        assert_eq!(&from_bottom.pixels[0..3], &[255, 0, 0]);
        bottom_up.clear();
    }

    /// A 32-bit bitmap is what most modern applications put on the clipboard.
    #[test]
    fn a_thirty_two_bit_bitmap_is_read() {
        let mut dib = vec![0u8; 40 + 4];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&1i32.to_le_bytes());
        dib[8..12].copy_from_slice(&1i32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        // One pixel, blue-green-red-unused.
        dib[40..44].copy_from_slice(&[10, 20, 30, 0]);

        let image = from_dib(&dib).expect("a 32-bit bitmap reads");
        // The fourth byte is zero, which many applications mean as padding:
        // read as alpha it would make the picture invisible.
        assert_eq!(&image.pixels[0..4], &[30, 20, 10, 255]);
    }

    /// Formats this cannot read are declined rather than guessed at: a wrong
    /// guess is a sheared picture presented as the real thing.
    #[test]
    fn an_unreadable_bitmap_is_declined() {
        let mut dib = to_dib(&image(2, 2, vec![0; 16]));

        let mut compressed = dib.clone();
        compressed[16..20].copy_from_slice(&3u32.to_le_bytes()); // BI_BITFIELDS
        assert!(from_dib(&compressed).is_none(), "a compressed bitmap was read anyway");

        let mut palette = dib.clone();
        palette[14..16].copy_from_slice(&8u16.to_le_bytes());
        assert!(from_dib(&palette).is_none(), "an 8-bit palette bitmap was read as truecolour");

        let mut odd_header = dib.clone();
        odd_header[0..4].copy_from_slice(&12u32.to_le_bytes()); // BITMAPCOREHEADER
        assert!(from_dib(&odd_header).is_none(), "a core-header bitmap was read as a modern one");

        assert!(from_dib(&[]).is_none());
        assert!(from_dib(&dib[..20]).is_none(), "a truncated bitmap was read");
        dib.clear();
    }

    /// Transparency is composited onto white rather than pasted as whatever
    /// the receiving application decides an unset fourth byte means.
    #[test]
    fn a_transparent_pixel_is_composited_onto_white() {
        // Fully transparent black, which is what a cut-out's outside holds.
        let dib = to_dib(&image(1, 1, vec![0, 0, 0, 0]));
        assert_eq!(&dib[40..43], &[255, 255, 255], "a transparent pixel did not come out white");

        // Half-transparent black is half way there.
        let half = to_dib(&image(1, 1, vec![0, 0, 0, 128]));
        for channel in 0..3 {
            assert!(
                (110..=145).contains(&half[40 + channel]),
                "a half-transparent pixel came out at {}",
                half[40 + channel],
            );
        }

        // And an opaque pixel is untouched.
        let opaque = to_dib(&image(1, 1, vec![10, 20, 30, 255]));
        assert_eq!(&opaque[40..43], &[30, 20, 10], "an opaque pixel was altered");
    }

    /// A sixteen-bit file goes out at eight, which is all a DIB carries.
    #[test]
    fn a_sixteen_bit_image_is_narrowed_for_the_clipboard() {
        let sample = 0x8040u16.to_ne_bytes();
        let opaque = 0xffffu16.to_ne_bytes();
        let pixels: Vec<u8> = [sample, sample, sample, opaque].concat();
        let dib = to_dib(&DecodedImage {
            width: 1,
            height: 1,
            pixels,
            depth: Depth::Sixteen,
        });

        assert_eq!(&dib[40..43], &[0x80, 0x80, 0x80], "the top byte is not what reached the clipboard");
    }

    /// A path with a space in it, pasted unquoted into a shell, is two
    /// arguments and an error message.
    #[test]
    fn a_path_is_quoted_only_when_it_needs_to_be() {
        assert_eq!(quote_for_shell(r"C:\photos\a.jpg"), r"C:\photos\a.jpg", "a plain path gained quotes");
        assert_eq!(quote_for_shell(r"C:\my photos\a.jpg"), "\"C:\\my photos\\a.jpg\"");
        // The characters a shell would otherwise act on.
        for awkward in [r"C:\a&b\c.jpg", r"C:\a(1)\c.jpg", r"C:\a;b\c.jpg", r"C:\a'b\c.jpg"] {
            assert!(quote_for_shell(awkward).starts_with('"'), "{awkward} was left unquoted");
        }
    }

    /// A quote inside a path is doubled, which is how both shells read a
    /// literal quote.
    #[test]
    fn a_quote_inside_a_path_is_doubled() {
        assert_eq!(quote_for_shell(r#"C:\a"b\c.jpg"#), r#""C:\a""b\c.jpg""#);
    }

    #[test]
    fn a_degenerate_image_does_not_panic() {
        let dib = to_dib(&image(0, 0, Vec::new()));
        assert!(dib.len() >= 40);
        // A truncated buffer is drawn as far as it goes rather than panicking.
        let short = to_dib(&image(4, 4, vec![0; 8]));
        assert_eq!(short.len(), 40 + row_stride(4) * 4);
    }
}

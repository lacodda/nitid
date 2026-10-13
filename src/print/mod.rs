//! Printing the picture on screen (`Ctrl+P`), through the dialog Windows
//! itself prints with: the one with the preview, the printer, the paper and
//! the orientation that the Photos app opens (ADR 0036).
//!
//! What reaches the paper is what a standard-range screen shows — the promise
//! an export with the colour baked in makes (ADR 0025): the shader's colour
//! path into sRGB, the turn on screen applied, the frame of an animation that
//! is up. sRGB because it is the one space every printer driver is certain to
//! read right; the driver maps it to its inks the way it maps everything else
//! it prints. Transparency lands on white, which is what paper is.
//!
//! The page is laid out here, in numbers a test can hold: where the picture
//! goes, which part of it shows, how many pixels it is sent at. Drawing it and
//! talking to the print system is `dialog`, which is Windows-only.

use anyhow::Result;

use crate::fit::Fit;
use crate::image_source::{DecodedImage, Depth, Orientation};

#[cfg(windows)]
mod dialog;
#[cfg(windows)]
pub use dialog::{show, write_xps};

/// How finely a picture is sent to the printer: three hundred pixels to the
/// inch, the resolution photographic printing is made for. More is spool and
/// time the paper cannot show; a picture with fewer is sent with what it has.
pub const PIXELS_PER_INCH: f64 = 300.0;

/// The print system's unit: a device-independent pixel, a ninety-sixth of an
/// inch.
pub const UNITS_PER_INCH: f64 = 96.0;

/// The longest side the picture is kept at while the dialog is open: three
/// hundred to the inch over twenty inches, more than any page short of a
/// poster asks for. A sixty-megapixel photograph is shrunk to this before
/// anything else is done with it.
pub const LONGEST: u32 = 6000;

/// The longest side of the copy the preview is drawn from.
pub const PREVIEW_LONGEST: u32 = 2048;

/// What was asked to be printed.
pub struct Job {
    /// What the dialog and the print queue call it: the file's name.
    pub title: String,
    pub picture: Picture,
    /// How the picture is turned on screen: the file's own orientation and
    /// the turn asked for on top of it.
    pub orientation: Orientation,
}

/// The picture to print, as the viewer holds it.
pub enum Picture {
    /// Pixels, with the profile that says what their numbers mean: a still,
    /// the frame of an animation that is on screen, or a picture pasted from
    /// the clipboard, which has no profile (ADR 0020).
    Raster {
        image: DecodedImage,
        profile: Option<Box<moxcms::ColorProfile>>,
    },
    /// A vector document, drawn at the size the paper needs.
    Vector(crate::vector::VectorImage),
}

impl Job {
    /// The picture's size as it is shown, upright.
    pub fn shown_size(&self) -> (u32, u32) {
        let stored = match &self.picture {
            Picture::Raster { image, .. } => (image.width, image.height),
            Picture::Vector(vector) => vector.intrinsic_size(),
        };
        if self.orientation.swaps_axes() { (stored.1, stored.0) } else { stored }
    }

    /// Whether the page starts on its side: for a picture wider than it is
    /// tall, which on an upright page would use half the sheet.
    pub fn landscape(&self) -> bool {
        let (width, height) = self.shown_size();
        width > height
    }
}

/// What came of a print, said in a toast.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The pages went to the printer's queue.
    Sent,
    /// The print system took the job and could not finish it.
    Failed,
}

/// The picture made ready for paper: sRGB, upright, on white, and in the
/// byte order the print system draws from — blue, green, red, then an alpha
/// that is always opaque.
pub struct Paper {
    /// For the printer: up to [`LONGEST`] on its long side.
    pub full: DecodedImage,
    /// For the preview: up to [`PREVIEW_LONGEST`].
    pub preview: DecodedImage,
}

impl Paper {
    pub fn new(job: &Job) -> Result<Self> {
        let upright = match &job.picture {
            Picture::Raster { image, profile } => crate::export::upright_srgb(image, profile.as_deref(), job.orientation, LONGEST)?,
            Picture::Vector(vector) => {
                // Drawn at the size kept for the printer, whatever size the
                // document declares: drawing at the size it is needed at is
                // what a vector is for.
                let (width, height) = vector.intrinsic_size();
                let scale = f64::from(LONGEST) / f64::from(width.max(height).max(1));
                let drawn = vector.rasterise(
                    ((f64::from(width) * scale).round() as u32).max(1),
                    ((f64::from(height) * scale).round() as u32).max(1),
                )?;
                // Drawn in sRGB already, so nothing is said about its colour.
                crate::export::upright_srgb(&drawn, None, job.orientation, LONGEST)?
            }
        };
        let full = on_paper(upright);
        let preview = crate::export::shrink(&full, PREVIEW_LONGEST);
        Ok(Self { full, preview })
    }
}

/// Lay transparency onto white and put the channels in blue-green-red order.
///
/// Blended in the stored numbers, the way every program that prints a
/// transparent picture does.
fn on_paper(mut image: DecodedImage) -> DecodedImage {
    debug_assert_eq!(image.depth, Depth::Eight, "paper is made from eight-bit sRGB");
    for pixel in image.pixels.as_chunks_mut::<4>().0 {
        let [red, green, blue, alpha] = *pixel;
        let over = |channel: u8| ((u32::from(channel) * u32::from(alpha) + 255 * (255 - u32::from(alpha)) + 127) / 255) as u8;
        *pixel = [over(blue), over(green), over(red), 255];
    }
    image
}

/// A rectangle: on the page, in the print system's units; within a picture,
/// in fractions of its width and height.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Area {
    /// The whole of a picture.
    pub const WHOLE: Area = Area {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };

    fn right(&self) -> f64 {
        self.x + self.width
    }

    fn bottom(&self) -> f64 {
        self.y + self.height
    }
}

/// A sheet, as the print system describes it for the printer, paper and
/// orientation chosen in the dialog.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Page {
    pub width: f64,
    pub height: f64,
    /// Where the printer can put ink: the sheet less the margins it cannot
    /// reach, or all of it on a printer printing without borders.
    pub printable: Area,
}

impl Page {
    /// The printable area, kept on the sheet; the whole sheet when what the
    /// driver reported holds nothing. A page with no room to print on is a
    /// driver's mistake, not the person's, and printing over its margins is
    /// a better answer to it than printing nothing.
    fn usable(&self) -> Area {
        let left = self.printable.x.clamp(0.0, self.width);
        let top = self.printable.y.clamp(0.0, self.height);
        let right = self.printable.right().clamp(left, self.width);
        let bottom = self.printable.bottom().clamp(top, self.height);
        if right - left <= 0.0 || bottom - top <= 0.0 {
            return Area {
                x: 0.0,
                y: 0.0,
                width: self.width,
                height: self.height,
            };
        }
        Area {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }
}

/// Where the picture goes on a page.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Placement {
    /// Where on the page the picture is drawn: always inside what the
    /// printer can reach.
    pub on_page: Area,
    /// Which part of the picture shows there: all of it when it is whole on
    /// the page, the middle of it when it fills the page.
    pub seen: Area,
}

/// Lay a picture of `picture` pixels out on `page`, centred.
///
/// The picture is scaled to meet the printable area as `fit` says, whatever
/// its pixel count: on paper a picture is as big as the sheet it is printed
/// on, and a small one printed whole on a page is enlarged the way the Photos
/// app enlarges it.
pub fn place(picture: (u32, u32), page: &Page, fit: Fit) -> Placement {
    let printable = page.usable();
    let size = (f64::from(picture.0.max(1)), f64::from(picture.1.max(1)));
    let scale = fit.scale(size, (printable.width, printable.height));
    let drawn = Area {
        x: printable.x + (printable.width - size.0 * scale) / 2.0,
        y: printable.y + (printable.height - size.1 * scale) / 2.0,
        width: size.0 * scale,
        height: size.1 * scale,
    };

    // What of the drawn picture lands inside the printable area — all of it
    // when whole, the printable area itself when filling — and which part of
    // the picture that is.
    let left = drawn.x.max(printable.x);
    let top = drawn.y.max(printable.y);
    let right = drawn.right().min(printable.right());
    let bottom = drawn.bottom().min(printable.bottom());
    let on_page = Area {
        x: left,
        y: top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    };
    let seen = if drawn.width <= 0.0 || drawn.height <= 0.0 {
        Area::WHOLE
    } else {
        Area {
            x: snap((left - drawn.x) / drawn.width),
            y: snap((top - drawn.y) / drawn.height),
            width: snap(on_page.width / drawn.width),
            height: snap(on_page.height / drawn.height),
        }
    };
    Placement { on_page, seen }
}

/// A fraction a hair from an edge of the picture, put on the edge: a whole
/// picture whose height came out at 0.9999999 of itself is the whole picture,
/// and is sent without cutting a row it never meant to lose.
fn snap(fraction: f64) -> f64 {
    const HAIR: f64 = 1e-9;
    if fraction.abs() < HAIR {
        0.0
    } else if (fraction - 1.0).abs() < HAIR {
        1.0
    } else {
        fraction
    }
}

/// The part of `paper` a placement shows, at `per_unit` pixels to a unit of
/// the page — never more pixels than the picture has there.
///
/// For the printer `per_unit` is [`PIXELS_PER_INCH`] over
/// [`UNITS_PER_INCH`]; for the preview it is the preview's own density.
pub fn sent(paper: &DecodedImage, placement: &Placement, per_unit: f64) -> DecodedImage {
    let part = region(paper, placement.seen);
    let wanted = placement.on_page.width.max(placement.on_page.height) * per_unit;
    let has = f64::from(part.width.max(part.height));
    if wanted >= has {
        return part;
    }
    crate::export::shrink(&part, (wanted.round() as u32).max(1))
}

/// Cut the part of an eight-bit picture `seen` names, taking in a pixel only
/// partly inside rather than leaving it out.
fn region(image: &DecodedImage, seen: Area) -> DecodedImage {
    let (width, height) = (f64::from(image.width), f64::from(image.height));
    let left = ((seen.x * width).floor().max(0.0) as u32).min(image.width.saturating_sub(1));
    let top = ((seen.y * height).floor().max(0.0) as u32).min(image.height.saturating_sub(1));
    let right = ((seen.right() * width).ceil() as u32).clamp(left + 1, image.width.max(left + 1));
    let bottom = ((seen.bottom() * height).ceil() as u32).clamp(top + 1, image.height.max(top + 1));
    if left == 0 && top == 0 && right == image.width && bottom == image.height {
        return image.clone();
    }

    let stride = image.width as usize * 4;
    let mut pixels = Vec::with_capacity((right - left) as usize * (bottom - top) as usize * 4);
    for row in top..bottom {
        let start = row as usize * stride + left as usize * 4;
        pixels.extend_from_slice(&image.pixels[start..start + (right - left) as usize * 4]);
    }
    DecodedImage {
        width: right - left,
        height: bottom - top,
        pixels,
        depth: Depth::Eight,
    }
}

#[cfg(not(windows))]
pub fn show(_window: &winit::window::Window, _job: Job, _told: impl Fn(Outcome) + Send + 'static) -> Result<()> {
    anyhow::bail!("printing is on Windows only")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An A4 sheet in the print system's units, with the quarter-inch margins
    /// an ordinary office printer cannot reach.
    fn a4() -> Page {
        Page {
            width: 793.7,
            height: 1122.5,
            printable: Area {
                x: 24.0,
                y: 24.0,
                width: 745.7,
                height: 1074.5,
            },
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn a_whole_picture_is_centred_inside_the_margins() {
        let placed = place((3000, 2000), &a4(), Fit::Fit);
        let printable = a4().printable;
        // Across the full printable width, centred down the page.
        assert!(close(placed.on_page.x, printable.x));
        assert!(close(placed.on_page.width, printable.width));
        assert!(close(placed.on_page.height, printable.width * 2.0 / 3.0));
        assert!(
            close(placed.on_page.y - printable.y, printable.bottom() - placed.on_page.bottom()),
            "not centred: {placed:?}"
        );
        assert_eq!(placed.seen, Area::WHOLE);
    }

    #[test]
    fn a_filling_picture_covers_the_printable_area_and_shows_its_middle() {
        let placed = place((3000, 2000), &a4(), Fit::Fill);
        let printable = a4().printable;
        assert_eq!(placed.on_page, printable);
        // The picture is scaled so its height meets the page's; its width
        // overhangs, evenly at both sides.
        let drawn_width = 3000.0 * printable.height / 2000.0;
        assert!(close(placed.seen.width, printable.width / drawn_width));
        assert!(close(placed.seen.height, 1.0));
        assert!(close(placed.seen.x, (1.0 - placed.seen.width) / 2.0));
        assert!(close(placed.seen.y, 0.0));
    }

    #[test]
    fn a_page_without_a_printable_area_is_printed_whole() {
        let page = Page {
            width: 800.0,
            height: 600.0,
            printable: Area {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
        };
        let placed = place((800, 600), &page, Fit::Fit);
        assert!(close(placed.on_page.width, 800.0) && close(placed.on_page.height, 600.0), "{placed:?}");
    }

    #[test]
    fn a_picture_wider_than_tall_turns_the_page() {
        let job = |width, height, orientation| Job {
            title: String::new(),
            picture: Picture::Raster {
                image: DecodedImage {
                    width,
                    height,
                    pixels: vec![0; (width * height * 4) as usize],
                    depth: Depth::Eight,
                },
                profile: None,
            },
            orientation,
        };
        assert!(job(30, 20, Orientation::Normal).landscape());
        assert!(!job(20, 30, Orientation::Normal).landscape());
        // A photograph stored on its side and turned upright by its tag is
        // printed the way it is shown, not the way it is stored.
        assert!(!job(30, 20, Orientation::Rotate90).landscape());
    }

    #[test]
    fn the_printer_gets_three_hundred_to_the_inch_and_never_more_than_there_is() {
        let paper = DecodedImage {
            width: 6000,
            height: 4000,
            pixels: vec![255; 6000 * 4000 * 4],
            depth: Depth::Eight,
        };
        let per_unit = PIXELS_PER_INCH / UNITS_PER_INCH;

        // A picture a hundred and ninety-two units — two inches — across
        // needs six hundred pixels.
        let small = Placement {
            on_page: Area {
                x: 0.0,
                y: 0.0,
                width: 192.0,
                height: 128.0,
            },
            seen: Area::WHOLE,
        };
        let sent_small = sent(&paper, &small, per_unit);
        assert_eq!((sent_small.width, sent_small.height), (600, 400));

        // Forty inches across would want twelve thousand; the picture has six.
        let large = Placement {
            on_page: Area {
                x: 0.0,
                y: 0.0,
                width: 3840.0,
                height: 2560.0,
            },
            seen: Area::WHOLE,
        };
        let sent_large = sent(&paper, &large, per_unit);
        assert_eq!((sent_large.width, sent_large.height), (6000, 4000));
    }

    #[test]
    fn only_the_part_that_shows_is_sent() {
        // Left half black, right half white: filling a tall page with a wide
        // picture shows its middle, so both halves arrive, split evenly.
        let mut pixels = Vec::new();
        for _row in 0..100 {
            for column in 0..300 {
                let value = if column < 150 { 0 } else { 255 };
                pixels.extend_from_slice(&[value, value, value, 255]);
            }
        }
        let paper = DecodedImage {
            width: 300,
            height: 100,
            pixels,
            depth: Depth::Eight,
        };
        let page = Page {
            width: 100.0,
            height: 100.0,
            printable: Area {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
        };
        let placed = place((300, 100), &page, Fit::Fill);
        let part = sent(&paper, &placed, 1.0);
        assert_eq!((part.width, part.height), (100, 100), "the middle third, at one pixel to a unit");
        let first = &part.pixels[..4];
        let last = &part.pixels[part.pixels.len() - 4..];
        assert_eq!(first[0], 0, "the left of the middle is the black half");
        assert_eq!(last[0], 255, "the right of the middle is the white half");
    }

    #[test]
    fn transparency_lands_on_white_and_the_channels_are_blue_first() {
        let image = DecodedImage {
            width: 2,
            height: 1,
            // Opaque pure red, and fully transparent black.
            pixels: vec![255, 0, 0, 255, 0, 0, 0, 0],
            depth: Depth::Eight,
        };
        let paper = on_paper(image);
        assert_eq!(&paper.pixels[..4], &[0, 0, 255, 255], "red, written blue first");
        assert_eq!(&paper.pixels[4..], &[255, 255, 255, 255], "nothing, on white paper");
    }

    #[test]
    fn paper_is_upright_and_in_srgb() {
        // Stored on its side: a red pixel at the top-left of the stored image
        // is at the top-right once turned a quarter clockwise.
        let mut pixels = vec![0u8; 2 * 3 * 4];
        for pixel in pixels.chunks_mut(4) {
            pixel.copy_from_slice(&[255, 255, 255, 255]);
        }
        pixels[..4].copy_from_slice(&[255, 0, 0, 255]);
        let job = Job {
            title: "turned.png".into(),
            picture: Picture::Raster {
                image: DecodedImage {
                    width: 2,
                    height: 3,
                    pixels,
                    depth: Depth::Eight,
                },
                profile: None,
            },
            orientation: Orientation::Rotate90,
        };
        let paper = Paper::new(&job).unwrap();
        assert_eq!((paper.full.width, paper.full.height), (3, 2));
        let top_right = &paper.full.pixels[2 * 4..3 * 4];
        assert_eq!(top_right, &[0, 0, 255, 255], "the red corner did not turn with the picture");
    }
}

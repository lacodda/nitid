//! The release gate for printing: **is the page the printer is handed the
//! picture, where it should be, the way it is shown?**
//!
//! The dialog belongs to Windows and cannot be driven from a test, but the
//! document it hands a printer can be made without it: the print module writes
//! the same page, through the same Direct2D print control, into an XPS file
//! instead of a printer's queue (`testing::print_to_xps`). No printer, no
//! spooler, so it runs on a build machine too. The file is a zip of XML and
//! images; this gate opens it and reads the page back — where the picture is
//! drawn, in the print system's units, and the picture itself, decoded by a
//! decoder that is not nitid's.
//!
//! Windows-only, because Direct2D and XPS are.

#![cfg(windows)]

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use nitid::testing::{Orientation, PrintArea, PrintPage, print_to_xps, save_orientation};

/// A US Letter sheet, upright, in the print system's units (a ninety-sixth of
/// an inch), with the half-inch margins an office printer cannot reach.
fn letter() -> PrintPage {
    PrintPage {
        width: 816.0,
        height: 1056.0,
        printable: PrintArea {
            x: 48.0,
            y: 48.0,
            width: 720.0,
            height: 960.0,
        },
    }
}

fn folder() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let path = dir.path().to_path_buf();
    (dir, path)
}

/// What the printer would be handed: the page's size, where the picture is
/// drawn on it, and the picture.
struct Printed {
    page: (f64, f64),
    viewport: [f64; 4],
    picture: image::RgbaImage,
}

fn print(source: &Path, page: PrintPage, fill: bool) -> Printed {
    let out = source.with_extension("xps");
    print_to_xps(source, page, fill, &out).unwrap_or_else(|error| panic!("{}: {error:#}", source.display()));
    let parts = unzip(&out);

    let (_, page_xml) = parts.iter().find(|(name, _)| name.ends_with(".fpage")).expect("the document has no page");
    let page_xml = String::from_utf8(page_xml.clone()).expect("the page is text");
    let number = |name: &str| attribute(&page_xml, "FixedPage", name).parse::<f64>().expect("a number");
    let size = (number("Width"), number("Height"));

    // One picture on the page, drawn by one image brush, with nothing moving
    // it but its viewport — a transform would have to be read too, and a test
    // that ignored one would pass on a page drawn somewhere else.
    assert_eq!(page_xml.matches("<ImageBrush").count(), 1, "the page does not hold exactly one picture: {page_xml}");
    for transform in page_xml.split("RenderTransform=\"").skip(1) {
        let value = transform.split('"').next().unwrap_or_default();
        assert_eq!(value, "1,0,0,1,0,0", "the picture is moved by a transform this gate does not read: {page_xml}");
    }
    let viewport: Vec<f64> = attribute(&page_xml, "ImageBrush", "Viewport")
        .split(',')
        .map(|value| value.trim().parse().expect("a number"))
        .collect();
    let viewport: [f64; 4] = viewport.try_into().expect("a viewport is four numbers");

    let source_name = attribute(&page_xml, "ImageBrush", "ImageSource");
    let file_name = source_name.rsplit('/').next().expect("an image name").to_string();
    let (_, bytes) = parts.iter().find(|(name, _)| name.ends_with(&file_name)).expect("the page names an image the document does not hold");
    let picture = image::load_from_memory(bytes).expect("an image another decoder can read").to_rgba8();

    Printed { page: size, viewport, picture }
}

/// The value of `name` on the first `element` in `xml`.
fn attribute(xml: &str, element: &str, name: &str) -> String {
    let start = xml.find(&format!("<{element} ")).unwrap_or_else(|| panic!("no <{element}> in {xml}"));
    let tag = &xml[start..start + xml[start..].find('>').expect("a closed tag")];
    let key = format!(" {name}=\"");
    let at = tag.find(&key).unwrap_or_else(|| panic!("<{element}> has no {name}: {tag}")) + key.len();
    tag[at..at + tag[at..].find('"').expect("a closed value")].to_string()
}

/// The parts of a zip, by name, inflated. An XPS document is one, and the
/// suite carries no zip reader; reading the central directory takes less
/// than a dependency would.
fn unzip(path: &Path) -> HashMap<String, Vec<u8>> {
    let bytes = std::fs::read(path).expect("the document was written");
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
    let u32_at = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;

    let end = (0..bytes.len().saturating_sub(21))
        .rev()
        .find(|&at| bytes[at..at + 4] == [0x50, 0x4b, 0x05, 0x06])
        .expect("the document is not a zip");
    let count = u16_at(end + 10);
    let mut at = u32_at(end + 16);

    let mut parts = HashMap::new();
    for _ in 0..count {
        assert_eq!(&bytes[at..at + 4], &[0x50, 0x4b, 0x01, 0x02], "a broken central directory");
        let method = u16_at(at + 10);
        let compressed = u32_at(at + 20);
        let name_length = u16_at(at + 28);
        let extra_length = u16_at(at + 30);
        let comment_length = u16_at(at + 32);
        let local = u32_at(at + 42);
        let name = String::from_utf8_lossy(&bytes[at + 46..at + 46 + name_length]).into_owned();

        let data_start = local + 30 + u16_at(local + 26) + u16_at(local + 28);
        let data = &bytes[data_start..data_start + compressed];
        let inflated = match method {
            0 => data.to_vec(),
            8 => {
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(data).read_to_end(&mut out).expect("a part that inflates");
                out
            }
            other => panic!("{name} is stored with method {other}, which this reader does not know"),
        };
        parts.insert(name, inflated);
        at += 46 + name_length + extra_length + comment_length;
    }
    parts
}

fn near(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 0.01
}

fn colour(picture: &image::RgbaImage, x: u32, y: u32) -> [u8; 4] {
    picture.get_pixel(x, y).0
}

/// Within a JPEG's error of the colour asked for, in case the print control
/// chooses to compress a photograph.
fn about(actual: [u8; 4], expected: [u8; 3]) -> bool {
    actual[..3].iter().zip(expected).all(|(&a, e)| a.abs_diff(e) <= 12) && actual[3] == 255
}

/// A picture whole on the page: inside the margins, centred, at its own
/// pixels, its colours where they were — and what was transparent is white,
/// because that is what paper is.
#[test]
fn a_picture_is_printed_whole_inside_the_margins() {
    let (_dir, dir) = folder();
    let png = dir.join("bands.png");
    let mut pixels = image::RgbaImage::new(300, 200);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = image::Rgba(if x < 20 && y < 20 {
            [20, 200, 20, 255]
        } else if x >= 280 && y >= 180 {
            [0, 0, 0, 0]
        } else if x < 150 {
            [230, 20, 20, 255]
        } else {
            [20, 20, 230, 255]
        });
    }
    pixels.save(&png).expect("writing the PNG");

    let printed = print(&png, letter(), false);
    assert_eq!(printed.page, (816.0, 1056.0), "the sheet is not the one asked for");

    // Across the printable width, centred down the page.
    let [x, y, width, height] = printed.viewport;
    assert!(near(x, 48.0) && near(width, 720.0), "not across the printable width: {:?}", printed.viewport);
    assert!(near(height, 480.0) && near(y, 288.0), "not centred down the page: {:?}", printed.viewport);

    // Three hundred to the inch over seven and a half inches would be 2250
    // pixels; the picture has 300, and is sent with what it has.
    assert_eq!(printed.picture.dimensions(), (300, 200), "the picture was not sent at its own pixels");
    let picture = &printed.picture;
    assert!(about(colour(picture, 5, 5), [20, 200, 20]), "the corner mark is {:?}", colour(picture, 5, 5));
    assert!(about(colour(picture, 100, 100), [230, 20, 20]), "red arrived as {:?}", colour(picture, 100, 100));
    assert!(about(colour(picture, 200, 100), [20, 20, 230]), "blue arrived as {:?}", colour(picture, 200, 100));
    assert!(about(colour(picture, 295, 195), [255, 255, 255]), "transparency arrived as {:?}", colour(picture, 295, 195));
}

/// Filling the page covers what the printer can reach and sends only the part
/// that shows — the middle of a picture wider than the page.
#[test]
fn a_filling_picture_sends_only_its_middle() {
    let (_dir, dir) = folder();
    let png = dir.join("thirds.png");
    let mut pixels = image::RgbImage::new(300, 100);
    for (x, _, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = image::Rgb(match x {
            0..100 => [230, 20, 20],
            100..200 => [20, 200, 20],
            _ => [20, 20, 230],
        });
    }
    pixels.save(&png).expect("writing the PNG");

    let square = PrintPage {
        width: 400.0,
        height: 400.0,
        printable: PrintArea {
            x: 0.0,
            y: 0.0,
            width: 400.0,
            height: 400.0,
        },
    };
    let printed = print(&png, square, true);
    let [x, y, width, height] = printed.viewport;
    assert!(near(x, 0.0) && near(y, 0.0) && near(width, 400.0) && near(height, 400.0), "the page is not covered: {:?}", printed.viewport);
    assert_eq!(printed.picture.dimensions(), (100, 100), "more than the middle third was sent");
    let picture = &printed.picture;
    for x in [0, 50, 99] {
        assert!(about(colour(picture, x, 50), [20, 200, 20]), "column {x} of the middle is {:?}", colour(picture, x, 50));
    }
}

/// A photograph turned by its tag is printed the way it is shown.
#[test]
fn a_turned_photograph_is_printed_upright() {
    let (_dir, dir) = folder();
    let jpeg = dir.join("sideways.jpg");
    // Wide as stored, red on the left half.
    let mut pixels = image::RgbImage::new(60, 40);
    for (x, _, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = image::Rgb(if x < 30 { [230, 20, 20] } else { [20, 20, 230] });
    }
    pixels.save(&jpeg).expect("writing the JPEG");
    save_orientation(&jpeg, Orientation::Rotate90).expect("tagging it as turned");

    let printed = print(&jpeg, letter(), false);
    assert_eq!(printed.picture.dimensions(), (40, 60), "the printed picture is not upright");
    // Turned a quarter clockwise, the stored left half is now on top.
    let top = colour(&printed.picture, 20, 5);
    assert!(top[0] > 150 && top[2] < 100, "the turn went the wrong way: the top is {top:?}");

    // Upright, it is taller than wide: the height of the printable area
    // decides, and it is centred across.
    let [x, y, width, height] = printed.viewport;
    assert!(near(height, 960.0) && near(width, 640.0), "{:?}", printed.viewport);
    assert!(near(y, 48.0) && near(x, 88.0), "{:?}", printed.viewport);
}

/// A large picture is sent at three hundred pixels to the inch of the paper it
/// covers, not at every pixel it has.
#[test]
fn a_large_picture_is_sent_at_three_hundred_to_the_inch() {
    let (_dir, dir) = folder();
    let png = dir.join("large.png");
    image::RgbImage::from_pixel(3000, 2000, image::Rgb([120, 120, 120])).save(&png).expect("writing the PNG");

    // Two inches of printable width.
    let small = PrintPage {
        width: 384.0,
        height: 384.0,
        printable: PrintArea {
            x: 96.0,
            y: 96.0,
            width: 192.0,
            height: 192.0,
        },
    };
    let printed = print(&png, small, false);
    assert_eq!(printed.picture.dimensions(), (600, 400), "not three hundred to the inch over two inches");
}

/// A vector is drawn at the size the paper needs, not enlarged from the size
/// its document declares.
#[test]
fn a_vector_is_drawn_at_the_size_the_paper_needs() {
    let (_dir, dir) = folder();
    let svg = dir.join("square.svg");
    std::fs::write(
        &svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="#e61414"/></svg>"##,
    )
    .expect("writing the SVG");

    let printed = print(&svg, letter(), false);
    // Seven and a half inches across, at three hundred to the inch.
    assert_eq!(printed.picture.dimensions(), (2250, 2250), "the vector was not drawn for the paper");
    assert!(about(colour(&printed.picture, 1125, 1125), [230, 20, 20]), "{:?}", colour(&printed.picture, 1125, 1125));
}

//! Rasterising vector images for the size they are shown at.
//!
//! A raster file has a resolution: decode it once and the pixels are the
//! image. An SVG has none — it describes shapes, and the pixels only exist
//! once a size is chosen. Zooming into a rasterisation made for a smaller
//! window gives a blurred picture, which is exactly the thing a vector format
//! exists to avoid, so the parsed document is kept and drawn again whenever
//! the size on screen changes materially.
//!
//! Parsing happens once per file; redrawing is the cheap half.

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, bail};
// usvg and tiny-skia arrive through resvg rather than as dependencies of
// their own, so the three versions cannot drift apart.
use resvg::tiny_skia;
use resvg::usvg::{self, fontdb};

use crate::image_source::DecodedImage;

/// The largest raster a vector image is allowed to produce, per side.
///
/// A vector image can be asked for any size at all, and at deep zoom the
/// number the viewer would like is far past what a texture can hold. The cap
/// keeps a zoom gesture from turning into a gigabyte allocation; past it the
/// picture is upscaled by the renderer like any other image.
const MAX_RASTER_SIDE: u32 = 8192;

/// A parsed vector document, ready to be drawn at any size.
///
/// Cloning is cheap: the tree sits behind an `Arc`, so the loader's cache and
/// the image on screen share one parse.
#[derive(Clone)]
pub struct VectorImage {
    tree: Arc<usvg::Tree>,
    /// The size the document declares, used to keep the aspect ratio and as
    /// the size the viewer lays out against before any zoom.
    width: f32,
    height: f32,
}

impl VectorImage {
    /// Parse an SVG document.
    ///
    /// Fails on anything usvg refuses — malformed XML, a missing or degenerate
    /// size, a document past its element limit. None of that reaches the
    /// renderer.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let tree = usvg::Tree::from_data(bytes, &options(needs_fonts(bytes))).context("the SVG could not be parsed")?;

        let size = tree.size();
        let (width, height) = (size.width(), size.height());
        if !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
            bail!("the SVG declares a size of {width}x{height}, which cannot be drawn");
        }

        Ok(Self {
            tree: Arc::new(tree),
            width,
            height,
        })
    }

    /// The size the document declares, rounded to whole pixels.
    ///
    /// This is what the viewer frames against: an SVG that says it is 512x512
    /// opens at 512x512, the same as a raster file of that size would.
    pub fn intrinsic_size(&self) -> (u32, u32) {
        (self.width.ceil().max(1.0) as u32, self.height.ceil().max(1.0) as u32)
    }

    /// Draw the document into an RGBA8 image `width` by `height`.
    ///
    /// The aspect ratio is taken from the caller: it asks for the box the
    /// image occupies on screen, which the viewer has already fitted.
    pub fn rasterise(&self, width: u32, height: u32) -> Result<DecodedImage> {
        let width = width.clamp(1, MAX_RASTER_SIDE);
        let height = height.clamp(1, MAX_RASTER_SIDE);

        let mut pixmap = tiny_skia::Pixmap::new(width, height).with_context(|| format!("{width}x{height} is more raster than this build can hold"))?;

        let scale_x = width as f32 / self.width;
        let scale_y = height as f32 / self.height;
        resvg::render(&self.tree, tiny_skia::Transform::from_scale(scale_x, scale_y), &mut pixmap.as_mut());

        Ok(DecodedImage {
            width,
            height,
            pixels: straight_alpha(&pixmap),
        })
    }
}

/// Parsing options, shared by every SVG this build opens.
///
/// Two things matter here and both are about treating the file as hostile.
fn options(with_fonts: bool) -> usvg::Options<'static> {
    usvg::Options {
        // An SVG may point `xlink:href` at another file, and usvg's default
        // resolver reads it straight off the disk. An image file is untrusted
        // input — opening one must never make the viewer fetch a path the
        // document chose — so every non-embedded reference is refused.
        // Embedded `data:` images still work, which is how a self-contained
        // SVG carries a bitmap.
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_string: Box::new(|_href, _options| None),
            ..usvg::ImageHrefResolver::default()
        },
        // Only a document that actually draws text pays for the font
        // directory: loading it costs hundreds of milliseconds, which is most
        // of the time an SVG takes to open, and the great majority of SVGs —
        // icons, logos, diagrams — have no `<text>` in them at all.
        fontdb: if with_fonts { Arc::clone(fonts()) } else { Arc::default() },
        ..usvg::Options::default()
    }
}

/// Whether the document draws text, and so needs real fonts to be loaded.
///
/// A conservative reading of the markup rather than a parse: `<text>` is what
/// requires a font, and finding the substring cannot miss one. It can say yes
/// where the answer is no — the word appearing inside a comment, say — and
/// that costs a font scan for a file that did not need one, which is the safe
/// direction to be wrong in.
fn needs_fonts(bytes: &[u8]) -> bool {
    bytes.windows(5).any(|window| window.eq_ignore_ascii_case(b"<text"))
}

/// The system fonts, loaded once.
///
/// `<text>` in an SVG needs real fonts to draw with, and finding them means
/// scanning the whole Windows font directory — a fixed cost of tens of
/// milliseconds that must not be paid per file, and must not be paid at all by
/// someone who never opens an SVG. So it happens on first use and is then
/// shared by every later parse.
fn fonts() -> &'static Arc<fontdb::Database> {
    static FONTS: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        Arc::new(database)
    })
}

/// Convert the pixmap's premultiplied samples to the straight alpha the
/// renderer uploads.
///
/// tiny-skia stores colour already multiplied by alpha, which is right for
/// compositing and wrong for a texture the shader samples: a half-transparent
/// red would arrive darkened. Every other decoder in this viewer hands over
/// straight alpha, so this one does too.
fn straight_alpha(pixmap: &tiny_skia::Pixmap) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(pixmap.data().len());
    for pixel in pixmap.pixels() {
        let colour = pixel.demultiply();
        pixels.extend_from_slice(&[colour.red(), colour.green(), colour.blue(), colour.alpha()]);
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10">
        <rect width="20" height="10" fill="#0AC85A"/>
    </svg>"##;

    #[test]
    fn parses_and_reports_the_declared_size() {
        let image = VectorImage::parse(SQUARE).unwrap();
        assert_eq!(image.intrinsic_size(), (20, 10));
    }

    /// The point of the format: the same document drawn larger is drawn from
    /// the shapes again, not stretched from an earlier raster.
    #[test]
    fn rasterises_at_whatever_size_it_is_asked_for() {
        let image = VectorImage::parse(SQUARE).unwrap();

        let small = image.rasterise(20, 10).unwrap();
        assert_eq!((small.width, small.height), (20, 10));
        assert_eq!(small.pixels.len(), 20 * 10 * 4);

        let large = image.rasterise(200, 100).unwrap();
        assert_eq!((large.width, large.height), (200, 100));
        assert_eq!(large.pixels.len(), 200 * 100 * 4);
    }

    #[test]
    fn draws_the_colour_the_document_asks_for() {
        let image = VectorImage::parse(SQUARE).unwrap();
        let raster = image.rasterise(20, 10).unwrap();
        // The middle of the rectangle, well away from any edge.
        let middle = ((5 * 20 + 10) * 4) as usize;
        assert_eq!(&raster.pixels[middle..middle + 4], &[0x0A, 0xC8, 0x5A, 0xFF]);
    }

    /// Transparency has to survive the trip: premultiplied samples handed
    /// straight to the shader would show a half-transparent colour darkened.
    #[test]
    fn a_half_transparent_fill_keeps_its_colour() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">
            <rect width="4" height="4" fill="#FFFFFF" fill-opacity="0.5"/>
        </svg>"##;
        let raster = VectorImage::parse(svg).unwrap().rasterise(4, 4).unwrap();

        let pixel = &raster.pixels[..4];
        assert!(pixel[3] > 0 && pixel[3] < 0xFF, "the fill should be partly transparent, got alpha {}", pixel[3]);
        // Premultiplied, white at half alpha would arrive as 0x80; straight, it
        // stays white.
        assert!(
            pixel[0] > 0xF0,
            "white came back as {:#04x}, which means the alpha was left premultiplied",
            pixel[0]
        );
    }

    /// Loading the font directory is most of the time an SVG takes to open —
    /// hundreds of milliseconds warm, seconds on the first read of a machine's
    /// 385 font files. A document without `<text>` must not pay it.
    #[test]
    fn only_a_document_with_text_asks_for_fonts() {
        assert!(!needs_fonts(SQUARE));
        assert!(needs_fonts(br##"<svg><text x="0" y="0">hello</text></svg>"##));
        // Case-insensitive, and matched on the element rather than the word,
        // so `textPath` and `<textArea` count too — both need a font.
        assert!(needs_fonts(br##"<svg><TEXT>hi</TEXT></svg>"##));
    }

    /// A document with text opens and draws whatever else it contains.
    ///
    /// What is deliberately *not* asserted here is that glyphs appear: that
    /// depends on the machine having fonts at all, and a CI container has
    /// none. The rule this test guards is that asking for fonts does not turn
    /// a document into a failure — the rectangle draws either way.
    #[test]
    fn a_document_with_text_opens_and_draws_the_rest() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="20">
            <rect width="60" height="20" fill="#3FA9D9"/>
            <text x="2" y="15" font-family="Segoe UI" font-size="14" fill="#FFFFFF">nitid</text>
        </svg>"##;
        let raster = VectorImage::parse(svg).unwrap().rasterise(60, 20).unwrap();
        assert_eq!(&raster.pixels[..4], &[0x3F, 0xA9, 0xD9, 0xFF], "the document behind the text did not draw");
    }

    #[test]
    fn refuses_what_is_not_an_svg() {
        assert!(VectorImage::parse(b"not markup at all").is_err());
        assert!(VectorImage::parse(b"<svg").is_err());
    }

    /// A document with no size to speak of must be refused rather than
    /// rasterised into nothing.
    #[test]
    fn refuses_a_document_without_a_usable_size() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="0"></svg>"##;
        assert!(VectorImage::parse(svg).is_err());
    }

    /// An SVG that points at a file on disk must not make the viewer read it:
    /// an image is untrusted input, and the document does not get to choose
    /// what the process opens.
    #[test]
    fn an_external_reference_is_not_followed() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
            <image href="/etc/passwd" width="8" height="8"/>
        </svg>"##;
        // Parsing succeeds; the reference simply resolves to nothing, so the
        // result is an empty raster rather than a file's contents.
        let raster = VectorImage::parse(svg).unwrap().rasterise(8, 8).unwrap();
        assert!(raster.pixels.iter().all(|sample| *sample == 0), "something was drawn for an external reference");
    }

    /// A vector image can be asked for any size; the cap is what keeps a deep
    /// zoom from turning into an allocation nothing can hold.
    #[test]
    fn an_absurd_size_is_capped_rather_than_attempted() {
        let raster = VectorImage::parse(SQUARE).unwrap().rasterise(u32::MAX, 10).unwrap();
        assert_eq!(raster.width, MAX_RASTER_SIDE);
    }
}

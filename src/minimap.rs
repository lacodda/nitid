//! A small copy of the whole picture, with a frame around the part on screen.
//!
//! Zoomed in, a viewer answers "what is here" and stops answering "where is
//! this". The minimap gives the second answer back: the whole photograph at
//! thumbnail size, and a rectangle showing which piece of it the window holds.
//! Panning moves the rectangle, so the picture stays navigable at a zoom where
//! nothing on screen says where in the frame you are.
//!
//! **It is drawn in display colours**, not the file's. The same reasoning as
//! the eyedropper's magnified neighbourhood (owner, 2026-09-07): this sits
//! beside the photograph, and a copy painted in the file's numbers would be a
//! visibly different colour from the picture it is a copy of. What the file
//! holds is a fact this viewer reports in numbers — the histogram, the
//! eyedropper's reading, the clipboard (ADR 0019) — and a picture is not a
//! number.
//!
//! The copy is built once per image rather than per frame, and by sampling
//! rather than by averaging: a thumbnail a couple of hundred pixels across
//! reads the same either way, and nearest-neighbour costs one read per drawn
//! pixel instead of one per source pixel. That matters because it runs on a
//! sixty-megapixel photograph on the same thread that has to keep the picture
//! moving under a drag.

use crate::color::ColorTransform;
use crate::eyedropper::{sample, shown_size, through};
use crate::image_source::{DecodedImage, Orientation};

/// The longest side of the small copy, in image pixels.
///
/// The copy is drawn a hundred-odd logical points across, and a source a
/// little larger than that keeps it crisp on a 200% display without paying for
/// pixels nobody sees. It is not a texture budget — it is 160 by 160 at worst,
/// which is a quarter of the memory a single row of a large photograph takes.
const LONGEST_SIDE: u32 = 160;

/// A picture small enough to sit in a corner, in the display's colours.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// Row-major RGB, three bytes to a pixel, `width * height` of them.
    ///
    /// Opaque: a minimap is a map, and a checkerboard showing through it would
    /// make a cut-out read as texture in the picture rather than as absence.
    /// Transparency is a question the picture itself answers, at full size.
    pub pixels: Vec<u8>,
}

impl Thumbnail {
    /// Shrink a decoded image to something that fits in a corner.
    ///
    /// `orientation` is applied, so the copy is turned the way the picture on
    /// screen is: a minimap of a portrait photograph stored sideways has to be
    /// portrait, or the frame drawn on it points at the wrong place.
    ///
    /// Returns `None` for an image with no pixels in it, which is what a
    /// caller needs to distinguish from a copy that is merely small.
    pub fn of(image: &DecodedImage, orientation: Orientation, transform: &ColorTransform) -> Option<Self> {
        let shown = shown_size(image, orientation);
        let (width, height) = fitted(shown)?;

        let mut pixels = Vec::with_capacity((width as usize) * (height as usize) * 3);
        for row in 0..height {
            for column in 0..width {
                // The centre of the source cell this destination pixel stands
                // for, rather than its corner: sampling the corner of the
                // first cell reads the very edge pixel of the picture, which
                // on a photograph with a dark border makes the whole copy a
                // shade darker than the picture it copies.
                let at = (
                    ((column as u64 * shown.0 as u64 + shown.0 as u64 / 2) / width as u64) as u32,
                    ((row as u64 * shown.1 as u64 + shown.1 as u64 / 2) / height as u64) as u32,
                );
                // Clamped, because the rounding above can land one past the
                // last row or column on a picture whose size divides awkwardly.
                let at = (at.0.min(shown.0.saturating_sub(1)), at.1.min(shown.1.saturating_sub(1)));

                let colour = match sample(image, orientation, shown, at) {
                    Some((raw, _)) => through(transform, raw, image.depth),
                    // A pixel the source cannot answer for. Black rather than
                    // a skip: the buffer's length is what says how big the
                    // copy is, and a short one would be read as a different
                    // shape.
                    None => [0, 0, 0],
                };
                pixels.extend_from_slice(&colour);
            }
        }

        Some(Self { width, height, pixels })
    }
}

/// The size of the small copy: the picture's own proportions, with the longer
/// side at [`LONGEST_SIDE`].
///
/// Never enlarged. A picture already smaller than that is copied at its own
/// size — a sixteen-pixel icon blown up to a hundred and sixty would be a map
/// four times the size of the thing it maps.
fn fitted(shown: (u32, u32)) -> Option<(u32, u32)> {
    if shown.0 == 0 || shown.1 == 0 {
        return None;
    }
    if shown.0 <= LONGEST_SIDE && shown.1 <= LONGEST_SIDE {
        return Some(shown);
    }

    let (width, height) = if shown.0 >= shown.1 {
        (LONGEST_SIDE, (shown.1 as u64 * LONGEST_SIDE as u64 / shown.0 as u64) as u32)
    } else {
        ((shown.0 as u64 * LONGEST_SIDE as u64 / shown.1 as u64) as u32, LONGEST_SIDE)
    };
    // An extreme panorama rounds its short side to zero. One pixel is a
    // degenerate map, but it is a map; zero is a copy with nothing in it.
    Some((width.max(1), height.max(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_source::Depth;

    /// A flat image of one colour, so a sample can be checked against a known
    /// value wherever it lands.
    fn flat(width: u32, height: u32, colour: [u8; 3]) -> DecodedImage {
        let mut pixels = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for _ in 0..(width as usize) * (height as usize) {
            pixels.extend_from_slice(&[colour[0], colour[1], colour[2], 255]);
        }
        DecodedImage {
            width,
            height,
            pixels,
            depth: Depth::Eight,
        }
    }

    fn identity() -> ColorTransform {
        ColorTransform::identity()
    }

    #[test]
    fn a_large_picture_is_shrunk_to_the_longest_side() {
        let image = flat(4000, 2000, [10, 20, 30]);
        let thumbnail = Thumbnail::of(&image, Orientation::Normal, &identity()).unwrap();

        assert_eq!(thumbnail.width, LONGEST_SIDE);
        assert_eq!(thumbnail.height, LONGEST_SIDE / 2);
        assert_eq!(thumbnail.pixels.len(), (thumbnail.width as usize) * (thumbnail.height as usize) * 3);
    }

    /// The proportions of the picture are the proportions of the map, or the
    /// frame drawn on it points at the wrong part.
    #[test]
    fn a_tall_picture_stays_tall() {
        let image = flat(1000, 3000, [10, 20, 30]);
        let thumbnail = Thumbnail::of(&image, Orientation::Normal, &identity()).unwrap();

        assert_eq!(thumbnail.height, LONGEST_SIDE);
        assert!(thumbnail.width < thumbnail.height, "{}x{}", thumbnail.width, thumbnail.height);
    }

    #[test]
    fn a_small_picture_is_not_enlarged() {
        let image = flat(16, 16, [10, 20, 30]);
        let thumbnail = Thumbnail::of(&image, Orientation::Normal, &identity()).unwrap();

        assert_eq!((thumbnail.width, thumbnail.height), (16, 16));
    }

    /// A picture stored sideways is copied the way it is shown, or the minimap
    /// of every rotated photograph is turned against the picture beside it.
    #[test]
    fn orientation_turns_the_copy() {
        let image = flat(4000, 2000, [10, 20, 30]);
        let turned = Thumbnail::of(&image, Orientation::Rotate90, &identity()).unwrap();

        assert!(turned.height > turned.width, "{}x{}", turned.width, turned.height);
    }

    #[test]
    fn the_colours_are_the_pictures_own() {
        let image = flat(500, 500, [200, 40, 90]);
        let thumbnail = Thumbnail::of(&image, Orientation::Normal, &identity()).unwrap();

        assert_eq!(&thumbnail.pixels[..3], &[200, 40, 90]);
    }

    #[test]
    fn an_empty_picture_has_no_copy() {
        let image = flat(0, 0, [0, 0, 0]);
        assert!(Thumbnail::of(&image, Orientation::Normal, &identity()).is_none());
    }

    /// An extreme panorama still gets a map, even if it is one pixel tall.
    #[test]
    fn an_extreme_panorama_keeps_a_row() {
        let image = flat(20_000, 20, [10, 20, 30]);
        let thumbnail = Thumbnail::of(&image, Orientation::Normal, &identity()).unwrap();

        assert!(thumbnail.height >= 1);
        assert_eq!(thumbnail.pixels.len(), (thumbnail.width as usize) * (thumbnail.height as usize) * 3);
    }
}

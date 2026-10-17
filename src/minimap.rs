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

    /// A copy with its longer side at `longest`, each pixel the average of
    /// the part of the picture it stands for.
    ///
    /// For a copy that is looked at as a picture rather than read as a map:
    /// the start screen's tiles. Sampling one pixel per cell, the way the
    /// minimap does, turns fine detail — foliage, a fabric, text in a
    /// screenshot — into noise once the picture is shrunk thirtyfold, and a
    /// tile is where a person decides whether this is the folder they meant.
    /// The average is taken over a grid of up to four by four samples in the
    /// cell rather than over every pixel in it: the noise is gone at sixteen,
    /// and a sixty-megapixel photograph costs two million reads instead of
    /// sixty. Off the event loop, so the difference is the grid filling in
    /// sooner rather than a frame kept.
    ///
    /// Averaged in the stored values, before the colour transform: averaging
    /// in light would be more correct at an edge between a bright and a dark
    /// region, and invisible at the size a tile is drawn.
    pub fn averaged(image: &DecodedImage, orientation: Orientation, transform: &ColorTransform, longest: u32) -> Option<Self> {
        const SAMPLES: u64 = 4;

        let shown = shown_size(image, orientation);
        let (width, height) = fitted_to(shown, longest)?;

        let mut pixels = Vec::with_capacity((width as usize) * (height as usize) * 3);
        for row in 0..height as u64 {
            for column in 0..width as u64 {
                // The cell this pixel stands for, in the shown picture.
                let left = column * shown.0 as u64 / width as u64;
                let right = ((column + 1) * shown.0 as u64 / width as u64).max(left + 1);
                let top = row * shown.1 as u64 / height as u64;
                let bottom = ((row + 1) * shown.1 as u64 / height as u64).max(top + 1);
                let across = (right - left).min(SAMPLES);
                let down = (bottom - top).min(SAMPLES);

                let mut sum = [0u64; 3];
                let mut count = 0u64;
                for step_y in 0..down {
                    for step_x in 0..across {
                        // Spread through the cell, each sample at the centre
                        // of its own share of it.
                        let x = left + ((2 * step_x + 1) * (right - left)) / (2 * across);
                        let y = top + ((2 * step_y + 1) * (bottom - top)) / (2 * down);
                        if let Some((raw, _)) = sample(image, orientation, shown, (x as u32, y as u32)) {
                            for (total, value) in sum.iter_mut().zip(raw) {
                                *total += u64::from(value);
                            }
                            count += 1;
                        }
                    }
                }

                let colour = match count {
                    0 => [0, 0, 0],
                    _ => through(transform, sum.map(|total| (total / count) as u16), image.depth),
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
    fitted_to(shown, LONGEST_SIDE)
}

/// The same, for any longest side.
fn fitted_to(shown: (u32, u32), longest: u32) -> Option<(u32, u32)> {
    if shown.0 == 0 || shown.1 == 0 || longest == 0 {
        return None;
    }
    if shown.0 <= longest && shown.1 <= longest {
        return Some(shown);
    }

    let (width, height) = if shown.0 >= shown.1 {
        (longest, (shown.1 as u64 * longest as u64 / shown.0 as u64) as u32)
    } else {
        ((shown.0 as u64 * longest as u64 / shown.1 as u64) as u32, longest)
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

    /// The reason the averaged copy exists: detail finer than a cell comes
    /// out as its average rather than as whichever pixel a sample hit. A
    /// one-pixel checkerboard shrunk tenfold is grey; sampled, it would be
    /// all black or all white — the same scene, reading as something else.
    #[test]
    fn an_averaged_copy_turns_fine_detail_into_its_average() {
        let (width, height) = (400u32, 400u32);
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let value = if (x + y) % 2 == 0 { 255 } else { 0 };
                pixels.extend_from_slice(&[value, value, value, 255]);
            }
        }
        let image = DecodedImage {
            width,
            height,
            pixels,
            depth: Depth::Eight,
        };

        let averaged = Thumbnail::averaged(&image, Orientation::Normal, &identity(), 40).unwrap();
        assert_eq!((averaged.width, averaged.height), (40, 40));
        for value in &averaged.pixels {
            assert!((100..=155).contains(value), "a checkerboard shrank to {value}, not to its grey");
        }
    }

    #[test]
    fn an_averaged_copy_keeps_proportions_and_turns_with_the_picture() {
        let image = flat(4000, 2000, [200, 40, 90]);
        let wide = Thumbnail::averaged(&image, Orientation::Normal, &identity(), 300).unwrap();
        assert_eq!((wide.width, wide.height), (300, 150));
        assert_eq!(&wide.pixels[..3], &[200, 40, 90]);

        let turned = Thumbnail::averaged(&image, Orientation::Rotate90, &identity(), 300).unwrap();
        assert_eq!((turned.width, turned.height), (150, 300));
    }

    /// A picture smaller than the tile is not blown up into one.
    #[test]
    fn an_averaged_copy_is_not_enlarged() {
        let image = flat(16, 9, [1, 2, 3]);
        let copy = Thumbnail::averaged(&image, Orientation::Normal, &identity(), 300).unwrap();
        assert_eq!((copy.width, copy.height), (16, 9));
        assert_eq!(&copy.pixels[..3], &[1, 2, 3]);
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

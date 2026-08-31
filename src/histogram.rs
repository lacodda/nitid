//! Counting what tones a picture is made of.
//!
//! The histogram answers a question the picture itself cannot: whether the
//! highlights are clipped, whether the shadows have anything in them, whether
//! an exposure is where the photographer meant it. Reading that off the screen
//! is guesswork — a bright room and a dim one disagree about the same file.
//!
//! **It counts the values in the file**, before the colour transform, and that
//! is the decision the rest of this module follows from (owner, 2026-08-31). A
//! photographer judging an exposure is judging what the camera recorded, not
//! what this display can show: a histogram measured after the profile would
//! move when the window is dragged to another monitor, and would report
//! clipping that belongs to the screen rather than to the picture.
//!
//! Nothing here touches the GPU or the decoder. It is arithmetic over the
//! pixels the loader already holds, which is what lets it run on a worker
//! thread after the picture is up: the first frame is the promise this viewer
//! is measured on, and a histogram nobody has asked to see yet must not be
//! anywhere near it.

use crate::image_source::{DecodedImage, Depth};

/// How many buckets each channel is counted into.
///
/// 256 is what the display can draw and what an 8-bit file distinguishes.
/// A 16-bit file has more levels than that, and they are folded in: a
/// histogram is a shape to read, and 65536 columns rendered into a panel a few
/// hundred pixels wide would be the same shape with more arithmetic behind it.
pub const BUCKETS: usize = 256;

/// Roughly how many pixels are counted, at most.
///
/// A histogram is a shape, and the shape of a 60-megapixel photograph is
/// settled long before the last pixel is counted: sampling every nth pixel
/// gives the same curve for a fraction of the work. Two hundred thousand keeps
/// even a rare tone — a small bright specular highlight, say — represented,
/// while holding the count on the largest files to a few milliseconds.
const SAMPLE_TARGET: usize = 200_000;

/// What tones a picture is made of.
///
/// Four counts per bucket: the three channels as the file stores them, and
/// luminance, which is what answers "is this exposed correctly" in one curve
/// rather than three.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Histogram {
    /// Red, green and blue, one row of [`BUCKETS`] counts each.
    pub channels: [Vec<u32>; 3],
    /// Luminance, weighted the way the eye weighs the three primaries.
    pub luma: Vec<u32>,
    /// How many pixels went into these counts.
    ///
    /// Not the pixel count of the image: a large picture is sampled. It is
    /// what the counts are a fraction *of*, which is what a drawn histogram
    /// needs to scale itself.
    pub counted: u32,
}

impl Histogram {
    /// Count the tones in a decoded image.
    ///
    /// Fully transparent pixels are skipped: they have colour values stored in
    /// them, and those values are whatever the encoder left behind rather than
    /// anything the picture shows. Counting them puts a spike at that value —
    /// usually black — in the histogram of every image with a cut-out.
    pub fn of(image: &DecodedImage) -> Self {
        let mut channels = [vec![0u32; BUCKETS], vec![0u32; BUCKETS], vec![0u32; BUCKETS]];
        let mut luma = vec![0u32; BUCKETS];
        let mut counted = 0u32;

        let pixels = (image.width as usize) * (image.height as usize);
        let step = stride(pixels);

        match image.depth {
            Depth::Eight => {
                for pixel in image.pixels.as_chunks::<4>().0.iter().step_by(step) {
                    if pixel[3] == 0 {
                        continue;
                    }
                    let (r, g, b) = (pixel[0], pixel[1], pixel[2]);
                    channels[0][r as usize] += 1;
                    channels[1][g as usize] += 1;
                    channels[2][b as usize] += 1;
                    luma[luminance(r, g, b) as usize] += 1;
                    counted += 1;
                }
            }
            // Samples are native-endian `u16` pairs, which is what the decoder
            // wrote and what the GPU reads. They are folded to eight bits for
            // the bucket, and the fold is a shift rather than a division:
            // taking the top eight bits is exactly what "which of 256 bands is
            // this in" asks.
            Depth::Sixteen => {
                for pixel in image.pixels.as_chunks::<8>().0.iter().step_by(step) {
                    let sample = |index: usize| u16::from_ne_bytes([pixel[index * 2], pixel[index * 2 + 1]]);
                    if sample(3) == 0 {
                        continue;
                    }
                    let (r, g, b) = ((sample(0) >> 8) as u8, (sample(1) >> 8) as u8, (sample(2) >> 8) as u8);
                    channels[0][r as usize] += 1;
                    channels[1][g as usize] += 1;
                    channels[2][b as usize] += 1;
                    luma[luminance(r, g, b) as usize] += 1;
                    counted += 1;
                }
            }
        }

        Self { channels, luma, counted }
    }

    /// The largest count in any bucket, which is what a drawn histogram is
    /// scaled against.
    ///
    /// The three channels and luminance share one scale: drawn against their
    /// own maxima, a flat channel and a peaked one would look alike, and the
    /// relative weight between channels — a colour cast — would disappear.
    pub fn peak(&self) -> u32 {
        self.channels
            .iter()
            .chain(std::iter::once(&self.luma))
            .flat_map(|counts| counts.iter().copied())
            .max()
            .unwrap_or(0)
    }

    /// Whether anything was counted at all.
    ///
    /// A fully transparent image counts nothing, and a histogram of nothing is
    /// a panel of empty axes rather than a shape to read.
    pub fn is_empty(&self) -> bool {
        self.counted == 0
    }
}

/// How many pixels to step over between samples.
///
/// One for anything small enough to count whole, so a small picture's
/// histogram is exact rather than approximate.
fn stride(pixels: usize) -> usize {
    if pixels <= SAMPLE_TARGET {
        return 1;
    }
    pixels.div_ceil(SAMPLE_TARGET)
}

/// Luminance in the same eight-bit terms as the channels.
///
/// Rec. 709 weights, on the stored values rather than on light. A histogram is
/// read against the tones the file records — the mid-grey of a photograph sits
/// where the encoding put it, which is the middle of the axis, and that is
/// where a photographer expects to find it. Weighting linear light instead
/// would push every ordinary photograph's luminance curve down into the
/// shadows and make a correct exposure look underexposed.
fn luminance(r: u8, g: u8, b: u8) -> u8 {
    // Integer arithmetic in the same pass as the counting: weights scaled by
    // 1024, so the sum divides by a shift.
    let weighted = 218 * u32::from(r) + 732 * u32::from(g) + 74 * u32::from(b);
    (weighted >> 10).min(255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An image of one flat colour, at eight bits.
    fn flat(width: u32, height: u32, colour: [u8; 4]) -> DecodedImage {
        DecodedImage {
            width,
            height,
            pixels: colour.repeat((width * height) as usize),
            depth: Depth::Eight,
        }
    }

    #[test]
    fn a_flat_colour_lands_in_one_bucket_per_channel() {
        let histogram = Histogram::of(&flat(16, 16, [200, 100, 50, 255]));

        assert_eq!(histogram.counted, 256);
        assert_eq!(histogram.channels[0][200], 256);
        assert_eq!(histogram.channels[1][100], 256);
        assert_eq!(histogram.channels[2][50], 256);
        // And nothing anywhere else.
        assert_eq!(histogram.channels[0].iter().sum::<u32>(), 256);
    }

    /// The counts have to be per channel, not one shared curve: a colour cast
    /// is exactly the three channels disagreeing, and it is what a histogram
    /// is read for.
    #[test]
    fn the_channels_are_counted_apart() {
        let histogram = Histogram::of(&flat(8, 8, [255, 0, 0, 255]));

        assert_eq!(histogram.channels[0][255], 64, "red did not land at the top");
        assert_eq!(histogram.channels[1][0], 64, "green did not land at the bottom");
        assert_eq!(histogram.channels[2][0], 64, "blue did not land at the bottom");
    }

    /// A photographer reads the middle of the axis as mid-grey. Weighting
    /// linear light instead would put an ordinary mid-grey down near a fifth
    /// of the way up and make a correct exposure read as underexposed.
    #[test]
    fn a_mid_grey_lands_in_the_middle_of_the_luminance_axis() {
        let histogram = Histogram::of(&flat(8, 8, [128, 128, 128, 255]));

        let bucket = histogram.luma.iter().position(|count| *count > 0).expect("nothing was counted");
        assert!((120..=136).contains(&bucket), "mid grey landed in bucket {bucket}");
    }

    /// Luminance follows the eye, which sees far more green than blue: the
    /// three primaries at full strength must not all read as the same tone.
    #[test]
    fn luminance_weighs_the_primaries_the_way_the_eye_does() {
        let bucket_of = |colour: [u8; 4]| {
            Histogram::of(&flat(4, 4, colour))
                .luma
                .iter()
                .position(|count| *count > 0)
                .expect("nothing was counted")
        };

        let (red, green, blue) = (bucket_of([255, 0, 0, 255]), bucket_of([0, 255, 0, 255]), bucket_of([0, 0, 255, 255]));
        assert!(green > red, "green ({green}) did not read brighter than red ({red})");
        assert!(red > blue, "red ({red}) did not read brighter than blue ({blue})");
    }

    /// White is the top of the axis and black the bottom, whatever the weights
    /// are: a histogram whose ends are not the ends cannot show clipping.
    #[test]
    fn white_and_black_reach_the_ends_of_the_luminance_axis() {
        let white = Histogram::of(&flat(4, 4, [255, 255, 255, 255]));
        assert_eq!(white.luma[255], 16, "white did not reach the top of the axis");

        let black = Histogram::of(&flat(4, 4, [0, 0, 0, 255]));
        assert_eq!(black.luma[0], 16, "black did not reach the bottom of the axis");
    }

    /// A 16-bit file is counted at its own depth and folded into the same 256
    /// buckets, so the shape is comparable with an 8-bit one.
    #[test]
    fn a_sixteen_bit_image_is_counted_into_the_same_buckets() {
        // Half of full scale, which is the middle of the axis at either depth.
        let sample = 0x8000u16.to_ne_bytes();
        let opaque = 0xffffu16.to_ne_bytes();
        let pixel: Vec<u8> = [sample, sample, sample, opaque].concat();
        let image = DecodedImage {
            width: 4,
            height: 4,
            pixels: pixel.repeat(16),
            depth: Depth::Sixteen,
        };

        let histogram = Histogram::of(&image);
        assert_eq!(histogram.counted, 16);
        assert_eq!(histogram.channels[0][128], 16, "a half-scale sample did not land mid-axis");
    }

    /// The two depths are read from the same bytes in different widths, and
    /// getting that wrong reads a 16-bit image as noise. The same picture at
    /// both depths must produce the same shape.
    #[test]
    fn the_same_picture_reads_the_same_at_both_depths() {
        let eight = Histogram::of(&flat(8, 8, [64, 128, 192, 255]));

        let wide = |value: u8| u16::from(value).wrapping_mul(257).to_ne_bytes();
        let pixel: Vec<u8> = [wide(64), wide(128), wide(192), wide(255)].concat();
        let sixteen = Histogram::of(&DecodedImage {
            width: 8,
            height: 8,
            pixels: pixel.repeat(64),
            depth: Depth::Sixteen,
        });

        assert_eq!(eight.channels, sixteen.channels, "the depths disagree about the same colour");
        assert_eq!(eight.luma, sixteen.luma);
    }

    /// A transparent pixel carries colour values that are whatever the encoder
    /// left there. Counting them puts a spike — usually at black — in the
    /// histogram of every image with a cut-out.
    #[test]
    fn fully_transparent_pixels_are_not_counted() {
        let mut image = flat(4, 4, [200, 200, 200, 255]);
        // Half the pixels transparent, with black left behind in them.
        for pixel in image.pixels.as_chunks_mut::<4>().0.iter_mut().take(8) {
            pixel.copy_from_slice(&[0, 0, 0, 0]);
        }

        let histogram = Histogram::of(&image);
        assert_eq!(histogram.counted, 8, "transparent pixels were counted");
        assert_eq!(histogram.channels[0][0], 0, "the transparent pixels' black reached the histogram");
        assert_eq!(histogram.channels[0][200], 8);
    }

    /// A partly transparent pixel *is* counted: it shows, so its colour is
    /// part of the picture.
    #[test]
    fn a_partly_transparent_pixel_still_counts() {
        let histogram = Histogram::of(&flat(4, 4, [200, 200, 200, 1]));
        assert_eq!(histogram.counted, 16);
    }

    /// An image of nothing but transparency says so, rather than reporting a
    /// shape drawn from no pixels.
    #[test]
    fn an_entirely_transparent_image_is_empty() {
        let histogram = Histogram::of(&flat(4, 4, [0, 0, 0, 0]));
        assert!(histogram.is_empty());
        assert_eq!(histogram.peak(), 0);
    }

    /// A large picture is sampled rather than counted whole, and the sampling
    /// must not change the shape it reports.
    #[test]
    fn a_large_image_is_sampled_and_keeps_its_shape() {
        // Two megapixels: ten times the sample target.
        let (width, height) = (2000u32, 1000u32);
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        // Half the picture dark, half bright, in alternating rows so any
        // stride lands on both.
        for y in 0..height {
            let value = if y % 2 == 0 { 40 } else { 210 };
            for _ in 0..width {
                pixels.extend_from_slice(&[value, value, value, 255]);
            }
        }
        let image = DecodedImage {
            width,
            height,
            pixels,
            depth: Depth::Eight,
        };

        let histogram = Histogram::of(&image);
        assert!(
            histogram.counted <= SAMPLE_TARGET as u32,
            "a large image was counted whole: {}",
            histogram.counted
        );
        assert!(histogram.counted > 0);

        // Both tones survived the sampling, in roughly equal measure.
        let (dark, bright) = (histogram.channels[0][40], histogram.channels[0][210]);
        assert!(dark > 0 && bright > 0, "sampling lost one of the two tones: {dark} dark, {bright} bright");
        let ratio = f64::from(dark) / f64::from(bright);
        assert!((0.5..=2.0).contains(&ratio), "sampling skewed the shape: {dark} dark against {bright} bright");
    }

    /// A small image is counted exactly: there is nothing to be gained by
    /// approximating sixteen pixels.
    #[test]
    fn a_small_image_is_counted_whole() {
        let histogram = Histogram::of(&flat(100, 100, [128, 128, 128, 255]));
        assert_eq!(histogram.counted, 10_000);
    }

    /// The channels and luminance share one scale, so a colour cast shows as
    /// the difference between them rather than being normalised away.
    ///
    /// The picture is built so the four curves peak at *different* heights: a
    /// flat colour makes every curve peak at the same count, and against that
    /// a `peak` that took the smallest — or one curve's own maximum — would
    /// look right. Drawn against a peak that is not the largest count, the
    /// tallest column runs off the top of the plot.
    #[test]
    fn the_peak_is_the_largest_count_anywhere() {
        // Red is constant across the whole picture, so its curve is one column
        // a hundred high. Green takes four values, so its tallest column is a
        // quarter of that; blue takes two, so its tallest is a half.
        let mut pixels = Vec::new();
        for index in 0..100u32 {
            let green = (index % 4) as u8 * 40;
            let blue = (index % 2) as u8 * 90;
            pixels.extend_from_slice(&[200, green, blue, 255]);
        }
        let histogram = Histogram::of(&DecodedImage {
            width: 10,
            height: 10,
            pixels,
            depth: Depth::Eight,
        });

        // The curves really do peak at different heights, or this proves
        // nothing about which of them `peak` reports.
        let tallest = |counts: &[u32]| counts.iter().copied().max().unwrap_or(0);
        assert_eq!(tallest(&histogram.channels[0]), 100, "the fixture's red is not flat");
        assert_eq!(tallest(&histogram.channels[1]), 25, "the fixture's green is not spread over four values");
        assert_eq!(tallest(&histogram.channels[2]), 50, "the fixture's blue is not spread over two values");

        assert_eq!(
            histogram.peak(),
            100,
            "the peak is not the largest count anywhere, so the tallest column would run off the plot",
        );
    }

    #[test]
    fn an_image_with_no_pixels_does_not_panic() {
        let histogram = Histogram::of(&DecodedImage {
            width: 0,
            height: 0,
            pixels: Vec::new(),
            depth: Depth::Eight,
        });
        assert!(histogram.is_empty());
    }
}

//! A small picture in each format Windows needs a codec for.
//!
//! Handed to Windows to find out whether it can decode the format at all
//! (`shell`), and to the gates as a file of another program's making. A flat
//! colour, sixty-four pixels square: large enough that no decoder thinks it
//! degenerate. Written by Pillow over libjxl, libaom and libheif, and kept as
//! text because the suite ships no binary files.

use crate::format::Format;

/// What every sample is a picture of.
#[cfg(test)]
const SIDE: u32 = 64;
#[cfg(test)]
const COLOUR: [u8; 3] = [40, 120, 200];

const JXL: &str = concat!(
    "ff0a4f060108010b541200800a9551c60d5ecef57cf9a1c362dac67586b6dab6b084b1d95e182404c0019131ea18e33508d56878882a851ed8b9c540",
    "c1786054e0830020a5c9549200",
);

const AVIF: &str = concat!(
    "00000020667479706176696600000000617669666d6966316d6961664d413142000000eb6d657461000000000000002168646c720000000000000000",
    "70696374000000000000000000000000000000000e7069746d0000000000010000001e696c6f63000000004400000100010000000100000113000000",
    "2c0000002869696e660000000000010000001a696e6665020000000001000061763031436f6c6f72000000006a697072700000004b6970636f000000",
    "1469737065000000000000004000000040000000107069786900000000030808080000000c6176314381000c0000000013636f6c726e636c78000100",
    "0d0006800000001769706d61000000000000000100010401028304000000346d64617412000a0918157ffda2021a0d08321d1947878621879e79e680",
    "0000cfcc4522be7f13198612bb0dd4c7ddeae0",
);

const HEIC: &str = concat!(
    "0000001c6674797068656963000000006d696631686569636d696166000001566d657461000000000000002168646c72000000000000000070696374",
    "0000000000000000000000000000000022696c6f630000000044400001000100000000017a0001000000000000001d0000002369696e660000000000",
    "0100000015696e6665020000000001000068766331000000000e7069746d000000000001000000d669707270000000b76970636f0000007868766343",
    "0103700000000000000000001ef000fcfdf8f800000f03600001001840010c01ffff03700000030090000003000003001eba0240610001002b420101",
    "03700000030090000003000003001ea020810596ea4929ae6e021a0c0800000300c800000300084062000100074401c172b0224000000013636f6c72",
    "6e636c780001000d0006800000001469737065000000000000004000000040000000107069786900000000030808080000001769706d610000000000",
    "00000100010481020304000000256d646174000000192801af1380e6a870d3fffc06515ff6d7eaffc9850b49e6fdf0",
);

/// The sample for a format, or `None` for one Windows has no codecs for to
/// ask about.
pub fn of(format: Format) -> Option<Vec<u8>> {
    let text = match format {
        Format::JpegXl => JXL,
        Format::Avif => AVIF,
        Format::Heic => HEIC,
        _ => return None,
    };
    let digit = |byte: u8| (byte as char).to_digit(16).unwrap_or_default() as u8;
    Some(
        text.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[high, low]| digit(*high) << 4 | digit(*low))
            .collect(),
    )
}

/// Every one of them, by extension.
pub fn all() -> Vec<(&'static str, Vec<u8>)> {
    Format::ALL
        .iter()
        .filter_map(|format| Some((*format.extensions().first()?, of(*format)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sample that is not what it is called would have Windows refuse it and
    /// nitid take a format over for nothing.
    #[test]
    fn every_sample_is_a_whole_picture_of_its_format() {
        for format in [Format::JpegXl, Format::Avif, Format::Heic] {
            let bytes = of(format).expect("a sample");
            assert_eq!(Format::detect(&bytes), Some(format));
            let decoded = crate::image_source::decode_here(&bytes).expect("a sample decodes");
            assert_eq!((decoded.image.width, decoded.image.height), (SIDE, SIDE), "{}", format.name());
            let middle = ((SIDE / 2 * SIDE + SIDE / 2) * 4 * decoded.image.depth.bytes()) as usize;
            for (channel, wanted) in COLOUR.into_iter().enumerate() {
                let got = decoded.image.pixels[middle + channel];
                assert!(got.abs_diff(wanted) <= 6, "{}: {got} is not {wanted}", format.name());
            }
        }
    }

    /// Every format nitid may draw for Windows either has a sample to ask with
    /// or is one Windows has no decoder for at all.
    #[test]
    fn only_svg_goes_unasked() {
        for format in Format::ALL.iter().filter(|format| format.explorer_may_not_draw()) {
            assert_eq!(of(*format).is_none(), *format == Format::Svg, "{}", format.name());
        }
    }
}

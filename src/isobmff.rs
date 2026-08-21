//! Walking the box tree that HEIC and AVIF are both built out of.
//!
//! ISOBMFF is a container of length-prefixed, four-letter-named, nested boxes.
//! Both formats keep the things a viewer wants — the rotation, which item is
//! the primary one, which item is a thumbnail of which — in that tree rather
//! than in the coded image, and neither decoder crate exposes them. So the
//! tree is walked here, by both.
//!
//! Everything in this module reads bytes from a file that may be lying about
//! them. A length that would not advance the walk ends it, the recursion is
//! bounded, and nothing is read past the end of the buffer.

/// How deep the search will follow nested boxes.
///
/// The properties these formats keep sit four levels down; twice that is room
/// for any real file and a bound against one built to recurse for ever.
const MAX_DEPTH: u32 = 8;

/// Find a box by name anywhere in `bytes`, and return its body.
///
/// The walk descends into every box it does not recognise rather than stepping
/// over the lot at one level, because what is wanted is nested several deep.
pub fn find_box<'a>(bytes: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
    find_box_within(bytes, name, 0)
}

fn find_box_within<'a>(bytes: &'a [u8], name: &[u8; 4], depth: u32) -> Option<&'a [u8]> {
    if depth > MAX_DEPTH {
        return None;
    }

    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let length = u32::from_be_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]]) as usize;
        let kind = &bytes[offset + 4..offset + 8];

        // A box shorter than its own header, or longer than what is left, is
        // a file this cannot walk any further.
        let end = if length < 8 || offset + length > bytes.len() {
            bytes.len()
        } else {
            offset + length
        };

        let body = bytes.get(offset + 8..end)?;

        if kind == name {
            return Some(body);
        }

        // Not this box: it may still hold the one being looked for. Some of
        // the containers are FullBoxes, whose four bytes of version and flags
        // sit between the header and the boxes inside — reading those as a
        // length would derail the walk one level down.
        let body = if is_full_box(kind) { body.get(4..).unwrap_or_default() } else { body };

        if let Some(found) = find_box_within(body, name, depth + 1) {
            return Some(found);
        }

        // A length that does not advance would leave the walk in place.
        if length < 8 || offset + length > bytes.len() {
            return None;
        }
        offset += length;
    }
    None
}

/// Whether a container box carries a version and flags before its children.
///
/// Only the containers matter here: a FullBox with no children is stepped
/// over like any other box, and getting it wrong costs nothing.
fn is_full_box(kind: &[u8]) -> bool {
    matches!(kind, b"meta" | b"iref" | b"iinf")
}

/// Where the `pitm` box's item identifier sits, and how wide it is.
///
/// `pitm` names the file's primary item — the picture a viewer shows. Version
/// 0 stores a 16-bit identifier, version 1 a 32-bit one.
pub struct PrimaryItem {
    /// Offset of the identifier from the start of the file.
    pub offset: usize,
    /// 2 for a version 0 box, 4 for version 1.
    pub width: usize,
    pub id: u32,
}

/// Locate the primary item declaration.
///
/// Returns where the identifier is as well as what it says, because the one
/// caller that wants this wants to *change* it — see `thumbnail_of`.
pub fn primary_item(bytes: &[u8]) -> Option<PrimaryItem> {
    let body = find_box(bytes, b"pitm")?;
    // The body is a slice of `bytes`, so its position in the file is the
    // difference between the two pointers. Both come from the same allocation.
    let start = body.as_ptr() as usize - bytes.as_ptr() as usize;

    let version = *body.first()?;
    // FullBox: one byte of version, three of flags, then the identifier.
    let offset = start + 4;
    let width = if version == 0 { 2 } else { 4 };

    let id = match width {
        2 => u32::from(u16::from_be_bytes(bytes.get(offset..offset + 2)?.try_into().ok()?)),
        _ => u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?),
    };

    Some(PrimaryItem { offset, width, id })
}

/// The identifier of the item that is a thumbnail of `primary`, if there is
/// one.
///
/// A `thmb` reference inside `iref` reads "item `from` is a thumbnail of the
/// items in `to`". The box is a FullBox whose version decides the width of
/// every identifier in it.
pub fn thumbnail_of(bytes: &[u8], primary: u32) -> Option<u32> {
    let references = find_box(bytes, b"iref")?;
    let version = *references.first()?;
    let width = if version == 0 { 2 } else { 4 };

    // The `iref` body is the version and flags, then a sequence of boxes, one
    // per reference type.
    let mut offset = 4;
    while offset + 8 <= references.len() {
        let length = u32::from_be_bytes(references.get(offset..offset + 4)?.try_into().ok()?) as usize;
        let kind = references.get(offset + 4..offset + 8)?;

        if length < 8 || offset + length > references.len() {
            return None;
        }

        if kind == b"thmb" {
            let body = references.get(offset + 8..offset + length)?;
            let from = read_id(body, 0, width)?;
            let count = usize::from(u16::from_be_bytes(body.get(width..width + 2)?.try_into().ok()?));

            // The reference lists what this item is a thumbnail *of*; only a
            // thumbnail of the picture being shown is of any use.
            for index in 0..count {
                let at = width + 2 + index * width;
                if read_id(body, at, width) == Some(primary) {
                    return Some(from);
                }
            }
        }

        offset += length;
    }

    None
}

fn read_id(bytes: &[u8], at: usize, width: usize) -> Option<u32> {
    match width {
        2 => Some(u32::from(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))),
        _ => Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?)),
    }
}

/// Where a `colr` box sits, and which kind it is.
pub struct ColourBox {
    /// Offset of the four-byte kind tag from the start of the file.
    pub kind_offset: usize,
    /// `true` when the box carries an ICC profile rather than CICP codes.
    pub is_icc: bool,
}

/// Locate the colour description box.
///
/// `colr` says what the pixels mean, in one of two ways: `nclx`, which is
/// three CICP code points, or `prof`, which is an embedded ICC profile.
pub fn colour_box(bytes: &[u8]) -> Option<ColourBox> {
    let body = find_box(bytes, b"colr")?;
    let start = body.as_ptr() as usize - bytes.as_ptr() as usize;
    let kind = body.get(..4)?;

    Some(ColourBox {
        kind_offset: start,
        is_icc: kind == b"prof" || kind == b"rICC",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a box: length, name, body.
    fn boxed(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(name);
        out.extend_from_slice(body);
        out
    }

    /// A FullBox: the same, with a version and flags ahead of the body.
    fn full_boxed(name: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
        let mut inner = vec![version, 0, 0, 0];
        inner.extend_from_slice(body);
        boxed(name, &inner)
    }

    #[test]
    fn finds_a_box_nested_several_deep() {
        let inner = boxed(b"irot", &[3]);
        let ipco = boxed(b"ipco", &inner);
        let iprp = boxed(b"iprp", &ipco);
        let meta = full_boxed(b"meta", 0, &iprp);

        assert_eq!(find_box(&meta, b"irot"), Some(&[3u8][..]));
    }

    /// `meta` carries a version and flags before its children; reading those
    /// four bytes as the next box's length loses everything inside it.
    #[test]
    fn descends_through_a_full_box() {
        let child = boxed(b"pitm", &[0, 0, 0, 0, 0, 7]);
        for container in [b"meta", b"iref", b"iinf"] {
            let wrapped = full_boxed(container, 0, &child);
            assert!(find_box(&wrapped, b"pitm").is_some(), "{} hid its children", String::from_utf8_lossy(container));
        }
    }

    #[test]
    fn reads_the_primary_item_in_both_versions() {
        let meta = full_boxed(b"meta", 0, &full_boxed(b"pitm", 0, &7u16.to_be_bytes()));
        let primary = primary_item(&meta).expect("a pitm box");
        assert_eq!(primary.id, 7);
        assert_eq!(primary.width, 2);
        // The offset must name where the identifier is, not where the box is.
        assert_eq!(&meta[primary.offset..primary.offset + 2], &7u16.to_be_bytes());

        let meta = full_boxed(b"meta", 0, &full_boxed(b"pitm", 1, &70000u32.to_be_bytes()));
        let primary = primary_item(&meta).expect("a pitm box");
        assert_eq!(primary.id, 70000);
        assert_eq!(primary.width, 4);
        assert_eq!(&meta[primary.offset..primary.offset + 4], &70000u32.to_be_bytes());
    }

    #[test]
    fn finds_the_thumbnail_of_the_primary_item() {
        // "item 2 is a thumbnail of item 1"
        let mut thmb = 2u16.to_be_bytes().to_vec();
        thmb.extend_from_slice(&1u16.to_be_bytes());
        thmb.extend_from_slice(&1u16.to_be_bytes());
        let iref = full_boxed(b"iref", 0, &boxed(b"thmb", &thmb));
        let meta = full_boxed(b"meta", 0, &iref);

        assert_eq!(thumbnail_of(&meta, 1), Some(2));
        // A thumbnail of some other item is no use for showing this one.
        assert_eq!(thumbnail_of(&meta, 9), None);
    }

    #[test]
    fn tells_an_icc_colour_box_from_a_cicp_one() {
        let mut icc = b"prof".to_vec();
        icc.extend_from_slice(&[0; 32]);
        let meta = full_boxed(b"meta", 0, &boxed(b"colr", &icc));
        let found = colour_box(&meta).expect("a colr box");
        assert!(found.is_icc);
        assert_eq!(&meta[found.kind_offset..found.kind_offset + 4], b"prof");

        let mut nclx = b"nclx".to_vec();
        nclx.extend_from_slice(&[0, 1, 0, 13, 0, 6, 0x80]);
        let meta = full_boxed(b"meta", 0, &boxed(b"colr", &nclx));
        let found = colour_box(&meta).expect("a colr box");
        assert!(!found.is_icc);
    }

    /// A file with no thumbnail is the ordinary case, not an error.
    #[test]
    fn a_file_without_a_thumbnail_says_so() {
        let meta = full_boxed(b"meta", 0, &full_boxed(b"pitm", 0, &1u16.to_be_bytes()));
        assert_eq!(thumbnail_of(&meta, 1), None);
    }

    /// Every one of these walks runs over bytes a file chose. None may loop,
    /// panic, or read past the end.
    #[test]
    fn lying_lengths_are_survived() {
        let meta = full_boxed(b"meta", 0, &full_boxed(b"pitm", 0, &1u16.to_be_bytes()));

        // A box claiming zero length, which would leave the walk in place.
        let mut zero = meta.clone();
        zero[0..4].copy_from_slice(&0u32.to_be_bytes());
        let _ = find_box(&zero, b"pitm");
        let _ = primary_item(&zero);
        let _ = thumbnail_of(&zero, 1);

        // A box claiming to run far past the file.
        let mut huge = meta.clone();
        huge[0..4].copy_from_slice(&u32::MAX.to_be_bytes());
        let _ = find_box(&huge, b"pitm");
        let _ = primary_item(&huge);
        let _ = thumbnail_of(&huge, 1);

        // And every truncation of a well-formed one.
        for cut in 0..meta.len() {
            let _ = find_box(&meta[..cut], b"pitm");
            let _ = primary_item(&meta[..cut]);
            let _ = thumbnail_of(&meta[..cut], 1);
        }
    }
}

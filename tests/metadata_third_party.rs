//! The Info panel read against files this project did not write.
//!
//! The unit tests in `metadata.rs` build their EXIF by hand, which proves the
//! reader against a fixture the same repository produced. That is a closed
//! circle: a shared misunderstanding of the layout would pass both halves.
//! These fixtures come from a third-party encoder instead.
//!
//! They are checked in as base64 rather than as binaries, the same way the
//! HEIC and JPEG XL fixtures are, so the suite still ships no binary files.

/// A 16x16 JPEG carrying camera EXIF, written by Pillow.
///
/// Pillow's own EXIF writer, not `piexif`: measured, `piexif` emits sub-IFDs
/// that `kamadak-exif` rejects outright ("Truncated next IFD offset"), so a
/// fixture from it would fail here for a reason that has nothing to do with
/// nitid. The generator matters as much as the fixture.
const PILLOW_JPEG: &str = concat!(
    "/9j/4AAQSkZJRgABAQAAAQABAAD/4QFIRXhpZgAATU0AKgAAAAgABAEPAAIAAAAGAAAAPgEQAAIAAAAKAAAARIdpAAQAAAABAAAA",
    "ToglAAQAAAABAAAA2gAAAABOSVRJRABQcm9iZSBPbmUAAAaCmgAFAAAAAQAAAJyCnQAFAAAAAQAAAKSIJwADAAAAAQGQAACQAwAC",
    "AAAAFAAAAKySCgAFAAAAAQAAAMCkNAACAAAAEQAAAMgAAAAAAAAAAQAAAPoAAAAOAAAABTIwMjY6MDg6MjggMTQ6MDM6MTEAAAAA",
    "IwAAAAFOSVRJRCAzNW1tIGYvMS44AAAABAABAAIAAAACUwAAAAACAAUAAAADAAABEAADAAIAAAACVwAAAAAEAAUAAAADAAABKAAA",
    "AAAAAAAZAAAAAQAAAA8AAAABAAAE0QAAABkAAAA5AAAAAQAAACIAAAABAAADPwAAABn/2wBDAAoHBwgHBgoICAgLCgoLDhgQDg0N",
    "Dh0VFhEYIx8lJCIfIiEmKzcvJik0KSEiMEExNDk7Pj4+JS5ESUM8SDc9Pjv/2wBDAQoLCw4NDhwQEBw7KCIoOzs7Ozs7Ozs7Ozs7",
    "Ozs7Ozs7Ozs7Ozs7Ozs7Ozs7Ozs7Ozs7Ozs7Ozs7Ozs7Ozs7Ozv/wAARCAAQABADASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAA",
    "AAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkK",
    "FhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ip",
    "qrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQF",
    "BgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRom",
    "JygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3",
    "uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwDFooorzzuP/9k=",
);

#[test]
fn a_third_party_file_reads_the_same_way_ours_does() {
    let bytes = from_base64(PILLOW_JPEG);
    assert_eq!(&bytes[..3], &[0xFF, 0xD8, 0xFF], "the fixture is not a JPEG");

    let metadata = nitid::testing::read_metadata(&bytes);
    let value = |label: &str| metadata.camera.iter().find(|entry| entry.label == label).map(|entry| entry.value.clone());

    // The same expectations the hand-built fixture is held to, against bytes
    // this project did not lay out.
    assert_eq!(value("Camera").as_deref(), Some("NITID Probe One"));
    assert_eq!(value("Lens").as_deref(), Some("NITID 35mm f/1.8"));
    assert_eq!(value("Exposure").as_deref(), Some("1/250 s"));
    assert_eq!(value("Aperture").as_deref(), Some("f/2.8"));
    assert_eq!(value("ISO").as_deref(), Some("400"));
    assert_eq!(value("Taken").as_deref(), Some("2026-08-28 14:03:11"));

    // And the place, which is the field a click copies.
    let place = metadata.location.expect("the fixture carries GPS");
    assert!((place.latitude - -25.2637).abs() < 0.001, "latitude was {}", place.latitude);
    assert!((place.longitude - -57.5759).abs() < 0.001, "longitude was {}", place.longitude);
}

/// Decoding a file must carry its metadata with it.
///
/// Found by mutation: with only `metadata::read` under test, dropping the call
/// from the decode path passed all 338 tests — the reader worked and nothing
/// used it, so the panel would have been permanently empty. This asks the
/// question the panel asks: decode a photograph, and see what it says.
#[test]
fn decoding_a_photograph_carries_what_it_says_about_itself() {
    let bytes = from_base64(PILLOW_JPEG);
    let decoded = nitid::testing::decode_here(&bytes).expect("the fixture decodes");

    assert!(
        !decoded.metadata.camera.is_empty(),
        "the decoded image carries no metadata, so the Info panel would be empty",
    );
    assert!(
        decoded.metadata.camera.iter().any(|entry| entry.label == "Camera"),
        "the camera did not survive the decode: {:?}",
        decoded.metadata.camera,
    );
    assert!(decoded.metadata.location.is_some(), "the place did not survive the decode");
}

fn from_base64(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = Vec::new();
    let mut accumulator: u32 = 0;
    let mut bits = 0;
    for byte in text.bytes().filter(|byte| *byte != b'=') {
        let value = ALPHABET.iter().position(|candidate| *candidate == byte).expect("a base64 character") as u32;
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    out
}

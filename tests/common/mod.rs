//! What more than one gate needs: fixtures kept as text, because the suite
//! ships no binary files of its own making.

/// A small HEIC with a thumbnail item beside the picture, as libheif writes
/// one and as a phone does.
///
/// Small on purpose: this gate is about the startup path, and a photograph-
/// sized HEVC payload would spend the measurement on the decoder rather than
/// on what the test is watching. Kept as text because the suite ships no
/// binary files.
pub fn heic_fixture() -> Vec<u8> {
    const BASE64: &str = concat!(
        "AAAAHGZ0eXBoZWljAAAAAG1pZjFoZWljbWlhZgAAAmJtZXRhAAAAAAAAACFoZGxyAAAAAAAAAABwaWN0AAAAAAAAAAAAAAAA",
        "AAAAADRpbG9jAAAAAERAAAIAAQAAAAAChgABAAAAAAAAAfkAAgAAAAAEfwABAAAAAAAAAKsAAAA4aWluZgAAAAAAAgAAABVp",
        "bmZlAgAAAAABAABodmMxAAAAABVpbmZlAgAAAAACAABodmMxAAAAAA5waXRtAAAAAAABAAABoWlwcnAAAAF6aXBjbwAAAHZo",
        "dmNDAQNwAAAAAAAAAAAAHvAA/P34+AAADwNgAAEAGEABDAH//wNwAAADAJAAAAMAAAMAHroCQGEAAQAqQgEBA3AAAAMAkAAA",
        "AwAAAwAeoCCBBZbq5Ka5uAhoMCAAAAMDIAAAAwAhYgABAAZEAcFzwIkAAAATY29scm5jbHgAAQANAAaAAAAAFGlzcGUAAAAA",
        "AAAAQAAAAEAAAAAoY2xhcAAAAEAAAAABAAAAMAAAAAEAAAAAAAAAAv////AAAAACAAAAEHBpeGkAAAAAAwgICAAAAHVodmND",
        "AQNwAAAAAAAAAAAAHvAA/P34+AAADwNgAAEAGEABDAH//wNwAAADAJAAAAMAAAMAHroCQGEAAQApQgEBA3AAAAMAkAAAAwAA",
        "AwAeoCCBBZbqrprm4CGgwIAAAAyAAAADAIRiAAEABkQBwXPBiQAAAChjbGFwAAAAEAAAAAEAAAAMAAAAAf///9AAAAAC////",
        "zAAAAAIAAAAfaXBtYQAAAAAAAAACAAEFgQIDBYQAAgSGAwWHAAAAGmlyZWYAAAAAAAAADnRobWIAAgABAAEAAAKsbWRhdAAA",
        "AfUoAa8GOOllh0y24R+1b38FdWdZTuD5dwhtN6PJqe1YKIOOvBc8+ihN9TE+OXBSE9SxAP0GxfEU/d//MaUXxds9jzT71tY6",
        "UaNk0p//RTlyWRIWPKCJX0/14eyMZXkgmRdNLxm1/BDztF32wn8nvh803+VOL7mR7pTmrrecO9tn/K9XXdx96RWIWiVgq+5J",
        "J97xVdz7AGPjwo/9neVw6g8/Jxp5ehIYYWZtk5O3C3drDDaiRQnutkZ/vkPIENf4h4TXmPxkBZ0vuv8I8xYTlI835zebdZm1",
        "ys4Cc/+Twtm27YrvSzx6fj2nTn0B+Q4SZOhDzPTW/FtJVEcdX/h//wWjQCSdOrNwNJV/20ifUy4yYBQc3EDL0oFOmsDJkJR2",
        "LeJ2WLsAQe9jWj3Zf2z6q5YV4Mks9tPsn/If5BKWH8LCxqPuficC95A7bkvpO/6CB0Xqi9JN6HKiLM1AXBKUJ/d8WH9vFQk8",
        "Ocf3f7relkYi87LGZWka/Ly/aeuCZ/3P5H6bxpBGfTSSKog2AOvGaeqckqkXiOL3AeFxFbt5TRL/DWid3gaIscpUw565rh4N",
        "ZIzUyQGEG6V2q6oT7M9JCaE7ZydC2Y64HjoODeBwqxvBBsBIDvGxqMQDwRPg1ecELx701Jbs/WIgQ9nV3yN80l6jmX1bK/4A",
        "AACnKAGvBjIeyRRQiQKs/NTUAT6FlCZD6UbwABv5nF56+QMX/gPVP0xOkl3jXeehAf9tmAXeka/8Joxx+T9T9KM6U8Gh6Vhl",
        "Rb3H+A606130rjgTHYIETU5ovmwv6o2jez/qvl9Acs/wfYJ11sZlrK/i4eU6199ApGfuY14gKMQU9L0v04HBj5VS2bftL+We",
        "Re971BP/RVjqIPldaiYgJ66SerO98+4BMLg=",
    );

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = Vec::new();
    let mut accumulator: u32 = 0;
    let mut bits = 0;
    for byte in BASE64.bytes().filter(|byte| *byte != b'=') {
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

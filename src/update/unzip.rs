//! Taking the executables out of a release's zip.
//!
//! About a hundred lines over `flate2`, which the viewer already carries, for
//! the one archive this program ever opens: its own release's, whose bytes have
//! matched the published digest before they get here. A zip crate would bring
//! every method and extension the format has grown for the sake of two files.
//!
//! The central directory is what is read, because the records in front of the
//! data may leave their sizes to a descriptor after it. Each file's CRC is
//! checked, and a name is only ever used for its last component, so nothing in
//! an archive can write anywhere but the directory it is unpacked into.

use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use flate2::Crc;
use flate2::read::DeflateDecoder;

const END_OF_DIRECTORY: u32 = 0x0605_4b50;
const DIRECTORY_ENTRY: u32 = 0x0201_4b50;
const LOCAL_ENTRY: u32 = 0x0403_4b50;

/// The fixed part of each record, in bytes.
const END_SIZE: usize = 22;
const ENTRY_SIZE: usize = 46;
const LOCAL_SIZE: usize = 30;

const STORED: u16 = 0;
const DEFLATED: u16 = 8;

/// The largest a single file may unpack to. Well above any executable here,
/// and the end of a file that claims more.
const LARGEST_FILE: u32 = 512 * 1024 * 1024;

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// One file the directory lists.
struct Entry<'a> {
    name: &'a [u8],
    method: u16,
    encrypted: bool,
    crc: u32,
    packed: u32,
    size: u32,
    offset: u32,
}

/// Where the directory starts and how many entries it has.
fn directory(archive: &[u8]) -> Option<(usize, usize)> {
    if archive.len() < END_SIZE {
        return None;
    }
    // From the end, because a comment may follow the record, and it is the
    // last signature that counts.
    let earliest = archive.len().saturating_sub(END_SIZE + usize::from(u16::MAX));
    let end = (earliest..=archive.len() - END_SIZE)
        .rev()
        .find(|&at| u32_at(archive, at) == Some(END_OF_DIRECTORY))?;
    let count = u16_at(archive, end + 10)?;
    let start = u32_at(archive, end + 16)?;
    Some((start as usize, usize::from(count)))
}

fn entries(archive: &[u8]) -> Option<Vec<Entry<'_>>> {
    let (mut at, count) = directory(archive)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if u32_at(archive, at)? != DIRECTORY_ENTRY {
            return None;
        }
        let name_length = usize::from(u16_at(archive, at + 28)?);
        let extra_length = usize::from(u16_at(archive, at + 30)?);
        let comment_length = usize::from(u16_at(archive, at + 32)?);
        out.push(Entry {
            name: archive.get(at + ENTRY_SIZE..at + ENTRY_SIZE + name_length)?,
            method: u16_at(archive, at + 10)?,
            encrypted: u16_at(archive, at + 8)? & 1 != 0,
            crc: u32_at(archive, at + 16)?,
            packed: u32_at(archive, at + 20)?,
            size: u32_at(archive, at + 24)?,
            offset: u32_at(archive, at + 42)?,
        });
        at += ENTRY_SIZE + name_length + extra_length + comment_length;
    }
    Some(out)
}

/// The last component of a name in an archive, whichever separator it uses.
fn last_component(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| matches!(byte, b'/' | b'\\')).next().unwrap_or(name)
}

/// The bytes an entry holds, unpacked and checked.
fn contents(archive: &[u8], entry: &Entry<'_>) -> Result<Vec<u8>> {
    if entry.encrypted {
        bail!("it is encrypted");
    }
    if entry.size > LARGEST_FILE {
        bail!("it claims {} bytes", entry.size);
    }

    let at = entry.offset as usize;
    if u32_at(archive, at) != Some(LOCAL_ENTRY) {
        bail!("the directory points at something else");
    }
    // The name and extra fields in front of the data may differ in length
    // from the directory's, so the record's own say where the data starts.
    let name_length = u16_at(archive, at + 26).context("the record is cut short")?;
    let extra_length = u16_at(archive, at + 28).context("the record is cut short")?;
    let start = at + LOCAL_SIZE + usize::from(name_length) + usize::from(extra_length);
    let packed = archive
        .get(start..start.saturating_add(entry.packed as usize))
        .context("the data runs past the end of the archive")?;

    let mut out = Vec::with_capacity(entry.size as usize);
    match entry.method {
        STORED => out.extend_from_slice(packed),
        // One byte past the declared size is enough to tell it lied, and it
        // stops a small file from unpacking into the whole disk.
        DEFLATED => {
            DeflateDecoder::new(packed)
                .take(u64::from(entry.size) + 1)
                .read_to_end(&mut out)
                .context("it does not inflate")?;
        }
        other => bail!("it is packed by method {other}, which this program does not read"),
    }

    if out.len() != entry.size as usize {
        bail!("it is {} bytes and said {}", out.len(), entry.size);
    }
    let mut crc = Crc::new();
    crc.update(&out);
    if crc.sum() != entry.crc {
        bail!("its checksum does not match");
    }
    Ok(out)
}

/// Unpack exactly the files `wanted` names into `into`.
///
/// Wherever in the archive they sit: its layout is the release workflow's to
/// choose. All of them or an error, and a name found twice is an error too,
/// because which one is the program would be a guess.
pub fn unpack(archive: &[u8], wanted: &[&str], into: &Path) -> Result<()> {
    let listed = entries(archive).context("this is not a zip this program can read")?;
    for name in wanted {
        let mut matching = listed.iter().filter(|entry| last_component(entry.name) == name.as_bytes());
        let entry = matching.next().with_context(|| format!("the archive has no {name}"))?;
        if matching.next().is_some() {
            bail!("the archive has {name} twice");
        }
        let bytes = contents(archive, entry).with_context(|| format!("{name} in the archive"))?;
        let target = into.join(name);
        fs::write(&target, bytes).with_context(|| format!("writing {}", target.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::DeflateEncoder;

    use super::*;

    /// An archive built by hand, one way of storing per file.
    fn archive(files: &[(&str, &[u8], u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data, method) in files {
            let packed = match *method {
                DEFLATED => {
                    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
                    encoder.write_all(data).unwrap();
                    encoder.finish().unwrap()
                }
                _ => data.to_vec(),
            };
            let mut crc = Crc::new();
            crc.update(data);
            let offset = out.len() as u32;

            out.extend_from_slice(&LOCAL_ENTRY.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 0]);
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&crc.sum().to_le_bytes());
            out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&packed);

            central.extend_from_slice(&DIRECTORY_ENTRY.to_le_bytes());
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
            central.extend_from_slice(&method.to_le_bytes());
            central.extend_from_slice(&[0; 4]);
            central.extend_from_slice(&crc.sum().to_le_bytes());
            central.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0; 12]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let start = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&END_OF_DIRECTORY.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    /// What the release workflow's archiver writes, from a program other than
    /// the writer above: `7z a seven-zip.zip ./dist/*`, as `release.yml` runs
    /// it. Kept as text, because the suite ships almost no binary files.
    fn from_seven_zip() -> Vec<u8> {
        let text = include_str!("../../tests/fixtures/seven-zip.zip.b64");
        let clean: Vec<u8> = text.bytes().filter(|byte| !byte.is_ascii_whitespace() && *byte != b'=').collect();
        let value = |byte: u8| match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            _ => 63,
        };
        let mut out = Vec::new();
        for group in clean.chunks(4) {
            let mut bits = 0u32;
            for byte in group {
                bits = (bits << 6) | u32::from(value(*byte));
            }
            bits <<= 6 * (4 - group.len());
            let bytes = bits.to_be_bytes();
            out.extend_from_slice(&bytes[1..group.len()]);
        }
        out
    }

    #[test]
    fn a_file_comes_out_whichever_way_it_was_packed() {
        let dir = tempfile::tempdir().unwrap();
        let body = vec![7u8; 100_000];
        let zip = archive(&[
            ("stored.exe", b"plain", STORED),
            ("deflated.exe", &body, DEFLATED),
            ("README.md", b"leave me", STORED),
        ]);

        unpack(&zip, &["stored.exe", "deflated.exe"], dir.path()).expect("unpacking");

        assert_eq!(fs::read(dir.path().join("stored.exe")).unwrap(), b"plain");
        assert_eq!(fs::read(dir.path().join("deflated.exe")).unwrap(), body);
        assert!(!dir.path().join("README.md").exists(), "a file nobody asked for came out");
    }

    /// The real archiver, not the writer above: what is read in a release is
    /// what 7-Zip makes, and it has to be read here before a release is the
    /// first to find out.
    #[test]
    fn what_seven_zip_writes_is_read() {
        let dir = tempfile::tempdir().unwrap();
        unpack(&from_seven_zip(), &["nitid.exe", "nitidw.exe"], dir.path()).expect("unpacking 7-Zip's archive");
        let console = fs::read(dir.path().join("nitid.exe")).unwrap();
        let windowed = fs::read(dir.path().join("nitidw.exe")).unwrap();
        assert!(console.starts_with(b"the console binary"), "{:?}", String::from_utf8_lossy(&console));
        assert!(windowed.starts_with(b"the windowed binary"), "{:?}", String::from_utf8_lossy(&windowed));
    }

    /// A release's layout is its workflow's business; a name in an archive is
    /// never allowed to choose where a file lands.
    #[test]
    fn only_the_last_part_of_a_name_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        fs::create_dir(&inner).unwrap();
        let zip = archive(&[("../../dist/nitid.exe", b"found", STORED)]);

        unpack(&zip, &["nitid.exe"], &inner).expect("unpacking");

        assert_eq!(fs::read(inner.join("nitid.exe")).unwrap(), b"found");
        assert!(!dir.path().join("nitid.exe").exists(), "a name walked out of the directory");
    }

    #[test]
    fn a_missing_or_repeated_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(unpack(&archive(&[("a.exe", b"a", STORED)]), &["a.exe", "b.exe"], dir.path()).is_err());
        assert!(unpack(&archive(&[("x/a.exe", b"1", STORED), ("y/a.exe", b"2", STORED)]), &["a.exe"], dir.path()).is_err());
    }

    /// One flipped byte in the data is caught by the checksum, and nothing
    /// short of it counts as unpacked.
    #[test]
    fn a_damaged_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut zip = archive(&[("a.exe", b"the whole program", STORED)]);
        let at = zip.windows(5).position(|window| window == b"whole").unwrap();
        zip[at] ^= 0xff;
        assert!(unpack(&zip, &["a.exe"], dir.path()).is_err());
    }

    /// A small file claiming to be small and unpacking large stops at its
    /// claim, rather than filling the disk first.
    #[test]
    fn a_file_longer_than_it_said_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let body = vec![0u8; 100_000];
        let mut zip = archive(&[("a.exe", &body, DEFLATED)]);
        // The directory's uncompressed size, in the one entry there is.
        let (start, _) = directory(&zip).unwrap();
        zip[start + 24..start + 28].copy_from_slice(&10u32.to_le_bytes());
        let error = unpack(&zip, &["a.exe"], dir.path()).expect_err("it was taken");
        assert!(format!("{error:#}").contains("said 10"), "{error:#}");
    }

    #[test]
    fn what_is_not_a_zip_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        for wrong in [&b""[..], b"PK", b"this is not an archive, whatever its name says"] {
            assert!(unpack(wrong, &["a.exe"], dir.path()).is_err());
        }
    }
}

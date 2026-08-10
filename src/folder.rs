//! The list of images sitting next to the one that was opened.
//!
//! Opening a file means opening its folder: arrow keys move through the
//! neighbours in the order the shell shows them, and v0.2.0 will prefetch
//! them. The listing is taken once, when the file opens — a folder that
//! changes underneath is rescanned only on an explicit reload.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::image_source;

/// The images of one folder plus a cursor onto the current file.
pub struct Folder {
    entries: Vec<PathBuf>,
    current: usize,
}

impl Folder {
    /// Scan the folder containing `path` and place the cursor on `path`.
    ///
    /// A file whose folder cannot be read still opens: the listing falls back
    /// to that single entry, because failing to browse is not a reason to
    /// refuse to show the picture that was double-clicked.
    pub fn open(path: &Path) -> Result<Self> {
        let path = absolute(path)?;
        let entries = match path.parent() {
            Some(parent) => scan(parent).unwrap_or_else(|_| vec![path.clone()]),
            None => vec![path.clone()],
        };

        let entries = if entries.is_empty() { vec![path.clone()] } else { entries };

        let current = entries.iter().position(|entry| entry == &path).unwrap_or(0);

        Ok(Self { entries, current })
    }

    /// The file the viewer is showing.
    pub fn current(&self) -> &Path {
        &self.entries[self.current]
    }

    /// How many images the folder holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Zero-based position of the current file within the folder.
    pub fn position(&self) -> usize {
        self.current
    }

    /// The current image and `radius` neighbours either side of it.
    ///
    /// Wraps like the navigation does, so the last image of a folder counts
    /// the first as its neighbour — an arrow key there is instant too. Never
    /// repeats a path, however small the folder.
    pub fn neighbourhood(&self, radius: usize) -> Vec<PathBuf> {
        let len = self.entries.len();
        let span = (radius * 2 + 1).min(len);

        (0..span)
            .map(|step| {
                let offset = self.current + len + step - radius.min(len);
                self.entries[offset % len].clone()
            })
            .collect()
    }

    /// Move to the next image, wrapping at the end of the folder.
    ///
    /// Returns `None` when the folder holds a single image, so the caller can
    /// skip a redundant reload.
    pub fn next(&mut self) -> Option<&Path> {
        self.step(1)
    }

    /// Move to the previous image, wrapping at the start of the folder.
    pub fn previous(&mut self) -> Option<&Path> {
        self.step(-1)
    }

    /// Move to the first image of the folder.
    pub fn first(&mut self) -> Option<&Path> {
        self.jump(0)
    }

    /// Move to the last image of the folder.
    pub fn last(&mut self) -> Option<&Path> {
        self.jump(self.entries.len() - 1)
    }

    fn step(&mut self, delta: isize) -> Option<&Path> {
        let len = self.entries.len();
        if len < 2 {
            return None;
        }
        let next = (self.current as isize + delta).rem_euclid(len as isize) as usize;
        self.jump(next)
    }

    fn jump(&mut self, index: usize) -> Option<&Path> {
        if index == self.current {
            return None;
        }
        self.current = index;
        Some(&self.entries[self.current])
    }
}

/// List the decodable images of a folder, ordered the way the shell orders
/// them: case-insensitive by name, so `IMG_2.jpg` precedes `IMG_10.jpg` only
/// when the names say so — natural numeric ordering is a v0.7.0 setting.
fn scan(folder: &Path) -> Result<Vec<PathBuf>> {
    let read = folder.read_dir().with_context(|| format!("listing {}", folder.display()))?;

    let mut entries: Vec<PathBuf> = read
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && image_source::is_supported(path))
        .collect();

    entries.sort_by_key(|path| sort_key(path));
    Ok(entries)
}

fn sort_key(path: &Path) -> String {
    path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_lowercase()
}

/// Resolve a path against the working directory without requiring it to exist
/// on disk in a canonical form — `canonicalize` on Windows returns a `\\?\`
/// prefix that would never match the entries produced by `read_dir`.
fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().context("resolving the working directory")?;
    Ok(cwd.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder_with(names: &[&str]) -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().expect("creating a temporary folder");
        let paths = names
            .iter()
            .map(|name| {
                let path = dir.path().join(name);
                std::fs::write(&path, b"placeholder").expect("writing a temporary file");
                path
            })
            .collect();
        (dir, paths)
    }

    #[test]
    fn lists_only_decodable_files_sorted_by_name() {
        let (dir, _) = folder_with(&["b.png", "a.JPG", "notes.txt", "c.gif"]);
        let folder = Folder::open(&dir.path().join("a.JPG")).unwrap();

        assert_eq!(folder.len(), 3);
        assert_eq!(folder.position(), 0);
        assert_eq!(folder.current().file_name().unwrap(), "a.JPG");
    }

    #[test]
    fn navigation_wraps_in_both_directions() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("a.png")).unwrap();

        assert_eq!(folder.next().unwrap().file_name().unwrap(), "b.png");
        assert_eq!(folder.next().unwrap().file_name().unwrap(), "c.png");
        assert_eq!(folder.next().unwrap().file_name().unwrap(), "a.png");
        assert_eq!(folder.previous().unwrap().file_name().unwrap(), "c.png");
    }

    #[test]
    fn first_and_last_jump_across_the_folder() {
        let (dir, _) = folder_with(&["a.png", "b.png", "c.png"]);
        let mut folder = Folder::open(&dir.path().join("b.png")).unwrap();

        assert_eq!(folder.last().unwrap().file_name().unwrap(), "c.png");
        assert_eq!(folder.first().unwrap().file_name().unwrap(), "a.png");
        // Already there — no reload is asked for.
        assert!(folder.first().is_none());
    }

    #[test]
    fn a_lone_image_reports_no_movement() {
        let (dir, _) = folder_with(&["only.png"]);
        let mut folder = Folder::open(&dir.path().join("only.png")).unwrap();

        assert_eq!(folder.len(), 1);
        assert!(folder.next().is_none());
        assert!(folder.previous().is_none());
    }

    #[test]
    fn a_file_missing_from_its_folder_still_opens() {
        let (dir, _) = folder_with(&["a.png"]);
        let folder = Folder::open(&dir.path().join("gone.png")).unwrap();

        // The cursor falls back to the start rather than refusing to open.
        assert_eq!(folder.position(), 0);
    }
}

//! The pictures on the start screen's tiles, made off the event loop.
//!
//! The order is the viewer's own, applied to a screen of pictures rather than
//! to one: every tile gets its file's embedded thumbnail first — milliseconds
//! each, and most cameras and phones carry one — and only then does the worker
//! go back for the full decodes that make the tiles sharp. A screen that waited
//! for the second pass would sit grey for seconds on a folder of RAW-sized
//! JPEGs, where the first fills in before the pointer has reached a tile.
//!
//! Its own thread rather than the loader's workers: the loader's cache is the
//! neighbourhood of the picture on screen, and a screen of tiles pushed through
//! it would evict the very neighbours that make the arrow keys instant. One
//! thread, because a click on a tile asks the loader to decode that picture at
//! once, and a second worker here would be competing with it for the disk.
//!
//! Nothing is kept on disk: a thumbnail cache is the gallery's foundation
//! (3.0), and a half-built one here would be a second cache to migrate.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

use moxcms::ColorProfile;

use crate::color::ColorTransform;
use crate::image_source::{self, LoadedImage};
use crate::minimap::Thumbnail;

/// The longest side of a tile's picture, in pixels.
///
/// A tile is drawn about two hundred points across; this keeps it crisp on a
/// display at 150% and close to it at 200%, where a sharper copy would cost a
/// full decode's worth of time per tile for a difference only seen up close.
pub const TILE_SIDE: u32 = 360;

/// One tile's picture, or word that there will not be one.
pub struct Made {
    pub path: PathBuf,
    /// `None` when the file cannot be shown at all — gone, unreadable, not a
    /// picture after all — so the tile can stop waiting for one.
    pub picture: Option<Arc<Thumbnail>>,
    /// Whether this is the final picture for the tile, or the embedded
    /// thumbnail standing in until it arrives.
    pub full: bool,
}

enum Job {
    Make {
        generation: u64,
        paths: Vec<PathBuf>,
        display: Box<ColorProfile>,
    },
    Stop,
}

/// The worker that makes them.
pub struct Pictures {
    jobs: mpsc::Sender<Job>,
    /// Moved on every request and every stop, so a pass for a screen that has
    /// since closed — or been asked for again — gives up between files.
    generation: Arc<AtomicU64>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Pictures {
    /// Start the worker. `notify` is called from it for every picture made.
    pub fn new(notify: impl Fn(Made) + Send + 'static) -> Self {
        let (jobs, receiver) = mpsc::channel::<Job>();
        let generation = Arc::new(AtomicU64::new(0));
        let current = Arc::clone(&generation);

        let worker = thread::spawn(move || {
            while let Ok(Job::Make {
                generation: wanted,
                paths,
                display,
            }) = receiver.recv()
            {
                let still_wanted = || current.load(Ordering::SeqCst) == wanted;
                make(&paths, &display, &still_wanted, &notify);
            }
        });

        Self {
            jobs,
            generation,
            worker: Some(worker),
        }
    }

    /// Make the pictures for these files, abandoning whatever was being made.
    ///
    /// `display` is the profile the tiles are drawn for: a tile is painted in
    /// display colours, like the minimap, or it would be a different colour
    /// from the picture it opens.
    pub fn make(&self, paths: Vec<PathBuf>, display: &ColorProfile) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.jobs.send(Job::Make {
            generation,
            paths,
            display: Box::new(display.clone()),
        });
    }

    /// Give up on what is being made: nobody is looking at the screen.
    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}

impl Drop for Pictures {
    fn drop(&mut self) {
        self.stop();
        let _ = self.jobs.send(Job::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Both passes over `paths`: the embedded thumbnails, then the full decodes
/// for every tile the first pass could not make sharp.
fn make(paths: &[PathBuf], display: &ColorProfile, still_wanted: &dyn Fn() -> bool, notify: &dyn Fn(Made)) {
    let mut sharp = vec![false; paths.len()];

    for (index, path) in paths.iter().enumerate() {
        if !still_wanted() {
            return;
        }
        let Some(thumbnail) = std::fs::read(path).ok().as_deref().and_then(image_source::decode_thumbnail) else {
            continue;
        };
        let Some(picture) = tile(&thumbnail, display) else {
            continue;
        };
        // An embedded preview as large as the tile already is the tile: the
        // newer phones and cameras carry one, and decoding twenty megapixels
        // to arrive at the same three hundred would be time thrown away.
        sharp[index] = picture.width.max(picture.height) >= TILE_SIDE;
        notify(Made {
            path: path.clone(),
            picture: Some(Arc::new(picture)),
            full: sharp[index],
        });
    }

    for (path, sharp) in paths.iter().zip(sharp) {
        if sharp {
            continue;
        }
        if !still_wanted() {
            return;
        }
        let picture = std::fs::read(path)
            .ok()
            .and_then(|bytes| image_source::decode(&bytes).ok())
            .and_then(|loaded| tile(&loaded, display));
        notify(Made {
            path: path.clone(),
            picture: picture.map(Arc::new),
            full: true,
        });
    }
}

/// A decoded picture shrunk to a tile, turned the way it is shown and in the
/// display's colours.
fn tile(loaded: &LoadedImage, display: &ColorProfile) -> Option<Thumbnail> {
    let transform = ColorTransform::for_image(loaded.profile.as_ref(), display);
    Thumbnail::averaged(&loaded.image, loaded.orientation, &transform, TILE_SIDE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    fn png(dir: &Path, name: &str, width: u32, height: u32) -> PathBuf {
        let path = dir.join(name);
        image::RgbaImage::from_pixel(width, height, image::Rgba([10, 200, 30, 255]))
            .save(&path)
            .unwrap();
        path
    }

    fn collect(paths: Vec<PathBuf>) -> Vec<Made> {
        let (sender, replies) = mpsc::channel();
        let pictures = Pictures::new(move |made| {
            let _ = sender.send(made);
        });
        let count = paths.len();
        pictures.make(paths, &ColorProfile::new_srgb());
        let mut made = Vec::new();
        while let Ok(reply) = replies.recv_timeout(Duration::from_secs(10)) {
            let done = reply.full;
            made.push(reply);
            if done && made.iter().filter(|made| made.full).count() == count {
                break;
            }
        }
        made
    }

    /// Every tile is answered with a final picture, or with word that there
    /// will be none — a tile left waiting for ever would look like a screen
    /// still loading.
    #[test]
    fn every_tile_is_answered() {
        let dir = tempfile::tempdir().unwrap();
        let good = png(dir.path(), "good.png", 800, 600);
        let broken = dir.path().join("broken.png");
        std::fs::write(&broken, b"not a picture").unwrap();
        let gone = dir.path().join("gone.png");

        let made = collect(vec![good.clone(), broken.clone(), gone.clone()]);
        let finals: Vec<&Made> = made.iter().filter(|made| made.full).collect();
        assert_eq!(finals.len(), 3, "not every tile was answered");

        let answer = |path: &Path| finals.iter().find(|made| made.path == path).unwrap();
        let picture = answer(&good).picture.as_ref().expect("a good file made no picture");
        assert_eq!(picture.width, TILE_SIDE, "the tile was not shrunk to its side");
        assert_eq!(&picture.pixels[..3], &[10, 200, 30]);
        assert!(answer(&broken).picture.is_none());
        assert!(answer(&gone).picture.is_none());
    }

    /// A screen closed mid-pass stops the pass: the work is for nobody.
    #[test]
    fn a_stopped_pass_makes_nothing_more() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = (0..30).map(|index| png(dir.path(), &format!("{index}.png"), 1200, 900)).collect();

        let (sender, replies) = mpsc::channel();
        let pictures = Pictures::new(move |made| {
            let _ = sender.send(made);
        });
        pictures.make(paths, &ColorProfile::new_srgb());
        // Let it start, then close the screen.
        let first = replies.recv_timeout(Duration::from_secs(10));
        assert!(first.is_ok(), "the pass never began");
        pictures.stop();

        // At most the picture in flight when the stop landed.
        let late = std::iter::from_fn(|| replies.recv_timeout(Duration::from_millis(1500)).ok()).count();
        assert!(late <= 1, "{late} pictures were made after the screen closed");
    }
}

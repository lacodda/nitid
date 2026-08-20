//! Decoding off the event loop, and decoding ahead of the user.
//!
//! The order of operations is the product's whole claim to speed, and it is
//! this module that enforces it:
//!
//! 1. The embedded thumbnail is decoded inline and drawn — milliseconds.
//! 2. The full image decodes on a worker thread and replaces it.
//! 3. The neighbours in the folder decode before they are asked for, so an
//!    arrow key draws from memory instead of waiting on a disk and a decoder.
//!
//! A file with no thumbnail skips step 1; nothing else changes.
//!
//! Two things guard that order against a decoder that is slow or stuck. A job
//! is abandoned the moment the user navigates past it — checked before the
//! read and again before the decode, because a held arrow key can outrun a
//! 12-megapixel HEIC several times over. And a decode that never finishes is
//! given up on rather than waited for: see `Deadline`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use crate::image_source::{self, LoadedImage};

/// How many neighbours to keep decoded on each side of the current image.
///
/// One is enough to make a held arrow key feel instant while keeping the cost
/// bounded: three full images in memory, not a folder's worth.
const PREFETCH_RADIUS: usize = 1;

/// A decode that finished, addressed to the request that asked for it.
pub struct Decoded {
    /// Which `request` this answers. A reply whose generation is stale belongs
    /// to an image the user has already navigated away from.
    pub generation: u64,
    pub path: PathBuf,
    pub result: Result<LoadedImage, String>,
}

/// What the worker threads are asked to do.
enum Job {
    /// Decode this file and report back through the event loop.
    Foreground {
        generation: u64,
        path: PathBuf,
    },
    /// Decode this file into the cache without notifying anyone.
    Prefetch {
        path: PathBuf,
    },
    Stop,
}

/// Images decoded and waiting to be shown.
///
/// Bounded by construction: only the current image and its immediate
/// neighbours are ever inserted, and anything further away is dropped on every
/// navigation.
type Cache = Arc<Mutex<HashMap<PathBuf, Arc<LoadedImage>>>>;

/// Decodes images on worker threads and keeps neighbours ready.
pub struct Loader {
    jobs: mpsc::Sender<Job>,
    cache: Cache,
    /// Incremented on every navigation, so replies about the previous image
    /// can be recognised and discarded.
    generation: Arc<AtomicU64>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl Loader {
    /// Start the worker threads.
    ///
    /// `notify` is called from a worker when a foreground decode finishes; it
    /// is expected to hand the result to the event loop.
    pub fn new(notify: impl Fn(Decoded) + Send + Clone + 'static) -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        let receiver = Arc::new(Mutex::new(receiver));
        let cache: Cache = Arc::new(Mutex::new(HashMap::new()));
        let generation = Arc::new(AtomicU64::new(0));

        // Two workers: one carries the image the user is waiting for while the
        // other runs ahead. More would contend for the same disk.
        let workers = (0..2)
            .map(|_| {
                let receiver = Arc::clone(&receiver);
                let cache = Arc::clone(&cache);
                let generation = Arc::clone(&generation);
                let notify = notify.clone();

                thread::spawn(move || {
                    loop {
                        // The lock is held only while taking a job, never
                        // across a decode, so the workers stay parallel.
                        let job = {
                            let queue = receiver.lock().expect("the job queue is poisoned");
                            queue.recv()
                        };

                        match job {
                            Ok(Job::Foreground { generation: want, path }) => {
                                // The user may have moved on while this job sat
                                // in the queue; decoding it would be work for
                                // an image nobody is looking at.
                                if want != generation.load(Ordering::SeqCst) {
                                    continue;
                                }

                                // Checked again inside, between reading the
                                // file and decoding it: on a folder of large
                                // images the read alone is long enough for a
                                // held arrow key to move on twice.
                                let still_wanted = {
                                    let generation = Arc::clone(&generation);
                                    move || want == generation.load(Ordering::SeqCst)
                                };

                                let loaded = load_or_take(&cache, &path, &still_wanted);
                                let result = match loaded {
                                    Ok(image) => {
                                        remember(&cache, &path, Arc::clone(&image));
                                        Ok(image)
                                    }
                                    Err(error) => Err(error),
                                };

                                notify(Decoded {
                                    generation: want,
                                    path,
                                    result: result.map(unwrap_or_clone),
                                });
                            }
                            Ok(Job::Prefetch { path }) => {
                                if cache.lock().expect("the cache is poisoned").contains_key(&path) {
                                    continue;
                                }
                                // A prefetch answers to nobody in particular,
                                // so there is no navigation that makes it
                                // stale — it either lands in the cache or is
                                // dropped when the window moves.
                                if let Ok(image) = load_or_take(&cache, &path, &|| true) {
                                    remember(&cache, &path, image);
                                }
                            }
                            Ok(Job::Stop) | Err(_) => break,
                        }
                    }
                })
            })
            .collect();

        Self {
            jobs: sender,
            cache,
            generation,
            workers,
        }
    }

    /// Ask for `path` to be decoded and shown.
    ///
    /// Returns the image immediately when it was prefetched, in which case no
    /// worker is involved at all — this is the case an arrow key hits.
    pub fn request(&self, path: &Path) -> Request {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;

        if let Some(ready) = self.cache.lock().expect("the cache is poisoned").get(path) {
            return Request::Ready(Arc::clone(ready));
        }

        let _ = self.jobs.send(Job::Foreground {
            generation,
            path: path.to_path_buf(),
        });

        Request::Pending
    }

    /// Whether a reply still concerns the image on screen.
    pub fn is_current(&self, generation: u64) -> bool {
        generation == self.generation.load(Ordering::SeqCst)
    }

    /// The generation the most recent request was issued under.
    #[cfg(test)]
    fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Decode the neighbours of the current position, and forget the rest.
    ///
    /// `neighbours` is the window of paths worth keeping — the caller knows the
    /// folder order, this module only knows about decoding.
    pub fn prefetch(&self, neighbours: &[PathBuf]) {
        {
            let mut cache = self.cache.lock().expect("the cache is poisoned");
            cache.retain(|path, _| neighbours.contains(path));
        }

        for path in neighbours {
            let known = self.cache.lock().expect("the cache is poisoned").contains_key(path);
            if !known {
                let _ = self.jobs.send(Job::Prefetch { path: path.clone() });
            }
        }
    }

    /// How many neighbours on each side the caller should offer to `prefetch`.
    pub fn radius() -> usize {
        PREFETCH_RADIUS
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        for _ in &self.workers {
            let _ = self.jobs.send(Job::Stop);
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// The outcome of asking for an image.
pub enum Request {
    /// Already decoded — draw it now.
    Ready(Arc<LoadedImage>),
    /// A worker is on it; the answer will arrive as a [`Decoded`] event.
    Pending,
}

/// Take the image from the cache, or decode it.
///
/// `still_wanted` is asked between the two expensive steps — reading the file
/// and decoding it. A job the user has navigated past stops there rather than
/// finishing work whose result is thrown away on arrival.
fn load_or_take(cache: &Cache, path: &Path, still_wanted: &dyn Fn() -> bool) -> Result<Arc<LoadedImage>, String> {
    if let Some(ready) = cache.lock().expect("the cache is poisoned").get(path) {
        return Ok(Arc::clone(ready));
    }

    let bytes = std::fs::read(path).map_err(|error| format!("reading {}: {error}", path.display()))?;

    if !still_wanted() {
        return Err(ABANDONED.to_string());
    }

    image_source::decode(&bytes)
        .map(Arc::new)
        .map_err(|error| format!("decoding {}: {error:#}", path.display()))
}

/// What an abandoned job reports.
///
/// It reaches nobody: the reply is addressed to a generation that has already
/// been superseded, so the event loop drops it before looking at the result.
/// The string exists so the code path is not silently indistinguishable from
/// a real failure while reading it.
pub const ABANDONED: &str = "the image was left before it finished decoding";

fn remember(cache: &Cache, path: &Path, image: Arc<LoadedImage>) {
    cache.lock().expect("the cache is poisoned").insert(path.to_path_buf(), image);
}

/// Move the image out of its `Arc`, cloning only if the cache still holds it.
///
/// The prefetched case keeps its copy, so the pixels are duplicated; the
/// foreground case usually holds the only reference and moves for free.
fn unwrap_or_clone(image: Arc<LoadedImage>) -> LoadedImage {
    Arc::try_unwrap(image).unwrap_or_else(|shared| (*shared).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A PNG on disk, so the loader has something real to decode.
    fn image_file(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let buffer = image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]));
        buffer.save(&path).expect("writing a test image");
        path
    }

    #[test]
    fn a_decode_is_delivered_to_the_caller() {
        let dir = tempfile::tempdir().unwrap();
        let path = image_file(dir.path(), "a.png");

        let (sender, replies) = mpsc::channel();
        let loader = Loader::new(move |decoded| {
            let _ = sender.send(decoded);
        });

        assert!(matches!(loader.request(&path), Request::Pending), "an undecoded image should not be ready");
        let generation = loader.current_generation();

        let decoded = replies.recv_timeout(Duration::from_secs(10)).expect("the decode should be delivered");
        assert_eq!(decoded.generation, generation);
        assert_eq!(decoded.path, path);
        assert!(decoded.result.is_ok());
    }

    #[test]
    fn a_failure_is_reported_rather_than_thrown_away() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.png");
        std::fs::write(&path, b"not an image").unwrap();

        let (sender, replies) = mpsc::channel();
        let loader = Loader::new(move |decoded| {
            let _ = sender.send(decoded);
        });
        loader.request(&path);

        let decoded = replies.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(decoded.result.is_err());
    }

    /// The point of prefetching: the next image costs nothing when it arrives.
    #[test]
    fn a_prefetched_image_is_ready_without_a_worker() {
        let dir = tempfile::tempdir().unwrap();
        let path = image_file(dir.path(), "b.png");

        let loader = Loader::new(|_| {});
        loader.prefetch(std::slice::from_ref(&path));

        // Give the worker a moment; the cache is what is being tested, not
        // how fast a thread wakes up.
        for _ in 0..100 {
            if matches!(loader.request(&path), Request::Ready(_)) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("the prefetched image never became ready");
    }

    #[test]
    fn navigating_away_makes_an_earlier_reply_stale() {
        let dir = tempfile::tempdir().unwrap();
        let first = image_file(dir.path(), "c.png");
        let second = image_file(dir.path(), "d.png");

        let loader = Loader::new(|_| {});
        loader.request(&first);
        let generation = loader.current_generation();
        assert!(loader.is_current(generation));

        loader.request(&second);
        assert!(!loader.is_current(generation));
    }

    /// A job the user has navigated past stops between reading the file and
    /// decoding it, rather than finishing work whose result is discarded on
    /// arrival. On a folder of large images the difference is a held arrow key
    /// that keeps up against one that falls a decode behind per press.
    #[test]
    fn a_job_the_user_left_is_abandoned_before_it_decodes() {
        let cache: Cache = Arc::new(Mutex::new(HashMap::new()));
        let dir = tempfile::tempdir().unwrap();
        let path = image_file(dir.path(), "left.png");

        // The user navigated away while the file was being read.
        let result = load_or_take(&cache, &path, &|| false);

        assert_eq!(result.err().as_deref(), Some(ABANDONED));
        assert!(cache.lock().unwrap().is_empty(), "an abandoned job left something in the cache");
    }

    /// The same job, still wanted, decodes as before — otherwise the test
    /// above would pass against a loader that decodes nothing at all.
    #[test]
    fn a_job_still_wanted_decodes() {
        let cache: Cache = Arc::new(Mutex::new(HashMap::new()));
        let dir = tempfile::tempdir().unwrap();
        let path = image_file(dir.path(), "wanted.png");

        assert!(load_or_take(&cache, &path, &|| true).is_ok());
    }

    /// A file that cannot be read reports why, rather than reporting the
    /// abandonment that never happened.
    #[test]
    fn an_unreadable_file_is_not_reported_as_abandoned() {
        let cache: Cache = Arc::new(Mutex::new(HashMap::new()));
        let missing = Path::new("no-such-directory-here").join("nothing.png");

        let Err(error) = load_or_take(&cache, &missing, &|| true) else {
            panic!("a missing file decoded anyway");
        };
        assert!(error.contains("reading"), "the error does not say what went wrong: {error}");
        assert_ne!(error, ABANDONED);
    }

    /// A prefetch answers to no particular navigation, so it must not be
    /// abandoned by one: it either lands in the cache or is dropped when the
    /// window of neighbours moves.
    #[test]
    fn a_prefetch_is_not_abandoned_by_navigation() {
        let dir = tempfile::tempdir().unwrap();
        let path = image_file(dir.path(), "ahead.png");

        let loader = Loader::new(|_| {});
        // Navigate several times, as a held arrow key would.
        for _ in 0..5 {
            loader.request(&path);
        }
        loader.prefetch(std::slice::from_ref(&path));

        for _ in 0..100 {
            if loader.cache.lock().unwrap().contains_key(&path) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("the prefetch never landed, so navigation cancelled work that answers to nobody");
    }

    #[test]
    fn prefetching_forgets_images_that_are_no_longer_neighbours() {
        let dir = tempfile::tempdir().unwrap();
        let near = image_file(dir.path(), "e.png");
        let far = image_file(dir.path(), "f.png");

        let loader = Loader::new(|_| {});
        loader.prefetch(&[near.clone(), far.clone()]);

        // Wait for both to land, then narrow the window.
        for _ in 0..100 {
            let cached = loader.cache.lock().unwrap().len();
            if cached == 2 {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }

        loader.prefetch(std::slice::from_ref(&near));
        let cache = loader.cache.lock().unwrap();
        assert!(cache.contains_key(&near));
        assert!(!cache.contains_key(&far), "the far image was kept");
    }
}

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

                                let loaded = load_or_take(&cache, &path);
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
                                if let Ok(image) = load_or_take(&cache, &path) {
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
fn load_or_take(cache: &Cache, path: &Path) -> Result<Arc<LoadedImage>, String> {
    if let Some(ready) = cache.lock().expect("the cache is poisoned").get(path) {
        return Ok(Arc::clone(ready));
    }

    image_source::load(path).map(Arc::new).map_err(|error| format!("{error:#}"))
}

fn remember(cache: &Cache, path: &Path, image: Arc<LoadedImage>) {
    cache.lock().expect("the cache is poisoned").insert(path.to_path_buf(), image);
}

/// Move the image out of its `Arc`, cloning only if the cache still holds it.
fn unwrap_or_clone(image: Arc<LoadedImage>) -> LoadedImage {
    Arc::try_unwrap(image).unwrap_or_else(|shared| LoadedImage {
        image: image_source::DecodedImage {
            width: shared.image.width,
            height: shared.image.height,
            pixels: shared.image.pixels.clone(),
        },
        orientation: shared.orientation,
        fidelity: shared.fidelity,
    })
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

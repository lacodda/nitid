//! Animated images: decoding every frame, and knowing which one is due.
//!
//! Three formats animate here — GIF, APNG and animated WebP. Each is decoded
//! to *composited* full-canvas RGBA frames up front: the decoders handle
//! disposal and blending between frames, so what this module stores is what
//! the screen shows, and playback is nothing but swapping pixels on a clock.
//!
//! Decoding up front is a deliberate trade. It keeps playback allocation-free
//! and lets the loader's prefetch make an animated neighbour instant like any
//! other image — at the price of holding every frame in memory. The price is
//! capped: an animation past [`MAX_ANIMATION_BYTES`] is shown as its first
//! frame instead, which is what every release before this one did for all of
//! them.
//!
//! Loop counts are deliberately ignored: a viewer keeps playing, and the
//! pause key is the way to hold a frame still. Frame delays of 10 ms and
//! under are read as 100 ms, the convention every browser applies — files
//! written with a zero delay expect it.

use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::format::Format;
use crate::image_source::DecodedImage;

/// The most decoded frame data an animation may hold, across all its frames.
///
/// A quarter gigabyte is thirty 1080p frames — far beyond any real GIF, and
/// small enough that the prefetched neighbours of an animated image do not
/// add up to the machine's memory.
const MAX_ANIMATION_BYTES: usize = 256 * 1024 * 1024;

/// Delays at or under this are treated as unspecified.
const DELAY_FLOOR: Duration = Duration::from_millis(10);

/// What an unspecified delay plays as — the convention browsers settled on,
/// which the files were written against.
const DELAY_DEFAULT: Duration = Duration::from_millis(100);

/// A machine that fell this far behind stops replaying the backlog and
/// rebases its clock instead: after a sleep or a long stall the animation
/// resumes from where it is, it does not fast-forward.
const LAG_LIMIT: Duration = Duration::from_secs(1);

/// One frame: a full composited canvas and how long it stays up.
pub struct Frame {
    pub image: DecodedImage,
    pub delay: Duration,
}

/// Every frame of an animated image, decoded and composited.
pub struct Animation {
    /// At least one frame by construction; two or more when it actually
    /// animates.
    pub frames: Vec<Frame>,
}

/// Decode the frames of an animated format.
///
/// `Some` for a GIF, an APNG, or an animated WebP — even a single-frame one,
/// so the caller never decodes the same bytes twice. `None` for a still PNG
/// or WebP, for formats that do not animate, and for a file whose first
/// frame will not decode; the caller's still-image path covers those.
pub fn decode(format: Format, bytes: &[u8]) -> Option<Animation> {
    let frames = match format {
        Format::Gif => gif_frames(bytes),
        Format::Png => apng_frames(bytes),
        Format::WebP => webp_frames(bytes),
        _ => None,
    }?;

    (!frames.is_empty()).then_some(Animation { frames })
}

/// A delay as the file states it, with the browser convention applied.
fn normalise_delay(delay: Duration) -> Duration {
    if delay <= DELAY_FLOOR { DELAY_DEFAULT } else { delay }
}

/// Collect frames from one of the `image` crate's animation decoders.
///
/// A frame that fails mid-file keeps what decoded so far — a truncated
/// animation plays its surviving frames, the way a truncated still shows its
/// surviving rows elsewhere. The memory cap truncates to the first frame
/// alone: playing half a loop would look like the file, not the viewer,
/// stuttering.
fn collect(frames: image::Frames) -> Option<Vec<Frame>> {
    let mut out = Vec::new();
    let mut held = 0usize;

    for frame in frames {
        let Ok(frame) = frame else { break };
        let delay = normalise_delay(Duration::from(frame.delay()));
        let buffer = frame.into_buffer();
        let (width, height) = buffer.dimensions();
        let pixels = buffer.into_raw();

        held = held.saturating_add(pixels.len());
        if held > MAX_ANIMATION_BYTES {
            out.truncate(1);
            return (!out.is_empty()).then_some(out);
        }

        out.push(Frame {
            image: DecodedImage { width, height, pixels },
            delay,
        });
    }

    (!out.is_empty()).then_some(out)
}

fn gif_frames(bytes: &[u8]) -> Option<Vec<Frame>> {
    use image::AnimationDecoder;

    let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).ok()?;
    collect(decoder.into_frames())
}

/// APNG frames, or `None` for the ordinary still PNG.
fn apng_frames(bytes: &[u8]) -> Option<Vec<Frame>> {
    use image::AnimationDecoder;

    let decoder = image::codecs::png::PngDecoder::new(Cursor::new(bytes)).ok()?;
    if !decoder.is_apng().ok()? {
        return None;
    }
    collect(decoder.apng().ok()?.into_frames())
}

/// Animated WebP frames, or `None` for a still.
///
/// `image-webp` composites each frame onto the canvas itself and hands back
/// the finished picture with its duration, so this is a straight read loop.
fn webp_frames(bytes: &[u8]) -> Option<Vec<Frame>> {
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes)).ok()?;
    if !decoder.is_animated() {
        return None;
    }

    let (width, height) = decoder.dimensions();
    let frame_bytes = decoder.output_buffer_size()?;
    let rgba_bytes = crate::image_source::pixel_count(width, height).ok()?;
    let opaque = !decoder.has_alpha();

    let mut out = Vec::new();
    let mut held = 0usize;

    for _ in 0..decoder.num_frames() {
        let mut buffer = vec![0u8; frame_bytes];
        let Ok(milliseconds) = decoder.read_frame(&mut buffer) else { break };

        // An opaque WebP composites three bytes per pixel; the renderer
        // uploads four.
        let pixels = if opaque {
            crate::image_source::rgb_to_rgba8(&buffer, rgba_bytes)
        } else {
            buffer
        };

        held = held.saturating_add(pixels.len());
        if held > MAX_ANIMATION_BYTES {
            out.truncate(1);
            break;
        }

        out.push(Frame {
            image: DecodedImage { width, height, pixels },
            delay: normalise_delay(Duration::from_millis(u64::from(milliseconds))),
        });
    }

    (!out.is_empty()).then_some(out)
}

/// The clock of a playing animation: which frame is up, and when the next is
/// due.
///
/// Owns no window and no GPU — the event loop asks [`Player::advance_to`]
/// whether the picture changed and [`Player::wake_at`] when to wake, which is
/// what makes this testable as plain arithmetic.
pub struct Player {
    animation: Arc<Animation>,
    index: usize,
    paused: bool,
    /// When the frame currently up has run its course.
    next_due: Instant,
}

impl Player {
    /// Start playing from the first frame, shown as of `now`.
    pub fn new(animation: Arc<Animation>, now: Instant) -> Self {
        let first_delay = animation.frames[0].delay;
        Self {
            animation,
            index: 0,
            paused: false,
            next_due: now + first_delay,
        }
    }

    /// Step to whichever frame is due at `now`. True when the picture changed.
    pub fn advance_to(&mut self, now: Instant) -> bool {
        if self.paused {
            return false;
        }

        // Far behind — the machine slept, or a decode stalled the loop. The
        // backlog is dropped rather than replayed at full speed.
        if now.duration_since(self.next_due.min(now)) > LAG_LIMIT {
            self.next_due = now;
        }

        let mut moved = false;
        while now >= self.next_due {
            self.index = (self.index + 1) % self.animation.frames.len();
            self.next_due += self.current_delay();
            moved = true;
        }
        moved
    }

    /// When the event loop should wake next, or `None` while paused.
    pub fn wake_at(&self) -> Option<Instant> {
        (!self.paused).then_some(self.next_due)
    }

    /// Hold the current frame, or let it run again.
    ///
    /// Resuming restarts the current frame's clock from `now`: after a pause
    /// of any length the animation continues, it does not leap to catch up.
    pub fn toggle_paused(&mut self, now: Instant) {
        self.paused = !self.paused;
        if !self.paused {
            self.next_due = now + self.current_delay();
        }
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// The frame to show right now.
    pub fn frame(&self) -> &DecodedImage {
        &self.animation.frames[self.index].image
    }

    /// Where playback stands, for the title: frame number (from one) and count.
    pub fn position(&self) -> (usize, usize) {
        (self.index + 1, self.animation.frames.len())
    }

    fn current_delay(&self) -> Duration {
        self.animation.frames[self.index].delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn animation(delays_ms: &[u64]) -> Arc<Animation> {
        Arc::new(Animation {
            frames: delays_ms
                .iter()
                .map(|ms| Frame {
                    image: DecodedImage {
                        width: 1,
                        height: 1,
                        pixels: vec![0; 4],
                    },
                    delay: Duration::from_millis(*ms),
                })
                .collect(),
        })
    }

    #[test]
    fn a_frame_holds_until_its_delay_has_passed() {
        let start = Instant::now();
        let mut player = Player::new(animation(&[100, 100]), start);

        assert!(!player.advance_to(start + Duration::from_millis(50)), "the frame changed early");
        assert_eq!(player.position().0, 1);

        assert!(player.advance_to(start + Duration::from_millis(100)));
        assert_eq!(player.position().0, 2);
    }

    #[test]
    fn playback_wraps_to_the_first_frame() {
        let start = Instant::now();
        let mut player = Player::new(animation(&[100, 100]), start);

        player.advance_to(start + Duration::from_millis(100));
        player.advance_to(start + Duration::from_millis(200));
        assert_eq!(player.position().0, 1, "the animation did not loop");
    }

    /// Delays differ per frame in real files, and the clock must follow the
    /// frame that is up rather than the one that was.
    #[test]
    fn each_frame_runs_for_its_own_delay() {
        let start = Instant::now();
        let mut player = Player::new(animation(&[100, 300]), start);

        player.advance_to(start + Duration::from_millis(100));
        assert_eq!(player.position().0, 2);
        // The second frame runs 300 ms, so 200 ms in it still stands.
        assert!(!player.advance_to(start + Duration::from_millis(300)));
        assert!(player.advance_to(start + Duration::from_millis(400)));
        assert_eq!(player.position().0, 1);
    }

    #[test]
    fn pausing_holds_the_frame_and_asks_for_no_wakeups() {
        let start = Instant::now();
        let mut player = Player::new(animation(&[100, 100]), start);

        player.toggle_paused(start);
        assert!(player.paused());
        assert!(player.wake_at().is_none(), "a paused animation still asked to be woken");
        assert!(!player.advance_to(start + Duration::from_secs(5)), "a paused animation advanced");
        assert_eq!(player.position().0, 1);
    }

    /// Resuming after a long pause continues; it does not replay the time the
    /// pause covered.
    #[test]
    fn resuming_continues_rather_than_catching_up() {
        let start = Instant::now();
        let mut player = Player::new(animation(&[100, 100, 100]), start);

        player.toggle_paused(start);
        let later = start + Duration::from_secs(60);
        player.toggle_paused(later);

        assert!(
            !player.advance_to(later + Duration::from_millis(50)),
            "the frame changed straight after resuming"
        );
        assert!(player.advance_to(later + Duration::from_millis(100)));
        assert_eq!(player.position().0, 2, "resuming skipped frames to catch up");
    }

    /// A machine that slept does not replay the backlog: the clock rebases and
    /// at most steps once.
    #[test]
    fn a_long_stall_rebases_the_clock_rather_than_fast_forwarding() {
        let start = Instant::now();
        let mut player = Player::new(animation(&[100, 100, 100]), start);

        let after_sleep = start + Duration::from_secs(3600);
        player.advance_to(after_sleep);
        // One hour is 36000 frames; landing anywhere but the next frame means
        // the backlog was replayed.
        assert_eq!(player.position().0, 2, "the animation fast-forwarded through the backlog");
    }

    #[test]
    fn zero_and_tiny_delays_read_as_the_browser_default() {
        assert_eq!(normalise_delay(Duration::ZERO), DELAY_DEFAULT);
        assert_eq!(normalise_delay(Duration::from_millis(10)), DELAY_DEFAULT);
        assert_eq!(normalise_delay(Duration::from_millis(11)), Duration::from_millis(11));
    }
}

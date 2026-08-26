//! The window and the event loop.
//!
//! Redraws are event-driven: a still image costs no GPU time, which is what
//! lets a viewer sit open on a laptop without draining the battery. A playing
//! animation is the one exception, and a bounded one — the loop wakes for its
//! next frame and for nothing else, and pausing restores the silence.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use moxcms::ColorProfile;

use crate::animation::Player;
use crate::color::{self, ColorTransform};
use crate::config::Config;
use crate::folder::Folder;
use crate::gpu::Renderer;
use crate::image_source::{self, Fidelity, LoadedImage, Orientation};
use crate::loader::{Decoded, Loader, Request};
use crate::startup;
use crate::vector::VectorImage;
use crate::view::{FitMode, View};

/// A wheel notch is 120 units of a high-resolution scroll device; a trackpad
/// reports fractions of that, so dividing keeps both feeling the same.
const PIXELS_PER_NOTCH: f32 = 120.0;

/// The window size used before an image sets one.
const DEFAULT_WINDOW: (u32, u32) = (1280, 800);

/// How often the display is asked whether it is still in HDR mode, while the
/// viewer is on an HDR surface. A second is well under the time it takes to
/// look back at the screen after flipping the setting, and 140 µs of it.
const DISPLAY_WATCH_INTERVAL: Duration = Duration::from_secs(1);

/// Run the viewer, optionally opening a file straight away.
pub fn run(paths: Vec<PathBuf>) -> Result<()> {
    start(paths, None)
}

/// The same, for the process that owns the instance pipe.
///
/// The listener is handed over here rather than started earlier because it
/// needs the event loop's proxy to wake the window, and that does not exist
/// until the loop is built.
#[cfg(windows)]
pub fn run_owning(paths: Vec<PathBuf>, listener: crate::single::channel::Listener) -> Result<()> {
    start(paths, Some(listener))
}

/// `listener` is `None` for a viewer that shares no window — either because
/// nothing was claimed, or because this is not Windows, where the whole
/// mechanism lives.
fn start(paths: Vec<PathBuf>, #[cfg(windows)] listener: Option<crate::single::channel::Listener>, #[cfg(not(windows))] listener: Option<()>) -> Result<()> {
    // A user event carries a finished background decode, a change in the
    // display, or files handed over by another instance, back to this thread.
    let event_loop = EventLoop::<Event>::with_user_event().build().context("creating the event loop")?;
    // Wait for input rather than spinning: nothing moves unless the user acts.
    event_loop.set_control_flow(ControlFlow::Wait);

    // A second instance wakes this loop the same way a finished decode does,
    // so nothing has to poll and a still image still costs no wakeups.
    #[cfg(windows)]
    if let Some(listener) = listener {
        let proxy = event_loop.create_proxy();
        listener.listen(move |paths| {
            let _ = proxy.send_event(Event::Open(paths));
        });
    }
    #[cfg(not(windows))]
    let _ = listener;

    let proxy = event_loop.create_proxy();
    let loader = Loader::new(move |decoded| {
        // The loop may already be gone if the window was closed mid-decode;
        // there is nobody left to tell, and that is fine.
        let _ = proxy.send_event(Event::Decoded(Box::new(decoded)));
    });

    let mut app = App::new(paths, loader);
    event_loop.run_app(&mut app).context("running the viewer")?;

    app.into_result()
}

/// What reaches the event loop from elsewhere.
enum Event {
    /// A background decode finished.
    ///
    /// Boxed because a decode carries a whole image and a hand-over carries a
    /// list of paths: without it every event in the queue would be the size of
    /// the larger one.
    Decoded(Box<Decoded>),
    /// Another instance handed these files over rather than opening a window.
    #[cfg(windows)]
    Open(Vec<PathBuf>),
}

/// What the viewer is showing, once a file has been opened.
struct Shown {
    orientation: Orientation,
    view: View,
    /// Whether this is the real image or the thumbnail standing in for it.
    fidelity: Fidelity,
    /// The document a vector image was drawn from, kept so the picture can be
    /// drawn again when the zoom moves. `None` for raster images.
    vector: Option<VectorImage>,
    /// The size the vector image was last drawn at, in physical pixels.
    ///
    /// Compared against the size the current zoom asks for, so a redraw only
    /// happens when it would actually show more detail.
    rasterised_at: (u32, u32),
    /// The clock of an animated image. `None` for a still, which is to say
    /// nearly always.
    player: Option<Player>,
}

struct App {
    /// The files to open once the window exists.
    initial: Vec<PathBuf>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    folder: Option<Folder>,
    shown: Option<Shown>,
    loader: Loader,
    config: Config,
    /// The profile Windows has assigned to the display.
    display_profile: ColorProfile,
    cursor: PhysicalPosition<f64>,
    dragging: bool,
    /// When the display was last asked about its dynamic range. `None` until
    /// the first ask; only set while the viewer is on an HDR surface, which is
    /// the only state that can go stale unannounced.
    display_checked_at: Option<Instant>,
    /// A failure that must end the run; reported after the loop exits, since
    /// `ApplicationHandler` methods cannot return one.
    failure: Option<anyhow::Error>,
}

impl App {
    fn new(initial: Vec<PathBuf>, loader: Loader) -> Self {
        Self {
            initial,
            window: None,
            renderer: None,
            folder: None,
            shown: None,
            loader,
            config: Config::load(),
            display_profile: color::display_profile(),
            cursor: PhysicalPosition::new(0.0, 0.0),
            dragging: false,
            display_checked_at: None,
            failure: None,
        }
    }

    fn into_result(self) -> Result<()> {
        match self.failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Show a file: the quick frame now, the real one as soon as it is ready.
    ///
    /// This is the order of operations the product is built around. Nothing
    /// here blocks on a full decode, so the window keeps answering the user
    /// while a 60-megapixel photo is still being unpacked.
    fn show(&mut self, path: &Path) {
        match self.loader.request(path) {
            // Prefetched: the neighbour the arrow key asked for is already in
            // memory, so it goes up in this frame with no intermediate.
            Request::Ready(image) => self.upload(&image),
            Request::Pending => self.show_quick_frame(path),
        }

        // `refresh` rather than a redraw on its own: a vector image is first
        // drawn at the size its document declares, which is rarely the size it
        // is then fitted to. Drawing it again for the framing it landed in is
        // what makes a 32-pixel icon filling the window look drawn rather than
        // enlarged.
        self.refresh();
        self.prefetch_neighbours();
    }

    /// Open what was named: one file browses its folder, several browse
    /// themselves.
    ///
    /// The two cases differ in what the arrow keys then walk through, and the
    /// difference is the point of multi-select — five chosen files should be
    /// five, not the hundreds sitting beside them.
    ///
    /// The folder is set before the picture is shown because `prefetch` keeps
    /// only the current file's neighbours in the cache: showing first would
    /// have the prefetch of the *old* neighbourhood evict the image just put
    /// on screen.
    fn open(&mut self, paths: &[PathBuf]) {
        let (folder, path) = match paths {
            [] => return,
            [single] => match Folder::open(single) {
                Ok(folder) => {
                    let path = folder.current().to_path_buf();
                    (Some(folder), path)
                }
                Err(error) => {
                    eprintln!("nitid: {error:#}");
                    (None, single.clone())
                }
            },
            several => match Folder::of_selection(several) {
                Some(folder) => {
                    let path = folder.current().to_path_buf();
                    (Some(folder), path)
                }
                // Nothing in the selection is an image this build opens.
                None => return,
            },
        };

        if let Some(folder) = folder {
            self.folder = Some(folder);
        }
        self.show(&path);
    }

    /// Files from another instance, which chose to hand them over rather than
    /// open a second window.
    ///
    /// Windows only, because the hand-over is: see `single`.
    ///
    /// They join what is already being browsed rather than replacing it: the
    /// window was showing something the user was looking at, and arriving
    /// files are an addition to that, not a reason to forget it. The cursor
    /// moves to the first of them, because that is the file that was just
    /// double-clicked.
    #[cfg(windows)]
    fn handed_over(&mut self, paths: Vec<PathBuf>) {
        // A messenger is another process and its message is not trusted: only
        // files that exist are opened, so a stray sender cannot make the
        // window jump to an error for a file nobody chose.
        let paths: Vec<PathBuf> = paths.into_iter().filter(|path| crate::single::openable(path)).collect();
        if paths.is_empty() {
            return;
        }

        match self.folder.as_mut() {
            Some(folder) => match folder.extend(&paths) {
                Some(path) => {
                    let path = path.to_path_buf();
                    self.show(&path);
                }
                None => return,
            },
            // Nothing open yet — the window was started bare.
            None => self.open(&paths),
        }

        self.raise();
    }

    /// Bring the window to the user's attention.
    ///
    /// A hand-over happens because somebody double-clicked a file, and a
    /// window that updates silently behind other windows looks like nothing
    /// happened at all.
    #[cfg(windows)]
    fn raise(&self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        // Restoring first: the window may be minimised, in which case
        // focusing alone leaves it in the taskbar.
        window.set_minimized(false);
        window.focus_window();
    }

    /// Put the embedded thumbnail on screen while the full decode runs.
    ///
    /// Decoding it inline is deliberate: it costs single-digit milliseconds
    /// and handing it to a worker would trade that for a scheduling round trip
    /// — the very delay this exists to avoid.
    fn show_quick_frame(&mut self, path: &Path) {
        let Some(thumbnail) = std::fs::read(path).ok().as_deref().and_then(image_source::decode_thumbnail) else {
            // No thumbnail: the window stays on the previous image, or on the
            // background if this is the first. Both beat a flash of white.
            return;
        };

        self.upload(&thumbnail);
        startup::milestone("thumbnail up");
    }

    fn upload(&mut self, loaded: &LoadedImage) {
        let scale_factor = self.scale_factor();
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let transform = ColorTransform::for_image(loaded.profile.as_ref(), &self.display_profile);
        startup::milestone(if transform.is_identity {
            "colour: no conversion needed"
        } else {
            "colour: converted to the display profile"
        });
        renderer.set_image(&loaded.image, &transform);

        // Replacing a thumbnail with the image it stood in for must not move
        // the picture: the framing carries over, rebased onto the resolution
        // that just arrived, so the swap reads as the picture sharpening.
        let view = match &self.shown {
            Some(shown) if shown.fidelity == Fidelity::Thumbnail => {
                let mut view = shown.view;
                view.rebase(loaded.display_size());
                view
            }
            _ => View::new(loaded.display_size(), renderer.size(), scale_factor),
        };

        self.shown = Some(Shown {
            orientation: loaded.orientation,
            view,
            fidelity: loaded.fidelity,
            vector: loaded.vector.clone(),
            rasterised_at: (loaded.image.width, loaded.image.height),
            // An animated image starts playing the moment it is up; the
            // event loop reads the player's clock in `about_to_wait`.
            player: loaded.animation.clone().map(|animation| Player::new(animation, Instant::now())),
        });
    }

    /// Draw a vector image again for the size it now occupies on screen.
    ///
    /// A raster image is scaled by the GPU when the zoom changes, which is
    /// right: there is no more detail to be had. A vector image has as much
    /// detail as the size it is drawn at, so zooming into a picture rasterised
    /// for a smaller window shows a blur where the format promises a clean
    /// edge.
    ///
    /// The redraw is deliberately not done on every notch of the wheel: it
    /// costs a full rasterisation, and at a smooth zoom that would land on
    /// every frame. It happens when the size on screen has moved far enough
    /// from what was drawn that the difference is visible.
    fn rerasterise_if_needed(&mut self) {
        let Some(shown) = self.shown.as_mut() else {
            return;
        };
        let Some(vector) = shown.vector.clone() else {
            return;
        };

        let (wanted_width, wanted_height) = shown.view.scaled_size();
        let (wanted_width, wanted_height) = (wanted_width.round().max(1.0) as u32, wanted_height.round().max(1.0) as u32);
        if !worth_redrawing(shown.rasterised_at.0, wanted_width) {
            return;
        }

        let Ok(raster) = vector.rasterise(wanted_width, wanted_height) else {
            // A rasterisation that fails leaves the previous one on screen,
            // which is a blurrier picture rather than no picture.
            return;
        };

        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        // A vector image carries no profile, so the transform is the identity
        // one every untagged image gets.
        renderer.set_image(&raster, &ColorTransform::identity());

        // The framing is kept: the picture is the same size on screen, drawn
        // from more pixels. Rebasing tells the view that the source resolution
        // changed underneath it.
        let (raster_width, raster_height) = (raster.width, raster.height);
        shown.view.rebase((raster_width, raster_height));
        shown.rasterised_at = (raster_width, raster_height);
        startup::milestone(&format!("vector redrawn at {raster_width}x{raster_height}"));
    }

    /// Rewrite the title bar: file name, position in the folder, and the zoom
    /// once the user has left the default framing.
    ///
    /// The title carries this until v0.7.0 brings a real status line.
    fn set_title(&self) {
        let Some(window) = &self.window else {
            return;
        };

        let name = self
            .folder
            .as_ref()
            .map(Folder::current)
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("nitid");

        let mut title = name.to_string();
        if let Some(folder) = &self.folder
            && folder.len() > 1
        {
            title.push_str(&format!(" — {} of {}", folder.position() + 1, folder.len()));
        }
        if let Some(shown) = &self.shown
            && shown.view.mode() == FitMode::Free
        {
            title.push_str(&format!(" — {:.0}%", shown.view.scale() * 100.0));
        }
        // The frame counter is the status line this stage has: the toolbar
        // and a real status row arrive with the interface stage.
        if let Some(player) = self.shown.as_ref().and_then(|shown| shown.player.as_ref()) {
            let (frame, count) = player.position();
            title.push_str(&format!(" — frame {frame}/{count}"));
            if player.paused() {
                title.push_str(" — paused");
            }
        }
        title.push_str(" — nitid");

        window.set_title(&title);
    }

    /// Decode the images either side of the current one, ahead of the request.
    ///
    /// This is what makes a held arrow key smooth: by the time the user asks
    /// for the next image, it is already in memory.
    fn prefetch_neighbours(&self) {
        let Some(folder) = &self.folder else {
            return;
        };
        self.loader.prefetch(&folder.neighbourhood(Loader::radius()));
    }

    /// A background decode came back.
    fn decoded(&mut self, decoded: Decoded) {
        // A reply for an image the user has already left is not an error, just
        // work that finished too late to matter.
        if !self.loader.is_current(decoded.generation) {
            return;
        }

        let Decoded { path, result, .. } = decoded;
        match result {
            Ok(image) => {
                startup::milestone("full image up");
                self.upload(&image);
                // `refresh` rather than a redraw on its own: a vector image
                // arrives drawn at its document size and has to be drawn again
                // for the framing it is fitted into.
                self.refresh();
            }
            Err(error) => {
                // A file that will not open is a fact about the file, not a
                // reason to close the viewer: name it and keep the window.
                eprintln!("nitid: {}: {error}", path.display());
            }
        }
    }

    /// Note the window's placement so the next run opens in the same spot.
    ///
    /// A maximised window keeps the size it had before being maximised: the
    /// maximised flag restores the state, and storing the full-screen size
    /// would leave the window stuck at that size once it is restored.
    fn remember_placement(&mut self) {
        let Some(window) = &self.window else {
            return;
        };

        let maximised = window.is_maximized();
        self.config.placement.maximised = maximised;

        if maximised {
            return;
        }

        if let Ok(position) = window.outer_position() {
            self.config.placement.position = Some((position.x, position.y));
        }
        let size = window.inner_size();
        if size.width > 0 && size.height > 0 {
            self.config.placement.size = Some((size.width, size.height));
        }
    }

    /// Physical pixels per logical pixel on the monitor showing the window.
    fn scale_factor(&self) -> f32 {
        self.window.as_ref().map(|window| window.scale_factor() as f32).unwrap_or(1.0)
    }

    /// The framing changed: redraw a vector image for its new size, repaint,
    /// and let the title follow the zoom.
    fn refresh(&mut self) {
        self.rerasterise_if_needed();
        self.set_title();
        self.request_redraw();
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Step through the folder, if a step is possible.
    fn navigate(&mut self, step: Step) {
        let Some(folder) = self.folder.as_mut() else {
            return;
        };
        let next = match step {
            Step::Next => folder.next(),
            Step::Previous => folder.previous(),
            Step::First => folder.first(),
            Step::Last => folder.last(),
        };
        let Some(next) = next.map(Path::to_path_buf) else {
            return;
        };
        self.show(&next);
    }

    fn cursor_position(&self) -> (f32, f32) {
        (self.cursor.x as f32, self.cursor.y as f32)
    }

    fn handle_key(&mut self, key: &Key, event_loop: &ActiveEventLoop) {
        match key {
            Key::Named(NamedKey::Escape) => event_loop.exit(),
            // On an animated image the space bar is its pause; everywhere
            // else it steps to the next file, as it always has.
            Key::Named(NamedKey::Space) => match self.shown.as_mut().and_then(|shown| shown.player.as_mut()) {
                Some(player) => {
                    player.toggle_paused(Instant::now());
                    self.set_title();
                }
                None => self.navigate(Step::Next),
            },
            Key::Named(NamedKey::ArrowRight | NamedKey::PageDown) => {
                self.navigate(Step::Next);
            }
            Key::Named(NamedKey::ArrowLeft | NamedKey::PageUp | NamedKey::Backspace) => {
                self.navigate(Step::Previous);
            }
            Key::Named(NamedKey::Home) => self.navigate(Step::First),
            Key::Named(NamedKey::End) => self.navigate(Step::Last),
            Key::Named(NamedKey::F11) => self.toggle_fullscreen(),
            Key::Character(character) => match character.as_str() {
                "+" | "=" => self.zoom(1.0),
                "-" | "_" => self.zoom(-1.0),
                "0" => self.reframe(Reframe::Fit),
                "1" => self.reframe(Reframe::Actual),
                _ => {}
            },
            _ => {}
        }
    }

    fn zoom(&mut self, notches: f32) {
        // Keyboard zoom has no cursor to anchor to, so it uses the centre.
        let centre = self
            .renderer
            .as_ref()
            .map(|renderer| {
                let (width, height) = renderer.size();
                (width as f32 / 2.0, height as f32 / 2.0)
            })
            .unwrap_or((0.0, 0.0));

        if let Some(shown) = self.shown.as_mut() {
            shown.view.zoom_at(notches, centre);
            self.refresh();
        }
    }

    fn reframe(&mut self, how: Reframe) {
        if let Some(shown) = self.shown.as_mut() {
            match how {
                Reframe::Fit => shown.view.fit(),
                Reframe::Actual => shown.view.set_actual(),
                Reframe::Toggle => shown.view.toggle_fit_actual(),
            }
            self.refresh();
        }
    }

    /// Reconfigure the swapchain if the display's dynamic range has changed.
    ///
    /// Asking costs about 140 microseconds — small next to a decode and far too
    /// much for a frame that would otherwise be free, which is why it happens
    /// on the events that can precede a change rather than on every redraw.
    /// The viewer's idle loop stays asleep either way.
    fn follow_display(&mut self) -> bool {
        self.renderer.as_mut().is_some_and(Renderer::follow_display)
    }

    /// Check on the display when the HDR surface is up and the check is due.
    ///
    /// Turning HDR *off* in Windows announces itself to nothing: measured with
    /// the viewer open, it produced no winit event and no `WM_DISPLAYCHANGE`,
    /// while turning it *on* arrives as `Focused(true)`. Asking is the only
    /// way to notice, and a viewer writing extended-range light into a surface
    /// the compositor has gone back to treating as standard range shows the
    /// wrong picture until something else wakes it.
    ///
    /// So the poll is narrow: only while nitid is itself on an HDR surface —
    /// the state that can go stale — and only once a second, for the 140 µs
    /// the query costs. On a standard-range surface, which is where an SDR
    /// display leaves it, nothing here runs and the loop sleeps as before.
    fn watch_the_display(&mut self) {
        let watching = self.renderer.as_ref().is_some_and(Renderer::is_hdr);
        let now = Instant::now();
        if !display_check_due(watching, self.display_checked_at, now) {
            return;
        }

        self.display_checked_at = Some(now);
        if self.follow_display() {
            self.request_redraw();
        }
    }

    /// How long the loop may sleep with nothing else pending.
    ///
    /// `Wait` — indefinitely — unless the display is being watched, in which
    /// case the next check bounds it.
    fn idle_until(&self) -> ControlFlow {
        let watching = self.renderer.as_ref().is_some_and(Renderer::is_hdr);
        match display_watch_deadline(watching, self.display_checked_at, Instant::now()) {
            Some(due) => ControlFlow::WaitUntil(due),
            None => ControlFlow::Wait,
        }
    }

    fn toggle_fullscreen(&self) {
        let Some(window) = &self.window else {
            return;
        };
        let next = match window.fullscreen() {
            Some(_) => None,
            None => Some(winit::window::Fullscreen::Borderless(None)),
        };
        window.set_fullscreen(next);
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        // With no image open the pass still runs and clears to the background,
        // so the window is never a hole showing whatever was behind it.
        let shown = self.shown.as_ref().map(|shown| (&shown.view, shown.orientation));
        let carries_image = shown.is_some();

        if let Err(error) = renderer.render(shown) {
            self.failure = Some(error);
            event_loop.exit();
            return;
        }

        // The frame is on screen now, and it has a picture in it. An empty
        // window reaching the screen is not what the promise is about.
        if carries_image {
            startup::first_pixels();
            if startup::exit_after_first_frame() {
                event_loop.exit();
            }
        }
    }
}

/// Whether the display should be asked about its dynamic range now.
///
/// Only while the viewer is on an HDR surface — a standard-range one cannot go
/// stale unannounced, because turning HDR *on* does reach the window as
/// `Focused(true)` — and only once the interval has passed since the last ask.
fn display_check_due(watching_hdr: bool, last_checked: Option<Instant>, now: Instant) -> bool {
    if !watching_hdr {
        return false;
    }
    match last_checked {
        // Never asked while on this surface: ask now, so the first tick after
        // switching to HDR establishes the baseline.
        None => true,
        Some(last) => now.duration_since(last) >= DISPLAY_WATCH_INTERVAL,
    }
}

/// When the loop must wake to ask again, if it must.
fn display_watch_deadline(watching_hdr: bool, last_checked: Option<Instant>, now: Instant) -> Option<Instant> {
    if !watching_hdr {
        return None;
    }
    Some(last_checked.unwrap_or(now) + DISPLAY_WATCH_INTERVAL)
}

enum Step {
    Next,
    Previous,
    First,
    Last,
}

enum Reframe {
    Fit,
    Actual,
    Toggle,
}

/// Whether a vector image shown at `wanted` pixels wide is far enough from the
/// `drawn` rasterisation to be worth drawing again.
///
/// Rasterising is not free, and a smooth zoom passes through every size on the
/// way; redrawing at each one would spend the whole gesture in the rasteriser.
/// A quarter either way is under one notch of the wheel, so a deliberate zoom
/// redraws promptly while a nudge or a rounding wobble does not.
fn worth_redrawing(drawn: u32, wanted: u32) -> bool {
    const TOLERANCE: f32 = 1.25;

    let (drawn, wanted) = (drawn.max(1) as f32, wanted as f32);
    wanted > drawn * TOLERANCE || wanted < drawn / TOLERANCE
}

impl ApplicationHandler<Event> for App {
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: Event) {
        match event {
            Event::Decoded(decoded) => self.decoded(*decoded),
            #[cfg(windows)]
            Event::Open(paths) => self.handed_over(paths),
        }
    }

    /// The animation tick. Runs after every batch of events, which is where
    /// the wake this handler itself scheduled comes back in.
    ///
    /// A still image asks for `Wait` and the loop sleeps until real input —
    /// the event-driven redraw promise at the top of this file stands. Only a
    /// playing animation asks to be woken, and only for its next frame.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.watch_the_display();

        let advanced = match self.shown.as_mut().and_then(|shown| shown.player.as_mut()) {
            Some(player) => player.advance_to(Instant::now()),
            None => {
                event_loop.set_control_flow(self.idle_until());
                return;
            }
        };

        if advanced {
            if let (Some(renderer), Some(shown)) = (self.renderer.as_mut(), self.shown.as_ref())
                && let Some(player) = shown.player.as_ref()
            {
                renderer.update_pixels(player.frame());
            }
            self.set_title();
            self.request_redraw();
        }

        let flow = match self.shown.as_ref().and_then(|shown| shown.player.as_ref()).and_then(Player::wake_at) {
            // Whichever comes first: the animation's next frame, or the check
            // on the display.
            Some(due) => match self.idle_until() {
                ControlFlow::WaitUntil(watch) => ControlFlow::WaitUntil(due.min(watch)),
                _ => ControlFlow::WaitUntil(due),
            },
            None => self.idle_until(),
        };
        event_loop.set_control_flow(flow);
    }

    /// The loop is finishing: write down where the window ended up.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.remember_placement();
        self.config.save();
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let mut attributes = Window::default_attributes()
            .with_title("nitid")
            // The window stays hidden until the first frame is ready: showing
            // it earlier is the white flash every other viewer opens with.
            .with_visible(false);

        // Reopen where the viewer was last closed. Without an explicit
        // position Windows cascades each new window a little further down and
        // right, so opening a folder file by file walks the window across the
        // screen — every image lands somewhere new.
        let stored = self.config.placement;
        attributes = match stored.size {
            Some((width, height)) => attributes.with_inner_size(PhysicalSize::new(width, height)),
            None => attributes.with_inner_size(LogicalSize::new(DEFAULT_WINDOW.0, DEFAULT_WINDOW.1)),
        };
        if let Some((x, y)) = stored.position {
            attributes = attributes.with_position(PhysicalPosition::new(x, y));
        }
        if stored.maximised {
            attributes = attributes.with_maximized(true);
        }

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.failure = Some(anyhow::Error::new(error).context("creating the window"));
                event_loop.exit();
                return;
            }
        };

        startup::milestone("window created");

        let size = window.inner_size();
        let renderer = match Renderer::new(window.clone(), (size.width, size.height)) {
            Ok(renderer) => renderer,
            Err(error) => {
                self.failure = Some(error);
                event_loop.exit();
                return;
            }
        };

        startup::milestone("gpu ready");

        self.window = Some(window.clone());
        self.renderer = Some(renderer);

        let initial = std::mem::take(&mut self.initial);
        if !initial.is_empty() {
            self.open(&initial);
        }

        window.set_visible(true);
        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                let scale_factor = self.scale_factor();
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize((size.width, size.height));
                    if let Some(shown) = self.shown.as_mut() {
                        shown.view.resize(renderer.size(), scale_factor);
                    }
                }
                self.follow_display();
                self.request_redraw();
            }

            // Turning HDR on in Windows re-sets the display mode, and dragging
            // the window to another monitor lands it on a display with its own
            // answer. Neither arrives as an event of its own, but both move the
            // window; coming back to the viewer after changing the setting is
            // the third way it is noticed. See `Renderer::follow_display`.
            WindowEvent::Moved(_) | WindowEvent::Focused(true) => {
                if self.follow_display() {
                    self.request_redraw();
                }
            }

            // Dragged onto a monitor with different scaling: the surface size
            // follows in a `Resized` event, but the framing must be redone
            // against the new scale factor either way.
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let (Some(renderer), Some(shown)) = (&self.renderer, self.shown.as_mut()) {
                    shown.view.resize(renderer.size(), scale_factor as f32);
                }
                self.refresh();
            }

            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                self.handle_key(&event.logical_key, event_loop);
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => lines,
                    MouseScrollDelta::PixelDelta(position) => position.y as f32 / PIXELS_PER_NOTCH,
                };
                let cursor = self.cursor_position();
                if let Some(shown) = self.shown.as_mut() {
                    shown.view.zoom_at(notches, cursor);
                    self.refresh();
                }
            }

            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => self.dragging = true,
                (MouseButton::Left, ElementState::Released) => self.dragging = false,
                (MouseButton::Middle, ElementState::Pressed) => self.reframe(Reframe::Toggle),
                _ => {}
            },

            WindowEvent::CursorMoved { position, .. } => {
                let delta = ((position.x - self.cursor.x) as f32, (position.y - self.cursor.y) as f32);
                self.cursor = position;
                if self.dragging
                    && let Some(shown) = self.shown.as_mut()
                {
                    shown.view.pan(delta);
                    self.request_redraw();
                }
            }

            WindowEvent::CursorLeft { .. } => self.dragging = false,

            WindowEvent::DroppedFile(path) => self.open(&[path]),

            WindowEvent::RedrawRequested => self.redraw(event_loop),

            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The redraw has to be worth its cost: a wheel gesture passes through
    /// many sizes, and rasterising at each one would spend the whole zoom in
    /// the rasteriser rather than showing the picture.
    #[test]
    fn a_small_change_in_size_does_not_redraw_a_vector_image() {
        assert!(!worth_redrawing(400, 400), "the same size asked for a redraw");
        assert!(!worth_redrawing(400, 420), "a nudge asked for a redraw");
        assert!(!worth_redrawing(400, 380), "a nudge asked for a redraw");
    }

    /// A deliberate zoom must redraw, or the format's whole advantage is lost:
    /// the picture would blur exactly where it promises a clean edge.
    #[test]
    fn a_real_zoom_redraws_a_vector_image() {
        assert!(worth_redrawing(400, 800), "doubling the size did not redraw");
        assert!(worth_redrawing(400, 200), "halving the size did not redraw");
    }

    /// The first rasterisation may be tiny, and dividing by it must not blow
    /// up on a degenerate size.
    #[test]
    fn a_degenerate_size_is_handled_rather_than_dividing_by_zero() {
        assert!(worth_redrawing(0, 100));
        assert!(!worth_redrawing(1, 1));
    }

    /// A standard-range surface is never polled.
    ///
    /// This is what keeps the idle promise: on an SDR display — where the
    /// viewer spends most of its life — the loop sleeps until the user acts,
    /// exactly as it did before HDR existed. The other direction needs no
    /// poll because turning HDR *on* reaches the window as `Focused(true)`.
    #[test]
    fn a_standard_range_surface_is_left_alone() {
        let now = Instant::now();

        assert!(!display_check_due(false, None, now));
        assert!(!display_check_due(false, Some(now - Duration::from_secs(60)), now));
        assert_eq!(display_watch_deadline(false, None, now), None);
    }

    /// The first tick on an HDR surface asks, so the interval has a baseline.
    #[test]
    fn the_first_check_on_an_hdr_surface_happens_at_once() {
        assert!(display_check_due(true, None, Instant::now()));
    }

    /// Asking is bounded to once an interval: `about_to_wait` runs after every
    /// batch of events, and a poll on each of them would turn 140 microseconds
    /// into a cost paid per mouse move.
    #[test]
    fn an_hdr_surface_is_asked_no_more_than_once_an_interval() {
        let now = Instant::now();
        let checked = now - DISPLAY_WATCH_INTERVAL / 2;

        assert!(!display_check_due(true, Some(checked), now), "asked again inside the interval");
        assert!(
            display_check_due(true, Some(now - DISPLAY_WATCH_INTERVAL), now),
            "the interval passed and the display was not asked"
        );
    }

    /// The loop must be woken for the next check, or the poll never happens on
    /// a still image — which is the only case it exists for.
    #[test]
    fn watching_the_display_bounds_how_long_the_loop_sleeps() {
        let now = Instant::now();
        let checked = now - Duration::from_millis(200);

        let due = display_watch_deadline(true, Some(checked), now).expect("an HDR surface should be watched");
        assert_eq!(due, checked + DISPLAY_WATCH_INTERVAL);
        assert!(due > now, "the deadline is in the past, so the loop would spin");
    }
}

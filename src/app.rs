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
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use moxcms::ColorProfile;

use crate::animation::Player;
use crate::color::{self, ColorTransform};
use crate::config::Config;
use crate::folder::Folder;
use crate::format::Format;
use crate::gpu::Renderer;
use crate::gpu::{Overlay, Painted};
use crate::histogram::Histogram;
use crate::image_source::{self, Depth, Fidelity, LoadedImage, Orientation};
use crate::interface::{Action, Interface, Status};
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

    let mut app = App::new(paths, loader, event_loop.create_proxy());
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
    /// A histogram finished counting, for the file it names.
    ///
    /// The path is what makes it safe to arrive late: the user may have
    /// stepped to another image while the count ran, and a histogram of the
    /// picture they left is not a histogram of the one they are looking at.
    Counted(PathBuf, Box<Histogram>),
    /// Another instance handed these files over rather than opening a window.
    #[cfg(windows)]
    Open(Vec<PathBuf>),
}

/// What the viewer is showing, once a file has been opened.
struct Shown {
    /// A quarter turn the user asked for, on top of what the file asks for.
    ///
    /// A viewing transform: the file is untouched, and this goes away with the
    /// picture it belongs to — the next image is shown as its own metadata
    /// asks. Writing a rotation back to the file is a later version.
    turn: Orientation,
    /// Which file this is. Kept so a thumbnail can be told from the picture
    /// it stands in for: `Fidelity` alone says "this is a thumbnail", not
    /// "of what", and a step to the next file arrives as a thumbnail while
    /// the previous file is still what `shown` holds.
    path: PathBuf,
    orientation: Orientation,
    view: View,
    /// Whether this is the real image or the thumbnail standing in for it.
    fidelity: Fidelity,
    /// Whether this picture came from the clipboard rather than from a file.
    ///
    /// It has no path, so everything that names one has to know: the title,
    /// the Info panel, the eyedropper's lookup, and `Ctrl+Shift+C`, which has
    /// no path to copy. Kept as its own fact rather than inferred from an
    /// invented path, which would leak into the folder and the loader's cache.
    pasted: bool,
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
    /// The image's own size in pixels, after orientation — what the status
    /// line reports, and the one fact `View` keeps to itself.
    size: (u32, u32),
    /// What the bytes turned out to be, and at what depth.
    ///
    /// `None` for a picture pasted from the clipboard, which was never a file
    /// and so is in no format — said rather than answered with an invented
    /// variant nothing else would know what to do with.
    format: Option<Format>,
    depth: Depth,
    /// What the colour transform does, in the words a person would use.
    colour: String,
    /// What the file says about itself: camera, exposure, place.
    metadata: crate::metadata::Metadata,
    /// The file's size on disk. `None` when it cannot be asked for, which is
    /// a fact about the moment rather than about the file.
    file_size: Option<u64>,
    /// What tones this picture is made of, once something has asked.
    ///
    /// Counted only when the histogram is actually on screen, and on a worker
    /// thread when it is: a file nobody has asked to measure is not measured,
    /// which is what keeps the count off the path to the first pixel.
    histogram: Option<Histogram>,
    /// Whether a count for this picture is already running, so opening and
    /// closing the panel does not start a second one.
    counting: bool,
}

struct App {
    /// The files to open once the window exists.
    initial: Vec<PathBuf>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    folder: Option<Folder>,
    shown: Option<Shown>,
    loader: Loader,
    /// A way to hand a finished background count back to this thread.
    proxy: EventLoopProxy<Event>,
    config: Config,
    /// The profile Windows has assigned to the display.
    display_profile: ColorProfile,
    cursor: PhysicalPosition<f64>,
    dragging: bool,
    /// Which modifier keys are down.
    ///
    /// winit reports these as their own event rather than on each key, so the
    /// state has to be kept: without it `Ctrl+C` arrives indistinguishable
    /// from `C`, and copying the picture would toggle the clipping zebra.
    modifiers: winit::keyboard::ModifiersState,
    /// When the display was last asked about its dynamic range. `None` until
    /// the first ask; only set while the viewer is on an HDR surface, which is
    /// the only state that can go stale unannounced.
    display_checked_at: Option<Instant>,
    /// A failure that must end the run; reported after the loop exits, since
    /// `ApplicationHandler` methods cannot return one.
    failure: Option<anyhow::Error>,
    /// What the status line and the key sheet show.
    interface: Interface,
    /// egui's own view of the window: pointer, keys, scale factor. Built with
    /// the window, so `None` until then.
    input: Option<egui_winit::State>,
    /// The interface's place on the GPU.
    ///
    /// Built the first time the interface is actually drawn, not with the
    /// renderer: building it costs 64–69 ms — measured — and it sat squarely
    /// between a ready GPU and the first pixels, which is the one stretch of
    /// the run the whole product is a promise about. A viewer opened on a
    /// photograph shows no chrome, so on that path it is never built at all.
    overlay: Option<Overlay>,
    /// What the last laid-out frame said, kept so it can be drawn again
    /// without asking egui to lay it out a second time.
    painted: Option<PaintedFrame>,
    /// When a toast next needs a frame, while one is fading.
    toast_deadline: Option<Instant>,
    /// Whether egui asked for a frame of its own.
    ///
    /// Set from what `on_window_event` reports. Without it the interface is
    /// laid out only when the *status* changes, and a press on a button
    /// changes no status: the toolbar drew and did nothing, which is how it
    /// shipped in v0.17.0 and what a hands-on run found.
    wants_frame: bool,
    /// The pixels of a picture pasted from the clipboard.
    ///
    /// The loader's cache is keyed by path and a pasted picture has none, so
    /// the one copy of it lives here — read by the histogram, the eyedropper
    /// and a second `Ctrl+C`.
    pasted: Option<crate::image_source::DecodedImage>,
    /// Whether the eyedropper is up.
    ///
    /// A mode rather than a held key, because its whole purpose is to be
    /// pointed about the picture and clicked: holding a key down and clicking
    /// at the same time is a two-handed gesture for a one-handed job.
    picking: bool,
    /// What the eyedropper reads under the cursor, while it is up.
    ///
    /// Recomputed on every pointer move rather than stored per pixel: reading
    /// one pixel out of memory the loader is already holding costs nothing
    /// worth caching.
    reading: Option<crate::eyedropper::Reading>,
    /// Whether the framing carries from one image to the next.
    ///
    /// Off by default: a folder of unrelated pictures wants each one framed
    /// for itself. On, it turns the arrow keys into a way to compare a series
    /// — the same magnification over the same part of each frame.
    zoom_locked: bool,
    /// Whether a frame with a picture in it has reached the screen yet.
    ///
    /// The interface waits for that frame. Laying it out and building its
    /// pipeline costs about forty milliseconds — measured — and putting that
    /// in front of the photograph spends the promise the whole product is
    /// built on. The status line arrives on the frame after, which is the
    /// same order the viewer already uses for an embedded thumbnail: show the
    /// picture first, then improve it.
    shown_once: bool,
}

/// One laid-out interface frame, held between the layout and the draw.
struct PaintedFrame {
    jobs: Vec<egui::ClippedPrimitive>,
    textures: egui::TexturesDelta,
    pixels_per_point: f32,
}

impl App {
    fn new(initial: Vec<PathBuf>, loader: Loader, proxy: EventLoopProxy<Event>) -> Self {
        Self {
            initial,
            window: None,
            renderer: None,
            folder: None,
            shown: None,
            loader,
            proxy,
            config: Config::load(),
            display_profile: color::display_profile(),
            cursor: PhysicalPosition::new(0.0, 0.0),
            dragging: false,
            modifiers: winit::keyboard::ModifiersState::empty(),
            display_checked_at: None,
            failure: None,
            interface: Interface::new(),
            input: None,
            overlay: None,
            painted: None,
            toast_deadline: None,
            wants_frame: false,
            zoom_locked: false,
            pasted: None,
            picking: false,
            reading: None,
            shown_once: false,
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
            Request::Ready(image) => self.upload(path, &image),
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

        self.upload(path, &thumbnail);
        startup::milestone("thumbnail up");
    }

    fn upload(&mut self, path: &Path, loaded: &LoadedImage) {
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

        let view = frame_arriving(
            self.shown.as_ref().map(|shown| Previous {
                view: shown.view,
                fidelity: shown.fidelity,
                same_file: shown.path == path,
            }),
            loaded.display_size(),
            renderer.size(),
            scale_factor,
            self.zoom_locked,
        );

        self.shown = Some(Shown {
            // A turn belongs to the picture on screen. The full decode
            // replacing its own thumbnail keeps it; a different file does not.
            turn: match &self.shown {
                Some(shown) if shown.path == path => shown.turn,
                _ => Orientation::Normal,
            },
            path: path.to_path_buf(),
            orientation: loaded.orientation,
            view,
            fidelity: loaded.fidelity,
            pasted: false,
            vector: loaded.vector.clone(),
            rasterised_at: (loaded.image.width, loaded.image.height),
            // An animated image starts playing the moment it is up; the
            // event loop reads the player's clock in `about_to_wait`.
            player: loaded.animation.clone().map(|animation| Player::new(animation, Instant::now())),
            size: loaded.display_size(),
            format: Some(loaded.format),
            depth: loaded.image.depth,
            colour: describe_colour(loaded.profile.as_ref(), &transform),
            metadata: loaded.metadata.clone(),
            file_size: std::fs::metadata(path).ok().map(|entry| entry.len()),
            // A thumbnail is not the picture: its histogram would be the
            // shape of a few hundred pixels standing in for millions, and it
            // would be replaced moments later anyway. The count waits for the
            // real image.
            histogram: None,
            counting: false,
        });

        // A file replaces whatever was pasted, and the pasted pixels go with
        // it: keeping them would leave a copy of a picture nobody is looking
        // at, and `Ctrl+C` would copy the wrong one.
        self.pasted = None;

        // If the panel is already open — the user stepped to this image with
        // the histogram up — the new picture has to be counted for itself.
        self.count_if_wanted();
        // The same for the eyedropper: a reading belongs to the picture it
        // was taken from, and carrying it across a step would report a colour
        // from the image the user just left.
        self.reading = None;
        self.take_reading();
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

        let name = match self.shown.as_ref().is_some_and(|shown| shown.pasted) {
            // A pasted picture has no name, and the title has to say what is
            // on screen rather than the name of the file that was open before.
            true => "clipboard",
            false => self
                .folder
                .as_ref()
                .map(Folder::current)
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or("nitid"),
        };

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
                self.upload(&path, &image);
                // `refresh` rather than a redraw on its own: a vector image
                // arrives drawn at its document size and has to be drawn again
                // for the framing it is fitted into.
                self.refresh();
            }
            Err(error) => {
                // A file that will not open is a fact about the file, not a
                // reason to close the viewer: name it and keep the window.
                //
                // The toast is how it gets said at all. The windowed binary
                // has no console — `nitidw.exe` is linked for the GUI
                // subsystem and its standard handles go nowhere — so until
                // now this line reached nobody who double-clicked a broken
                // file.
                eprintln!("nitid: {}: {error}", path.display());
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("that file");
                self.interface.toast(format!("{name} will not open"), Instant::now());
                self.request_redraw();
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

    /// Tell the interface where the pointer is, in the units it lays out in.
    ///
    /// Returns whether the toolbar's visibility changed, which is the only
    /// reason a bare pointer move would need a frame.
    fn follow_pointer(&mut self, position: Option<PhysicalPosition<f64>>) -> bool {
        let scale = self.window.as_ref().map_or(1.0, |window| window.scale_factor());
        let logical = position.map(|position| ((position.x / scale) as f32, (position.y / scale) as f32));
        self.interface.follow_pointer(logical)
    }

    fn handle_key(&mut self, key: &Key, event_loop: &ActiveEventLoop) {
        debug_assert!(
            handled(key),
            "the viewer was handed a key it does not answer: {key:?} — a toolbar button asking for it would do nothing",
        );
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
                // What the file says about itself. The panel is an overlay,
                // so the picture keeps its framing while it is up.
                "i" | "I" => {
                    self.interface.toggle_info();
                    self.request_redraw();
                }
                // What shows through a transparent pixel. A cut-out judged
                // against one backdrop is a cut-out judged against one
                // background, which is how a halo reaches a customer.
                "b" | "B" => {
                    if let Some(renderer) = self.renderer.as_mut() {
                        let next = renderer.backdrop().next();
                        renderer.set_backdrop(next);
                        self.interface.toast(
                            match next.name() {
                                Some(name) => format!("backdrop: {name}"),
                                None => "backdrop: the viewer's own".to_string(),
                            },
                            Instant::now(),
                        );
                        self.request_redraw();
                    }
                }
                // A quarter turn of the view. Shift for the other way, which
                // arrives as the capital letter and needs no modifier state.
                "r" => self.turn(true),
                "R" => self.turn(false),
                // Hold the framing across a step, for comparing a series
                // frame by frame. Announced by a toast and shown in the
                // status line: a mode with no sign of itself is a mode the
                // user fights rather than uses.
                "l" | "L" => {
                    self.zoom_locked = !self.zoom_locked;
                    self.interface
                        .toast(if self.zoom_locked { "zoom locked" } else { "zoom unlocked" }, Instant::now());
                    self.request_redraw();
                }
                // A look at the pixels, held rather than toggled. The release
                // is answered in the event loop, not here.
                "z" | "Z" => self.hold_loupe(),
                // The colour under the cursor. A mode, so it can be pointed
                // about the picture and clicked with one hand.
                "p" | "P" => self.toggle_picking(),
                // The colour path, spelled out. Reachable from the keyboard
                // as well as by clicking the status line's colour chip: the
                // toolbar and the status line carry nothing the keys do not.
                "k" | "K" => {
                    self.interface.toggle_passport();
                    self.request_redraw();
                }
                // Which pixels the file itself clipped. A viewing aid, so
                // it belongs to the renderer rather than to the picture: it
                // costs one uniform write and survives a step to the next
                // image, which is what makes it usable for checking a series.
                "c" | "C" => {
                    if let Some(renderer) = self.renderer.as_mut() {
                        let next = !renderer.zebra();
                        renderer.set_zebra(next);
                        self.interface.toast(if next { "clipping shown" } else { "clipping hidden" }, Instant::now());
                        self.request_redraw();
                    }
                }
                // What tones the picture is made of. The count is started
                // here rather than at load: a file nobody asked to measure
                // stays unmeasured, which is what keeps this off the path to
                // the first pixel.
                "h" | "H" => {
                    self.interface.toggle_histogram();
                    self.count_if_wanted();
                    self.request_redraw();
                }
                // The chrome disappears by design, so the list of what the
                // keys do has to be reachable from the keys themselves.
                "?" => {
                    self.interface.toggle_keys();
                    self.request_redraw();
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Count the tones of the picture on screen, if the histogram is showing
    /// and this picture has not been counted yet.
    ///
    /// The count runs on a worker thread and comes back as an event, for the
    /// same reason a decode does: a 60-megapixel file takes long enough that
    /// doing it here would stall the key that asked for it. The pixels are
    /// taken from the loader's cache, which is already holding them for the
    /// picture on screen — nothing is decoded twice.
    fn count_if_wanted(&mut self) {
        if !self.interface.histogram_shown() {
            return;
        }
        let Some(shown) = self.shown.as_ref() else {
            return;
        };
        // Already counted, already counting, or standing in for the real
        // image — in each case there is nothing to start.
        if shown.histogram.is_some() || shown.counting || shown.fidelity != Fidelity::Full {
            return;
        }
        let path = shown.path.clone();
        // A pasted picture is not in the loader's cache — it was never a file
        // — so its pixels come from the one copy the application holds.
        let pixels = if shown.pasted {
            self.pasted.clone()
        } else {
            match self.loader.request(&path) {
                Request::Ready(image) => Some(image.image.clone()),
                // The full decode is still on its way. `upload` calls back
                // here when it lands, so the count is not lost — only deferred.
                Request::Pending => None,
            }
        };
        let Some(pixels) = pixels else {
            return;
        };

        if let Some(shown) = self.shown.as_mut() {
            shown.counting = true;
        }

        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let histogram = Histogram::of(&pixels);
            // The window may be gone by the time this finishes; there is
            // nobody left to tell, and that is fine.
            let _ = proxy.send_event(Event::Counted(path, Box::new(histogram)));
        });
    }

    /// A background count came back.
    fn counted(&mut self, path: PathBuf, histogram: Histogram) {
        let Some(shown) = self.shown.as_mut() else {
            return;
        };
        // The user may have stepped away while it ran. A histogram of the
        // picture they left is not a histogram of the one they are looking at.
        if shown.path != path {
            return;
        }
        shown.histogram = Some(histogram);
        shown.counting = false;
        self.request_redraw();
    }

    /// What is happening to the colour of the picture on screen.
    ///
    /// Built on demand rather than kept: it is asked for only while the panel
    /// is open, and it costs a couple of profile lookups.
    fn passport(&self) -> Option<crate::color::Passport> {
        if !self.interface.passport_shown() {
            return None;
        }
        let shown = self.shown.as_ref()?;
        if shown.pasted {
            // A clipboard bitmap carries no profile, so it is passed through
            // like an untagged file — and the passport says exactly that.
            return Some(crate::color::Passport::new(None, &self.display_profile, &ColorTransform::identity()));
        }
        let Request::Ready(image) = self.loader.request(&shown.path) else {
            return None;
        };
        let transform = ColorTransform::for_image(image.profile.as_ref(), &self.display_profile);
        Some(crate::color::Passport::new(image.profile.as_ref(), &self.display_profile, &transform))
    }

    /// Answer a key pressed with Ctrl held.
    ///
    /// Kept apart from `handle_key` because the two answer different keys:
    /// `C` toggles the clipping zebra and `Ctrl+C` copies the picture, and a
    /// handler that could not tell them apart would do the wrong one.
    fn handle_chord(&mut self, key: &Key) {
        match chord_for(key) {
            Some(Chord::CopyPath) => self.copy_path(),
            Some(Chord::CopyPicture) => self.copy_picture(),
            Some(Chord::Paste) => self.paste_picture(),
            None => {}
        }
    }

    /// Put the picture on the clipboard.
    ///
    /// The file's own pixels, unconverted — see the clipboard module and
    /// ADR 0019. What is copied is the whole picture as the file holds it, not
    /// what is framed on screen: a zoomed-in view is a way of looking at the
    /// picture rather than a crop of it, and cropping by accident is worse
    /// than copying more than was wanted.
    fn copy_picture(&mut self) {
        let Some(shown) = self.shown.as_ref() else {
            return;
        };

        let image = if shown.pasted {
            // A pasted picture has no file to go back to, so the pixels on
            // screen are the only copy there is.
            self.pasted.clone()
        } else {
            match self.loader.request(&shown.path) {
                Request::Ready(image) => Some(image.image.clone()),
                Request::Pending => None,
            }
        };

        let Some(image) = image else {
            self.interface.toast("still opening", Instant::now());
            self.request_redraw();
            return;
        };

        match crate::clipboard::set_dib(&crate::clipboard::to_dib(&image)) {
            Ok(()) => self.interface.toast("picture copied", Instant::now()),
            Err(error) => {
                eprintln!("nitid: {error:#}");
                self.interface.toast("could not reach the clipboard", Instant::now());
            }
        }
        self.request_redraw();
    }

    /// Put the file's path on the clipboard, quoted for a terminal.
    fn copy_path(&mut self) {
        let Some(shown) = self.shown.as_ref() else {
            return;
        };
        if shown.pasted {
            // Nothing to copy: a pasted picture is not anywhere.
            self.interface.toast("this picture has no file", Instant::now());
            self.request_redraw();
            return;
        }

        let quoted = crate::clipboard::quote_for_shell(&shown.path.display().to_string());
        self.interface.context().copy_text(quoted);
        self.interface.toast("path copied", Instant::now());
        self.request_redraw();
    }

    /// Show whatever picture is on the clipboard.
    ///
    /// Nothing is written to disk: the picture is shown as itself, with no
    /// file behind it (owner's decision). A viewer that quietly saved a
    /// temporary file on every paste would be writing to disk without being
    /// asked and leaving the results behind.
    fn paste_picture(&mut self) {
        let bytes = match crate::clipboard::get_dib() {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                self.interface.toast("no picture on the clipboard", Instant::now());
                self.request_redraw();
                return;
            }
            Err(error) => {
                eprintln!("nitid: {error:#}");
                self.interface.toast("could not reach the clipboard", Instant::now());
                self.request_redraw();
                return;
            }
        };

        let Some(image) = crate::clipboard::from_dib(&bytes) else {
            self.interface.toast("the clipboard holds a bitmap nitid cannot read", Instant::now());
            self.request_redraw();
            return;
        };

        self.show_pasted(image);
    }

    /// Put a picture with no file behind it on screen.
    fn show_pasted(&mut self, image: crate::image_source::DecodedImage) {
        let scale_factor = self.scale_factor();
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        // A clipboard bitmap says nothing about its colours, so it is passed
        // through untouched — the same rule an untagged file gets (ADR 0005).
        let transform = ColorTransform::identity();
        renderer.set_image(&image, &transform);

        let size = (image.width, image.height);
        let view = View::new(size, renderer.size(), scale_factor);

        // The folder goes with the file: there is nothing beside a pasted
        // picture to step to, and leaving the old folder in place would let
        // an arrow key silently replace the paste with a neighbour of the
        // file that used to be open.
        self.folder = None;
        self.shown = Some(Shown {
            turn: Orientation::Normal,
            path: PathBuf::new(),
            orientation: Orientation::Normal,
            view,
            fidelity: Fidelity::Full,
            pasted: true,
            vector: None,
            rasterised_at: size,
            player: None,
            size,
            format: None,
            depth: image.depth,
            colour: "from the clipboard".to_string(),
            metadata: crate::metadata::Metadata::default(),
            file_size: None,
            histogram: None,
            counting: false,
        });
        self.pasted = Some(image);

        // The tools that read pixels look them up by path, which a pasted
        // picture has not got: whatever they were showing belongs to the file
        // that was open before.
        self.reading = None;
        self.count_if_wanted();
        self.refresh();
    }

    /// Show or hide the eyedropper.
    fn toggle_picking(&mut self) {
        self.picking = !self.picking;
        if self.picking {
            self.take_reading();
        } else {
            self.reading = None;
        }
        self.request_redraw();
    }

    /// Read the pixel under the cursor, if the eyedropper is up.
    ///
    /// The pixels come from the loader's cache — the same ones the histogram
    /// counts — so pointing about a picture costs a lookup rather than a
    /// read-back from the GPU, which would stall the pipeline on every mouse
    /// move.
    fn take_reading(&mut self) {
        if !self.picking {
            return;
        }
        let cursor = self.cursor_position();
        let Some(shown) = self.shown.as_ref() else {
            self.reading = None;
            return;
        };
        // A thumbnail stands in for the picture at a fraction of its size, so
        // its pixels are not the file's: the eyedropper waits for the real
        // image rather than reporting a colour off an approximation.
        if shown.fidelity != Fidelity::Full {
            self.reading = None;
            return;
        }

        let Some(at) = shown.view.pixel_under(cursor) else {
            // Off the picture: no reading rather than a stale one, or the
            // panel would keep reporting the last pixel that was under the
            // cursor as though it still were.
            self.reading = None;
            return;
        };

        let path = shown.path.clone();
        // What the file asks for, then what the user asked for on top of it —
        // the same composition the renderer draws with, so the pixel named is
        // the pixel shown.
        let orientation = shown.orientation.then(shown.turn);
        let pasted = shown.pasted;

        // A pasted picture has no file behind it, so its pixels come from the
        // application's own copy and pass through untagged.
        let (pixels, transform) = if pasted {
            let Some(pixels) = self.pasted.clone() else {
                self.reading = None;
                return;
            };
            (pixels, ColorTransform::identity())
        } else {
            let Request::Ready(image) = self.loader.request(&path) else {
                self.reading = None;
                return;
            };
            let transform = ColorTransform::for_image(image.profile.as_ref(), &self.display_profile);
            (image.image.clone(), transform)
        };
        self.reading = crate::eyedropper::read(&pixels, orientation, &transform, at);
    }

    /// Put the colour under the cursor on the clipboard.
    ///
    /// The file's value, not the display's, for the reason the module states:
    /// it is a fact about the picture rather than about this monitor.
    fn copy_reading(&mut self) {
        let Some(reading) = self.reading else {
            return;
        };
        let hex = reading.hex();
        self.interface.context().copy_text(hex.clone());
        self.interface.toast(format!("{hex} copied"), Instant::now());
        self.request_redraw();
    }

    /// Hold the loupe: 100% under the cursor, until the key comes back up.
    ///
    /// The cursor is where the eye already is, which is what makes this
    /// cheaper than zooming: no pan to the place, no pan back.
    fn hold_loupe(&mut self) {
        let cursor = self.cursor_position();
        if let Some(shown) = self.shown.as_mut() {
            if shown.view.loupe_held() {
                return;
            }
            shown.view.hold_loupe(cursor);
            self.refresh();
        }
    }

    /// Give the framing back.
    ///
    /// Called on the key's release, and again whenever the window loses focus:
    /// a key let go while another window is in front sends its release there,
    /// and without this the picture would stay at 100% with nothing holding
    /// it — a mode entered by accident and with no key to leave by.
    fn release_loupe(&mut self) {
        let held = self.shown.as_ref().is_some_and(|shown| shown.view.loupe_held());
        if !held {
            return;
        }
        if let Some(shown) = self.shown.as_mut() {
            shown.view.release_loupe();
        }
        self.refresh();
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

    /// Turn the picture on screen a quarter turn.
    ///
    /// The view is reframed afterwards because a turned image has swapped its
    /// width and height: a landscape photograph fitted to the window is a
    /// portrait one that no longer fits.
    fn turn(&mut self, clockwise: bool) {
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let window = renderer.size();
        let scale_factor = self.scale_factor();
        let Some(shown) = self.shown.as_mut() else {
            return;
        };

        let before = shown.turn;
        shown.turn = shown.turn.turned(clockwise);

        if let Some(turned) = size_after_turn(shown.size, before, shown.turn) {
            shown.size = turned;
            shown.view = View::new(turned, window, scale_factor);
        }
        self.refresh();
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
        let changed = self.renderer.as_mut().is_some_and(Renderer::follow_display);
        if changed {
            // The interface is composited by a pipeline built against the
            // format it writes, and encoded by a rule that depends on the
            // surface. Both have just changed, so both are redone — the same
            // reason the image pipeline is rebuilt next door.
            if let (Some(overlay), Some(renderer)) = (self.overlay.as_mut(), self.renderer.as_ref()) {
                renderer.follow_interface(overlay);
            }
        }
        changed
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
        let display = display_watch_deadline(watching, self.display_checked_at, Instant::now());
        // A fading toast has to be drawn to be seen, so it is the one thing
        // the interface asks the loop to wake for — and only while one is up.
        // With no toast and no HDR watch this is still `Wait`, which is the
        // promise a still picture rests on.
        match soonest(display, self.toast_deadline) {
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

    /// What the status line should say about what is on screen.
    fn status(&self) -> Status {
        let name = match self.shown.as_ref().is_some_and(|shown| shown.pasted) {
            true => "clipboard".to_string(),
            false => self
                .folder
                .as_ref()
                .map(Folder::current)
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
        };

        let position = self
            .folder
            .as_ref()
            .filter(|folder| folder.len() > 1)
            .map(|folder| (folder.position() + 1, folder.len()));

        Status {
            name,
            position,
            size: self.shown.as_ref().map(|shown| shown.size),
            format: self.shown.as_ref().and_then(|shown| shown.format),
            depth: self.shown.as_ref().map(|shown| shown.depth),
            colour: self.shown.as_ref().map(|shown| shown.colour.clone()),
            scale: self.shown.as_ref().map_or(1.0, |shown| shown.view.scale()),
            fit: self.shown.as_ref().map_or(FitMode::Fit, |shown| shown.view.mode()),
            frame: self.shown.as_ref().and_then(|shown| shown.player.as_ref()).map(|player| {
                let (frame, count) = player.position();
                (frame, count, player.paused())
            }),
            hdr: self.renderer.as_ref().is_some_and(Renderer::is_hdr),
            locked: self.zoom_locked,
            backdrop: self.renderer.as_ref().and_then(|renderer| renderer.backdrop().name()),
            metadata: self.shown.as_ref().map(|shown| shown.metadata.clone()).unwrap_or_default(),
            path: self.shown.as_ref().map(|shown| shown.path.clone()),
            file_size: self.shown.as_ref().and_then(|shown| shown.file_size),
            histogram: self.shown.as_ref().and_then(|shown| shown.histogram.clone()),
            clipping: self.renderer.as_ref().is_some_and(Renderer::zebra),
            picking: self.picking,
            reading: self.reading,
            passport: self.passport(),
        }
    }

    /// Lay out an interface frame, but only if it would look different.
    ///
    /// This is what keeps the interface from spending the idle promise: the
    /// loop already wakes for real input, and an unchanged status is not laid
    /// out at all, let alone drawn.
    fn lay_out_interface(&mut self) -> Option<Action> {
        // Everything read from `self` is gathered before the window and the
        // input state are borrowed, because both of those borrows outlive the
        // reads and the compiler is right to say so.
        // Nothing is laid out until a picture has been on screen once: the
        // first frame belongs to the photograph.
        if !self.shown_once {
            return None;
        }

        let status = self.status();
        let now = Instant::now();
        // A toast fades, so while one is up every frame differs from the last.
        self.toast_deadline = self.interface.tick(now);
        let fading = self.toast_deadline.is_some();
        if !worth_laying_out(self.interface.changed(&status), fading, self.wants_frame, self.painted.is_some()) {
            return None;
        }
        self.wants_frame = false;

        let window = self.window.clone()?;
        let input = self.input.as_mut()?;

        let raw = input.take_egui_input(&window);
        let (output, action) = self.interface.layout(raw, &status, now);
        input.handle_platform_output(&window, output.platform_output);

        // Worth a milestone of its own: this is where the interface could
        // start costing the startup promise, and it is measured at about ten
        // milliseconds rather than left to be assumed.
        crate::startup::milestone("interface laid out");
        self.painted = Some(PaintedFrame {
            jobs: self.interface.context().tessellate(output.shapes, output.pixels_per_point),
            textures: output.textures_delta,
            pixels_per_point: output.pixels_per_point,
        });
        action
    }

    /// Do what a toolbar button asked for.
    ///
    /// The button does not do anything of its own: it names the key it stands
    /// for and the key handler does the rest. A second implementation here
    /// would be a place for the toolbar and the keyboard to drift apart, and
    /// the drift would be invisible — both would work, differently.
    fn act(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        self.handle_key(&key_for(action), event_loop);
        // A button press changes what is on screen, and the frame it was
        // pressed in was laid out before it happened.
        self.request_redraw();
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(action) = self.lay_out_interface() {
            self.act(action, event_loop);
        }

        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        // With no image open the pass still runs and clears to the background,
        // so the window is never a hole showing whatever was behind it.
        // What the file asks for, then what the user asked for on top of it.
        let shown = self.shown.as_ref().map(|shown| (&shown.view, shown.orientation.then(shown.turn)));
        let carries_image = shown.is_some();

        // The interface's place on the GPU is built here, the first time
        // there is something to draw into it, and never on the path to a
        // photograph with no chrome over it.
        if self.painted.is_some() && self.overlay.is_none() {
            self.overlay = Some(renderer.interface());
            startup::milestone("interface ready");
        }

        let drew_interface = self.painted.is_some();
        let interface = match (self.overlay.as_mut(), self.painted.as_ref()) {
            (Some(layer), Some(frame)) => Some(Painted {
                layer,
                jobs: &frame.jobs,
                textures: &frame.textures,
                pixels_per_point: frame.pixels_per_point,
            }),
            _ => None,
        };

        if let Err(error) = renderer.render(shown, interface) {
            self.failure = Some(error);
            event_loop.exit();
            return;
        }

        // The deltas have been applied to the GPU; egui insists they are
        // acknowledged rather than dropped, and a frame that is drawn again
        // must not apply them twice.
        if let Some(frame) = self.painted.as_mut() {
            frame.textures.clear();
        }

        // A frame carrying both the picture and the chrome is what the gate
        // waits for when it is checking the order of the two.
        if drew_interface && carries_image && startup::exit_after_interface() {
            startup::interface_drawn();
            event_loop.exit();
        }

        // The frame is on screen now, and it has a picture in it. An empty
        // window reaching the screen is not what the promise is about.
        if carries_image {
            startup::first_pixels();
            if startup::exit_after_first_frame() {
                event_loop.exit();
            }
            // The picture is up, so the interface may have its turn — on the
            // next frame, which this asks for.
            //
            // Removing the request measured as harmless: the window is handed
            // a frame by the system anyway, and the chrome still appeared. It
            // stays because that is the system's choice and not a guarantee —
            // on a still picture with nothing else asking, the viewer sleeps,
            // and a frame that never comes is a status line that never
            // appears. No test holds this down for the same reason: the thing
            // it guards against is a frame *not* arriving, which nothing here
            // can make happen on demand.
            if !self.shown_once {
                self.shown_once = true;
                self.request_redraw();
            }
        }
    }
}

/// How to describe the colour of what is on screen, in a few words.
///
/// The distinction that matters to a person is not the matrix but whether the
/// file said anything at all: an untagged image is shown exactly as it is
/// (ADR 0005), and that is worth saying rather than leaving it to look like a
/// conversion that happened to be the identity.
fn describe_colour(profile: Option<&ColorProfile>, transform: &ColorTransform) -> String {
    match profile {
        None => "untagged".into(),
        Some(_) if transform.is_identity => "matches the display".into(),
        Some(_) => "converted".into(),
    }
}

/// Whether the key handler answers this key at all.
///
/// Written from the same arms as `handle_key`, and the only reason it exists
/// is that a toolbar button asking for a key nobody answers is a dead button
/// that still looks live — the failure mutation testing found here, where the
/// button was wired but the action went nowhere.
fn handled(key: &Key) -> bool {
    match key {
        Key::Named(
            NamedKey::Escape
            | NamedKey::Space
            | NamedKey::ArrowRight
            | NamedKey::PageDown
            | NamedKey::ArrowLeft
            | NamedKey::PageUp
            | NamedKey::Backspace
            | NamedKey::Home
            | NamedKey::End
            | NamedKey::F11,
        ) => true,
        Key::Character(character) => matches!(
            character.as_str(),
            "+" | "=" | "-" | "_" | "0" | "1" | "?" | "l" | "L" | "r" | "R" | "b" | "B" | "i" | "I" | "z" | "Z" | "h" | "H" | "c" | "C" | "p" | "P" | "k" | "K"
        ),
        _ => false,
    }
}

/// What a key pressed with Ctrl asks for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Chord {
    CopyPicture,
    CopyPath,
    Paste,
}

/// Which chord a key is, with Ctrl already known to be down.
///
/// Its own function so the one thing that is easy to get wrong here can be
/// tested: `Ctrl+C` and `C` are different keys to a person, and a viewer that
/// confused them would toggle the clipping zebra when asked to copy.
fn chord_for(key: &Key) -> Option<Chord> {
    let Key::Character(character) = key else {
        return None;
    };
    match character.as_str() {
        // Ctrl+Shift+C arrives as the capital letter, the same way the
        // anticlockwise turn does.
        "C" => Some(Chord::CopyPath),
        "c" => Some(Chord::CopyPicture),
        "v" | "V" => Some(Chord::Paste),
        _ => None,
    }
}

/// Whether this is the loupe's key.
///
/// Its own function because the loupe is answered twice — on the press and on
/// the release — and the two must name the same key or it would stick down.
fn is_loupe(key: &Key) -> bool {
    matches!(key, Key::Character(character) if matches!(character.as_str(), "z" | "Z"))
}

/// The key a toolbar button stands for.
///
/// The toolbar carries nothing the keyboard does not, which is what lets the
/// button reuse the key handler rather than repeat it. It is also what the
/// tooltips promise, and a test holds this against the key sheet so the three
/// cannot come apart.
fn key_for(action: Action) -> Key {
    match action {
        Action::Previous => Key::Named(NamedKey::ArrowLeft),
        Action::Next => Key::Named(NamedKey::ArrowRight),
        Action::ZoomOut => Key::Character("-".into()),
        Action::ZoomIn => Key::Character("+".into()),
        Action::Fit => Key::Character("0".into()),
        Action::Actual => Key::Character("1".into()),
        Action::TurnRight => Key::Character("r".into()),
        Action::TurnLeft => Key::Character("R".into()),
        Action::Backdrop => Key::Character("b".into()),
        Action::Info => Key::Character("i".into()),
        Action::Histogram => Key::Character("h".into()),
        Action::Clipping => Key::Character("c".into()),
        Action::Pick => Key::Character("p".into()),
        Action::Passport => Key::Character("k".into()),
        Action::Lock => Key::Character("l".into()),
        Action::FullScreen => Key::Named(NamedKey::F11),
        Action::Keys => Key::Character("?".into()),
    }
}

/// Whether an event egui asked to repaint for should also earn a new layout.
///
/// `RedrawRequested` is excluded, and the reason was measured rather than
/// reasoned: egui answers every redraw by asking for another, so treating that
/// as a reason to lay out again is a loop. It ran at 573 layouts in three
/// seconds on a still picture — the idle promise spent entirely on chrome.
///
/// Real input is what earns a frame; a redraw *is* the frame.
fn earns_a_layout(repaint: bool, event: &WindowEvent) -> bool {
    repaint && !matches!(event, WindowEvent::RedrawRequested)
}

/// Whether the interface should be laid out this frame.
///
/// Laying out costs about ten milliseconds, so a frame that would look
/// identical is skipped — that is what keeps a still picture free. But
/// "identical" is about what the *status* says, and the status is not the only
/// reason egui needs a frame.
///
/// `input_pending` is that other reason, and it is the one that shipped
/// broken in v0.17.0: a press on a toolbar button changes no status at all —
/// the file, the zoom, the mode are all what they were — so the frame carrying
/// the press was never laid out and the button did nothing. A hands-on run
/// found it; 316 tests did not.
fn worth_laying_out(status_changed: bool, toast_fading: bool, input_pending: bool, has_frame: bool) -> bool {
    // With nothing drawn yet there is nothing to keep showing, so the first
    // frame is always laid out.
    !has_frame || status_changed || toast_fading || input_pending
}

/// The size a picture presents after a turn, when the turn changed it.
///
/// `None` when the shape is unchanged and the framing should be left where the
/// user put it. A quarter turn exchanges the axes — a landscape photograph
/// fitted to the window becomes a portrait one that no longer fits — while a
/// half turn presents exactly the same rectangle.
///
/// A free function because the alternative is not testable: the condition
/// lives inside a method that needs a renderer and an image on screen, and
/// mutation testing showed it inverted there without a single test noticing.
fn size_after_turn(size: (u32, u32), before: Orientation, after: Orientation) -> Option<(u32, u32)> {
    (before.swaps_axes() != after.swaps_axes()).then_some((size.1, size.0))
}

/// What was on screen when a new image arrived.
struct Previous {
    view: View,
    fidelity: Fidelity,
    /// Whether it is the same file, rather than a neighbour.
    same_file: bool,
}

/// How to frame an image that has just arrived.
///
/// A free function rather than three arms inside `upload`, because the choice
/// is the feature: mutation testing showed that deleting the zoom lock's arm
/// there passed every test and the whole gate — the lock's own unit tests
/// prove what `carry_onto` computes, not that anything calls it. This is a
/// decision made of plain values, so it can be asked directly.
fn frame_arriving(previous: Option<Previous>, image: (u32, u32), window: (u32, u32), scale_factor: f32, locked: bool) -> View {
    let fresh = || View::new(image, window, scale_factor);
    let Some(previous) = previous else {
        return fresh();
    };

    // The same file arriving at its full resolution: the framing is kept and
    // rebased, so the swap reads as the picture sharpening rather than as
    // anything moving. `same_file` is what makes this the same file — the
    // fidelity alone would also match the thumbnail of the *next* file, which
    // is a different picture entirely.
    if previous.fidelity == Fidelity::Thumbnail && previous.same_file {
        let mut view = previous.view;
        view.rebase(image);
        return view;
    }

    // With the lock on, the framing carries from the picture before — the same
    // magnification over the same part of the frame, which is what makes a
    // series comparable by stepping through it.
    if locked {
        return previous.view.carry_onto(image, window, scale_factor);
    }

    fresh()
}

/// The earlier of two deadlines, when there is one.
fn soonest(left: Option<Instant>, right: Option<Instant>) -> Option<Instant> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (deadline, None) | (None, deadline) => deadline,
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
            Event::Counted(path, histogram) => self.counted(path, *histogram),
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
        // egui's view of the window, and its place on the GPU. Both are built
        // here because both need something that does not exist until now: the
        // window for one, the device and the surface format for the other.
        self.input = Some(egui_winit::State::new(
            self.interface.context().clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        ));
        self.renderer = Some(renderer);

        let initial = std::mem::take(&mut self.initial);
        if !initial.is_empty() {
            self.open(&initial);
        }

        window.set_visible(true);
        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // The interface sees every event first, and says whether it wants one.
        // A click on the status line is the interface's; a click on the
        // photograph is the viewer's.
        let claimed = match (self.window.clone(), self.input.as_mut()) {
            (Some(window), Some(input)) => {
                let response = input.on_window_event(&window, &event);
                // `RedrawRequested` is excluded deliberately, and the reason
                // was measured: egui answers every redraw by asking for
                // another, so treating that as a reason to lay out again is a
                // loop — 573 layouts in three seconds on a still picture,
                // which is the idle promise spent entirely on chrome. Real
                // input is what earns a frame; a redraw is the frame.
                if earns_a_layout(response.repaint, &event) {
                    // Both halves matter: the window has to be asked for a
                    // frame, and the interface has to be allowed to lay one
                    // out when it comes. Asking without allowing is a redraw
                    // that skips straight past egui, which is how the toolbar
                    // shipped drawing and not answering.
                    self.wants_frame = true;
                    window.request_redraw();
                }
                response.consumed
            }
            _ => false,
        };

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
                if let (Some(overlay), Some(renderer)) = (self.overlay.as_mut(), self.renderer.as_ref()) {
                    overlay.resize(renderer.size());
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

            // A key let go while another window is in front sends its release
            // there, so the loupe would stay down with nothing holding it.
            WindowEvent::Focused(false) => self.release_loupe(),

            // Dragged onto a monitor with different scaling: the surface size
            // follows in a `Resized` event, but the framing must be redone
            // against the new scale factor either way.
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let (Some(renderer), Some(shown)) = (&self.renderer, self.shown.as_mut()) {
                    shown.view.resize(renderer.size(), scale_factor as f32);
                }
                self.refresh();
            }

            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),

            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() && !claimed => {
                // A chord is answered here rather than in `handle_key`, which
                // knows nothing about modifiers: `Ctrl+C` and `C` are
                // different keys to a person and must be to the viewer.
                if self.modifiers.control_key() {
                    self.handle_chord(&event.logical_key);
                } else {
                    self.handle_key(&event.logical_key, event_loop);
                }
            }

            // The loupe is the one key that does something on the way up: it
            // is held rather than toggled, so the release is what gives the
            // framing back. It is answered even when egui claimed the press —
            // a release swallowed by a panel that opened mid-press would leave
            // the picture stuck at 100% with nothing holding it there.
            WindowEvent::KeyboardInput { event, .. } if !event.state.is_pressed() => {
                if is_loupe(&event.logical_key) {
                    self.release_loupe();
                }
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
                (MouseButton::Left, ElementState::Pressed) => {
                    if self.picking {
                        // The eyedropper owns the left button while it is up:
                        // a click is what takes the colour, and panning the
                        // picture out from under the pointer mid-read would
                        // make the reading name a pixel nobody aimed at.
                        self.copy_reading();
                    } else {
                        self.dragging = true;
                    }
                }
                (MouseButton::Left, ElementState::Released) => self.dragging = false,
                (MouseButton::Middle, ElementState::Pressed) => self.reframe(Reframe::Toggle),
                _ => {}
            },

            WindowEvent::CursorMoved { position, .. } => {
                let delta = ((position.x - self.cursor.x) as f32, (position.y - self.cursor.y) as f32);
                self.cursor = position;
                // The toolbar comes back when the pointer reaches for it. egui
                // lays out in logical points, so the reveal band is measured
                // there too — on a scaled display the physical pixel count
                // would name a different strip of the window than the one the
                // toolbar occupies.
                if self.follow_pointer(Some(position)) {
                    self.request_redraw();
                }
                if self.picking {
                    self.take_reading();
                    self.request_redraw();
                }
                if self.dragging
                    && let Some(shown) = self.shown.as_mut()
                {
                    shown.view.pan(delta);
                    self.request_redraw();
                }
            }

            WindowEvent::CursorLeft { .. } => {
                self.dragging = false;
                // A pointer that has left the window is not reaching for
                // anything, so the chrome goes away with it.
                if self.follow_pointer(None) {
                    self.request_redraw();
                }
            }

            WindowEvent::DroppedFile(path) => self.open(&[path]),

            WindowEvent::RedrawRequested => self.redraw(event_loop),

            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture arriving with nothing before it is framed for itself.
    #[test]
    fn the_first_image_is_framed_for_itself() {
        let view = frame_arriving(None, (4000, 3000), (1000, 800), 1.0, false);
        assert_eq!(view.mode(), FitMode::Fit);
    }

    /// The zoom lock has to actually be consulted.
    ///
    /// Found by mutation: with the decision inline in `upload`, replacing the
    /// lock's condition with `false` passed all 316 tests and the whole gate.
    /// `carry_onto` had tests of its own, which proved what it computes and
    /// not that anything calls it.
    #[test]
    fn the_lock_decides_how_a_neighbour_is_framed() {
        let mut zoomed = View::new((4000, 3000), (1000, 800), 1.0);
        zoomed.zoom_to_at(3.0, (500.0, 400.0));

        let previous = || {
            Some(Previous {
                view: zoomed,
                fidelity: Fidelity::Full,
                same_file: false,
            })
        };

        // Locked: the neighbour arrives at the zoom the user set.
        let locked = frame_arriving(previous(), (4000, 3000), (1000, 800), 1.0, true);
        assert_eq!(locked.mode(), FitMode::Free, "the lock did not carry the framing");
        assert!((locked.scale() - 3.0).abs() < 0.01, "the neighbour came out at {}", locked.scale());

        // Unlocked: it is fitted, as a folder of unrelated pictures wants.
        let unlocked = frame_arriving(previous(), (4000, 3000), (1000, 800), 1.0, false);
        assert_eq!(unlocked.mode(), FitMode::Fit, "an unlocked neighbour kept the previous framing");
    }

    /// A thumbnail being replaced by its own full image keeps its framing —
    /// and the *next* file's thumbnail must not, which is what the path
    /// comparison is for. Without it a zoomed-in view would jump onto a
    /// different picture as though it were the same one.
    #[test]
    fn a_thumbnail_is_only_rebased_onto_the_file_it_stood_in_for() {
        let mut zoomed = View::new((400, 300), (1000, 800), 1.0);
        zoomed.zoom_to_at(4.0, (500.0, 400.0));

        let same = frame_arriving(
            Some(Previous {
                view: zoomed,
                fidelity: Fidelity::Thumbnail,
                same_file: true,
            }),
            (4000, 3000),
            (1000, 800),
            1.0,
            false,
        );
        assert_eq!(same.mode(), FitMode::Free, "the framing was thrown away when the full image arrived");

        let other = frame_arriving(
            Some(Previous {
                view: zoomed,
                fidelity: Fidelity::Thumbnail,
                same_file: false,
            }),
            (4000, 3000),
            (1000, 800),
            1.0,
            false,
        );
        assert_eq!(other.mode(), FitMode::Fit, "another file's thumbnail inherited this one's framing");
    }

    /// Even under the lock, a file's own full image replaces its thumbnail by
    /// rebasing rather than by carrying: the two are the same picture, and
    /// carrying would re-derive the framing from a fraction instead of
    /// keeping it exactly.
    #[test]
    fn the_lock_does_not_disturb_a_thumbnail_becoming_its_own_image() {
        let mut zoomed = View::new((400, 300), (1000, 800), 1.0);
        zoomed.zoom_to_at(4.0, (500.0, 400.0));
        zoomed.pan((80.0, 0.0));

        let framed = frame_arriving(
            Some(Previous {
                view: zoomed,
                fidelity: Fidelity::Thumbnail,
                same_file: true,
            }),
            (4000, 3000),
            (1000, 800),
            1.0,
            true,
        );
        // Rebasing keeps the picture the same size on screen: a thumbnail at
        // 4x becomes the full image at 0.4x, ten times smaller in pixels.
        assert!((framed.scale() - 0.4).abs() < 0.01, "the swap moved the picture: {}", framed.scale());
    }

    /// A redraw must not earn another layout, or the viewer never sleeps.
    ///
    /// Measured, not reasoned: letting it through ran at 573 layouts in three
    /// seconds on a still picture. The fix for the dead toolbar buttons
    /// introduced exactly this, and a hands-on run caught it — a still image
    /// costing nothing is a promise from v0.12.0.
    #[test]
    fn a_redraw_does_not_earn_another_layout() {
        // Real input, which is what should earn one. `Focused` stands for it
        // here because a pointer event needs a `DeviceId`, which only the
        // event loop can make.
        assert!(
            earns_a_layout(true, &WindowEvent::Focused(true)),
            "real input earned no layout, so the toolbar would not answer",
        );

        // The redraw egui asks for in answer to the last one.
        assert!(
            !earns_a_layout(true, &WindowEvent::RedrawRequested),
            "a redraw earned another layout, which is the loop that spent the idle promise",
        );

        // And an event egui does not want a frame for earns nothing either.
        assert!(!earns_a_layout(false, &WindowEvent::RedrawRequested));
        assert!(!earns_a_layout(false, &WindowEvent::Focused(true)));
    }

    /// A press on a toolbar button changes no status, and must still get a
    /// frame laid out for it.
    ///
    /// This is the defect that shipped in v0.17.0: the interface was laid out
    /// only when its own digest moved, and a click moves nothing in it. The
    /// button drew and did nothing. Found by hand, not by 316 tests, and the
    /// reason the condition lives in a function that can be asked.
    #[test]
    fn pending_input_earns_a_frame_even_when_nothing_else_changed() {
        // The settled case: a picture up, nothing moving, nobody touching it.
        assert!(!worth_laying_out(false, false, false, true), "an idle viewer laid out a frame for nothing");

        // Each reason on its own earns one.
        assert!(worth_laying_out(true, false, false, true), "a changed status did not earn a frame");
        assert!(worth_laying_out(false, true, false, true), "a fading toast did not earn a frame");
        assert!(
            worth_laying_out(false, false, true, true),
            "a press earned no frame, so a toolbar button would draw and do nothing",
        );

        // And with nothing drawn yet, there is always a first frame.
        assert!(worth_laying_out(false, false, false, false), "the first frame was skipped");
    }

    /// A quarter turn exchanges what the picture presents; a half turn does
    /// not, and must leave the framing where the user put it.
    ///
    /// Found by mutation: inverting this condition inside `turn` passed every
    /// test and the whole gate, because the method needs a renderer and an
    /// image on screen and nothing can call it.
    #[test]
    fn only_a_quarter_turn_changes_the_shape_a_picture_presents() {
        let size = (4000, 3000);

        // Upright to a quarter turn: the axes exchange.
        assert_eq!(
            size_after_turn(size, Orientation::Normal, Orientation::Rotate90),
            Some((3000, 4000)),
            "a quarter turn did not exchange the axes",
        );
        // A quarter turn onward from there: they exchange back.
        assert_eq!(size_after_turn((3000, 4000), Orientation::Rotate90, Orientation::Rotate180), Some((4000, 3000)));

        // A half turn presents the same rectangle, so the framing stands.
        assert_eq!(
            size_after_turn(size, Orientation::Normal, Orientation::Rotate180),
            None,
            "a half turn reframed a picture whose shape did not change",
        );
        assert_eq!(size_after_turn(size, Orientation::Rotate90, Orientation::Rotate270), None);

        // And no turn at all changes nothing.
        assert_eq!(size_after_turn(size, Orientation::Normal, Orientation::Normal), None);
    }

    /// Four quarter turns bring the picture back to the shape it started as,
    /// which is what stops a series of turns from drifting.
    #[test]
    fn four_turns_return_the_shape_it_started_with() {
        let mut size = (4000, 3000);
        let mut orientation = Orientation::Normal;
        for _ in 0..4 {
            let next = orientation.turned(true);
            if let Some(turned) = size_after_turn(size, orientation, next) {
                size = turned;
            }
            orientation = next;
        }
        assert_eq!(size, (4000, 3000), "the shape drifted over four turns");
        assert_eq!(orientation, Orientation::Normal);
    }

    /// Every action there is.
    ///
    /// Held against the enum by a match below rather than written out and
    /// trusted: a hand-kept list is one an added action is forgotten from, and
    /// the two tests over it would then pass while saying nothing about the
    /// new button. Adding a variant now fails to compile until it is listed.
    const EVERY_ACTION: [Action; 17] = [
        Action::Previous,
        Action::Next,
        Action::ZoomOut,
        Action::ZoomIn,
        Action::Fit,
        Action::Actual,
        Action::TurnLeft,
        Action::TurnRight,
        Action::Backdrop,
        Action::Lock,
        Action::Info,
        Action::Histogram,
        Action::Clipping,
        Action::Pick,
        Action::Passport,
        Action::FullScreen,
        Action::Keys,
    ];

    /// What makes `EVERY_ACTION` exhaustive: this match has no catch-all, so a
    /// new variant stops the build here, and the assertion catches one that
    /// was added to the enum and to this match but not to the list.
    #[test]
    fn every_action_is_in_the_list_the_tests_walk() {
        for action in EVERY_ACTION {
            // The arms exist to be exhaustive; what they map to is unused.
            let _ = match action {
                Action::Previous => 0,
                Action::Next => 1,
                Action::ZoomOut => 2,
                Action::ZoomIn => 3,
                Action::Fit => 4,
                Action::Actual => 5,
                Action::TurnLeft => 6,
                Action::TurnRight => 7,
                Action::Backdrop => 8,
                Action::Lock => 9,
                Action::Info => 10,
                Action::Histogram => 11,
                Action::Clipping => 12,
                Action::Pick => 13,
                Action::Passport => 14,
                Action::FullScreen => 15,
                Action::Keys => 16,
            };
        }

        // Every variant appears once: a list with a duplicate would be the
        // right length while leaving an action unwalked.
        let mut seen: Vec<String> = EVERY_ACTION.iter().map(|action| format!("{action:?}")).collect();
        seen.sort();
        let before = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), before, "an action is listed twice, so another one is missing");
    }

    /// Every action the toolbar offers has to resolve to a key the viewer
    /// actually answers.
    ///
    /// Found by mutation: with the button wired straight to its own `match`,
    /// swapping two arms — the back button stepping forward — passed every
    /// test and the whole gate. The button now names a key instead, and this
    /// is what stops it naming one that does nothing.
    #[test]
    fn every_toolbar_action_names_a_key_the_viewer_answers() {
        for action in EVERY_ACTION {
            let key = key_for(action);
            assert!(handled(&key), "the toolbar's {action:?} asks for {key:?}, which the viewer ignores");
        }
    }

    /// Two buttons meaning the same key would be two buttons doing the same
    /// thing, and one of them would be wrong.
    #[test]
    fn no_two_toolbar_actions_mean_the_same_key() {
        let actions = EVERY_ACTION;
        for (index, action) in actions.iter().enumerate() {
            for other in &actions[index + 1..] {
                assert_ne!(
                    format!("{:?}", key_for(*action)),
                    format!("{:?}", key_for(*other)),
                    "{action:?} and {other:?} both press the same key",
                );
            }
        }
    }

    /// `handled` is written from the same arms as `handle_key` and would drift
    /// if nothing held it down: every key the sheet advertises must be one.
    #[test]
    fn the_keys_the_sheet_advertises_are_all_answered() {
        for key in [
            Key::Named(NamedKey::ArrowLeft),
            Key::Named(NamedKey::ArrowRight),
            Key::Named(NamedKey::Home),
            Key::Named(NamedKey::End),
            Key::Named(NamedKey::Space),
            Key::Named(NamedKey::F11),
            Key::Named(NamedKey::Escape),
            Key::Character("0".into()),
            Key::Character("1".into()),
            Key::Character("+".into()),
            Key::Character("-".into()),
            Key::Character("?".into()),
            Key::Character("l".into()),
            Key::Character("r".into()),
            Key::Character("R".into()),
            Key::Character("b".into()),
            Key::Character("i".into()),
            Key::Character("z".into()),
            Key::Character("h".into()),
            Key::Character("c".into()),
            Key::Character("p".into()),
            Key::Character("k".into()),
        ] {
            assert!(handled(&key), "the key sheet lists {key:?}, which the viewer ignores");
        }

        // And a key nobody claims is not answered, or this would pass on a
        // `handled` that always says yes.
        assert!(!handled(&Key::Character("q".into())), "an unclaimed key was reported as answered");
        assert!(!handled(&Key::Named(NamedKey::Tab)));
    }

    /// The thing this stage could most easily get wrong: `Ctrl+C` and `C` are
    /// different keys to a person, and a viewer that confused them would
    /// toggle the clipping zebra when asked to copy the picture.
    #[test]
    fn a_chord_is_not_the_bare_key() {
        assert_eq!(chord_for(&Key::Character("c".into())), Some(Chord::CopyPicture));
        // The same key without Ctrl is the zebra, which `handled` answers and
        // `chord_for` is never asked about — the two paths are separate, and
        // this is what says so.
        assert!(handled(&Key::Character("c".into())), "the bare key lost its own meaning");

        assert_eq!(chord_for(&Key::Character("v".into())), Some(Chord::Paste));
        // Shift arrives as the capital letter.
        assert_eq!(chord_for(&Key::Character("C".into())), Some(Chord::CopyPath));
    }

    /// A chord nobody claims does nothing, rather than falling through to the
    /// key handler and doing something unrelated.
    #[test]
    fn an_unclaimed_chord_does_nothing() {
        for key in [
            Key::Character("z".into()),
            Key::Character("h".into()),
            Key::Character("1".into()),
            Key::Named(NamedKey::ArrowRight),
        ] {
            assert_eq!(chord_for(&key), None, "{key:?} was taken for a chord");
        }
    }

    /// Copying the picture and copying the path are different things, and the
    /// only difference in the key is Shift. Getting them the wrong way round
    /// would put a path where a picture was wanted.
    #[test]
    fn shift_is_what_separates_the_two_copies() {
        assert_eq!(chord_for(&Key::Character("c".into())), Some(Chord::CopyPicture));
        assert_eq!(chord_for(&Key::Character("C".into())), Some(Chord::CopyPath));
        assert_ne!(
            chord_for(&Key::Character("c".into())),
            chord_for(&Key::Character("C".into())),
            "Shift made no difference",
        );
    }

    /// The key sheet advertises the chords, so they have to be ones the viewer
    /// answers — the same rule the plain keys are held to, which is what stops
    /// the sheet promising something that does nothing.
    #[test]
    fn every_chord_the_sheet_advertises_is_answered() {
        let advertised: Vec<&str> = crate::interface::KEYS
            .iter()
            .map(|(key, _)| *key)
            .filter(|key| key.starts_with("Ctrl+"))
            .collect();
        assert_eq!(advertised.len(), 3, "the sheet lists {advertised:?}, which is not the three chords");

        for key in advertised {
            // The letter at the end of the chord, and whether Shift is in it.
            let letter = key.rsplit('+').next().expect("a chord names a key");
            let shifted = key.contains("Shift");
            let character = if shifted { letter.to_uppercase() } else { letter.to_lowercase() };
            assert!(
                chord_for(&Key::Character(character.as_str().into())).is_some(),
                "the key sheet lists {key}, which the viewer ignores",
            );
        }
    }

    /// The loupe is the one key answered on the way up as well as on the way
    /// down, and the two have to name the same key or it would stick down:
    /// pressed with one spelling, released with another, and nothing left to
    /// let go of.
    #[test]
    fn the_loupe_is_recognised_on_the_press_and_on_the_release() {
        for key in [Key::Character("z".into()), Key::Character("Z".into())] {
            assert!(handled(&key), "the loupe's {key:?} is not answered on the press");
            assert!(is_loupe(&key), "the loupe's {key:?} is not recognised on the release");
        }

        // And nothing else is taken for it, or an unrelated key coming up
        // would drop a loupe somebody was holding.
        for key in [
            Key::Character("i".into()),
            Key::Character("h".into()),
            Key::Character("1".into()),
            Key::Named(NamedKey::Space),
        ] {
            assert!(!is_loupe(&key), "{key:?} was taken for the loupe");
        }
    }

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

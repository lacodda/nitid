//! The window and the event loop.
//!
//! Redraws are event-driven: a still image costs no GPU time, which is what
//! lets a viewer sit open on a laptop without draining the battery.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use moxcms::ColorProfile;

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

/// Run the viewer, optionally opening a file straight away.
pub fn run(path: Option<PathBuf>) -> Result<()> {
    // A user event carries a finished background decode back to this thread.
    let event_loop = EventLoop::<Decoded>::with_user_event().build().context("creating the event loop")?;
    // Wait for input rather than spinning: nothing moves unless the user acts.
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    let loader = Loader::new(move |decoded| {
        // The loop may already be gone if the window was closed mid-decode;
        // there is nobody left to tell, and that is fine.
        let _ = proxy.send_event(decoded);
    });

    let mut app = App::new(path, loader);
    event_loop.run_app(&mut app).context("running the viewer")?;

    app.into_result()
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
}

struct App {
    /// The file to open once the window exists.
    initial: Option<PathBuf>,
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
    /// A failure that must end the run; reported after the loop exits, since
    /// `ApplicationHandler` methods cannot return one.
    failure: Option<anyhow::Error>,
}

impl App {
    fn new(initial: Option<PathBuf>, loader: Loader) -> Self {
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

        self.set_title();
        self.request_redraw();
        self.prefetch_neighbours();
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
        /// How far the size on screen may drift from what was drawn before it
        /// is worth drawing again. A quarter is under one wheel notch, so a
        /// deliberate zoom redraws while a nudge does not.
        const TOLERANCE: f32 = 1.25;

        let Some(shown) = self.shown.as_mut() else {
            return;
        };
        let Some(vector) = shown.vector.clone() else {
            return;
        };

        let (wanted_width, wanted_height) = shown.view.scaled_size();
        let (wanted_width, wanted_height) = (wanted_width.round().max(1.0) as u32, wanted_height.round().max(1.0) as u32);
        // Width alone decides: the aspect ratio is fixed by the document, so
        // height moves with it and testing both would say the same thing twice.
        let drawn_width = shown.rasterised_at.0;

        let grew = wanted_width as f32 > drawn_width as f32 * TOLERANCE;
        let shrank = (wanted_width as f32) < drawn_width as f32 / TOLERANCE;
        if !(grew || shrank) {
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
                self.set_title();
                self.request_redraw();
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
            Key::Named(NamedKey::ArrowRight | NamedKey::PageDown | NamedKey::Space) => {
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

impl ApplicationHandler<Decoded> for App {
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: Decoded) {
        self.decoded(event);
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

        if let Some(path) = self.initial.take() {
            match Folder::open(&path) {
                Ok(folder) => self.folder = Some(folder),
                Err(error) => eprintln!("nitid: {error:#}"),
            }
            self.show(&path);
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
                self.request_redraw();
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

            WindowEvent::DroppedFile(path) => {
                match Folder::open(&path) {
                    Ok(folder) => self.folder = Some(folder),
                    Err(error) => eprintln!("nitid: {error:#}"),
                }
                self.show(&path);
            }

            WindowEvent::RedrawRequested => self.redraw(event_loop),

            _ => {}
        }
    }
}

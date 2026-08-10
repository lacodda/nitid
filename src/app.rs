//! The window and the event loop.
//!
//! Redraws are event-driven: a still image costs no GPU time, which is what
//! lets a viewer sit open on a laptop without draining the battery.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::folder::Folder;
use crate::gpu::Renderer;
use crate::image_source::{self, LoadedImage, Orientation};
use crate::view::{FitMode, View};

/// A wheel notch is 120 units of a high-resolution scroll device; a trackpad
/// reports fractions of that, so dividing keeps both feeling the same.
const PIXELS_PER_NOTCH: f32 = 120.0;

/// The window size used before an image sets one.
const DEFAULT_WINDOW: (u32, u32) = (1280, 800);

/// Run the viewer, optionally opening a file straight away.
pub fn run(path: Option<PathBuf>) -> Result<()> {
    let event_loop = EventLoop::new().context("creating the event loop")?;
    // Wait for input rather than spinning: nothing moves unless the user acts.
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(path);
    event_loop.run_app(&mut app).context("running the viewer")?;

    app.into_result()
}

/// What the viewer is showing, once a file has been opened.
struct Shown {
    orientation: Orientation,
    view: View,
}

struct App {
    /// The file to open once the window exists.
    initial: Option<PathBuf>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    folder: Option<Folder>,
    shown: Option<Shown>,
    cursor: PhysicalPosition<f64>,
    dragging: bool,
    /// A failure that must end the run; reported after the loop exits, since
    /// `ApplicationHandler` methods cannot return one.
    failure: Option<anyhow::Error>,
}

impl App {
    fn new(initial: Option<PathBuf>) -> Self {
        Self {
            initial,
            window: None,
            renderer: None,
            folder: None,
            shown: None,
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

    /// Decode a file, upload it, and frame it in the current window.
    fn show(&mut self, path: &Path) {
        let loaded = match image_source::load(path) {
            Ok(loaded) => loaded,
            Err(error) => {
                // A file that will not open is a fact about the file, not a
                // reason to close the viewer: report it and keep the window.
                eprintln!("nitid: {error:#}");
                return;
            }
        };

        self.upload(&loaded);
        self.set_title();
        self.request_redraw();
    }

    fn upload(&mut self, loaded: &LoadedImage) {
        let scale_factor = self.scale_factor();
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        renderer.set_image(&loaded.image);
        self.shown = Some(Shown {
            orientation: loaded.orientation,
            view: View::new(loaded.display_size(), renderer.size(), scale_factor),
        });
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

    /// Physical pixels per logical pixel on the monitor showing the window.
    fn scale_factor(&self) -> f32 {
        self.window.as_ref().map(|window| window.scale_factor() as f32).unwrap_or(1.0)
    }

    /// The framing changed: repaint and let the title follow the zoom.
    fn refresh(&self) {
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

        if let Err(error) = renderer.render(shown) {
            self.failure = Some(error);
            event_loop.exit();
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

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title("nitid")
            .with_inner_size(LogicalSize::new(DEFAULT_WINDOW.0, DEFAULT_WINDOW.1))
            // The window stays hidden until the first frame is ready: showing
            // it earlier is the white flash every other viewer opens with.
            .with_visible(false);

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.failure = Some(anyhow::Error::new(error).context("creating the window"));
                event_loop.exit();
                return;
            }
        };

        let size = window.inner_size();
        let renderer = match Renderer::new(window.clone(), (size.width, size.height)) {
            Ok(renderer) => renderer,
            Err(error) => {
                self.failure = Some(error);
                event_loop.exit();
                return;
            }
        };

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

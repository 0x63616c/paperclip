//! The Mac desktop preview backend.
//!
//! Opens a window, fits the fixed canvas into it with letterbox bars, maps
//! pointer input back into canvas space, and can capture both the canvas and
//! the window at full resolution.
//!
//! What this is for: seeing whether a screen reads at the real aspect ratio
//! and whether touch targets land where they look like they land, without a
//! tablet in the loop. What it is not: evidence about the device. Rendering
//! here says nothing about whether the Paper Pro can present this, how an
//! e-ink refresh looks, or what the pen reports (§15, and the project's
//! standing constraints).

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::canvas::Canvas;
use crate::display::DisplayMapping;
use crate::geometry::{Point, Size};
use crate::input::{Pointer, PointerEvent, PointerPhase};

/// What the preview hands back to the caller.
#[derive(Debug)]
#[non_exhaustive]
pub enum PreviewEvent<'a> {
    /// Draw a frame. The canvas arrives cleared to the paper colour.
    Render(&'a mut Canvas),
    /// A pointer event, already in canvas space. Events that landed in the
    /// letterbox never arrive.
    Pointer(PointerEvent),
    /// A printable key, lower-cased. Escape closes the window and is not
    /// forwarded.
    Key(char),
}

/// What to capture, and whether to stop afterwards.
///
/// `window` is the whole window buffer including letterbox bars — the
/// evidence that the fit is right. `canvas` is the screen itself at full
/// target resolution, which is what a reviewer actually wants to look at.
#[derive(Debug, Clone, Default)]
pub struct Capture {
    /// Where to write the canvas at full target resolution.
    pub canvas: Option<PathBuf>,
    /// Where to write the window buffer at its physical resolution.
    pub window: Option<PathBuf>,
    /// Close the window once the capture is written.
    pub exit_after: bool,
}

impl Capture {
    fn is_empty(&self) -> bool {
        self.canvas.is_none() && self.window.is_none()
    }
}

/// How to open the preview window.
#[derive(Debug, Clone)]
pub struct PreviewOptions {
    /// Window title.
    pub title: String,
    /// Canvas extent to present. Normally [`SCREEN`](crate::SCREEN).
    pub canvas: Size,
    /// Tallest the window may open, in logical points. A 2160 px portrait
    /// canvas does not fit on a laptop screen at 1:1, so the window opens
    /// scaled and the mapping absorbs the difference.
    pub max_logical_height: f64,
    /// Exact window size in logical points, overriding
    /// [`Self::max_logical_height`]. Useful for capturing a window whose
    /// aspect ratio does not match the canvas, which is the only way to see
    /// the letterbox working.
    pub logical_size: Option<(f64, f64)>,
    /// Optional screenshot capture.
    pub capture: Capture,
}

impl PreviewOptions {
    /// Default options for a screen-sized canvas.
    pub fn new(title: impl Into<String>, canvas: Size) -> Self {
        Self {
            title: title.into(),
            canvas,
            max_logical_height: 880.0,
            logical_size: None,
            capture: Capture::default(),
        }
    }

    fn initial_logical_size(&self) -> LogicalSize<f64> {
        if let Some((width, height)) = self.logical_size {
            return LogicalSize::new(width.max(240.0), height.max(320.0));
        }
        let scale = (self.max_logical_height / self.canvas.height.max(1) as f64).min(1.0);
        LogicalSize::new(
            (self.canvas.width as f64 * scale).max(240.0),
            (self.canvas.height as f64 * scale).max(320.0),
        )
    }
}

/// Why the preview could not run.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PreviewError {
    /// The platform would not give us an event loop — no window server, most
    /// likely, which is what happens over a plain SSH session.
    #[error("cannot open a window on this display")]
    EventLoop(#[source] winit::error::EventLoopError),
    /// The window itself could not be created.
    #[error("cannot create the preview window")]
    Window(#[source] winit::error::OsError),
    /// No drawing surface for the window.
    #[error("cannot create a drawing surface for the preview window: {0}")]
    Surface(String),
    /// A canvas of the requested size could not be allocated.
    #[error("cannot allocate a {width}x{height} canvas")]
    Canvas {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// A screenshot could not be written.
    #[error("cannot write {path}")]
    Capture {
        /// The file that was being written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

/// Opens the preview window and runs until it closes.
///
/// `handler` is called for every frame and every input event; it owns whatever
/// state the screens need.
pub fn run(
    options: PreviewOptions,
    handler: impl FnMut(PreviewEvent<'_>),
) -> Result<(), PreviewError> {
    let event_loop = EventLoop::new().map_err(PreviewError::EventLoop)?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let canvas = Canvas::new(options.canvas).ok_or(PreviewError::Canvas {
        width: options.canvas.width,
        height: options.canvas.height,
    })?;

    let mut app = Preview {
        options,
        handler,
        canvas,
        surface_canvas: None,
        window: None,
        graphics: None,
        cursor: PhysicalPosition::new(0.0, 0.0),
        pressed: false,
        captured: false,
        failure: None,
    };

    event_loop
        .run_app(&mut app)
        .map_err(PreviewError::EventLoop)?;

    match app.failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

struct Graphics {
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}

struct Preview<H> {
    options: PreviewOptions,
    handler: H,
    canvas: Canvas,
    surface_canvas: Option<Canvas>,
    window: Option<Rc<Window>>,
    graphics: Option<Graphics>,
    cursor: PhysicalPosition<f64>,
    pressed: bool,
    captured: bool,
    failure: Option<PreviewError>,
}

impl<H: FnMut(PreviewEvent<'_>)> Preview<H> {
    fn mapping(&self, surface: PhysicalSize<u32>) -> DisplayMapping {
        DisplayMapping::fit(
            self.options.canvas,
            Size::new(surface.width, surface.height),
        )
    }

    fn stop(&mut self, event_loop: &ActiveEventLoop, error: PreviewError) {
        self.failure = Some(error);
        event_loop.exit();
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let physical = window.inner_size();
        let (Some(width), Some(height)) = (
            NonZeroU32::new(physical.width),
            NonZeroU32::new(physical.height),
        ) else {
            return; // Minimised; nothing to present into.
        };

        self.canvas.clear(crate::color::palette::PAPER);
        (self.handler)(PreviewEvent::Render(&mut self.canvas));

        let mapping = self.mapping(physical);
        let surface_size = Size::new(physical.width, physical.height);
        let needs_new_surface = self
            .surface_canvas
            .as_ref()
            .is_none_or(|canvas| canvas.size() != surface_size);
        if needs_new_surface {
            let Some(canvas) = Canvas::new(surface_size) else {
                self.stop(
                    event_loop,
                    PreviewError::Canvas {
                        width: surface_size.width,
                        height: surface_size.height,
                    },
                );
                return;
            };
            self.surface_canvas = Some(canvas);
        }
        let surface_canvas = self
            .surface_canvas
            .as_mut()
            .expect("the surface canvas was just ensured");
        self.canvas.present_into(surface_canvas, mapping);

        if let Some(graphics) = self.graphics.as_mut() {
            if let Err(error) = graphics.surface.resize(width, height) {
                self.stop(event_loop, PreviewError::Surface(error.to_string()));
                return;
            }
            match graphics.surface.buffer_mut() {
                Ok(mut buffer) => {
                    surface_canvas.fill_argb_buffer(&mut buffer);
                    if let Err(error) = buffer.present() {
                        self.stop(event_loop, PreviewError::Surface(error.to_string()));
                        return;
                    }
                }
                Err(error) => {
                    self.stop(event_loop, PreviewError::Surface(error.to_string()));
                    return;
                }
            }
        }

        if !self.captured && !self.options.capture.is_empty() {
            self.captured = true;
            if let Err(error) = self.write_capture() {
                self.stop(event_loop, error);
                return;
            }
            if self.options.capture.exit_after {
                event_loop.exit();
            }
        }
    }

    fn write_capture(&self) -> Result<(), PreviewError> {
        if let Some(path) = &self.options.capture.canvas {
            self.canvas
                .write_png(path)
                .map_err(|source| PreviewError::Capture {
                    path: path.clone(),
                    source,
                })?;
        }
        if let Some(path) = &self.options.capture.window
            && let Some(surface_canvas) = &self.surface_canvas
        {
            surface_canvas
                .write_png(path)
                .map_err(|source| PreviewError::Capture {
                    path: path.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    fn pointer(&mut self, phase: PointerPhase) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let mapping = self.mapping(window.inner_size());
        let physical = Point::new(self.cursor.x as f32, self.cursor.y as f32);
        if let Some(at) = mapping.to_canvas(physical) {
            (self.handler)(PreviewEvent::Pointer(PointerEvent::new(
                at,
                phase,
                Pointer::Mouse,
            )));
            window.request_redraw();
        }
    }
}

impl<H: FnMut(PreviewEvent<'_>)> ApplicationHandler for Preview<H> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title(self.options.title.clone())
            .with_inner_size(self.options.initial_logical_size());
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Rc::new(window),
            Err(error) => {
                self.stop(event_loop, PreviewError::Window(error));
                return;
            }
        };

        match softbuffer::Context::new(window.clone())
            .and_then(|context| softbuffer::Surface::new(&context, window.clone()))
        {
            Ok(surface) => self.graphics = Some(Graphics { surface }),
            Err(error) => {
                self.stop(event_loop, PreviewError::Surface(error.to_string()));
                return;
            }
        }

        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(event_loop),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                if self.pressed {
                    self.pointer(PointerPhase::Moved);
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if self.pressed {
                    self.pressed = false;
                    self.pointer(PointerPhase::Cancelled);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let phase = match state {
                    ElementState::Pressed => {
                        self.pressed = true;
                        PointerPhase::Down
                    }
                    ElementState::Released => {
                        self.pressed = false;
                        PointerPhase::Up
                    }
                };
                self.pointer(phase);
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match event.logical_key {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Character(ref text) => {
                        if let Some(character) = text.chars().next() {
                            (self.handler)(PreviewEvent::Key(character.to_ascii_lowercase()));
                            if let Some(window) = self.window.as_ref() {
                                window.request_redraw();
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

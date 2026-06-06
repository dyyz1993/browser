//! Window creation + event loop.
//!
//! M5.1 scope: open a winit window, paint text directly into the
//! softbuffer pixel buffer using our hand-written 5x7 bitmap font.
//! No tiny-skia (we don't need vector paths yet). No scrolling / no
//! input handling — M5.3 territory.
//!
//! ## Why no tiny-skia in M5.1
//! tiny-skia 0.11's Path/Rect API changed substantially from earlier
//! versions, and M5.1's goal is "prove GUI toolchain wires up". We
//! can do that with raw pixel writes via softbuffer alone. tiny-skia
//! will land in M5.3 once we need rounded rects / anti-aliased lines
//! for the URL bar.
//!
//! ## Why the window module is not unit-tested
//! Window creation requires a display server. Headless CI machines
//! can't run winit. The `gui_demo` example is the manual acceptance
//! test.

use std::num::NonZeroU32;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

use crate::bitmap_font::{draw_text, BitmapFont};

/// Window configuration. Width/height are in physical pixels (the
/// pixmap size we hand to softbuffer).
#[derive(Clone, Debug)]
pub struct WindowConfig {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub scale: usize,
    pub text: String,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "browser".into(),
            width: 800,
            height: 600,
            scale: 2,
            text: String::new(),
        }
    }
}

/// Open a window and run the event loop until the user closes it.
///
/// This function never returns under normal conditions (it blocks on
/// `EventLoop::run`). It's meant to be called from `main()`.
///
/// # Errors
/// Returns an error if the event loop or window can't be created
/// (typical on a headless system without a display server).
pub fn run_window(config: WindowConfig) -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    let mut state = AppState::Loading(config);
    event_loop.run_app(&mut state)?;
    Ok(())
}

/// Internal application state. Driven by winit's `ApplicationHandler`.
enum AppState {
    /// Window not yet created. We create it on the first `Resumed`
    /// event, which is how winit handles cross-platform lifecycle
    /// (some platforms destroy/re-create windows on suspend).
    Loading(WindowConfig),
    /// Window created and ready.
    Ready(Box<RunningState>),
}

struct RunningState {
    window: std::sync::Arc<Window>,
    text: String,
    scale: usize,
    url_buffer: String, // M7.5.1: URL input buffer.
    scroll_y: usize,    // M7.5.4: vertical scroll offset (lines).
    surface: softbuffer::Surface<std::sync::Arc<Window>, std::sync::Arc<Window>>,
}

impl ApplicationHandler for AppState {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let config = match std::mem::replace(self, AppState::Loading(WindowConfig::default())) {
            AppState::Loading(c) => c,
            AppState::Ready(_) => return,
        };
        let window_attrs = Window::default_attributes()
            .with_title(&config.title)
            .with_inner_size(LogicalSize::new(config.width, config.height));
        let window = match event_loop.create_window(window_attrs) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("[gui] failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };
        let window_arc = std::sync::Arc::new(window);
        let context = match softbuffer::Context::new(window_arc.clone()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[gui] failed to create softbuffer context: {e}");
                event_loop.exit();
                return;
            }
        };
        let mut surface = match softbuffer::Surface::new(&context, window_arc.clone()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[gui] failed to create softbuffer surface: {e}");
                event_loop.exit();
                return;
            }
        };
        surface
            .resize(
                NonZeroU32::new(config.width).unwrap(),
                NonZeroU32::new(config.height).unwrap(),
            )
            .ok();
        *self = AppState::Ready(Box::new(RunningState {
            window: window_arc,
            text: config.text,
            scale: config.scale,
            url_buffer: String::new(),
            scroll_y: 0,
            surface,
        }));
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let st = match self {
            AppState::Ready(s) => s,
            AppState::Loading(_) => return,
        };

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            // M7.5.4: mouse wheel → scroll.
            WindowEvent::MouseWheel { delta, .. } => {
                use winit::event::MouseScrollDelta;
                let delta_y = match delta {
                    MouseScrollDelta::LineDelta(_, dy) => dy,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                };
                // Negative delta = scroll down → increase scroll_y.
                // One scroll event ≈ 1 line.
                if delta_y < 0.0 {
                    st.scroll_y += 1;
                } else if st.scroll_y > 0 {
                    st.scroll_y -= 1;
                }
                st.window.request_redraw();
            }
            // M7.5.2: keyboard input → URL buffer.
            WindowEvent::KeyboardInput { event, .. } => {
                use winit::keyboard::{Key, NamedKey};
                match event.logical_key.as_ref() {
                    Key::Character(c) => {
                        for ch in c.chars() {
                            if !ch.is_control() {
                                st.url_buffer.push(ch);
                            }
                        }
                        st.window.request_redraw();
                    }
                    Key::Named(NamedKey::Backspace) => {
                        st.url_buffer.pop();
                        st.window.request_redraw();
                    }
                    Key::Named(NamedKey::Enter) => {
                        if st.url_buffer.is_empty() {
                            return;
                        }
                        let url = st.url_buffer.clone();
                        eprintln!("[gui] navigate to: {url}");
                        st.url_buffer.clear();
                        st.window.request_redraw();
                        // M7.5.3: fetch + render (deferred to later).
                    }
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = render_frame(st) {
                    eprintln!("[gui] render error: {e}");
                }
                // Don't request_redraw unconditionally — that pegs CPU.
                // The OS / user input will trigger redraws as needed.
            }
            _ => {}
        }
    }
}

/// ARGB pixel packer (matches softbuffer's expected format).
#[inline]
fn pack_argb(r: u8, g: u8, b: u8) -> u32 {
    0xff00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// Render one frame into the softbuffer surface.
fn render_frame(st: &mut RunningState) -> Result<(), Box<dyn std::error::Error>> {
    let w = st.window.inner_size().width;
    let h = st.window.inner_size().height;
    let w_nz = NonZeroU32::new(w).ok_or("zero window width")?;
    let h_nz = NonZeroU32::new(h).ok_or("zero window height")?;
    st.surface.resize(w_nz, h_nz)?;

    let bg = pack_argb(0xff, 0xff, 0xff); // white background
    let title_bg = pack_argb(0xd9, 0xd9, 0xd9); // gray title bar
    let fg = pack_argb(0x00, 0x00, 0x00); // black text

    let url_bar_height = 30u32;
    let text_y0 = 40usize;
    let margin = 20usize;
    let url_bar_text_y = 6usize; // y offset inside URL bar for text.

    let mut buffer = st.surface.buffer_mut()?;
    let buf_len = (w as usize) * (h as usize);
    // Sanity: buffer length should match window dimensions.
    if buffer.len() < buf_len {
        return Err(format!("softbuffer len {} < window {}*{}", buffer.len(), w, h).into());
    }

    // 1) Fill background white.
    for px in buffer.iter_mut().take(buf_len) {
        *px = bg;
    }

    // 2) URL bar (gray) at the top (M7.5.1).
    for y in 0..url_bar_height {
        let row = y as usize * w as usize;
        for x in 0..w {
            buffer[row + x as usize] = title_bg;
        }
    }

    // 2b) Render URL buffer text inside URL bar (M7.5.1).
    // If empty, show placeholder "https://".
    let display_url = if st.url_buffer.is_empty() {
        "https://".to_string()
    } else {
        st.url_buffer.clone()
    };
    draw_text(margin, url_bar_text_y, st.scale, &display_url, |x, y| {
        if x < w as usize && y < h as usize {
            buffer[y * w as usize + x] = fg;
        }
    });

    // 3) Text body — paint glyph pixels in black.
    // M7.5.4: scroll_y offset — skip first N lines.
    let scrolled_text = st.text.lines().skip(st.scroll_y).collect::<Vec<_>>().join(
        "
",
    );
    draw_text(margin, text_y0, st.scale, &scrolled_text, |x, y| {
        if x < w as usize && y < h as usize {
            buffer[y * w as usize + x] = fg;
        }
    });

    // Title-bar text — paint the window's first line as a "page title"
    // surrogate. Real browsers show the <title> here; we'll wire that
    // in M5.3. For now just echo the first line of body.
    if let Some(first_line) = st.text.lines().next() {
        let title_text = if first_line.is_empty() {
            "browser"
        } else {
            first_line
        };
        draw_text(margin, 8, st.scale, title_text, |x, y| {
            if x < w as usize && y < h as usize {
                buffer[y * w as usize + x] = fg;
            }
        });
    }

    // Quiet warnings about BitmapFont being unused as a type — we
    // only use its constants indirectly via draw_text.
    let _ = BitmapFont::CHAR_WIDTH;

    buffer.present()?;
    Ok(())
}

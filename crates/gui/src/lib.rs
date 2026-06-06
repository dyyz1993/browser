//! `browser-gui` — cross-platform GUI window.
//!
//! M5.1 scope: open a winit window, paint a single frame of text
//! using tiny-skia. No font yet — text is rendered as filled rects
//! representing a 5x7 bitmap font. The point of M5.1 is to prove the
//! GUI toolchain (winit + softbuffer + tiny-skia) wires up cleanly
//! on all three target platforms (Windows / macOS / Linux).
//!
//! M5.2 will swap in a real font (cosmic-text or fontdue).

#![forbid(unsafe_code)]

pub mod bitmap_font;
pub mod font;
pub mod window;

pub use bitmap_font::{draw_text, measure_text, BitmapGlyph};
pub use window::{run_window, WindowConfig};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name() {
        assert_eq!(env!("CARGO_PKG_NAME"), "browser-gui");
    }
}

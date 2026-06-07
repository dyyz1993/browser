//! `browser-render` — rasterizes the layout tree to characters/pixels.
//!
//! M2.7 scope: terminal ASCII renderer.

#![forbid(unsafe_code)]

pub mod ascii;
pub mod font;
pub mod image;

pub use ascii::{render_ascii, render_ascii_colored};
pub use font::{FontRenderer, LayoutMetrics};
pub use image::{image_file_to_ascii, image_to_ascii_from_img, resolve_local_image_src};

//! `browser-render` — rasterizes the layout tree to characters/pixels.
//!
//! M2.7 scope: terminal ASCII renderer.

#![forbid(unsafe_code)]

pub mod ascii;

pub use ascii::render_ascii;

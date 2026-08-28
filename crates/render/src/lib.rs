//! `browser-render` — rasterizes the layout tree to characters/pixels.
//!
//! M2.7 scope: terminal ASCII renderer.

#![forbid(unsafe_code)]

pub mod ascii;
pub mod font;
pub mod image;
pub mod pixel;
pub mod svg;

pub use ascii::{render_ascii, render_ascii_colored};
pub use font::{FontRenderer, LayoutMetrics};
pub use image::{
    image_file_to_ascii, image_file_to_ascii_colored, image_to_ascii_from_img,
    image_to_ascii_from_img_colored, resolve_local_image_src,
};
pub use pixel::{cell_metrics, layout_columns_for_px, render_pixel, StyleMap, BASE_FONT_PX};
pub use svg::{parse_svg_shapes, svg_to_ascii, SvgShape};

//! gui_demo — minimal M5.1 acceptance test.
//!
//! Opens an 800x600 window and paints a static text message using
//! the hand-written 5x7 bitmap font + softbuffer. No HTML / JS
//! involved — this example proves the winit + softbuffer + bitmap
//! font pipeline works end-to-end on the host system.
//!
//! # Run
//! ```sh
//! cargo run --example gui_demo -p browser-gui
//! ```
//!
//! # Acceptance
//! A window titled "browser" appears showing two pieces of text:
//! - A gray title bar at the top with "browser"
//! - Below: "Hello, browser!\nM5.1 GUI toolchain wires up."
//!
//! Close the window to exit (exit code 0).

use browser_gui::{run_window, WindowConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = WindowConfig {
        title: "browser".into(),
        width: 800,
        height: 600,
        scale: 3,
        text: "Hello, browser!\nM5.1 GUI toolchain wires up.".into(),
    };
    run_window(config)
}

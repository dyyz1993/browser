//! `browser-cdp` — Chrome DevTools Protocol server.
//!
//! ## Overview
//!
//! Exposes the browser to CDP clients (Puppeteer, Playwright, Chrome DevTools)
//! over a WebSocket server on a configurable port (default 9222).
//!
//! **Why CDP?** (M40 assessment finding) Real-website JS compatibility is
//! capped by the boa engine (ES6 shorthand syntax unsupported). CDP lets
//! tools drive the browser via domains that **don't depend on JS execution**
//! (`Page.captureScreenshot`, `DOM.querySelector`, `Network.getResponseBody`),
//! so the tool ecosystem mitigates the engine limitation.
//!
//! ## Architecture
//!
//! ```text
//! CDP client (Puppeteer/Playwright)
//!       │ WebSocket (ws://localhost:9222)
//!       ▼
//! ┌─────────────────────────────────────────┐
//! │ CdpServer                                │
//! │   TcpListener (tokio)                    │
//! │     └─ accept loop → CdpSession          │
//! │           ├─ WS handshake (RFC 6455)     │  ← reuses browser_ws
//! │           ├─ read CDP messages (JSON-RPC)│  ← this crate
//! │           └─ dispatch → Domain handlers  │  ← M44+ (Page/Runtime/DOM…)
//! └─────────────────────────────────────────┘
//! ```
//!
//! ## M42 scope
//!
//! - WebSocket server (handshake + frame I/O, reusing `browser_ws`)
//! - JSON-RPC message parsing/serialization (hand-rolled, no serde)
//! - `CdpSession`: accept a connection, echo unknown methods with an error
//! - CLI `cdp --port 9222` subcommand
//!
//! M43+ adds the HTTP discovery endpoints and domain handlers.

#![forbid(unsafe_code)]

pub mod discovery;
pub mod dom_domain;
pub mod jsonrpc;
pub mod page;
pub mod runtime_domain;
pub mod server;

pub use jsonrpc::{CdpError, CdpMessage, CdpRequest, CdpResponse};
pub use server::{CdpServer, CdpSession};

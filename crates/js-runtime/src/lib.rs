//! `browser-js-runtime` — JavaScript engine embedding + DOM bridge.
//!
//! M3.1 scope: basic eval.
//! - [`JsRuntime`] — owns a boa `Context`
//! - [`JsRuntime::eval`] — run code, return result as String
//! - [`JsRuntime::execute`] — run code, ignore result

#![forbid(unsafe_code)]

pub mod runtime;

pub use runtime::JsRuntime;

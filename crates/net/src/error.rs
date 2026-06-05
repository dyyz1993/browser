//! Error types for `browser-net`.

use thiserror::Error;

/// All errors produced by this crate.
#[derive(Debug, Error)]
pub enum NetError {
    /// URL failed to parse or had no host.
    #[error("invalid URL: {url:?}")]
    InvalidUrl { url: String },

    /// URL parsed but scheme is not `http` / `https`.
    #[error("unsupported scheme: {scheme:?} (only http/https)")]
    UnsupportedScheme { scheme: String },

    /// Request failed at the transport / connect / write layer.
    #[error("request failed: {0}")]
    RequestFailed(String),

    /// Response body could not be read.
    #[error("read body failed: {0}")]
    ReadFailed(String),

    /// Server returned a non-2xx status code.
    #[error("HTTP error status: {code}")]
    BadStatus { code: u16 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_is_informative() {
        let e = NetError::InvalidUrl {
            url: "not a url".into(),
        };
        assert!(format!("{e}").contains("not a url"));
    }
}

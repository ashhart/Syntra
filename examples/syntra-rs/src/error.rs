// Copyright 2026 Ash Hart. Apache-2.0.

//! Error type for the Syntra client.

use thiserror::Error;

/// Errors returned by the Syntra client.
///
/// The variants cover the failure modes the Python and Go ports surface:
/// transport failures, non-2xx HTTP responses, and malformed JSON payloads.
/// `RetryClient` swallows these and falls back to its default policy; direct
/// users of [`crate::SyntraClient`] receive them and decide how to react.
#[derive(Debug, Error)]
pub enum Error {
    /// The underlying HTTP request failed to send or receive bytes (DNS
    /// failure, connection refused, timeout, TLS handshake failure, etc.).
    #[error("syntra: transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// Syntra returned a non-2xx status. The body is included verbatim for
    /// debugging — it is typically a short JSON error from the appliance.
    #[error("syntra: HTTP {status}: {body}")]
    Status {
        /// HTTP status code as a `u16`.
        status: u16,
        /// Response body verbatim (may be empty).
        body: String,
    },

    /// JSON serialization (request body) or deserialization (response body)
    /// failed.
    #[error("syntra: JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// The decide payload did not match the expected shape. Returned when the
    /// caller supplies a [`crate::DecideContext::Features`] that is not a JSON
    /// object, or any other client-side validation failure.
    #[error("syntra: invalid request: {0}")]
    InvalidRequest(String),
}

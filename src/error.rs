//! What can go wrong in a satellite.

use thiserror::Error;

use crate::audio::AudioError;
use crate::protocol::ProtocolError;

/// An error of this SDK.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// A value or a message does not follow `nestor.satellite/1`.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// An observation the device did not declare, or the household did not allow.
    #[error("{message_type} needs the {capability} capability, declared and allowed")]
    CapabilityNotAllowed {
        /// The observation's message type.
        message_type: &'static str,
        /// The capability it needs.
        capability: &'static str,
    },
    /// The core refused the manifest (HTTP 422): retrying would not help.
    #[error("the core refused the manifest: {0}")]
    ManifestRefused(String),
    /// The core does not know this device's token any more: pair again.
    #[error("the core refused this satellite's token")]
    TokenRefused,
    /// The core answered something unexpected.
    #[error("the core answered {status}: {detail}")]
    Core {
        /// The HTTP status.
        status: u16,
        /// What the core said.
        detail: String,
    },
    /// The core could not be reached, or its answer could not be read.
    #[error("HTTP: {0}")]
    Http(#[from] reqwest::Error),
    /// The WebRTC connection failed.
    #[error("WebRTC: {0}")]
    WebRtc(#[from] webrtc::error::Error),
    /// An audio device failed.
    #[error(transparent)]
    Audio(#[from] AudioError),
    /// Opus could not encode or decode.
    #[error("Opus: {0}")]
    Opus(#[from] opus::Error),
    /// Reading or writing the token file failed.
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    /// A JSON document could not be read or written.
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The WebRTC link is not as expected (no codec negotiated, no track…).
    #[error("link: {0}")]
    Link(&'static str),
    /// Something took longer than allowed.
    #[error("timed out: {0}")]
    Timeout(&'static str),
}

/// A result of this SDK.
pub type Result<T, E = Error> = std::result::Result<T, E>;

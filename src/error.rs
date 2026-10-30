//! What can go wrong in a satellite.

use thiserror::Error;

use crate::audio::AudioError;
use crate::protocol::{PairingMethod, ProtocolError};

/// An error of this SDK.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// A value or a message does not follow `nestor.satellite/2`.
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
    /// The core speaks another version of the protocol (`protocol_unsupported`): update the device
    /// or the core.
    #[error("the core does not speak {protocol}: {0}", protocol = crate::protocol::PROTOCOL)]
    ProtocolUnsupported(String),
    /// The core refused the pairing request as malformed (`invalid_request`): retrying would not help.
    #[error("the core refused the pairing request: {0}")]
    PairingRefused(String),
    /// What was handed in is neither an enrollment code nor a pairing URI.
    #[error("not an enrollment code: {0}")]
    InvalidEnrollment(ProtocolError),
    /// The pairing URI is for another core than the satellite's.
    #[error("this pairing URI is for the core at {uri_core}, not {core}")]
    OtherCore {
        /// The core of the pairing URI.
        uri_core: String,
        /// The satellite's core.
        core: String,
    },
    /// An enrollment code handed in by a method the satellite does not declare.
    #[error("{0} is not among this satellite's pairing methods")]
    MethodNotDeclared(PairingMethod),
    /// An enrollment code was handed in while the satellite was not waiting for one.
    #[error("the satellite is not pairing")]
    NotPairing,
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

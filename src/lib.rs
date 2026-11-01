//! Write a [Nestor](https://github.com/nestor-home/nestor) satellite in Rust: pair with the core,
//! stream audio over WebRTC, carry out its intents and report observations, following the
//! `nestor.satellite/1` protocol.
//!
//! ```no_run
//! use nestor_satellite::{AUDIO_CAPTURE, AUDIO_PLAYBACK, Device, Manifest, Satellite, TokenStore};
//!
//! # async fn run() -> nestor_satellite::Result<()> {
//! let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon")?;
//! let manifest = Manifest::new(device, [AUDIO_CAPTURE, AUDIO_PLAYBACK])?;
//! let tokens = TokenStore::new("~/.config/nestor-satellite/enceinte-salon.json");
//! Satellite::builder("http://nestor.local:7860", manifest, tokens).build()?.run().await
//! # }
//! ```

pub mod audio;
pub mod body;
mod client;
pub mod codes;
#[cfg(feature = "audio")]
pub mod devices;
pub mod dispatch;
mod error;
pub mod i18n;
mod media;
pub mod pairing;
pub mod protocol;
mod speech;
pub mod wire;

pub use audio::{AudioError, AudioSink, AudioSource, RecordingSink, SilenceSource};
pub use body::{Body, NoBody};
pub use client::{Backoff, Observer, PAIRING_CODE_ENV, PAIRING_URI_ENV, Satellite, SatelliteBuilder};
pub use codes::{Enrollment, EnrollmentCode, PairingUri};
pub use error::{Error, Result};
pub use i18n::Language;
pub use pairing::{Display, Enroller, TokenStore, Voice};
pub use protocol::{
    AUDIO_CAPTURE, AUDIO_PLAYBACK, Behavior, Capability, Device, EXPRESSION, EnrollmentMethod, Expression, HEAD_MOTION,
    IDENTIFY, Manifest, PERSON_DETECTION, PROTOCOL, PairingMethod, ProtocolError, SPEECH_ACTIVITY, Sdk,
};

//! Write a [Nestor](https://github.com/nestor-home/nestor) satellite in Rust: pair with the core,
//! hold one WebSocket link to it, carry out its intents, report observations and talk in writing,
//! following the `nestor.satellite/1` protocol. With the `voice` feature, it talks aloud too: WebRTC
//! audio, and the device code said with a bundled voice.
//!
//! ```no_run
//! use nestor_satellite::{Device, IDENTIFY, Manifest, Satellite, TokenStore};
//!
//! # async fn run() -> nestor_satellite::Result<()> {
//! let device = Device::new("ecran-cuisine", "raspberry-pi-zero", "cuisine")?;
//! let manifest = Manifest::new(device, [IDENTIFY])?;
//! let tokens = TokenStore::new("~/.config/nestor-satellite/ecran-cuisine.json");
//! let satellite = Satellite::builder("http://nestor.local:8471", manifest, tokens)
//!     .text_display(|text: &str, _last: bool| println!("{text}")) // declares text.display
//!     .text_input() // declares text.input: send lines with satellite.observer().input_text(…)
//!     .build()?;
//! satellite.run().await
//! # }
//! ```
//!
//! Features: `voice` (WebRTC audio with your own [`AudioSource`]/[`AudioSink`]), `audio` (the
//! machine's microphone and speaker through cpal, with `voice`), `cli` (the `nestor-satellite`
//! command).

#[cfg(feature = "voice")]
pub mod audio;
pub mod body;
mod client;
pub mod codes;
#[cfg(feature = "audio")]
pub mod devices;
pub mod dispatch;
mod error;
pub mod i18n;
#[cfg(feature = "voice")]
mod media;
pub mod pairing;
pub mod protocol;
#[cfg(feature = "voice")]
mod speech;
pub mod text;
pub mod update;
#[cfg(feature = "voice")]
mod voice;
#[cfg(not(feature = "voice"))]
#[path = "voiceless.rs"]
mod voice;
pub mod wire;

#[cfg(feature = "voice")]
pub use audio::{AudioError, AudioSink, AudioSource, RecordingSink, SilenceSource};
pub use body::{Body, NoBody};
pub use client::{Backoff, Observer, PAIRING_CODE_ENV, PAIRING_URI_ENV, Satellite, SatelliteBuilder};
pub use codes::{Enrollment, EnrollmentCode, PairingUri};
pub use error::{Error, Result};
pub use i18n::Language;
pub use pairing::{Display, Enroller, TokenStore, Voice};
pub use protocol::{
    AUDIO_CAPTURE, AUDIO_PLAYBACK, Behavior, Capability, Device, EXPRESSION, EnrollmentMethod, Expression, HEAD_MOTION,
    IDENTIFY, Manifest, PERSON_DETECTION, PROTOCOL, PairingMethod, ProtocolError, SPEECH_ACTIVITY, Software,
    TEXT_DISPLAY, TEXT_INPUT, UPDATE,
};
pub use text::TextDisplay;
pub use update::{Restart, UpdateError, Updater};

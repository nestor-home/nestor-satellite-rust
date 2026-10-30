//! Write a [Nestor](https://github.com/nestor-home/nestor) satellite in Rust: pair with the core,
//! stream audio over WebRTC, carry out its intents and report observations, following the
//! `nestor.satellite/1` protocol.

pub mod body;
pub mod dispatch;
pub mod protocol;
pub mod wire;

pub use body::{Body, NoBody};
pub use protocol::{
    AUDIO_CAPTURE, AUDIO_PLAYBACK, Behavior, Capability, Device, EXPRESSION, Expression, HEAD_MOTION, Manifest,
    PERSON_DETECTION, PROTOCOL, ProtocolError, SPEECH_ACTIVITY, Sdk,
};

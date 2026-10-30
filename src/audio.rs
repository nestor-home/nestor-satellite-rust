//! Audio in and out of a satellite: 16-bit mono PCM, whatever the hardware.
//!
//! An [`AudioSource`] gives the audio to send to the core (the microphone); an [`AudioSink`] plays
//! what the core says (the speaker). Both work at one of the rates Opus encodes natively (8, 12,
//! 16, 24 or 48 kHz), so the SDK never resamples. `nestor_satellite::devices` (feature `audio`)
//! implements both with cpal; the doubles here serve tests and devices without one or the other.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

/// The rate of the core's audio, and the default of sources and sinks.
pub const SAMPLE_RATE: u32 = 48_000;
/// The rates Opus encodes and decodes natively.
pub const OPUS_RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
/// The length of an audio frame on the wire.
pub const FRAME: Duration = Duration::from_millis(20);

/// The number of samples in one [`FRAME`] at `sample_rate`.
pub fn frame_samples(sample_rate: u32) -> usize {
    (sample_rate / 50) as usize
}

/// An audio device failed, or does not exist.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("audio: {0}")]
pub struct AudioError(pub String);

/// Where the satellite's audio comes from (a microphone).
#[async_trait]
pub trait AudioSource: Send + 'static {
    /// Its rate, one of [`OPUS_RATES`].
    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    /// Open the device; nothing to open by default.
    async fn start(&mut self) -> Result<(), AudioError> {
        Ok(())
    }

    /// The next audio, mono, as soon as it is captured; 20 ms chunks are best, any length works.
    async fn read(&mut self) -> Result<Vec<i16>, AudioError>;

    /// Release the device.
    async fn close(&mut self) {}
}

/// Where the core's voice goes (a speaker).
#[async_trait]
pub trait AudioSink: Send + 'static {
    /// Its rate, one of [`OPUS_RATES`].
    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    /// Open the device; nothing to open by default.
    async fn start(&mut self) -> Result<(), AudioError> {
        Ok(())
    }

    /// Play mono audio as it arrives; must not block for long.
    async fn write(&mut self, pcm: &[i16]) -> Result<(), AudioError>;

    /// Release the device.
    async fn close(&mut self) {}
}

pub(crate) fn check_rate(rate: u32) -> Result<u32, AudioError> {
    if OPUS_RATES.contains(&rate) {
        Ok(rate)
    } else {
        Err(AudioError(format!("{rate} Hz is not an Opus rate ({OPUS_RATES:?})")))
    }
}

/// A device without a microphone still sends a (silent) stream: the core's turn taking expects one.
///
/// It returns 20 ms of silence at once; the SDK paces what it sends in real time.
#[derive(Debug, Clone)]
pub struct SilenceSource {
    sample_rate: u32,
}

impl SilenceSource {
    /// Silence at 48 kHz.
    pub fn new() -> Self {
        Self { sample_rate: SAMPLE_RATE }
    }
}

impl Default for SilenceSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AudioSource for SilenceSource {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    async fn read(&mut self) -> Result<Vec<i16>, AudioError> {
        Ok(vec![0; frame_samples(self.sample_rate)])
    }
}

/// Keeps what was played, e.g. to check a device received the core's voice.
#[derive(Debug, Clone, Default)]
pub struct RecordingSink {
    played: Arc<Mutex<Vec<i16>>>,
}

impl RecordingSink {
    /// An empty recording.
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything played so far (clones of the sink share the recording).
    pub fn played(&self) -> Vec<i16> {
        self.played.lock().map(|p| p.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl AudioSink for RecordingSink {
    async fn write(&mut self, pcm: &[i16]) -> Result<(), AudioError> {
        let mut played = self.played.lock().map_err(|_| AudioError("recording poisoned".into()))?;
        played.extend_from_slice(pcm);
        Ok(())
    }
}

/// No speaker: received audio is decoded (so the link stays healthy) and dropped.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Drain;

#[async_trait]
impl AudioSink for Drain {
    async fn write(&mut self, _pcm: &[i16]) -> Result<(), AudioError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn silence_comes_in_20_ms_frames() {
        let mut source = SilenceSource::new();
        assert_eq!(source.read().await.unwrap(), vec![0; 960]);
    }

    #[tokio::test]
    async fn clones_of_a_recording_sink_share_what_was_played() {
        let sink = RecordingSink::new();
        let mut writer = sink.clone();
        writer.write(&[1, 2]).await.unwrap();
        writer.write(&[3]).await.unwrap();
        assert_eq!(sink.played(), [1, 2, 3]);
    }

    #[test]
    fn only_opus_rates_are_accepted() {
        assert!(check_rate(16_000).is_ok());
        assert!(check_rate(44_100).is_err());
    }
}

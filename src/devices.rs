//! The machine's microphone and speaker through cpal (feature `audio`): CoreAudio, ALSA, WASAPI…
//!
//! cpal streams live on a thread of their own (they are not `Send` on every platform) and call
//! back from the audio thread: captured chunks cross into async code through a small queue that
//! keeps the freshest second, and the speaker plays from a bounded buffer (late audio is dropped
//! rather than letting the voice lag further and further behind). Devices whose rate is not an
//! Opus rate are resampled (linearly) to and from 48 kHz; channels are mixed down or duplicated.

use std::collections::VecDeque;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::audio::{AudioError, AudioSink, AudioSource, OPUS_RATES, SAMPLE_RATE, frame_samples};

const MIC_CHUNKS: usize = 50; // one second of 20 ms chunks
const SPEAKER_BUFFER_S: f64 = 0.5;

/// The machine's microphone (the default input, or one whose name contains `device`).
pub struct Microphone {
    device: Option<String>,
    rate: u32,
    chunks: Arc<Chunks>,
    stream: Option<StreamThread>,
}

/// The machine's speaker (the default output, or one whose name contains `device`).
pub struct Speaker {
    device: Option<String>,
    rate: u32,
    buffer: Arc<Mutex<VecDeque<f32>>>,
    resampler: Option<Resampler>,
    stream: Option<StreamThread>,
}

impl Microphone {
    /// The default input device.
    pub fn new() -> Self {
        Self { device: None, rate: SAMPLE_RATE, chunks: Arc::new(Chunks::default()), stream: None }
    }

    /// The first input device whose name contains `name`.
    pub fn named(name: impl Into<String>) -> Self {
        Self { device: Some(name.into()), ..Self::new() }
    }
}

impl Default for Microphone {
    fn default() -> Self {
        Self::new()
    }
}

impl Speaker {
    /// The default output device.
    pub fn new() -> Self {
        Self {
            device: None,
            rate: SAMPLE_RATE,
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            resampler: None,
            stream: None,
        }
    }

    /// The first output device whose name contains `name`.
    pub fn named(name: impl Into<String>) -> Self {
        Self { device: Some(name.into()), ..Self::new() }
    }
}

impl Default for Speaker {
    fn default() -> Self {
        Self::new()
    }
}

/// The names of the input and output devices, to choose one.
pub fn device_names() -> Result<(Vec<String>, Vec<String>), AudioError> {
    let host = cpal::default_host();
    let inputs = host.input_devices().map_err(audio_error)?;
    let outputs = host.output_devices().map_err(audio_error)?;
    Ok((inputs.filter_map(|d| name_of(&d)).collect(), outputs.filter_map(|d| name_of(&d)).collect()))
}

#[async_trait]
impl AudioSource for Microphone {
    fn sample_rate(&self) -> u32 {
        self.rate
    }

    async fn start(&mut self) -> Result<(), AudioError> {
        let chunks = self.chunks.clone();
        let wanted = self.device.clone();
        let (thread, rate) = StreamThread::spawn(move || {
            let host = cpal::default_host();
            let device = match &wanted {
                Some(name) => find(host.input_devices().map_err(audio_error)?, name),
                None => host.default_input_device(),
            }
            .ok_or_else(|| AudioError(format!("no input device {}", wanted.as_deref().unwrap_or("(default)"))))?;
            let supported = device.default_input_config().map_err(audio_error)?;
            let config = supported.config();
            let stream = match supported.sample_format() {
                SampleFormat::F32 => capture::<f32>(&device, config, chunks),
                SampleFormat::I16 => capture::<i16>(&device, config, chunks),
                SampleFormat::U16 => capture::<u16>(&device, config, chunks),
                SampleFormat::I32 => capture::<i32>(&device, config, chunks),
                other => Err(AudioError(format!("unsupported input sample format {other}"))),
            }?;
            stream.play().map_err(audio_error)?;
            info!(device = name_of(&device).unwrap_or_default(), rate = config.sample_rate, "microphone open");
            Ok((stream, opus_rate(config.sample_rate)))
        })
        .await?;
        self.rate = rate;
        self.stream = Some(thread);
        Ok(())
    }

    async fn read(&mut self) -> Result<Vec<i16>, AudioError> {
        if self.stream.is_none() {
            return Err(AudioError("the microphone is not started".into()));
        }
        loop {
            if let Some(chunk) = self.chunks.pop() {
                return Ok(chunk);
            }
            self.chunks.ready.notified().await;
        }
    }

    async fn close(&mut self) {
        if let Some(stream) = self.stream.take() {
            stream.stop();
        }
    }
}

#[async_trait]
impl AudioSink for Speaker {
    fn sample_rate(&self) -> u32 {
        self.rate
    }

    async fn start(&mut self) -> Result<(), AudioError> {
        let buffer = self.buffer.clone();
        let wanted = self.device.clone();
        let (thread, (rate, device_rate)) = StreamThread::spawn(move || {
            let host = cpal::default_host();
            let device = match &wanted {
                Some(name) => find(host.output_devices().map_err(audio_error)?, name),
                None => host.default_output_device(),
            }
            .ok_or_else(|| AudioError(format!("no output device {}", wanted.as_deref().unwrap_or("(default)"))))?;
            let supported = device.default_output_config().map_err(audio_error)?;
            let config = supported.config();
            let stream = match supported.sample_format() {
                SampleFormat::F32 => playback::<f32>(&device, config, buffer),
                SampleFormat::I16 => playback::<i16>(&device, config, buffer),
                SampleFormat::U16 => playback::<u16>(&device, config, buffer),
                SampleFormat::I32 => playback::<i32>(&device, config, buffer),
                other => Err(AudioError(format!("unsupported output sample format {other}"))),
            }?;
            stream.play().map_err(audio_error)?;
            info!(device = name_of(&device).unwrap_or_default(), rate = config.sample_rate, "speaker open");
            Ok((stream, (opus_rate(config.sample_rate), config.sample_rate)))
        })
        .await?;
        self.rate = rate;
        self.resampler = (rate != device_rate).then(|| Resampler::new(rate, device_rate));
        self.stream = Some(thread);
        Ok(())
    }

    async fn write(&mut self, pcm: &[i16]) -> Result<(), AudioError> {
        let samples: Vec<f32> = pcm.iter().map(|s| f32::from(*s) / 32768.0).collect();
        let samples = match &mut self.resampler {
            Some(resampler) => resampler.process(&samples),
            None => samples,
        };
        let limit = (f64::from(self.device_rate()) * SPEAKER_BUFFER_S) as usize;
        let mut buffer = self.buffer.lock().map_err(|_| AudioError("speaker buffer poisoned".into()))?;
        buffer.extend(samples);
        let excess = buffer.len().saturating_sub(limit);
        buffer.drain(..excess);
        Ok(())
    }

    async fn close(&mut self) {
        if let Some(stream) = self.stream.take() {
            stream.stop();
        }
    }
}

impl Speaker {
    fn device_rate(&self) -> u32 {
        self.resampler.as_ref().map_or(self.rate, |r| r.to)
    }
}

/// The device rate when Opus can work at it, 48 kHz otherwise (and the device is resampled).
fn opus_rate(device_rate: u32) -> u32 {
    if OPUS_RATES.contains(&device_rate) { device_rate } else { SAMPLE_RATE }
}

#[derive(Default)]
struct Chunks {
    queue: Mutex<VecDeque<Vec<i16>>>,
    ready: Notify,
}

impl Chunks {
    fn push(&self, chunk: Vec<i16>) {
        if let Ok(mut queue) = self.queue.lock() {
            if queue.len() == MIC_CHUNKS {
                queue.pop_front(); // nobody reads: keep the freshest second
            }
            queue.push_back(chunk);
        }
        self.ready.notify_one();
    }

    fn pop(&self) -> Option<Vec<i16>> {
        self.queue.lock().ok()?.pop_front()
    }
}

fn capture<T>(device: &cpal::Device, config: StreamConfig, chunks: Arc<Chunks>) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels.max(1));
    let rate = opus_rate(config.sample_rate);
    let mut resampler = (rate != config.sample_rate).then(|| Resampler::new(config.sample_rate, rate));
    let chunk = frame_samples(rate);
    let mut pending: Vec<i16> = Vec::with_capacity(chunk * 2);
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mono: Vec<f32> = data
                    .chunks(channels)
                    .map(|frame| frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / frame.len() as f32)
                    .collect();
                let mono = match &mut resampler {
                    Some(resampler) => resampler.process(&mono),
                    None => mono,
                };
                pending.extend(mono.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16));
                while pending.len() >= chunk {
                    chunks.push(pending.drain(..chunk).collect());
                }
            },
            |error| warn!(%error, "microphone stream"),
            None,
        )
        .map_err(audio_error)
}

fn playback<T>(
    device: &cpal::Device,
    config: StreamConfig,
    buffer: Arc<Mutex<VecDeque<f32>>>,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels.max(1));
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
                let mut buffer = buffer.lock().ok();
                for frame in out.chunks_mut(channels) {
                    // silence when there is nothing to play
                    let sample = buffer.as_mut().and_then(|b| b.pop_front()).unwrap_or(0.0);
                    frame.fill(T::from_sample_(sample));
                }
            },
            |error| warn!(%error, "speaker stream"),
            None,
        )
        .map_err(audio_error)
}

/// A cpal stream on a thread of its own, until stopped.
struct StreamThread {
    stop: std_mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

impl StreamThread {
    async fn spawn<V: Send + 'static>(
        open: impl FnOnce() -> Result<(cpal::Stream, V), AudioError> + Send + 'static,
    ) -> Result<(Self, V), AudioError> {
        let (opened_tx, opened) = tokio::sync::oneshot::channel();
        let (stop, stopped) = std_mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("nestor-audio".into())
            .spawn(move || match open() {
                Ok((stream, value)) => {
                    let _ = opened_tx.send(Ok(value));
                    let _ = stopped.recv(); // keeps the stream alive until stopped (or dropped)
                    drop(stream);
                }
                Err(error) => {
                    let _ = opened_tx.send(Err(error));
                }
            })
            .map_err(|e| AudioError(format!("cannot start the audio thread: {e}")))?;
        let value = opened.await.map_err(|_| AudioError("the audio thread ended".into()))??;
        Ok((Self { stop, thread }, value))
    }

    fn stop(self) {
        let _ = self.stop.send(());
        if self.thread.join().is_err() {
            warn!("the audio thread panicked");
        }
    }
}

fn find<I: Iterator<Item = cpal::Device>>(devices: I, name: &str) -> Option<cpal::Device> {
    let mut devices = devices;
    devices.find(|d| name_of(d).is_some_and(|n| n.contains(name)))
}

fn name_of(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_owned())
}

fn audio_error(error: impl std::fmt::Display) -> AudioError {
    AudioError(error.to_string())
}

/// A linear resampler for a mono stream cut in chunks of any length.
pub(crate) struct Resampler {
    step: f64,
    position: f64,
    previous: f32,
    to: u32,
}

impl Resampler {
    pub(crate) fn new(from: u32, to: u32) -> Self {
        Self { step: f64::from(from) / f64::from(to), position: 1.0, previous: 0.0, to }
    }

    /// Positions count from the previous chunk's last sample (0) to this chunk's last (`len`).
    pub(crate) fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity((input.len() as f64 / self.step) as usize + 1);
        let end = input.len() as f64;
        while self.position <= end {
            let index = self.position.floor() as usize;
            let fraction = (self.position - index as f64) as f32;
            let a = if index == 0 { self.previous } else { input[index - 1] };
            let b = input.get(index).copied().unwrap_or(a);
            out.push(a + (b - a) * fraction);
            self.position += self.step;
        }
        self.position -= end;
        if let Some(last) = input.last() {
            self.previous = *last;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_keeps_the_duration_across_chunks() {
        let mut resampler = Resampler::new(44_100, 48_000);
        let produced: usize = (0..100).map(|_| resampler.process(&[0.5; 441]).len()).sum();
        assert!((produced as i64 - 48_000).abs() <= 1, "{produced}");
    }

    #[test]
    fn resampling_interpolates_between_samples() {
        let mut up = Resampler::new(1, 2);
        assert_eq!(up.process(&[1.0, 3.0]), [1.0, 2.0, 3.0]);
        assert_eq!(up.process(&[5.0]), [4.0, 5.0]);
    }

    #[test]
    fn a_device_at_an_opus_rate_is_not_resampled() {
        assert_eq!(opus_rate(16_000), 16_000);
        assert_eq!(opus_rate(44_100), 48_000);
    }

    #[test]
    fn the_microphone_queue_keeps_the_freshest_second() {
        let chunks = Chunks::default();
        for i in 0..60 {
            chunks.push(vec![i]);
        }
        assert_eq!(chunks.pop(), Some(vec![10]));
    }
}

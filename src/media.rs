//! Audio over WebRTC: the source encoded to Opus and paced in real time, the core's Opus voice
//! decoded into the sink.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use rtc::media::Sample;
use tokio::sync::Mutex;
use tracing::{debug, warn};
use webrtc::media_stream::Track;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::rtp_transceiver::RtpSender;

use crate::audio::{AudioSink, AudioSource, FRAME, check_rate, frame_samples};
use crate::body::Body;
use crate::error::{Error, Result};

/// Encodes mono PCM of any chunk length into 20 ms Opus packets.
pub(crate) struct Encoder {
    opus: opus::Encoder,
    frame: usize,
    pending: Vec<i16>,
    out: Vec<u8>,
}

impl Encoder {
    pub(crate) fn new(sample_rate: u32) -> Result<Self> {
        let rate = check_rate(sample_rate)?;
        let opus = opus::Encoder::new(rate, opus::Channels::Mono, opus::Application::Voip)?;
        Ok(Self { opus, frame: frame_samples(rate), pending: Vec::new(), out: vec![0; 4000] })
    }

    /// The Opus packets completed by `pcm` (none while less than 20 ms is buffered).
    pub(crate) fn push(&mut self, pcm: &[i16]) -> Result<Vec<Bytes>> {
        self.pending.extend_from_slice(pcm);
        let mut packets = Vec::new();
        while self.pending.len() >= self.frame {
            let size = self.opus.encode(&self.pending[..self.frame], &mut self.out)?;
            packets.push(Bytes::copy_from_slice(&self.out[..size]));
            self.pending.drain(..self.frame);
        }
        Ok(packets)
    }
}

/// Decodes Opus packets (mono or stereo) into mono PCM at the sink's rate.
pub(crate) struct Decoder {
    opus: opus::Decoder,
    out: Vec<i16>,
}

impl Decoder {
    pub(crate) fn new(sample_rate: u32) -> Result<Self> {
        let rate = check_rate(sample_rate)?;
        Ok(Self {
            opus: opus::Decoder::new(rate, opus::Channels::Mono)?,
            out: vec![0; (rate / 1000 * 120) as usize], // the longest Opus packet: 120 ms
        })
    }

    pub(crate) fn decode(&mut self, packet: &[u8]) -> Result<&[i16]> {
        let samples = self.opus.decode(packet, &mut self.out, false)?;
        Ok(&self.out[..samples])
    }
}

/// Sends the source to the core until it fails or the task is aborted.
pub(crate) async fn send(
    source: Arc<Mutex<Box<dyn AudioSource>>>,
    track: Arc<TrackLocalStaticSample>,
    sender: Arc<dyn RtpSender>,
) -> Result<()> {
    let payload_type = sender
        .get_parameters()
        .await?
        .rtp_parameters
        .codecs
        .first()
        .map(|codec| codec.payload_type)
        .ok_or(Error::Link("no audio codec negotiated"))?;
    let ssrc = *track.ssrcs().await.first().ok_or(Error::Link("the audio track has no SSRC"))?;
    let mut source = source.lock().await;
    let rate = source.sample_rate();
    let mut encoder = Encoder::new(rate)?;
    let started = Instant::now();
    let mut sent: u64 = 0;
    loop {
        // Paced by the samples sent: a microphone waits by itself, silence would not.
        let due = started + Duration::from_secs_f64(sent as f64 / f64::from(rate));
        tokio::time::sleep_until(due.into()).await;
        let pcm = source.read().await?;
        for packet in encoder.push(&pcm)? {
            track
                .sample_writer(ssrc, payload_type)
                .write_sample(&Sample { data: packet, duration: FRAME, ..Sample::new(Instant::now()) })
                .await?;
        }
        sent += pcm.len() as u64;
    }
}

/// Plays the core's track into the sink until the track ends.
pub(crate) async fn play(
    track: Arc<dyn TrackRemote>,
    sink: Arc<Mutex<Box<dyn AudioSink>>>,
    body: Arc<dyn Body>,
) -> Result<()> {
    let mut sink = sink.lock().await;
    let mut decoder = Decoder::new(sink.sample_rate())?;
    while let Some(event) = track.poll().await {
        match event {
            TrackRemoteEvent::OnRtpPacket(packet) => {
                if packet.payload.is_empty() {
                    continue;
                }
                body.heartbeat();
                match decoder.decode(&packet.payload) {
                    Ok(pcm) => sink.write(pcm).await?,
                    Err(error) => warn!(%error, "an undecodable audio packet from the core"),
                }
            }
            TrackRemoteEvent::OnEnded => break,
            _ => {}
        }
    }
    debug!("the core's audio track ended");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_is_cut_in_20_ms_opus_packets_whatever_the_chunks() {
        let mut encoder = Encoder::new(48_000).unwrap();
        assert!(encoder.push(&[0; 500]).unwrap().is_empty());
        assert_eq!(encoder.push(&[0; 1420]).unwrap().len(), 2);
    }

    #[test]
    fn what_is_encoded_decodes_to_the_same_length() {
        let mut encoder = Encoder::new(16_000).unwrap();
        let tone: Vec<i16> = (0..320).map(|i| ((f64::from(i) * 0.3).sin() * 8000.0) as i16).collect();
        let packet = encoder.push(&tone).unwrap().remove(0);
        let mut decoder = Decoder::new(16_000).unwrap();
        assert_eq!(decoder.decode(&packet).unwrap().len(), 320);
    }

    #[test]
    fn a_rate_opus_does_not_know_is_refused() {
        assert!(Encoder::new(44_100).is_err());
        assert!(Decoder::new(44_100).is_err());
    }
}

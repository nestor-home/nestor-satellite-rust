//! The connection of one media session over WebRTC (feature `voice`; the camera, feature `video`).
//!
//! Asked by the core on the link, the satellite connects for that session
//! (`POST /satellites/{device_id}/media`, its token and the session's id, its ICE gathering complete):
//! the camera's video in H.264 when the session films, and for a call its microphone while it plays
//! what the core sends. It only ever connects out; [`crate::sessions`] keeps the rules of when.

use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use rtc::rtp_transceiver::rtp_sender::{RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RtpCodecKind};
use serde_json::json;
use tokio::task::JoinSet;
use tracing::{debug, info, trace};
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::peer_connection::{MediaEngine, PeerConnection, RTCPeerConnectionState, RTCSessionDescription};

use crate::error::{Error, Result};
use crate::media;
use crate::pairing::success;
use crate::sessions::{Connect, Means, MediaLink, Still, Stop};
use crate::voice::{Answer, Audio, Events, Signals, gathered, local_track, log_end, opus_codec, peer};
use crate::wire::MediaOpen;

/// How long the core may take to answer an offer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// What the sessions of `link` film and call with (see [`Audio::media`]).
pub(crate) fn means(audio: &Audio, link: &MediaLink, films: bool, calls: bool) -> Means {
    let connection = Arc::new(Connection { audio: audio.clone(), link: link.clone() });
    let connect: Connect = Arc::new(move |opening, stop| {
        let connection = connection.clone();
        Box::pin(async move { connection.carry(opening, stop).await })
    });
    #[cfg(feature = "video")]
    let still: Option<Still> = audio.camera.clone().map(|camera| -> Still {
        Arc::new(move || {
            let camera = camera.clone();
            Box::pin(async move { camera.still().await })
        })
    });
    #[cfg(not(feature = "video"))]
    let still: Option<Still> = None;
    Means { films, calls, connect: Some(connect), still }
}

struct Connection {
    audio: Audio,
    link: MediaLink,
}

impl Connection {
    /// Connect for `opening`, until the connection ends or `stop` says so; then everything is
    /// released (the connection closed, the camera switched off if nothing else films).
    async fn carry(&self, opening: MediaOpen, mut stop: Stop) -> Result<()> {
        let (events, mut signals) = Events::new();
        let peer = peer(engine()?, &self.audio.ice_servers, events).await?;
        let mut tasks = JoinSet::new();
        #[cfg_attr(not(feature = "video"), allow(unused_mut))]
        let mut filming = false;
        let outcome = tokio::select! {
            outcome = self.converse(&peer, &opening, &mut signals, &mut tasks, &mut filming) => outcome,
            () = stop.wait() => Ok(()),
        };
        tasks.shutdown().await;
        if let Err(error) = peer.close().await {
            debug!(%error, "closing the session's peer connection");
        }
        #[cfg(feature = "video")]
        if let (true, Some(camera)) = (filming, &self.audio.camera) {
            camera.release().await;
        }
        outcome
    }

    #[cfg_attr(not(feature = "video"), allow(unused_variables))] // nothing films without a camera
    async fn converse(
        &self,
        peer: &Arc<dyn PeerConnection>,
        opening: &MediaOpen,
        signals: &mut Signals,
        tasks: &mut JoinSet<()>,
        filming: &mut bool,
    ) -> Result<()> {
        let device = &self.link.device_id;
        if opening.video {
            #[cfg(feature = "video")]
            {
                let camera = self.audio.camera.clone().ok_or(Error::Link("no camera to film with"))?;
                let images = camera.open().await?;
                *filming = true;
                let track = local_track(device, "camera", RtpCodecKind::Video, h264_codec())?;
                let sender = peer.add_track(track.clone() as Arc<dyn TrackLocal>).await?;
                let mut state = signals.state.clone();
                tasks.spawn(async move {
                    if state.wait_for(|s| *s == RTCPeerConnectionState::Connected).await.is_ok() {
                        log_end("camera", film(images, track, sender)).await;
                    }
                });
            }
            #[cfg(not(feature = "video"))]
            return Err(Error::Link("no camera to film with"));
        }
        if opening.audio {
            let track = local_track(device, "microphone", RtpCodecKind::Audio, opus_codec())?;
            let sender = peer.add_track(track.clone() as Arc<dyn TrackLocal>).await?;
            let (mut state, source) = (signals.state.clone(), self.audio.source.clone());
            tasks.spawn(async move {
                if state.wait_for(|s| *s == RTCPeerConnectionState::Connected).await.is_ok() {
                    log_end("microphone", media::send(source, track, sender)).await;
                }
            });
        }

        let offer = gathered(peer, signals, self.audio.gathering_timeout).await?;
        trace!(sdp = %offer, "media offer");
        let response = self
            .link
            .http
            .post(format!("{}/satellites/{device}/media", self.link.core))
            .bearer_auth(&self.link.token)
            .timeout(ANSWER_TIMEOUT)
            .json(&json!({"session": opening.session, "sdp": offer, "type": "offer"}))
            .send()
            .await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            return Err(Error::TokenRefused);
        }
        let answer: Answer = success(response).await?.json().await?;
        trace!(sdp = %answer.sdp, "media answer");
        peer.set_remote_description(RTCSessionDescription::answer(answer.sdp)?).await?;

        loop {
            tokio::select! {
                Some(track) = signals.tracks.recv() => {
                    if opening.audio {
                        let (sink, body) = (self.audio.sink.clone(), self.link.body.clone());
                        tasks.spawn(log_end("speaker", media::play(track, sink, body)));
                    }
                }
                changed = signals.state.changed() => {
                    let state = *signals.state.borrow();
                    if state == RTCPeerConnectionState::Connected {
                        info!(session = %opening.session, "media connected");
                    }
                    if changed.is_err() || matches!(state, RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) {
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Opus for calls, H.264 constrained baseline for the camera: what the core decodes, nothing else.
fn engine() -> Result<MediaEngine> {
    let mut engine = MediaEngine::default();
    engine.register_codec(RTCRtpCodecParameters { rtp_codec: opus_codec(), payload_type: 111 }, RtpCodecKind::Audio)?;
    engine.register_codec(RTCRtpCodecParameters { rtp_codec: h264_codec(), payload_type: 125 }, RtpCodecKind::Video)?;
    Ok(engine)
}

fn h264_codec() -> RTCRtpCodec {
    let feedback = |typ: &str, parameter: &str| RTCPFeedback { typ: typ.to_owned(), parameter: parameter.to_owned() };
    RTCRtpCodec {
        mime_type: rtc::peer_connection::configuration::media_engine::MIME_TYPE_H264.to_owned(),
        clock_rate: 90_000,
        channels: 0,
        sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_owned(),
        rtcp_feedback: vec![feedback("nack", ""), feedback("nack", "pli"), feedback("ccm", "fir")],
    }
}

/// Sends the camera's latest images, encoded in H.264, until the camera stops or the task is aborted.
#[cfg(feature = "video")]
async fn film(
    mut images: crate::video::Images,
    track: Arc<webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample>,
    sender: Arc<dyn webrtc::rtp_transceiver::RtpSender>,
) -> Result<()> {
    use rtc::media::Sample;
    use webrtc::media_stream::Track;

    let payload_type = sender
        .get_parameters()
        .await?
        .rtp_parameters
        .codecs
        .first()
        .map(|codec| codec.payload_type)
        .ok_or(Error::Link("no video codec negotiated"))?;
    let ssrc = *track.ssrcs().await.first().ok_or(Error::Link("the video track has no SSRC"))?;
    let mut encoder = Some(crate::video::H264::new()?);
    let mut last: Option<std::time::Instant> = None;
    loop {
        images.changed().await.map_err(|_| Error::Video("the camera stopped".into()))?;
        let Some(image) = images.borrow_and_update().clone() else { continue };
        let Some(mut working) = encoder.take() else { break Ok(()) };
        let (working, encoded) = tokio::task::spawn_blocking(move || {
            let encoded = working.encode(&image);
            (working, encoded)
        })
        .await
        .map_err(|e| Error::Video(e.to_string()))?;
        encoder = Some(working);
        let encoded = encoded?;
        if encoded.is_empty() {
            continue; // skipped by the rate control
        }
        let now = std::time::Instant::now();
        let duration = last.map_or(Duration::from_millis(33), |last| now - last);
        last = Some(now);
        let sample = Sample { data: encoded.into(), duration, ..Sample::new(now) };
        track.sample_writer(ssrc, payload_type).write_sample(&sample).await?;
    }
}

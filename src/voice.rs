//! Talking aloud (feature `voice`): once the link is welcomed with `audio.capture` or
//! `audio.playback` allowed, the satellite offers a WebRTC connection that carries Opus audio only,
//! the microphone to the core and the core's voice to the speaker. It lives with the link, and is
//! offered again, waiting longer between attempts, when it fails while the link is up.
//!
//! Offers are sent once ICE gathering is complete (no trickle ICE), so no candidate follows.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use rtc::rtp_transceiver::rtp_sender::{RTCRtpCodec, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{Mutex, Notify, mpsc, watch};
use tokio::task::JoinSet;
use tracing::{debug, info, trace, warn};
use webrtc::media_stream::MediaStreamTrack;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_remote::TrackRemote;
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceGatheringState, RTCIceServer, RTCPeerConnectionState, RTCSessionDescription, Registry,
    register_default_interceptors,
};
use webrtc::rtp_transceiver::RtpSender;

use crate::audio::{AudioSink, AudioSource, Drain, SilenceSource};
use crate::body::Body;
use crate::client::Backoff;
use crate::error::{Error, Result};
use crate::i18n::Language;
use crate::media;
use crate::pairing::success;
use crate::sessions::{Hear, Means, MediaLink};
pub(crate) use crate::speech::SpokenCode;

/// What the driver gave to talk aloud and film with, before the satellite runs.
pub(crate) struct VoiceSetup {
    /// The microphone: the satellite declares `audio.capture`.
    pub(crate) source: Option<Box<dyn AudioSource>>,
    /// The speaker: the satellite declares `audio.playback`.
    pub(crate) sink: Option<Box<dyn AudioSink>>,
    /// The sink is a real speaker the device code can be said through.
    pub(crate) audible: bool,
    /// The camera: the satellite declares `video.capture`.
    #[cfg(feature = "video")]
    pub(crate) camera: Option<Box<dyn crate::camera::Camera>>,
    pub(crate) ice_servers: Vec<String>,
    pub(crate) gathering_timeout: Duration,
}

impl Default for VoiceSetup {
    fn default() -> Self {
        Self {
            source: None,
            sink: None,
            audible: false,
            #[cfg(feature = "video")]
            camera: None,
            ice_servers: Vec::new(),
            gathering_timeout: Duration::from_secs(5),
        }
    }
}

impl VoiceSetup {
    /// What it was given: a microphone, a speaker, a camera.
    pub(crate) fn given(&self) -> (bool, bool, bool) {
        #[cfg(feature = "video")]
        let camera = self.camera.is_some();
        #[cfg(not(feature = "video"))]
        let camera = false;
        (self.source.is_some(), self.sink.is_some(), camera)
    }

    /// Whether the sink is a real speaker the device code can be said through.
    pub(crate) fn audible(&self) -> bool {
        self.audible
    }

    /// Open the audio devices given, for the whole run; the camera opens only when a session or a
    /// picture needs it.
    pub(crate) async fn open(&mut self) -> Result<Audio> {
        let mut source = self.source.take().unwrap_or_else(|| Box::new(SilenceSource::new()));
        let mut sink = self.sink.take().unwrap_or_else(|| Box::new(Drain));
        source.start().await?;
        if let Err(error) = sink.start().await {
            source.close().await;
            return Err(error.into());
        }
        Ok(Audio {
            source: Arc::new(Mutex::new(source)),
            sink: Arc::new(Mutex::new(sink)),
            #[cfg(feature = "video")]
            camera: self.camera.take().map(|camera| Arc::new(crate::video::CameraFeed::new(camera))),
            ice_servers: self.ice_servers.clone(),
            gathering_timeout: self.gathering_timeout,
        })
    }
}

/// The audio devices, open while the satellite runs, and the camera.
#[derive(Clone)]
pub(crate) struct Audio {
    pub(crate) source: Arc<Mutex<Box<dyn AudioSource>>>,
    pub(crate) sink: Arc<Mutex<Box<dyn AudioSink>>>,
    #[cfg(feature = "video")]
    pub(crate) camera: Option<Arc<crate::video::CameraFeed>>,
    pub(crate) ice_servers: Vec<String>,
    pub(crate) gathering_timeout: Duration,
}

impl Audio {
    /// The device code, said through the sink with the bundled clips.
    pub(crate) fn spoken_code(&self, language: Language) -> Option<Arc<SpokenCode>> {
        Some(Arc::new(SpokenCode::new(self.sink.clone(), language)))
    }

    /// The voice of `link` with the assistant, sending the `microphone` or silence; a refused token
    /// is told on `refused`.
    pub(crate) fn hear(&self, link: &MediaLink, microphone: bool, refused: mpsc::UnboundedSender<()>) -> Option<Hear> {
        let call = Call {
            http: link.http.clone(),
            url: format!("{}/satellites/{}/audio", link.core, link.device_id),
            token: link.token.clone(),
            device_id: link.device_id.clone(),
            ice_servers: self.ice_servers.clone(),
            gathering_timeout: self.gathering_timeout,
            backoff: link.backoff.clone(),
            body: link.body.clone(),
            audio: self.clone(),
            microphone,
        };
        Some(Arc::new(move || {
            let (call, refused) = (call.clone(), refused.clone());
            Box::pin(async move {
                if let Err(Error::TokenRefused) = call.run().await {
                    let _ = refused.send(());
                }
            })
        }))
    }

    /// What the sessions of `link` film and call with: the camera when it `films`, the microphone
    /// and the speaker when it `calls`.
    pub(crate) fn media(&self, link: &MediaLink, films: bool, calls: bool) -> Means {
        crate::session::means(self, link, films, calls)
    }

    /// Release the devices.
    pub(crate) async fn close(&self) {
        self.source.lock().await.close().await;
        self.sink.lock().await.close().await;
        #[cfg(feature = "video")]
        if let Some(camera) = &self.camera {
            camera.shut().await;
        }
    }
}

/// The audio of one link: offered, and offered again when it fails, until the link ends (the task is
/// aborted then) or the core refuses the token.
#[derive(Clone)]
pub(crate) struct Call {
    pub(crate) http: Client,
    /// `…/satellites/{device_id}/audio`.
    pub(crate) url: String,
    pub(crate) token: String,
    pub(crate) device_id: String,
    pub(crate) ice_servers: Vec<String>,
    pub(crate) gathering_timeout: Duration,
    pub(crate) backoff: Backoff,
    pub(crate) body: Arc<dyn Body>,
    pub(crate) audio: Audio,
    /// The microphone is sent (declared and allowed); silence otherwise.
    pub(crate) microphone: bool,
}

impl Call {
    /// Talk until the token is refused ([`Error::TokenRefused`]); any other failure is offered again.
    pub(crate) async fn run(mut self) -> Result<()> {
        loop {
            let connected = match self.once().await {
                Ok(connected) => connected,
                Err(Error::TokenRefused) => return Err(Error::TokenRefused),
                Err(error) => {
                    warn!(%error, "audio failed");
                    false
                }
            };
            let delay = self.backoff.next(connected);
            debug!(?delay, "offering audio again");
            tokio::time::sleep(delay).await;
        }
    }

    /// One peer connection, until it ends; whether it connected.
    async fn once(&self) -> Result<bool> {
        let (events, mut signals) = Events::new();
        let peer = self.peer(events).await?;
        let mut tasks = JoinSet::new();
        let outcome = self.converse(&peer, &mut signals, &mut tasks).await;
        tasks.shutdown().await;
        if let Err(error) = peer.close().await {
            debug!(%error, "closing the peer connection");
        }
        outcome
    }

    async fn peer(&self, events: Arc<Events>) -> Result<Arc<dyn PeerConnection>> {
        let mut media = MediaEngine::default();
        media.register_default_codecs()?;
        peer(media, &self.ice_servers, events).await
    }

    async fn converse(
        &self,
        peer: &Arc<dyn PeerConnection>,
        signals: &mut Signals,
        tasks: &mut JoinSet<()>,
    ) -> Result<bool> {
        let track = local_track(&self.device_id, "microphone", RtpCodecKind::Audio, opus_codec())?;
        let sender = peer.add_track(track.clone() as Arc<dyn TrackLocal>).await?;

        let local = gathered(peer, signals, self.gathering_timeout).await?;
        trace!(sdp = %local, "offer");
        let answer = self.offer(&local).await?;
        trace!(sdp = %answer.sdp, "answer");
        peer.set_remote_description(RTCSessionDescription::answer(answer.sdp)?).await?;

        let mut state = signals.state.clone();
        let source = self.audio.source.clone();
        let microphone = self.microphone;
        tasks.spawn(async move {
            // Media flows once DTLS is up: earlier packets could not be encrypted.
            if state.wait_for(|s| *s == RTCPeerConnectionState::Connected).await.is_err() {
                return;
            }
            if microphone {
                log_end("microphone", media::send(source, track, sender)).await;
            } else {
                log_end("silence", send_silence(track, sender)).await;
            }
        });

        let mut connected = false;
        loop {
            tokio::select! {
                Some(track) = signals.tracks.recv() => {
                    tasks.spawn(log_end("speaker", media::play(track, self.audio.sink.clone(), self.body.clone())));
                }
                changed = signals.state.changed() => {
                    let state = *signals.state.borrow();
                    if state == RTCPeerConnectionState::Connected && !connected {
                        connected = true;
                        info!("audio connected");
                    }
                    if changed.is_err() || matches!(state, RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) {
                        info!(%state, "the audio ended");
                        return Ok(connected);
                    }
                }
            }
        }
    }

    async fn offer(&self, sdp: &str) -> Result<Answer> {
        let response = self
            .http
            .post(&self.url)
            .bearer_auth(&self.token)
            .json(&json!({"sdp": sdp, "type": "offer"}))
            .send()
            .await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            return Err(Error::TokenRefused);
        }
        Ok(success(response).await?.json().await?)
    }
}

#[derive(Deserialize)]
pub(crate) struct Answer {
    pub(crate) sdp: String,
}

/// A peer connection with `media`'s codecs, its ICE candidates gathered on this machine's interfaces
/// (and through `ice_servers`): it only connects out, it listens for nobody (M4).
pub(crate) async fn peer(
    mut media: MediaEngine,
    ice_servers: &[String],
    events: Arc<Events>,
) -> Result<Arc<dyn PeerConnection>> {
    let registry = register_default_interceptors(Registry::new(), &mut media)?;
    let ice_servers = if ice_servers.is_empty() {
        Vec::new()
    } else {
        vec![RTCIceServer { urls: ice_servers.to_vec(), ..Default::default() }]
    };
    let config = RTCConfigurationBuilder::new().with_ice_servers(ice_servers).build();
    let peer = PeerConnectionBuilder::new()
        .with_configuration(config)
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .with_handler(events)
        .with_udp_addrs(vec!["0.0.0.0:0".to_owned()])
        .build()
        .await?;
    Ok(Arc::new(peer))
}

/// An offer for `peer`, once its ICE gathering is complete (no trickle): its SDP.
pub(crate) async fn gathered(peer: &Arc<dyn PeerConnection>, signals: &Signals, timeout: Duration) -> Result<String> {
    let offer = peer.create_offer(None).await?;
    peer.set_local_description(offer).await?;
    if tokio::time::timeout(timeout, signals.gathered.notified()).await.is_err() {
        warn!("ICE gathering still running: offering the candidates found so far");
    }
    Ok(peer.local_description().await.ok_or(Error::Link("no local description"))?.sdp)
}

/// A track the device sends, of `kind` in `codec`.
pub(crate) fn local_track(
    device_id: &str,
    name: &str,
    kind: RtpCodecKind,
    codec: RTCRtpCodec,
) -> Result<Arc<TrackLocalStaticSample>> {
    let encoding = RTCRtpEncodingParameters {
        rtp_coding_parameters: RTCRtpCodingParameters { ssrc: Some(rand_ssrc()), ..Default::default() },
        codec,
        ..Default::default()
    };
    let track =
        MediaStreamTrack::new(format!("nestor-{device_id}"), name.to_owned(), name.to_owned(), kind, vec![encoding]);
    Ok(Arc::new(TrackLocalStaticSample::new(Instant::now(), track)?))
}

pub(crate) struct Signals {
    pub(crate) gathered: Arc<Notify>,
    pub(crate) state: watch::Receiver<RTCPeerConnectionState>,
    pub(crate) tracks: mpsc::UnboundedReceiver<Arc<dyn TrackRemote>>,
}

/// The peer connection's callbacks, turned into channels the connection loop selects on.
pub(crate) struct Events {
    gathered: Arc<Notify>,
    state: watch::Sender<RTCPeerConnectionState>,
    tracks: mpsc::UnboundedSender<Arc<dyn TrackRemote>>,
}

impl Events {
    pub(crate) fn new() -> (Arc<Self>, Signals) {
        let gathered = Arc::new(Notify::new());
        let (state, state_rx) = watch::channel(RTCPeerConnectionState::New);
        let (tracks, tracks_rx) = mpsc::unbounded_channel();
        let events = Arc::new(Self { gathered: gathered.clone(), state, tracks });
        let signals = Signals { gathered, state: state_rx, tracks: tracks_rx };
        (events, signals)
    }
}

#[async_trait]
impl PeerConnectionEventHandler for Events {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        debug!(%state, "peer connection state");
        let _ = self.state.send(state);
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let _ = self.tracks.send(track);
    }
}

/// A device without a microphone still sends a silent stream.
async fn send_silence(track: Arc<TrackLocalStaticSample>, sender: Arc<dyn RtpSender>) -> Result<()> {
    let source: Box<dyn AudioSource> = Box::new(SilenceSource::new());
    media::send(Arc::new(Mutex::new(source)), track, sender).await
}

pub(crate) async fn log_end(what: &'static str, work: impl Future<Output = Result<()>>) {
    if let Err(error) = work.await {
        warn!(%error, "{what} stopped");
    }
}

pub(crate) fn opus_codec() -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: rtc::peer_connection::configuration::media_engine::MIME_TYPE_OPUS.to_owned(),
        clock_rate: 48_000,
        channels: 2,
        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
        rtcp_feedback: vec![],
    }
}

fn rand_ssrc() -> u32 {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

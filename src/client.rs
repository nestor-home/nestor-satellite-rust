//! A satellite's life: pair once, then stay connected to the core, reconnecting when the link drops.
//!
//! Each connection is a WebRTC peer connection: the microphone and the core's voice as Opus audio
//! tracks, the `nestor` data channel for `hello`/`welcome`, intents (acknowledged) and
//! observations. Offers are sent once ICE gathering is complete (no trickle ICE).

use std::collections::{BTreeSet, HashSet};
use std::future::{Future, pending};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, mpsc, watch};
use tokio::task::JoinSet;
use tracing::{debug, info, trace, warn};
use webrtc::data_channel::{DataChannel, DataChannelEvent};
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
use crate::body::{Body, NoBody};
use crate::codes::{Enrollment, EnrollmentCode, base_url};
use crate::dispatch::{Dispatcher, Outcome};
use crate::error::{Error, Result};
use crate::i18n::Language;
use crate::media;
use crate::pairing::{Display, Enroller, Handed, Pairing, Presenter, TokenStore, Voice, code_for, success};
use crate::protocol::{self, EnrollmentMethod, Manifest, PairingMethod};
use crate::wire::{self, Welcome};

/// Where an enrollment code handed in is waiting for the pairing in progress.
type Slot = Arc<StdMutex<Option<mpsc::UnboundedSender<Handed>>>>;

/// The environment variable holding a pairing URI (`nestor+http://host:port/?code=K7QF2M9X`).
pub const PAIRING_URI_ENV: &str = "NESTOR_PAIRING_URI";
/// The environment variable holding an enrollment code alone (`K7QF-2M9X`), the core's address
/// being given separately.
pub const PAIRING_CODE_ENV: &str = "NESTOR_PAIRING_CODE";

/// A satellite of a Nestor core. Build it with [`Satellite::builder`], then [`run`](Self::run) it.
pub struct Satellite {
    core: String,
    manifest: Manifest,
    tokens: TokenStore,
    body: Arc<dyn Body>,
    source: Box<dyn AudioSource>,
    sink: Box<dyn AudioSink>,
    presenter: Presenter,
    configured: StdMutex<Option<Handed>>,
    refused: StdMutex<HashSet<EnrollmentCode>>,
    slot: Slot,
    ice_servers: Vec<String>,
    poll: Duration,
    backoff: (Duration, Duration),
    gathering_timeout: Duration,
    welcome_timeout: Duration,
    shared: Arc<Shared>,
}

/// Configures a [`Satellite`].
pub struct SatelliteBuilder {
    satellite: Satellite,
    source: Option<Box<dyn AudioSource>>,
    enrollment: Option<(Enrollment, EnrollmentMethod)>,
    accepted: BTreeSet<EnrollmentMethod>,
}

impl Satellite {
    /// A satellite of the core at `core` (`http://nestor.local:7860`), keeping its token in `tokens`.
    pub fn builder(core: impl Into<String>, manifest: Manifest, tokens: TokenStore) -> SatelliteBuilder {
        let core = core.into();
        let declared = manifest.capabilities().iter().map(|c| c.name().to_owned()).collect();
        SatelliteBuilder {
            satellite: Satellite {
                core,
                manifest,
                tokens,
                body: Arc::new(NoBody),
                source: Box::new(SilenceSource::new()),
                sink: Box::new(Drain),
                presenter: Presenter { display: None, voice: None, language: Language::from_env() },
                configured: StdMutex::new(None),
                refused: StdMutex::new(HashSet::new()),
                slot: Arc::new(StdMutex::new(None)),
                ice_servers: Vec::new(),
                poll: Duration::from_secs(1),
                backoff: (Duration::from_millis(500), Duration::from_secs(30)),
                gathering_timeout: Duration::from_secs(5),
                welcome_timeout: Duration::from_secs(10),
                shared: Arc::new(Shared {
                    declared,
                    welcome: StdMutex::new(None),
                    channel: StdMutex::new(None),
                    connections: AtomicUsize::new(0),
                }),
            },
            source: None,
            enrollment: None,
            accepted: BTreeSet::new(),
        }
    }

    /// A handle to hand in enrollment codes while the satellite waits to be paired.
    pub fn enroller(&self) -> Enroller {
        Enroller {
            core: self.core.clone(),
            methods: self.manifest.pairing_methods().collect(),
            slot: self.slot.clone(),
        }
    }

    /// A handle to report observations and see the link, usable while the satellite runs.
    pub fn observer(&self) -> Observer {
        Observer { shared: self.shared.clone() }
    }

    /// The manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Run until a fatal error: pair when needed, connect, and reconnect with a growing delay.
    ///
    /// Only a core speaking another protocol, a pairing request it refuses as malformed or an audio
    /// device that cannot open stop it; a core that is down, a link that fails, a refused token
    /// (forgotten, then paired again) do not.
    pub async fn run(self) -> Result<()> {
        self.run_until(pending()).await
    }

    /// [`run`](Self::run) until `shutdown` completes, then release the audio devices.
    pub async fn run_until(mut self, shutdown: impl Future<Output = ()>) -> Result<()> {
        self.source.start().await?;
        if let Err(error) = self.sink.start().await {
            self.source.close().await;
            return Err(error.into());
        }
        let audio = Audio {
            source: Arc::new(Mutex::new(std::mem::replace(&mut self.source, Box::new(SilenceSource::new())))),
            sink: Arc::new(Mutex::new(std::mem::replace(&mut self.sink, Box::new(Drain)))),
        };
        let outcome = tokio::select! {
            outcome = self.serve(&audio) => outcome,
            () = shutdown => Ok(()),
        };
        if self.shared.channel().is_some() {
            self.shared.set_channel(None);
            self.body.connected(false);
        }
        audio.source.lock().await.close().await;
        audio.sink.lock().await.close().await;
        outcome
    }

    async fn serve(&self, audio: &Audio) -> Result<()> {
        let http = Client::builder().timeout(Duration::from_secs(10)).build()?;
        let mut backoff = Backoff::new(self.backoff.0, self.backoff.1);
        loop {
            let welcomed = match self.attempt(&http, audio).await {
                Ok(welcomed) => welcomed,
                Err(Error::TokenRefused) => {
                    warn!("the core refused this satellite's token: pairing again");
                    self.tokens.forget()?;
                    continue;
                }
                Err(error @ (Error::ProtocolUnsupported(_) | Error::PairingRefused(_))) => return Err(error),
                Err(error) => {
                    warn!(%error, "satellite link failed");
                    false
                }
            };
            let delay = backoff.next(welcomed);
            debug!(?delay, "reconnecting");
            tokio::time::sleep(delay).await;
        }
    }

    async fn attempt(&self, http: &Client, audio: &Audio) -> Result<bool> {
        let token = self.token(http).await?;
        self.connect_once(http, &token, audio).await
    }

    async fn token(&self, http: &Client) -> Result<String> {
        let device = self.manifest.device().id();
        if let Some(token) = self.tokens.load(&self.core, device) {
            return Ok(token);
        }
        let token = Pairing {
            http,
            core: &self.core,
            manifest: &self.manifest,
            body: &*self.body,
            presenter: &self.presenter,
            poll: self.poll,
            configured: &self.configured,
            refused: &self.refused,
            slot: &self.slot,
        }
        .run()
        .await?;
        self.tokens.save(&self.core, device, &token)?;
        info!(path = %self.tokens.path().display(), "paired: token saved");
        Ok(token)
    }

    /// One connection, until the core hangs up or the link fails; whether it was welcomed.
    async fn connect_once(&self, http: &Client, token: &str, audio: &Audio) -> Result<bool> {
        let (events, signals) = Events::new();
        let mut session = Session { peer: self.peer(events).await?, signals, tasks: JoinSet::new(), welcomed: false };
        let outcome = self.converse(http, token, audio, &mut session).await;
        self.shared.set_channel(None);
        session.tasks.abort_all();
        while session.tasks.join_next().await.is_some() {}
        if let Err(error) = session.peer.close().await {
            debug!(%error, "closing the peer connection");
        }
        if session.welcomed {
            self.body.connected(false);
        }
        outcome.map(|()| session.welcomed)
    }

    async fn peer(&self, events: Arc<Events>) -> Result<Arc<dyn PeerConnection>> {
        let mut media = MediaEngine::default();
        media.register_default_codecs()?;
        let registry = register_default_interceptors(Registry::new(), &mut media)?;
        let ice_servers = if self.ice_servers.is_empty() {
            Vec::new()
        } else {
            vec![RTCIceServer { urls: self.ice_servers.clone(), ..Default::default() }]
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

    async fn converse(&self, http: &Client, token: &str, audio: &Audio, session: &mut Session) -> Result<()> {
        let Session { peer, signals, tasks, welcomed } = session;
        let track = Arc::new(TrackLocalStaticSample::new(
            Instant::now(),
            MediaStreamTrack::new(
                format!("nestor-{}", self.manifest.device().id()),
                "microphone".to_owned(),
                "microphone".to_owned(),
                rtc::rtp_transceiver::rtp_sender::RtpCodecKind::Audio,
                vec![rtc::rtp_transceiver::rtp_sender::RTCRtpEncodingParameters {
                    rtp_coding_parameters: rtc::rtp_transceiver::rtp_sender::RTCRtpCodingParameters {
                        ssrc: Some(rand_ssrc()),
                        ..Default::default()
                    },
                    codec: opus_codec(),
                    ..Default::default()
                }],
            ),
        )?);
        let sender = peer.add_track(track.clone() as Arc<dyn TrackLocal>).await?;
        let channel = peer.create_data_channel("nestor", None).await?;
        let (inbox, mut messages) = mpsc::unbounded_channel();
        tasks.spawn(read_channel(channel.clone(), inbox));

        let offer = peer.create_offer(None).await?;
        peer.set_local_description(offer).await?;
        if tokio::time::timeout(self.gathering_timeout, signals.gathered.notified()).await.is_err() {
            warn!("ICE gathering still running: offering the candidates found so far");
        }
        let local = peer.local_description().await.ok_or(Error::Link("no local description"))?;
        let sdp = dtls_client(&local.sdp);
        trace!(%sdp, "offer");
        let answer = self.offer(http, token, &sdp).await?;
        trace!(sdp = %answer.sdp, "answer");
        peer.set_remote_description(RTCSessionDescription::answer(answer.sdp)?).await?;
        self.shared.connections.fetch_add(1, Ordering::SeqCst);

        let mut state = signals.state.clone();
        let source = audio.source.clone();
        let microphone = self.manifest.declares(protocol::AUDIO_CAPTURE);
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

        let dispatcher =
            Dispatcher::new(Arc::new(Announced { body: self.body.clone(), language: self.presenter.language }));
        let welcome_deadline = tokio::time::sleep(self.welcome_timeout);
        tokio::pin!(welcome_deadline);
        loop {
            tokio::select! {
                message = messages.recv() => match message {
                    Some(ChannelMessage::Open) => {
                        send_json(&*channel, &wire::hello(&self.manifest)).await?;
                    }
                    Some(ChannelMessage::Text(text)) => match dispatcher.handle(&text) {
                        Outcome::Nothing => {}
                        Outcome::Reply(reply) => channel.send_text(&reply).await?,
                        Outcome::Welcomed(welcome) => {
                            info!(room = %welcome.room, capabilities = ?welcome.capabilities, "welcomed by the core");
                            self.shared.set_welcome(welcome);
                            self.shared.set_channel(Some(channel.clone()));
                            if !*welcomed {
                                *welcomed = true;
                                self.body.connected(true);
                            }
                        }
                        Outcome::HungUp => {
                            info!("the core hung up");
                            break;
                        }
                    },
                    None => {
                        debug!("the data channel closed");
                        break;
                    }
                },
                Some(track) = signals.tracks.recv() => {
                    tasks.spawn(log_end("speaker", media::play(track, audio.sink.clone(), self.body.clone())));
                }
                changed = signals.state.changed() => {
                    let state = *signals.state.borrow();
                    if changed.is_err() || matches!(state, RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) {
                        info!(%state, "the peer connection ended");
                        break;
                    }
                }
                () = &mut welcome_deadline, if !*welcomed => {
                    return Err(Error::Timeout("no welcome from the core"));
                }
            }
        }
        Ok(())
    }

    async fn offer(&self, http: &Client, token: &str, sdp: &str) -> Result<Answer> {
        let response = http
            .post(format!("{}/satellites/{}/offer", self.core, self.manifest.device().id()))
            .bearer_auth(token)
            .json(&json!({"sdp": sdp, "type": "offer"}))
            .send()
            .await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            return Err(Error::TokenRefused);
        }
        Ok(success(response).await?.json().await?)
    }
}

impl SatelliteBuilder {
    /// The body carrying out the core's intents (default: [`NoBody`]).
    pub fn body(mut self, body: impl Body) -> Self {
        self.satellite.body = Arc::new(body);
        self
    }

    /// A shared body, when the driver keeps a handle on it too.
    pub fn shared_body(mut self, body: Arc<dyn Body>) -> Self {
        self.satellite.body = body;
        self
    }

    /// The microphone; used only when the manifest declares `audio.capture` (silence otherwise).
    pub fn source(mut self, source: impl AudioSource) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// The speaker (default: the core's audio is decoded and dropped).
    pub fn sink(mut self, sink: impl AudioSink) -> Self {
        self.satellite.sink = Box::new(sink);
        self
    }

    /// A screen to show the device code on: the satellite declares `device_code.display`.
    pub fn display(mut self, display: impl Display) -> Self {
        self.satellite.presenter.display = Some(Arc::new(display));
        self
    }

    /// A voice to say the device code with: the satellite declares `device_code.speech`.
    pub fn voice(mut self, voice: impl Voice) -> Self {
        self.satellite.presenter.voice = Some(Arc::new(voice));
        self
    }

    /// The language the satellite speaks to people in (default: from `LANG`, else English).
    pub fn language(mut self, language: Language) -> Self {
        self.satellite.presenter.language = language;
        self
    }

    /// The enrollment code or pairing URI set in the device's configuration
    /// (`enrollment.provisioning`): sent with the first pairing request, no device code presented
    /// unless it is refused. Without one, [`build`](Self::build) reads `NESTOR_PAIRING_URI`, then
    /// `NESTOR_PAIRING_CODE`.
    pub fn enrollment(self, enrollment: Enrollment) -> Self {
        self.enrollment_by(enrollment, EnrollmentMethod::Provisioning)
    }

    /// [`enrollment`](Self::enrollment), received another way (typed, scanned) before the
    /// satellite started; that method is declared.
    pub fn enrollment_by(mut self, enrollment: Enrollment, method: EnrollmentMethod) -> Self {
        self.enrollment = Some((enrollment, method));
        self
    }

    /// The driver hands in enrollment codes this way through an [`Enroller`]: typed on the device
    /// (`enrollment.input`) or scanned (`enrollment.qr`). The method is declared.
    pub fn accepts_enrollment(mut self, method: EnrollmentMethod) -> Self {
        self.accepted.insert(method);
        self
    }

    /// STUN/TURN servers (`stun:stun.l.google.com:19302`), only needed beyond the local network.
    pub fn ice_servers(mut self, urls: Vec<String>) -> Self {
        self.satellite.ice_servers = urls;
        self
    }

    /// How often the pairing is polled while waiting for approval (default 1 s).
    pub fn poll_interval(mut self, poll: Duration) -> Self {
        self.satellite.poll = poll;
        self
    }

    /// The reconnection delay: from `min`, doubled after each failure, up to `max` (default 0.5 s to 30 s).
    pub fn backoff(mut self, min: Duration, max: Duration) -> Self {
        self.satellite.backoff = (min, max.max(min));
        self
    }

    /// How long a connection may wait for the core's `welcome` (default 10 s).
    pub fn welcome_timeout(mut self, timeout: Duration) -> Self {
        self.satellite.welcome_timeout = timeout;
        self
    }

    /// The satellite, with the pairing methods its outputs and inputs carry out.
    ///
    /// Fails when the core's address is not an http(s) URL, when the enrollment code (given here or
    /// in `NESTOR_PAIRING_URI` / `NESTOR_PAIRING_CODE`) is not one, or when its pairing URI is for
    /// another core.
    pub fn build(mut self) -> Result<Satellite> {
        let satellite = &mut self.satellite;
        satellite.core = base_url(&satellite.core)?;
        if let Some(source) = self.source {
            if satellite.manifest.declares(protocol::AUDIO_CAPTURE) {
                satellite.source = source;
            } else {
                warn!("a microphone was given but audio.capture is not declared: silence is sent");
            }
        }
        let enrollment = match self.enrollment {
            Some(given) => Some(given),
            None => from_env()?.map(|enrollment| (enrollment, EnrollmentMethod::Provisioning)),
        };
        let mut methods = BTreeSet::from([PairingMethod::EnrollmentProvisioning]);
        methods.extend(self.accepted.iter().map(|&m| PairingMethod::from(m)));
        if let Some((enrollment, method)) = &enrollment {
            let code = code_for(enrollment, &satellite.core)?.clone();
            methods.insert((*method).into());
            satellite.configured = StdMutex::new(Some(Handed { code, method: *method }));
        }
        if satellite.presenter.display.is_some() {
            methods.insert(PairingMethod::DeviceCodeDisplay);
        }
        if satellite.presenter.voice.is_some() {
            methods.insert(PairingMethod::DeviceCodeSpeech);
        }
        satellite.manifest = satellite.manifest.clone().with_pairing_methods(methods)?;
        Ok(self.satellite)
    }
}

/// The enrollment configured in the environment: `NESTOR_PAIRING_URI`, else `NESTOR_PAIRING_CODE`.
fn from_env() -> Result<Option<Enrollment>> {
    for name in [PAIRING_URI_ENV, PAIRING_CODE_ENV] {
        if let Some(value) = std::env::var(name).ok().filter(|v| !v.trim().is_empty()) {
            return Enrollment::parse(&value).map(Some).map_err(Error::InvalidEnrollment);
        }
    }
    Ok(None)
}

/// The driver's body, with identify requests said in the log too (a person looks for the device).
struct Announced {
    body: Arc<dyn Body>,
    language: Language,
}

impl Body for Announced {
    fn behavior(&self, behavior: protocol::Behavior) {
        self.body.behavior(behavior);
    }

    fn expression(&self, expression: protocol::Expression, intensity: f64) {
        self.body.expression(expression, intensity);
    }

    fn beat(&self) {
        self.body.beat();
    }

    fn heartbeat(&self) {
        self.body.heartbeat();
    }

    fn identify(&self) {
        info!("{}", self.language.say("identify-requested", &[]));
        self.body.identify();
    }

    fn connected(&self, up: bool) {
        self.body.connected(up);
    }
}

/// Reports observations and tells whether the link is up, while the satellite runs.
#[derive(Clone)]
pub struct Observer {
    shared: Arc<Shared>,
}

impl Observer {
    /// Report people seen now; `false` when not connected (observations are not queued).
    pub async fn person_detected(&self, count: u32, confidence: f64) -> Result<bool> {
        self.observe(protocol::PERSON_DETECTION, wire::PERSON_DETECTED, wire::person_detected(count, confidence)?).await
    }

    /// Report that voice activity started (`true`) or stopped; `false` when not connected.
    pub async fn speech_activity(&self, active: bool) -> Result<bool> {
        self.observe(protocol::SPEECH_ACTIVITY, wire::SPEECH_ACTIVITY, wire::speech_activity(active)).await
    }

    /// The core's last welcome: the paired room and what the household allowed.
    pub fn welcome(&self) -> Option<Welcome> {
        self.shared.welcome.lock().ok().and_then(|w| w.clone())
    }

    /// Whether the satellite is connected and welcomed now.
    pub fn is_connected(&self) -> bool {
        self.shared.channel().is_some()
    }

    /// How many connections the core answered so far.
    pub fn connections(&self) -> usize {
        self.shared.connections.load(Ordering::SeqCst)
    }

    async fn observe(&self, capability: &'static str, message_type: &'static str, message: Value) -> Result<bool> {
        let allowed = self.welcome().is_none_or(|welcome| welcome.capabilities.contains(capability));
        if !self.shared.declared.contains(capability) || !allowed {
            return Err(Error::CapabilityNotAllowed { message_type, capability });
        }
        let Some(channel) = self.shared.channel() else {
            return Ok(false);
        };
        send_json(&*channel, &message).await?;
        Ok(true)
    }
}

/// The reconnection delay: back to `min` after a connection that was welcomed, doubled (up to
/// `max`) after one that was not.
#[derive(Debug, Clone)]
pub struct Backoff {
    min: Duration,
    max: Duration,
    current: Duration,
}

impl Backoff {
    /// From `min` to `max`.
    pub fn new(min: Duration, max: Duration) -> Self {
        Self { min, max, current: min }
    }

    /// The delay before the next attempt, after one that was `welcomed` or not.
    pub fn next(&mut self, welcomed: bool) -> Duration {
        self.current = if welcomed { self.min } else { (self.current * 2).min(self.max) };
        self.current
    }
}

struct Shared {
    declared: BTreeSet<String>,
    welcome: StdMutex<Option<Welcome>>,
    channel: StdMutex<Option<Arc<dyn DataChannel>>>,
    connections: AtomicUsize,
}

impl Shared {
    fn channel(&self) -> Option<Arc<dyn DataChannel>> {
        self.channel.lock().ok().and_then(|c| c.clone())
    }

    fn set_channel(&self, channel: Option<Arc<dyn DataChannel>>) {
        if let Ok(mut current) = self.channel.lock() {
            *current = channel;
        }
    }

    fn set_welcome(&self, welcome: Welcome) {
        if let Ok(mut current) = self.welcome.lock() {
            *current = Some(welcome);
        }
    }
}

/// One peer connection and what runs for it.
struct Session {
    peer: Arc<dyn PeerConnection>,
    signals: Signals,
    tasks: JoinSet<()>,
    welcomed: bool,
}

struct Audio {
    source: Arc<Mutex<Box<dyn AudioSource>>>,
    sink: Arc<Mutex<Box<dyn AudioSink>>>,
}

#[derive(Deserialize)]
struct Answer {
    sdp: String,
}

enum ChannelMessage {
    Open,
    Text(String),
}

struct Signals {
    gathered: Arc<Notify>,
    state: watch::Receiver<RTCPeerConnectionState>,
    tracks: mpsc::UnboundedReceiver<Arc<dyn TrackRemote>>,
}

/// The peer connection's callbacks, turned into channels the connection loop selects on.
struct Events {
    gathered: Arc<Notify>,
    state: watch::Sender<RTCPeerConnectionState>,
    tracks: mpsc::UnboundedSender<Arc<dyn TrackRemote>>,
}

impl Events {
    fn new() -> (Arc<Self>, Signals) {
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

async fn read_channel(channel: Arc<dyn DataChannel>, inbox: mpsc::UnboundedSender<ChannelMessage>) {
    while let Some(event) = channel.poll().await {
        let message = match event {
            DataChannelEvent::OnOpen => ChannelMessage::Open,
            DataChannelEvent::OnMessage(message) => match String::from_utf8(message.data.to_vec()) {
                Ok(text) => ChannelMessage::Text(text),
                Err(_) => {
                    warn!("ignoring a binary message from the core");
                    continue;
                }
            },
            DataChannelEvent::OnClose => break,
            _ => continue,
        };
        if inbox.send(message).is_err() {
            break;
        }
    }
}

async fn send_json(channel: &dyn DataChannel, message: &Value) -> Result<()> {
    channel.send_text(&message.to_string()).await?;
    Ok(())
}

/// A device without a microphone still sends a silent stream.
async fn send_silence(track: Arc<TrackLocalStaticSample>, sender: Arc<dyn RtpSender>) -> Result<()> {
    let source: Box<dyn AudioSource> = Box::new(SilenceSource::new());
    media::send(Arc::new(Mutex::new(source)), track, sender).await
}

async fn log_end(what: &'static str, work: impl Future<Output = Result<()>>) {
    if let Err(error) = work.await {
        warn!(%error, "{what} stopped");
    }
}

/// The offer as sent: this side takes the DTLS client role (`setup:active`).
///
/// webrtc-rs opens the SCTP association from the DTLS client, aiortc (the core) from the ICE
/// controlling side — this offerer. Left to choose (`actpass`), aiortc answers `active` and both
/// wait for the other's SCTP INIT: the data channel never opens. Offering `active` makes aiortc the
/// DTLS server, so this side is DTLS client and ICE controlling, and opens the association. The
/// local description keeps `actpass`, which a `passive` answer resolves to the same role.
fn dtls_client(sdp: &str) -> String {
    sdp.replace("a=setup:actpass", "a=setup:active")
}

fn opus_codec() -> rtc::rtp_transceiver::rtp_sender::RTCRtpCodec {
    rtc::rtp_transceiver::rtp_sender::RTCRtpCodec {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Device, PERSON_DETECTION, SPEECH_ACTIVITY};

    fn observer(capabilities: &[&str]) -> (Satellite, Observer) {
        let device = Device::new("capteur-entree", "esp32", "entree").unwrap();
        let manifest = Manifest::new(device, capabilities.iter().copied()).unwrap();
        let satellite =
            Satellite::builder("http://127.0.0.1:9", manifest, TokenStore::new("unused.json")).build().unwrap();
        let observer = satellite.observer();
        (satellite, observer)
    }

    #[tokio::test]
    async fn an_observation_needs_its_capability_declared() {
        let (_satellite, observer) = observer(&[PERSON_DETECTION]);
        assert!(matches!(
            observer.speech_activity(true).await,
            Err(Error::CapabilityNotAllowed { capability: SPEECH_ACTIVITY, .. })
        ));
        assert!(!observer.person_detected(1, 0.9).await.unwrap(), "not connected: nothing is queued");
        assert!(observer.person_detected(1, 1.5).await.is_err(), "a confidence is between 0 and 1");
    }

    #[tokio::test]
    async fn an_observation_the_household_did_not_allow_is_refused() {
        let (satellite, observer) = observer(&[PERSON_DETECTION, SPEECH_ACTIVITY]);
        satellite.shared.set_welcome(Welcome {
            device_id: "capteur-entree".into(),
            room: "entree".into(),
            capabilities: BTreeSet::from([SPEECH_ACTIVITY.to_owned()]),
        });
        assert!(matches!(
            observer.person_detected(1, 0.9).await,
            Err(Error::CapabilityNotAllowed { capability: PERSON_DETECTION, .. })
        ));
        assert!(!observer.speech_activity(false).await.unwrap());
        assert!(!observer.is_connected());
    }

    #[test]
    fn the_offer_makes_this_side_the_dtls_client() {
        let sdp = "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=setup:actpass\r\nm=application 9\r\na=setup:actpass\r\n";
        assert_eq!(dtls_client(sdp).matches("a=setup:active").count(), 2);
        assert!(!dtls_client(sdp).contains("actpass"));
    }

    #[test]
    fn the_delay_doubles_after_failures_and_resets_after_a_welcome() {
        let mut backoff = Backoff::new(Duration::from_millis(500), Duration::from_secs(3));
        let failures: Vec<_> = (0..4).map(|_| backoff.next(false).as_millis()).collect();
        assert_eq!(failures, [1000, 2000, 3000, 3000]);
        assert_eq!(backoff.next(true), Duration::from_millis(500));
        assert_eq!(backoff.next(false), Duration::from_secs(1));
    }
}

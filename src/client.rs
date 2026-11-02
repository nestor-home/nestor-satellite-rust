//! A satellite's life: pair once, then hold its link to the core, reconnecting when it drops.
//!
//! The link is one WebSocket (`/satellites/{device_id}/link`) that carries every message: `hello`
//! (manifest and token) then `welcome`, intents (acknowledged), observations and typed text. With
//! the `voice` feature, a device allowed audio also offers WebRTC once welcomed, for audio only.

use std::collections::{BTreeSet, HashSet};
use std::future::{Future, pending};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::Message as Frame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tracing::{debug, info, warn};

use crate::body::{Body, NoBody};
use crate::codes::{Enrollment, EnrollmentCode, base_url};
use crate::dispatch::{Dispatcher, Outcome};
use crate::error::{Error, Result};
use crate::i18n::Language;
use crate::pairing::{Display, Enroller, Handed, Pairing, Presenter, TokenStore, Voice, code_for};
use crate::protocol::{self, EnrollmentMethod, Manifest, PairingMethod};
use crate::text::TextDisplay;
use crate::wire::{self, Welcome};
#[cfg(feature = "voice")]
use crate::{
    audio::{AudioSink, AudioSource, Drain, SilenceSource},
    speech::SpokenCode,
    voice::{self, Audio, Call, VoiceSetup},
};

/// Where an enrollment code handed in is waiting for the pairing in progress.
type Slot = Arc<StdMutex<Option<mpsc::UnboundedSender<Handed>>>>;

/// The environment variable holding a pairing URI (`nestor+http://host:port/?code=K7QF2M9X`).
pub const PAIRING_URI_ENV: &str = "NESTOR_PAIRING_URI";
/// The environment variable holding an enrollment code alone (`K7QF-2M9X`), the core's address
/// being given separately.
pub const PAIRING_CODE_ENV: &str = "NESTOR_PAIRING_CODE";

/// The core closes a link with these (L2, L4, L5).
const OFF_CONTRACT: u16 = 4400;
const TOKEN_REFUSED: u16 = 4401;
const REPLACED: u16 = 4409;
/// The core pings every 20 s: this long without hearing from it, the link is lost (L4).
const LINK_SILENCE: Duration = Duration::from_secs(60);
/// How long opening the WebSocket may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A satellite of a Nestor core. Build it with [`Satellite::builder`], then [`run`](Self::run) it.
pub struct Satellite {
    core: String,
    manifest: Manifest,
    tokens: TokenStore,
    body: Arc<dyn Body>,
    text: Option<Arc<dyn TextDisplay>>,
    presenter: Presenter,
    #[cfg(feature = "voice")]
    voice: VoiceSetup,
    /// The device code is said through the sink with the bundled clips.
    #[cfg(feature = "voice")]
    speaks: bool,
    configured: StdMutex<Option<Handed>>,
    refused: StdMutex<HashSet<EnrollmentCode>>,
    slot: Slot,
    poll: Duration,
    backoff: (Duration, Duration),
    welcome_timeout: Duration,
    link_silence: Duration,
    shared: Arc<Shared>,
}

/// Configures a [`Satellite`].
pub struct SatelliteBuilder {
    satellite: Satellite,
    enrollment: Option<(Enrollment, EnrollmentMethod)>,
    accepted: BTreeSet<EnrollmentMethod>,
    /// The driver sends typed text through the [`Observer`].
    typed: bool,
}

impl Satellite {
    /// A satellite of the core at `core` (`http://nestor.local:8471`), keeping its token in `tokens`.
    ///
    /// The manifest declares what the device does beyond talking (`device.identify`, expressions,
    /// observations…); the builder declares `text.*` and `audio.*` from what it is given.
    pub fn builder(core: impl Into<String>, manifest: Manifest, tokens: TokenStore) -> SatelliteBuilder {
        SatelliteBuilder {
            satellite: Satellite {
                core: core.into(),
                manifest,
                tokens,
                body: Arc::new(NoBody),
                text: None,
                presenter: Presenter {
                    display: None,
                    voice: None,
                    #[cfg(feature = "voice")]
                    speech: None,
                    language: Language::from_env(),
                },
                #[cfg(feature = "voice")]
                voice: VoiceSetup::default(),
                #[cfg(feature = "voice")]
                speaks: false,
                configured: StdMutex::new(None),
                refused: StdMutex::new(HashSet::new()),
                slot: Arc::new(StdMutex::new(None)),
                poll: Duration::from_secs(1),
                backoff: (Duration::from_millis(500), Duration::from_secs(30)),
                welcome_timeout: Duration::from_secs(10),
                link_silence: LINK_SILENCE,
                shared: Arc::new(Shared::default()),
            },
            enrollment: None,
            accepted: BTreeSet::new(),
            typed: false,
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

    /// A handle to report observations, send typed text and see the link, usable while the
    /// satellite runs.
    pub fn observer(&self) -> Observer {
        Observer { shared: self.shared.clone() }
    }

    /// The manifest, with the capabilities and pairing methods the builder declared.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Run until a fatal error: pair when needed, hold the link, and reconnect with a growing delay.
    ///
    /// Only a core speaking another protocol, a pairing request it refuses as malformed, a newer link
    /// of the same device ([`Error::Replaced`]) or an audio device that cannot open stop it; a core
    /// that is down, a link that drops, a refused token (forgotten, then paired again) do not.
    pub async fn run(self) -> Result<()> {
        self.run_until(pending()).await
    }

    /// [`run`](Self::run) until `shutdown` completes, then release the audio devices.
    #[cfg_attr(not(feature = "voice"), allow(unused_mut))]
    pub async fn run_until(mut self, shutdown: impl Future<Output = ()>) -> Result<()> {
        #[cfg(feature = "voice")]
        let audio = self.voice.open().await?;
        let http = Client::builder().timeout(Duration::from_secs(10)).build()?;
        let session = Session {
            http: &http,
            #[cfg(feature = "voice")]
            audio: &audio,
        };
        let outcome = tokio::select! {
            outcome = self.serve(&session) => outcome,
            () = shutdown => Ok(()),
        };
        if self.shared.set_link(None) {
            self.body.connected(false);
        }
        #[cfg(feature = "voice")]
        audio.close().await;
        outcome
    }

    async fn serve(&self, session: &Session<'_>) -> Result<()> {
        let mut backoff = Backoff::new(self.backoff.0, self.backoff.1);
        loop {
            let welcomed = match self.attempt(session).await {
                Ok(welcomed) => welcomed,
                Err(Error::TokenRefused) => {
                    warn!("the core refused this satellite's token: pairing again");
                    self.tokens.forget()?;
                    continue;
                }
                Err(error @ (Error::ProtocolUnsupported(_) | Error::PairingRefused(_) | Error::Replaced)) => {
                    return Err(error);
                }
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

    async fn attempt(&self, session: &Session<'_>) -> Result<bool> {
        let token = self.token(session).await?;
        self.link(session, &token).await
    }

    async fn token(&self, session: &Session<'_>) -> Result<String> {
        let device = self.manifest.device().id();
        if let Some(token) = self.tokens.load(&self.core, device) {
            return Ok(token);
        }
        #[cfg(feature = "voice")]
        let presenter = Presenter {
            speech: self.speaks.then(|| Arc::new(SpokenCode::new(session.audio.sink.clone(), self.presenter.language))),
            ..self.presenter.clone()
        };
        #[cfg(not(feature = "voice"))]
        let presenter = self.presenter.clone();
        let token = Pairing {
            http: session.http,
            core: &self.core,
            manifest: &self.manifest,
            tokens: &self.tokens,
            body: &*self.body,
            presenter: &presenter,
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

    /// One link, until the core closes it or it is lost; whether it was welcomed.
    #[cfg_attr(not(feature = "voice"), allow(unused_variables))]
    async fn link(&self, session: &Session<'_>, token: &str) -> Result<bool> {
        let device = self.manifest.device().id();
        let url = format!("{}/satellites/{device}/link", ws_url(&self.core));
        debug!(%url, "opening the link");
        let (socket, _) = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url.as_str()))
            .await
            .map_err(|_| Error::Timeout("opening the link"))??;
        let (mut write, mut read) = socket.split();
        write.send(Frame::text(wire::hello(&self.manifest, token).to_string())).await?;

        let mut dispatcher =
            Dispatcher::new(Arc::new(Announced { body: self.body.clone(), language: self.presenter.language }));
        if let Some(text) = &self.text {
            dispatcher = dispatcher.with_text(text.clone());
        }
        let (outbox, mut outgoing) = mpsc::unbounded_channel::<String>();
        let mut tasks: JoinSet<Result<()>> = JoinSet::new();
        let mut welcomed = false;
        let welcome_deadline = tokio::time::sleep(self.welcome_timeout);
        tokio::pin!(welcome_deadline);
        let silence = tokio::time::sleep(self.link_silence);
        tokio::pin!(silence);

        let end = loop {
            tokio::select! {
                frame = read.next() => {
                    silence.as_mut().reset(tokio::time::Instant::now() + self.link_silence);
                    match frame {
                        Some(Ok(Frame::Text(text))) => match dispatcher.handle(&text) {
                            Outcome::Nothing => {}
                            Outcome::Reply(reply) => {
                                if let Err(error) = write.send(Frame::text(reply)).await {
                                    warn!(%error, "the link broke");
                                    break End::Lost;
                                }
                            }
                            Outcome::Welcomed(welcome) => {
                                info!(room = %welcome.room, capabilities = ?welcome.capabilities, "welcomed by the core");
                                if !welcomed {
                                    welcomed = true;
                                    self.shared.connections.fetch_add(1, Ordering::SeqCst);
                                    #[cfg(feature = "voice")]
                                    self.talk(session, token, &welcome, &mut tasks);
                                    self.shared.set_welcome(welcome);
                                    self.shared.set_link(Some(outbox.clone()));
                                    self.body.connected(true);
                                } else {
                                    self.shared.set_welcome(welcome);
                                }
                            }
                        },
                        Some(Ok(Frame::Close(frame))) => break End::Closed(frame.map(|f| u16::from(f.code))),
                        Some(Ok(_)) => {} // pings are answered by the WebSocket itself
                        Some(Err(error)) => {
                            warn!(%error, "the link broke");
                            break End::Lost;
                        }
                        None => break End::Closed(None),
                    }
                }
                Some(message) = outgoing.recv() => {
                    if let Err(error) = write.send(Frame::text(message)).await {
                        warn!(%error, "the link broke");
                        break End::Lost;
                    }
                }
                Some(ended) = tasks.join_next() => {
                    if let Ok(Err(Error::TokenRefused)) = ended {
                        break End::Refused;
                    }
                }
                () = &mut welcome_deadline, if !welcomed => {
                    break End::Late;
                }
                () = &mut silence => {
                    warn!(seconds = self.link_silence.as_secs(), "nothing heard from the core: the link is lost");
                    break End::Lost;
                }
            }
        };

        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        if self.shared.set_link(None) {
            self.body.connected(false);
        }
        if let End::Lost | End::Late | End::Refused = end {
            let _ = tokio::time::timeout(Duration::from_secs(1), write.send(Frame::Close(None))).await;
        }
        match end {
            End::Closed(Some(TOKEN_REFUSED)) | End::Refused => Err(Error::TokenRefused),
            End::Closed(Some(REPLACED)) => {
                warn!("a newer link of this device replaced this one: stopping");
                Err(Error::Replaced)
            }
            End::Closed(Some(OFF_CONTRACT)) => {
                warn!("the core turned the hello away (4400): reconnecting");
                Ok(welcomed)
            }
            End::Closed(code) => {
                info!(code = code.unwrap_or(u16::from(CloseCode::Status)), "the core closed the link");
                Ok(welcomed)
            }
            End::Lost => Ok(welcomed),
            End::Late => Err(Error::Timeout("no welcome from the core")),
        }
    }

    /// Offer audio for this link when the welcome allows some the device declared.
    #[cfg(feature = "voice")]
    fn talk(&self, session: &Session<'_>, token: &str, welcome: &Welcome, tasks: &mut JoinSet<Result<()>>) {
        if !voice::allowed(welcome, |c| self.manifest.declares(c)) {
            debug!("no audio allowed: none offered");
            return;
        }
        let device = self.manifest.device().id();
        let call = Call {
            http: session.http.clone(),
            url: format!("{}/satellites/{device}/audio", self.core),
            token: token.to_owned(),
            device_id: device.to_owned(),
            ice_servers: self.voice.ice_servers.clone(),
            gathering_timeout: self.voice.gathering_timeout,
            backoff: Backoff::new(self.backoff.0, self.backoff.1),
            body: self.body.clone(),
            audio: session.audio.clone(),
            microphone: self.manifest.declares(protocol::AUDIO_CAPTURE)
                && welcome.capabilities.contains(protocol::AUDIO_CAPTURE),
        };
        tasks.spawn(call.run());
    }
}

/// What one run holds: the HTTP client, the open audio devices.
struct Session<'a> {
    http: &'a Client,
    #[cfg(feature = "voice")]
    audio: &'a Audio,
}

/// Why a link ended.
enum End {
    /// The core closed it, with this code.
    Closed(Option<u16>),
    /// Nothing heard for too long, or the connection broke.
    Lost,
    /// No welcome in time.
    Late,
    /// The core refused the token on an audio offer.
    Refused,
}

/// The link's address: the core's base URL with `ws` for `http`, `wss` for `https`.
fn ws_url(core: &str) -> String {
    match core.strip_prefix("http") {
        Some(rest) => format!("ws{rest}"),
        None => core.to_owned(),
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

    /// Where the assistant's words are shown: the satellite declares `text.display`.
    pub fn text_display(mut self, display: impl TextDisplay) -> Self {
        self.satellite.text = Some(Arc::new(display));
        self
    }

    /// A person types on the device and the driver sends it with [`Observer::input_text`]: the
    /// satellite declares `text.input`.
    pub fn text_input(mut self) -> Self {
        self.typed = true;
        self
    }

    /// The microphone: the satellite declares `audio.capture`.
    #[cfg(feature = "voice")]
    pub fn source(mut self, source: impl AudioSource) -> Self {
        self.satellite.voice.source = Some(Box::new(source));
        self
    }

    /// The speaker: the satellite declares `audio.playback`. When the device has neither
    /// [`display`](Self::display) nor [`voice`](Self::voice), it says its device code through it
    /// with the bundled recorded clips (English or French): it declares `device_code.speech`.
    #[cfg(feature = "voice")]
    pub fn sink(mut self, sink: impl AudioSink) -> Self {
        self.satellite.voice.sink = Some(Box::new(sink));
        self.satellite.voice.audible = true;
        self
    }

    /// Audio both ways without a sound card (tests, conformance runs): silence is sent, the core's
    /// voice is decoded and dropped, and no device code is said.
    #[cfg(feature = "voice")]
    pub fn null_audio(mut self) -> Self {
        self.satellite.voice.source = Some(Box::new(SilenceSource::new()));
        self.satellite.voice.sink = Some(Box::new(Drain));
        self.satellite.voice.audible = false;
        self
    }

    /// STUN/TURN servers (`stun:stun.l.google.com:19302`), only needed beyond the local network.
    #[cfg(feature = "voice")]
    pub fn ice_servers(mut self, urls: Vec<String>) -> Self {
        self.satellite.voice.ice_servers = urls;
        self
    }

    /// A screen to show the device code on: the satellite declares `device_code.display`.
    pub fn display(mut self, display: impl Display) -> Self {
        self.satellite.presenter.display = Some(Arc::new(display));
        self
    }

    /// A voice to say the device code with, instead of the bundled clips: the satellite declares
    /// `device_code.speech`.
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

    /// [`enrollment`](Self::enrollment), received another way (typed) before the satellite
    /// started; that method is declared.
    pub fn enrollment_by(mut self, enrollment: Enrollment, method: EnrollmentMethod) -> Self {
        self.enrollment = Some((enrollment, method));
        self
    }

    /// The driver hands in enrollment codes this way through an [`Enroller`], typed on the device
    /// (`enrollment.input`): the method is declared.
    pub fn accepts_enrollment(mut self, method: EnrollmentMethod) -> Self {
        self.accepted.insert(method);
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

    /// How long a link may wait for the core's `welcome` (default 10 s).
    pub fn welcome_timeout(mut self, timeout: Duration) -> Self {
        self.satellite.welcome_timeout = timeout;
        self
    }

    /// How long the link may stay silent before it is treated as lost (default 60 s; the core pings
    /// every 20 s).
    pub fn link_silence(mut self, silence: Duration) -> Self {
        self.satellite.link_silence = silence;
        self
    }

    /// The satellite, declaring the capabilities and pairing methods of what it was given.
    ///
    /// Fails when the core's address is not an http(s) URL, when the enrollment code (given here or
    /// in `NESTOR_PAIRING_URI` / `NESTOR_PAIRING_CODE`) is not one, when its pairing URI is for
    /// another core, or when the manifest declares `text.*` or `audio.*` without what carries it out
    /// ([`Error::NotProvided`]).
    pub fn build(mut self) -> Result<Satellite> {
        let satellite = &mut self.satellite;
        satellite.core = base_url(&satellite.core)?;
        let given = self.given();
        let satellite = &mut self.satellite;
        for (capability, provided) in given {
            if satellite.manifest.declares(capability) && !provided {
                return Err(Error::NotProvided(capability.to_owned()));
            }
        }
        let provided = given.iter().filter(|(_, provided)| *provided).map(|(capability, _)| *capability);
        satellite.manifest = satellite.manifest.clone().declaring(provided)?;

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
        let presenter = &satellite.presenter;
        if presenter.display.is_some() {
            methods.insert(PairingMethod::DeviceCodeDisplay);
        }
        #[cfg(feature = "voice")]
        {
            satellite.speaks = presenter.voice.is_none() && presenter.display.is_none() && satellite.voice.audible;
        }
        #[cfg(feature = "voice")]
        let speaks = satellite.speaks;
        #[cfg(not(feature = "voice"))]
        let speaks = false;
        if presenter.voice.is_some() || speaks {
            methods.insert(PairingMethod::DeviceCodeSpeech);
        }
        satellite.manifest = satellite.manifest.clone().with_pairing_methods(methods)?;
        satellite.shared = Arc::new(Shared { declared: capabilities(&satellite.manifest), ..Shared::default() });
        Ok(self.satellite)
    }

    /// The capabilities the builder declares, and whether it was given what carries each out.
    fn given(&self) -> [(&'static str, bool); 4] {
        let satellite = &self.satellite;
        #[cfg(feature = "voice")]
        let (microphone, speaker) = (satellite.voice.source.is_some(), satellite.voice.sink.is_some());
        #[cfg(not(feature = "voice"))]
        let (microphone, speaker) = (false, false);
        [
            (protocol::TEXT_INPUT, self.typed),
            (protocol::TEXT_DISPLAY, satellite.text.is_some()),
            (protocol::AUDIO_CAPTURE, microphone),
            (protocol::AUDIO_PLAYBACK, speaker),
        ]
    }
}

fn capabilities(manifest: &Manifest) -> BTreeSet<String> {
    manifest.capabilities().iter().map(|c| c.name().to_owned()).collect()
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

/// Reports observations, sends typed text and tells whether the link is up, while the satellite
/// runs.
#[derive(Clone)]
pub struct Observer {
    shared: Arc<Shared>,
}

impl Observer {
    /// Report people seen now; `false` when not connected (observations are not queued).
    pub fn person_detected(&self, count: u32, confidence: f64) -> Result<bool> {
        self.send(protocol::PERSON_DETECTION, wire::PERSON_DETECTED, wire::person_detected(count, confidence)?)
    }

    /// Report that voice activity started (`true`) or stopped; `false` when not connected.
    pub fn speech_activity(&self, active: bool) -> Result<bool> {
        self.send(protocol::SPEECH_ACTIVITY, wire::SPEECH_ACTIVITY, wire::speech_activity(active))
    }

    /// Send what a person typed (1 to 4,000 characters): a turn of the room's conversation, answered
    /// on the [`TextDisplay`] when the device has one and was allowed it, else aloud. `false` when
    /// not connected (nothing is queued). Needs [`SatelliteBuilder::text_input`].
    pub fn input_text(&self, text: &str) -> Result<bool> {
        self.send(protocol::TEXT_INPUT, wire::INPUT_TEXT, wire::input_text(text)?)
    }

    /// The core's last welcome: the paired room and what the household allowed.
    pub fn welcome(&self) -> Option<Welcome> {
        self.shared.welcome.lock().ok().and_then(|w| w.clone())
    }

    /// Whether the satellite is connected and welcomed now.
    pub fn is_connected(&self) -> bool {
        self.shared.link().is_some()
    }

    /// How many links the core welcomed so far.
    pub fn connections(&self) -> usize {
        self.shared.connections.load(Ordering::SeqCst)
    }

    fn send(&self, capability: &'static str, message_type: &'static str, message: Value) -> Result<bool> {
        let allowed = self.welcome().is_none_or(|welcome| welcome.capabilities.contains(capability));
        if !self.shared.declared.contains(capability) || !allowed {
            return Err(Error::CapabilityNotAllowed { message_type, capability });
        }
        let Some(link) = self.shared.link() else {
            return Ok(false);
        };
        Ok(link.send(message.to_string()).is_ok())
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

#[derive(Default)]
struct Shared {
    declared: BTreeSet<String>,
    welcome: StdMutex<Option<Welcome>>,
    /// The welcomed link's outbox.
    link: StdMutex<Option<mpsc::UnboundedSender<String>>>,
    connections: AtomicUsize,
}

impl Shared {
    fn link(&self) -> Option<mpsc::UnboundedSender<String>> {
        self.link.lock().ok().and_then(|l| l.clone())
    }

    /// Set the link; whether one was up before.
    fn set_link(&self, link: Option<mpsc::UnboundedSender<String>>) -> bool {
        self.link.lock().map(|mut current| std::mem::replace(&mut *current, link).is_some()).unwrap_or(false)
    }

    fn set_welcome(&self, welcome: Welcome) {
        if let Ok(mut current) = self.welcome.lock() {
            *current = Some(welcome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Device, PERSON_DETECTION, SPEECH_ACTIVITY, TEXT_INPUT};

    fn observer(capabilities: &[&str]) -> (Satellite, Observer) {
        let device = Device::new("capteur-entree", "esp32", "entree").unwrap();
        let manifest = Manifest::new(device, capabilities.iter().copied()).unwrap();
        let satellite = Satellite::builder("http://127.0.0.1:9", manifest, TokenStore::new("unused.json"))
            .text_input()
            .build()
            .unwrap();
        let observer = satellite.observer();
        (satellite, observer)
    }

    #[test]
    fn an_observation_needs_its_capability_declared() {
        let (_satellite, observer) = observer(&[PERSON_DETECTION]);
        assert!(matches!(
            observer.speech_activity(true),
            Err(Error::CapabilityNotAllowed { capability: SPEECH_ACTIVITY, .. })
        ));
        assert!(!observer.person_detected(1, 0.9).unwrap(), "not connected: nothing is queued");
        assert!(observer.person_detected(1, 1.5).is_err(), "a confidence is between 0 and 1");
        assert!(!observer.input_text("bonjour").unwrap(), "typed text is declared by the builder");
        assert!(observer.input_text("").is_err());
    }

    #[test]
    fn a_message_the_household_did_not_allow_is_refused() {
        let (satellite, observer) = observer(&[PERSON_DETECTION, SPEECH_ACTIVITY]);
        satellite.shared.set_welcome(Welcome {
            device_id: "capteur-entree".into(),
            room: "entree".into(),
            capabilities: BTreeSet::from([SPEECH_ACTIVITY.to_owned()]),
        });
        assert!(matches!(
            observer.person_detected(1, 0.9),
            Err(Error::CapabilityNotAllowed { capability: PERSON_DETECTION, .. })
        ));
        assert!(matches!(
            observer.input_text("bonjour"),
            Err(Error::CapabilityNotAllowed { capability: TEXT_INPUT, .. })
        ));
        assert!(!observer.speech_activity(false).unwrap());
        assert!(!observer.is_connected());
    }

    #[test]
    fn the_link_follows_the_core_s_scheme() {
        assert_eq!(ws_url("http://nestor.local:8471"), "ws://nestor.local:8471");
        assert_eq!(ws_url("https://nestor.example.org/proxy"), "wss://nestor.example.org/proxy");
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

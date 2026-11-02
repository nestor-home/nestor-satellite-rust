//! Pairing (`nestor.satellite/1`): a person proves they mean to pair the device, and the device gets
//! its token.
//!
//! The proof travels either way. With an enrollment code (made in Nestor, configured or handed in),
//! the device sends it and is paired at once. Without one, the device asks for a pairing and presents
//! its device code on every output it has, a [`Display`], a [`Voice`], and always its log, until a
//! person types it in Nestor. While it waits, an enrollment code handed in through an [`Enroller`]
//! is sent too: the first proof accepted wins. A refused enrollment code is never presented again.
//!
//! The token is the device's only credential: it is written once, readable by its owner only (mode
//! 600), and forgotten when the core refuses it (revoked, or paired again elsewhere). The file
//! format is the Python SDK's, so a device can switch SDKs without pairing again.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fluent_bundle::FluentValue;
use reqwest::header::RETRY_AFTER;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use crate::body::Body;
use crate::codes::{Enrollment, EnrollmentCode};
use crate::error::{Error, Result};
use crate::i18n::{Language, SPOKEN_CODE, digits};
use crate::protocol::{EnrollmentMethod, Manifest, PairingMethod};
#[cfg(feature = "voice")]
use crate::speech::SpokenCode;

/// One device's state for one core, in a JSON file: its token, and the enrollment codes the core
/// refused (their SHA-256 only), so that a restarted device never sends them again.
///
/// `{"core": …, "device_id": …, "token": … (once paired), "refused": {"<sha256>": "<reason>"}}`,
/// readable by its owner only.
#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    core: String,
    device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    refused: BTreeMap<String, String>,
}

impl TokenStore {
    /// A store at `path`; a leading `~/` is the user's home.
    pub fn new(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let path = match (path.strip_prefix("~"), home()) {
            (Ok(rest), Some(home)) => home.join(rest),
            _ => path.to_path_buf(),
        };
        Self { path }
    }

    /// Where the token is kept.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The token of `device_id` for `core`, if one was saved (a token for another core or device
    /// is useless here).
    pub fn load(&self, core: &str, device_id: &str) -> Option<String> {
        self.read(core, device_id)?.token
    }

    /// Keep the token, readable by its owner only (the refused codes stay).
    pub fn save(&self, core: &str, device_id: &str, token: &str) -> io::Result<()> {
        let mut saved = self.read_or_new(core, device_id);
        saved.token = Some(token.to_owned());
        self.write(&saved)
    }

    /// Forget the token (nothing to do when there is none). The refused codes stay.
    pub fn forget(&self) -> io::Result<()> {
        let saved = fs::read(&self.path).ok().and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok());
        match saved {
            Some(saved) if !saved.refused.is_empty() => self.write(&Saved { token: None, ..saved }),
            _ => match fs::remove_file(&self.path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            },
        }
    }

    /// Why `core` refused `code` for `device_id`, if it did.
    pub fn refusal(&self, core: &str, device_id: &str, code: &EnrollmentCode) -> Option<String> {
        self.read(core, device_id)?.refused.remove(&code.digest())
    }

    /// Keep that `core` refused `code` for `device_id`, and why: its SHA-256 only, never the code.
    pub fn refuse(&self, core: &str, device_id: &str, code: &EnrollmentCode, reason: &str) -> io::Result<()> {
        let mut saved = self.read_or_new(core, device_id);
        saved.refused.insert(code.digest(), reason.to_owned());
        self.write(&saved)
    }

    fn read(&self, core: &str, device_id: &str) -> Option<Saved> {
        let saved: Saved = serde_json::from_slice(&fs::read(&self.path).ok()?).ok()?;
        (saved.core == core && saved.device_id == device_id).then_some(saved)
    }

    /// What is kept for this core and device; a file for another one is replaced.
    fn read_or_new(&self, core: &str, device_id: &str) -> Saved {
        self.read(core, device_id).unwrap_or_else(|| Saved {
            core: core.to_owned(),
            device_id: device_id.to_owned(),
            token: None,
            refused: BTreeMap::new(),
        })
    }

    fn write(&self, saved: &Saved) -> io::Result<()> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            create_private_dir(parent)?;
        }
        let mut file = open_private(&self.path)?;
        file.write_all(&serde_json::to_vec(saved)?)?;
        restrict(&self.path) // whoever made the file, only its owner reads it
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

#[cfg(unix)]
fn open_private(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().write(true).create(true).truncate(true).open(path)
}

#[cfg(unix)]
fn restrict(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// A screen the device code is shown on: the device declares `device_code.display`.
///
/// Calls come from the network task: show and return at once. A closure `Fn(&str, &str)` is a
/// display that shows the code only.
pub trait Display: Send + Sync + 'static {
    /// Show the device code until [`clear`](Self::clear) or the next code. `text` tells the person
    /// what to do with it, in the device's language.
    fn show_code(&self, text: &str, code: &str);

    /// Show a short message, in the device's language (an enrollment code was refused, the core
    /// speaks another protocol). By default nothing.
    fn show_message(&self, _text: &str) {}

    /// The device is paired: stop showing the code. By default nothing.
    fn clear(&self) {}
}

impl<F: Fn(&str, &str) + Send + Sync + 'static> Display for F {
    fn show_code(&self, text: &str, code: &str) {
        self(text, code);
    }
}

/// A voice the device says its code with: the device declares `device_code.speech`.
///
/// Calls come from the network task: start speaking and return at once. A closure `Fn(&str)` is a
/// voice.
pub trait Voice: Send + Sync + 'static {
    /// Say `text`, written in the device's language (the device code's digits one by one, said
    /// twice; an enrollment code was refused).
    fn say(&self, text: &str);
}

impl<F: Fn(&str) + Send + Sync + 'static> Voice for F {
    fn say(&self, text: &str) {
        self(text);
    }
}

/// Hands an enrollment code to the satellite while it waits to be paired: typed on its keypad
/// ([`EnrollmentMethod::Input`]).
///
/// Get one with [`Satellite::enroller`](crate::Satellite::enroller), before running the satellite;
/// declare how codes are handed in with
/// [`SatelliteBuilder::accepts_enrollment`](crate::SatelliteBuilder::accepts_enrollment).
#[derive(Clone)]
pub struct Enroller {
    pub(crate) core: String,
    pub(crate) methods: BTreeSet<PairingMethod>,
    pub(crate) slot: Arc<Mutex<Option<mpsc::UnboundedSender<Handed>>>>,
}

impl Enroller {
    /// Hand in an enrollment code or a pairing URI. It is checked here (a code, for this core, by a
    /// declared method) and sent to the core by the pairing in progress.
    ///
    /// Fails with [`Error::NotPairing`] when the satellite is not waiting for a proof (not started,
    /// or already paired).
    pub fn enroll(&self, code_or_uri: &str, method: EnrollmentMethod) -> Result<()> {
        let enrollment = Enrollment::parse(code_or_uri).map_err(Error::InvalidEnrollment)?;
        let code = code_for(&enrollment, &self.core)?.clone();
        if !self.methods.contains(&method.into()) {
            return Err(Error::MethodNotDeclared(method.into()));
        }
        let slot = self.slot.lock().map_err(|_| Error::NotPairing)?;
        let sender = slot.as_ref().ok_or(Error::NotPairing)?;
        sender.send(Handed { code, method }).map_err(|_| Error::NotPairing)
    }

    /// Whether the satellite waits for a proof now.
    pub fn is_pairing(&self) -> bool {
        self.slot.lock().is_ok_and(|slot| slot.is_some())
    }
}

/// The enrollment code of a pairing URI may only be sent to that URI's core.
pub(crate) fn code_for<'a>(enrollment: &'a Enrollment, core: &str) -> Result<&'a EnrollmentCode> {
    match enrollment.core() {
        Some(uri_core) if uri_core != core => {
            Err(Error::OtherCore { uri_core: uri_core.to_owned(), core: core.to_owned() })
        }
        _ => Ok(enrollment.code()),
    }
}

/// An enrollment code and how the device received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Handed {
    pub(crate) code: EnrollmentCode,
    pub(crate) method: EnrollmentMethod,
}

/// Presents what pairing has to say to people: on the display, by the voice (the driver's, else the
/// bundled clips through the speaker), always in the log.
#[derive(Clone)]
pub(crate) struct Presenter {
    pub(crate) display: Option<Arc<dyn Display>>,
    pub(crate) voice: Option<Arc<dyn Voice>>,
    /// The device code said with the bundled clips, for a speaker without a voice of its own.
    #[cfg(feature = "voice")]
    pub(crate) speech: Option<Arc<SpokenCode>>,
    pub(crate) language: Language,
}

impl Presenter {
    fn code(&self, code: &str) {
        let text = self.language.say("pairing-code-shown", &[]);
        info!("{text} {code}");
        if let Some(display) = &self.display {
            display.show_code(&text, code);
        }
        if let Some(voice) = &self.voice {
            voice.say(&self.language.say(SPOKEN_CODE, &[("digits", FluentValue::from(digits(code)))]));
        } else {
            #[cfg(feature = "voice")]
            if let Some(speech) = &self.speech {
                speech.say(code);
            }
        }
        if self.display.is_none() && self.voice.is_none() && !self.speaks() {
            info!("{}", self.language.say("pairing-headless", &[]));
        }
    }

    fn message(&self, text: &str) {
        if let Some(display) = &self.display {
            display.show_message(text);
        }
        if let Some(voice) = &self.voice {
            voice.say(text);
        }
    }

    fn refused(&self, reason: &str) {
        let text = self.language.say("pairing-refused", &[("reason", FluentValue::from(reason))]);
        warn!(reason, "{text}");
        self.message(&text);
    }

    fn waiting(&self, id: &str, wait: Duration) {
        warn!("{}", self.language.say(id, &[("seconds", FluentValue::from(wait.as_secs_f64().ceil()))]));
    }

    fn failed(&self, error: &Error) {
        let text = self.language.describe(error);
        error!(%error, "{text}");
        self.message(&text);
    }

    fn paired(&self) {
        info!("{}", self.language.say("pairing-paired", &[]));
        if let Some(display) = &self.display {
            display.clear();
        }
    }

    /// Whether the bundled voice says the device code.
    fn speaks(&self) -> bool {
        #[cfg(feature = "voice")]
        return self.speech.is_some();
        #[cfg(not(feature = "voice"))]
        false
    }

    /// Pairing is over: stop saying the code.
    async fn hush(&self) {
        #[cfg(feature = "voice")]
        if let Some(speech) = &self.speech {
            speech.hush().await;
        }
    }
}

/// One pairing, from the first request to the token.
pub(crate) struct Pairing<'a> {
    pub(crate) http: &'a Client,
    pub(crate) core: &'a str,
    pub(crate) manifest: &'a Manifest,
    /// Where refused enrollment codes are kept across restarts.
    pub(crate) tokens: &'a TokenStore,
    pub(crate) body: &'a dyn Body,
    pub(crate) presenter: &'a Presenter,
    pub(crate) poll: Duration,
    /// The configured enrollment code: forgotten once spent or refused.
    pub(crate) configured: &'a Mutex<Option<Handed>>,
    /// Enrollment codes refused while running (also kept in `tokens`): never presented again.
    pub(crate) refused: &'a Mutex<HashSet<EnrollmentCode>>,
    pub(crate) slot: &'a Mutex<Option<mpsc::UnboundedSender<Handed>>>,
}

/// How long to wait when the core is busy and says nothing about it.
const BUSY_WAIT: Duration = Duration::from_secs(10);

impl Pairing<'_> {
    /// Pair until a proof is accepted; the token comes once: save it.
    pub(crate) async fn run(&self) -> Result<String> {
        let (sender, mut inbox) = mpsc::unbounded_channel();
        set(self.slot, Some(sender));
        let _open = Open(self.slot); // closed however this ends, cancelled included
        let outcome = self.pair(&mut inbox).await;
        self.presenter.hush().await; // approved, failed or cancelled: the code is no longer said
        match &outcome {
            Ok(_) => self.presenter.paired(),
            Err(error @ (Error::ProtocolUnsupported(_) | Error::PairingRefused(_))) => self.presenter.failed(error),
            Err(_) => {}
        }
        outcome
    }

    async fn pair(&self, inbox: &mut mpsc::UnboundedReceiver<Handed>) -> Result<String> {
        let mut delayed: Option<Handed> = None; // a code handed in, to send again after a wait
        loop {
            // A code handed in before the request goes with it, else the configured one.
            let proof = delayed
                .take()
                .or_else(|| inbox.try_recv().ok())
                .or_else(|| self.configured.lock().ok().and_then(|c| c.clone()))
                .filter(|handed| !self.skip_refused(handed));
            let response = self.request(proof.as_ref()).await?;
            let ticket: Ticket = match response {
                Answer::Ok(value) => serde_json::from_value(value)?,
                Answer::Wait { id, wait } => {
                    self.presenter.waiting(id, wait);
                    tokio::time::sleep(wait).await;
                    delayed = proof;
                    continue;
                }
                Answer::Refused { reason } => {
                    let Some(handed) = &proof else {
                        return Err(Error::Core {
                            status: 403,
                            detail: format!("{reason} without an enrollment code"),
                        });
                    };
                    self.refuse(handed, &reason);
                    continue;
                }
            };
            if proof.is_some() {
                self.forget_configured(); // spent
            }
            if let Some(code) = &ticket.device_code {
                self.presenter.code(code);
            }
            match self.wait_for_token(&ticket, inbox).await? {
                Some(token) => return Ok(token),
                None => info!("{}", self.presenter.language.say("pairing-expired", &[])),
            }
        }
    }

    /// Poll until the token comes (`None` when the pairing expired), sending the enrollment codes
    /// handed in meanwhile.
    async fn wait_for_token(
        &self,
        ticket: &Ticket,
        inbox: &mut mpsc::UnboundedReceiver<Handed>,
    ) -> Result<Option<String>> {
        let mut retry: Option<(Instant, Handed)> = None;
        loop {
            let retry_at = retry.as_ref().map(|(at, _)| *at);
            tokio::select! {
                () = tokio::time::sleep(self.poll) => {
                    let response = self.http.get(format!("{}/satellites/pairings/{}", self.core, ticket.pairing_id)).send().await?;
                    let state: State = success(response).await?.json().await?;
                    match (state.status.as_str(), state.token) {
                        ("approved", Some(token)) => return Ok(Some(token)),
                        ("pending" | "approved", _) => {
                            if state.identify {
                                info!("{}", self.presenter.language.say("identify-requested", &[]));
                                self.body.identify();
                            }
                        }
                        (status, _) => {
                            debug!(%status, "the pairing ended without a token");
                            return Ok(None);
                        }
                    }
                }
                Some(handed) = inbox.recv(), if retry.is_none() => {
                    retry = self.enroll(ticket, handed).await?;
                }
                () = tokio::time::sleep_until(retry_at.unwrap_or_else(Instant::now)), if retry_at.is_some() => {
                    if let Some((_, handed)) = retry.take() {
                        retry = self.enroll(ticket, handed).await?;
                    }
                }
            }
        }
    }

    /// Send an enrollment code for a pending pairing; what to retry, and when, if rate-limited.
    async fn enroll(&self, ticket: &Ticket, handed: Handed) -> Result<Option<(Instant, Handed)>> {
        if self.was_refused(&handed.code) {
            warn!("{}", self.presenter.language.say("pairing-already-refused", &[]));
            return Ok(None);
        }
        let url = format!("{}/satellites/pairings/{}/enrollment", self.core, ticket.pairing_id);
        let proof = json!({"enrollment_code": handed.code.as_str(), "method": handed.method});
        let response = match self.http.post(url).json(&proof).send().await {
            Ok(response) => response,
            Err(error) => {
                warn!(%error, "the enrollment code could not be sent: hand it in again");
                return Ok(None);
            }
        };
        let answer = match answer(response).await {
            Ok(answer) => answer,
            Err(error @ Error::ProtocolUnsupported(_)) => return Err(error),
            Err(error) => {
                error!(%error, "the core did not take the enrollment code: hand it in again");
                return Ok(None);
            }
        };
        match answer {
            Answer::Ok(_) => info!(method = %handed.method, "enrollment code accepted"),
            Answer::Wait { id, wait } => {
                self.presenter.waiting(id, wait);
                return Ok(Some((Instant::now() + wait, handed)));
            }
            Answer::Refused { reason } if reason == NOT_PENDING => {
                debug!("the pairing no longer waits for a proof: polling for the token");
            }
            Answer::Refused { reason } => {
                self.refuse(&handed, &reason);
                if let Some(code) = &ticket.device_code {
                    self.presenter.code(code); // a person can still pair it with the device code
                }
            }
        }
        Ok(None)
    }

    async fn request(&self, proof: Option<&Handed>) -> Result<Answer> {
        let mut request = json!({"manifest": self.manifest.to_json()});
        if let Some(handed) = proof {
            request["enrollment_code"] = Value::from(handed.code.as_str());
            request["method"] = json!(handed.method);
        }
        let response = self.http.post(format!("{}/satellites/pairings", self.core)).json(&request).send().await?;
        answer(response).await
    }

    fn refuse(&self, handed: &Handed, reason: &str) {
        if let Ok(mut refused) = self.refused.lock() {
            refused.insert(handed.code.clone());
        }
        let device = self.manifest.device().id();
        if let Err(error) = self.tokens.refuse(self.core, device, &handed.code, reason) {
            warn!(%error, path = %self.tokens.path().display(), "the refused enrollment code could not be kept");
        }
        if self.configured.lock().is_ok_and(|c| c.as_ref().is_some_and(|c| c.code == handed.code)) {
            self.forget_configured();
        }
        self.presenter.refused(reason);
    }

    fn was_refused(&self, code: &EnrollmentCode) -> bool {
        self.refused.lock().is_ok_and(|refused| refused.contains(code))
            || self.tokens.refusal(self.core, self.manifest.device().id(), code).is_some()
    }

    /// Whether `handed` was refused before (in an earlier run too): then it is said, and forgotten
    /// if it is the configured code.
    fn skip_refused(&self, handed: &Handed) -> bool {
        if !self.was_refused(&handed.code) {
            return false;
        }
        warn!("{}", self.presenter.language.say("pairing-already-refused", &[]));
        if self.configured.lock().is_ok_and(|c| c.as_ref().is_some_and(|c| c.code == handed.code)) {
            self.forget_configured();
        }
        true
    }

    fn forget_configured(&self) {
        if let Ok(mut configured) = self.configured.lock() {
            *configured = None;
        }
    }
}

/// While it lives, the satellite takes enrollment codes; once dropped, codes handed in are refused
/// (nothing waits for them).
struct Open<'a>(&'a Mutex<Option<mpsc::UnboundedSender<Handed>>>);

impl Drop for Open<'_> {
    fn drop(&mut self) {
        set(self.0, None);
    }
}

fn set(slot: &Mutex<Option<mpsc::UnboundedSender<Handed>>>, sender: Option<mpsc::UnboundedSender<Handed>>) {
    if let Ok(mut slot) = slot.lock() {
        *slot = sender;
    }
}

/// `201 pairing.ticket`.
#[derive(Debug, Deserialize)]
struct Ticket {
    pairing_id: String,
    device_code: Option<String>,
}

/// `200 pairing.state`.
#[derive(Debug, Deserialize)]
struct State {
    status: String,
    token: Option<String>,
    #[serde(default)]
    identify: bool,
}

/// `pairing.error`.
#[derive(Debug, Default, Deserialize)]
struct Refusal {
    error: Option<String>,
    #[serde(default)]
    detail: String,
    reason: Option<String>,
    retry_after_s: Option<u64>,
}

const NOT_PENDING: &str = "not_pending";

/// What a pairing endpoint answered.
enum Answer {
    /// Its body.
    Ok(Value),
    /// Rate-limited: try again after `wait`; `id` says why, in the catalogue.
    Wait { id: &'static str, wait: Duration },
    /// The enrollment code is refused (`enrollment_refused` and its reason), or the pairing no longer
    /// waits for a proof ([`NOT_PENDING`]).
    Refused { reason: String },
}

async fn answer(response: Response) -> Result<Answer> {
    let status = response.status();
    if status.is_success() {
        return Ok(Answer::Ok(response.json().await?));
    }
    let header =
        response.headers().get(RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    let text = response.text().await.unwrap_or_default();
    let refusal: Refusal =
        serde_json::from_str(&text).unwrap_or_else(|_| Refusal { detail: text, ..Default::default() });
    let detail = refusal.detail.clone();
    match (status, refusal.error.as_deref()) {
        (StatusCode::TOO_MANY_REQUESTS, error) => {
            let told = header.into_iter().chain(refusal.retry_after_s).max().map(Duration::from_secs);
            let (id, default) = match error {
                Some("too_many_attempts") => ("pairing-rate-limited", Duration::from_secs(1)),
                _ => ("pairing-busy", BUSY_WAIT),
            };
            Ok(Answer::Wait { id, wait: told.unwrap_or(default).max(Duration::from_secs(1)) })
        }
        (_, Some("enrollment_refused")) => {
            Ok(Answer::Refused { reason: refusal.reason.unwrap_or_else(|| "unknown".into()) })
        }
        (_, Some(NOT_PENDING)) => Ok(Answer::Refused { reason: NOT_PENDING.into() }),
        (_, Some("protocol_unsupported")) => Err(Error::ProtocolUnsupported(detail)),
        (StatusCode::UNPROCESSABLE_ENTITY, _) => Err(Error::PairingRefused(detail)),
        _ => Err(Error::Core { status: status.as_u16(), detail }),
    }
}

pub(crate) async fn success(response: Response) -> Result<Response> {
    if response.status().is_success() {
        Ok(response)
    } else {
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<Refusal>(&text).map(|r| r.detail).unwrap_or(text);
        Err(Error::Core { status, detail })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `work` logs, as text.
    fn logged(work: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);
        impl io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt().with_ansi(false).with_writer(move || writer.clone()).finish();
        tracing::subscriber::with_default(subscriber, work);
        String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap()
    }

    fn presenter(display: bool, language: Language) -> Presenter {
        let display: Option<Arc<dyn Display>> = display.then(|| Arc::new(|_: &str, _: &str| {}) as Arc<dyn Display>);
        Presenter {
            display,
            voice: None,
            #[cfg(feature = "voice")]
            speech: None,
            language,
        }
    }

    #[test]
    fn the_device_code_is_always_logged_with_the_way_to_pair_a_device_without_screen_or_voice() {
        let headless = logged(|| presenter(false, Language::Fr).code("042917"));
        assert!(headless.contains("saisissez ce code dans Nestor : 042917"), "{headless}");
        assert!(headless.contains("NESTOR_PAIRING_URI"), "the way to pair it: {headless}");
        let shown = logged(|| presenter(true, Language::En).code("042917"));
        assert!(shown.contains("type this code in Nestor: 042917"), "{shown}");
        assert!(!shown.contains("NESTOR_PAIRING_URI"), "{shown}");
    }

    #[test]
    fn a_refused_enrollment_code_is_logged_in_the_device_s_language() {
        let log = logged(|| presenter(false, Language::Fr).refused("cancelled"));
        assert!(log.contains("WARN") && log.contains("Ce code d'enrôlement a été annulé."), "{log}");
    }

    #[test]
    fn a_token_is_kept_for_its_core_and_device_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path().join("nested/token.json"));
        assert_eq!(store.load("http://core", "a"), None);
        store.save("http://core", "a", "nst1_secret").unwrap();
        assert_eq!(store.load("http://core", "a").as_deref(), Some("nst1_secret"));
        assert_eq!(store.load("http://other", "a"), None);
        assert_eq!(store.load("http://core", "b"), None);
        store.forget().unwrap();
        store.forget().unwrap();
        assert_eq!(store.load("http://core", "a"), None);
    }

    #[test]
    fn refused_codes_are_kept_as_digests_for_their_core_and_device_and_outlive_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path().join("state.json"));
        let code = EnrollmentCode::parse("K7QF-2M9X").unwrap();
        store.refuse("http://core", "a", &code, "used").unwrap();
        assert_eq!(store.load("http://core", "a"), None, "no token until paired");
        store.save("http://core", "a", "nst1_secret").unwrap();
        store.forget().unwrap();
        assert_eq!(store.load("http://core", "a"), None);
        assert_eq!(store.refusal("http://core", "a", &code).as_deref(), Some("used"));
        assert_eq!(store.refusal("http://other", "a", &code), None);
        let other = EnrollmentCode::parse("ABCD-EFGH").unwrap();
        assert_eq!(store.refusal("http://core", "a", &other), None);
        let file = fs::read_to_string(store.path()).unwrap();
        assert!(!file.contains("K7QF2M9X") && file.contains(&code.digest()), "{file}");
    }

    #[cfg(unix)]
    #[test]
    fn the_token_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.json");
        fs::write(&path, "{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        TokenStore::new(&path).save("c", "a", "t").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_file_that_is_not_a_token_is_no_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.json");
        fs::write(&path, "not json").unwrap();
        assert_eq!(TokenStore::new(&path).load("c", "a"), None);
    }

    #[test]
    fn a_path_under_home_is_expanded() {
        if let Some(home) = home() {
            assert_eq!(TokenStore::new("~/x.json").path(), home.join("x.json"));
        }
    }
}

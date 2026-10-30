//! What the conformance suite does not see: how pairing presents itself on a display and a voice,
//! enrollment codes handed in, refusals, rate limits and a core that is down, against a fake core
//! over HTTP.
#![allow(clippy::unwrap_used)] // test helpers fail loudly

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use nestor_satellite::{
    AUDIO_CAPTURE, AUDIO_PLAYBACK, AudioError, AudioSource, Body, Device, Display, Enrollment, EnrollmentMethod, Error,
    IDENTIFY, Language, Manifest, RecordingSink, Satellite, SatelliteBuilder, SilenceSource, TokenStore, Voice,
};
use serde_json::{Value, json};

/// What the fake core does with enrollment codes.
#[derive(Clone, Copy, Default, PartialEq)]
enum Codes {
    /// Accepted.
    #[default]
    Accept,
    /// Refused: already used.
    Refuse,
    /// The pairing no longer waits for a proof: the device code won.
    NotPending,
}

/// A core that hands out device codes, approves a pending pairing after `pending` polls (unless
/// `never_approve`), judges enrollment codes per `codes`, and answers offers with 503 for the tokens
/// it handed out (401 for any other).
#[derive(Default)]
struct FakeCore {
    pending: usize,
    never_approve: bool,
    identify: bool,
    codes: Codes,
    rate_limit_s: Option<u64>,
    unsupported: bool,
    requests: Mutex<Vec<(Instant, Value)>>,
    enrollments: Mutex<Vec<Value>>,
    approved: Mutex<Vec<String>>,
    polls: AtomicUsize,
    offers: Mutex<Vec<String>>,
}

impl FakeCore {
    async fn serve(self) -> (String, Arc<Self>) {
        let core = Arc::new(self);
        let app = Router::new()
            .route("/satellites/pairings", post(request))
            .route("/satellites/pairings/{id}", get(poll))
            .route("/satellites/pairings/{id}/enrollment", post(enroll))
            .route("/satellites/{device}/offer", post(offer))
            .with_state(core.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, core)
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().iter().map(|(_, body)| body.clone()).collect()
    }

    fn coded_requests(&self) -> usize {
        self.requests().iter().filter(|r| r.get("enrollment_code").is_some()).count()
    }

    fn enrollments(&self) -> Vec<Value> {
        self.enrollments.lock().unwrap().clone()
    }

    fn offers(&self) -> Vec<String> {
        self.offers.lock().unwrap().clone()
    }

    fn approve(&self, id: &str) {
        self.approved.lock().unwrap().push(id.to_owned());
    }
}

fn refusal(status: StatusCode, error: &str, extra: Value) -> Response {
    let mut body = json!({"error": error, "detail": "in English, for logs"});
    body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    (status, Json(body)).into_response()
}

async fn request(State(core): State<Arc<FakeCore>>, Json(body): Json<Value>) -> Response {
    let n = {
        let mut requests = core.requests.lock().unwrap();
        requests.push((Instant::now(), body.clone()));
        requests.len()
    };
    if core.unsupported {
        return refusal(StatusCode::UNPROCESSABLE_ENTITY, "protocol_unsupported", json!({}));
    }
    if let (Some(wait), 1) = (core.rate_limit_s, n) {
        let mut response = refusal(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts", json!({"retry_after_s": wait}));
        response.headers_mut().insert("retry-after", wait.to_string().parse().unwrap());
        return response;
    }
    let id = format!("p{n}");
    let expires_at = "2026-10-30T12:10:00+00:00";
    if body.get("enrollment_code").is_some() {
        if core.codes == Codes::Refuse {
            return refusal(StatusCode::FORBIDDEN, "enrollment_refused", json!({"reason": "used"}));
        }
        core.approve(&id);
        return (StatusCode::CREATED, Json(json!({"pairing_id": id, "status": "approved", "expires_at": expires_at})))
            .into_response();
    }
    let ticket =
        json!({"pairing_id": id, "status": "pending", "expires_at": expires_at, "device_code": format!("00000{n}")});
    (StatusCode::CREATED, Json(ticket)).into_response()
}

async fn enroll(State(core): State<Arc<FakeCore>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    core.enrollments.lock().unwrap().push(body);
    match core.codes {
        Codes::Refuse => refusal(StatusCode::FORBIDDEN, "enrollment_refused", json!({"reason": "expired"})),
        Codes::NotPending => {
            core.approve(&id); // a person typed the device code first
            refusal(StatusCode::CONFLICT, "not_pending", json!({}))
        }
        Codes::Accept => {
            core.approve(&id);
            Json(json!({"status": "approved", "device_id": "enceinte-salon", "room": "salon", "capabilities": []}))
                .into_response()
        }
    }
}

async fn poll(State(core): State<Arc<FakeCore>>, Path(id): Path<String>) -> Json<Value> {
    let polls = core.polls.fetch_add(1, Ordering::SeqCst);
    let approved = core.approved.lock().unwrap().contains(&id);
    if approved || (!core.never_approve && polls >= core.pending) {
        if core.offers().contains(&format!("nst2_{id}")) {
            return Json(json!({"status": "claimed"}));
        }
        return Json(json!({"status": "approved", "token": format!("nst2_{id}"), "device_id": "enceinte-salon",
                           "room": "salon", "capabilities": ["audio.capture"]}));
    }
    if core.identify && polls == 0 {
        return Json(json!({"status": "pending", "identify": true}));
    }
    Json(json!({"status": "pending"}))
}

async fn offer(State(core): State<Arc<FakeCore>>, headers: HeaderMap, Json(body): Json<Value>) -> StatusCode {
    assert_eq!(body["type"], "offer");
    assert!(body["sdp"].as_str().unwrap().contains("m=application"), "the data channel is offered");
    let token = headers["authorization"].to_str().unwrap().trim_start_matches("Bearer ").to_owned();
    core.offers.lock().unwrap().push(token.clone());
    if token.starts_with("nst2_p") { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::UNAUTHORIZED }
}

fn manifest() -> Manifest {
    let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon").unwrap();
    Manifest::new(device, [AUDIO_CAPTURE, AUDIO_PLAYBACK, IDENTIFY]).unwrap()
}

/// A display, a voice and a body that note what they are asked to do.
#[derive(Clone, Default)]
struct Outputs(Arc<Mutex<Vec<String>>>);

impl Outputs {
    fn noted(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    fn note(&self, what: String) {
        self.0.lock().unwrap().push(what);
    }

    fn count(&self, prefix: &str) -> usize {
        self.noted().iter().filter(|n| n.starts_with(prefix)).count()
    }
}

impl Display for Outputs {
    fn show_code(&self, text: &str, code: &str) {
        self.note(format!("show {text} {code}"));
    }

    fn show_message(&self, text: &str) {
        self.note(format!("message {text}"));
    }

    fn clear(&self) {
        self.note("clear".into());
    }
}

impl Voice for Outputs {
    fn say(&self, text: &str) {
        self.note(format!("say {text}"));
    }
}

impl Body for Outputs {
    fn identify(&self) {
        self.note("identify".into());
    }
}

fn builder(url: &str, tokens: &TokenStore) -> SatelliteBuilder {
    Satellite::builder(url, manifest(), tokens.clone())
        .language(Language::En)
        .poll_interval(Duration::from_millis(10))
        .backoff(Duration::from_millis(20), Duration::from_millis(40))
}

fn tokens() -> (tempfile::TempDir, TokenStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::new(dir.path().join("token.json"));
    (dir, store)
}

fn code() -> Enrollment {
    Enrollment::parse("k7qf-2m9x").unwrap()
}

#[tokio::test]
async fn the_device_code_is_shown_and_said_twice_digit_by_digit_until_it_is_typed() {
    let (url, core) = FakeCore { pending: 3, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).display(outputs.clone()).voice(outputs.clone()).build().unwrap();
    satellite.run_until(wait_for(|| !core.offers().is_empty())).await.unwrap();

    assert_eq!(
        outputs.noted(),
        [
            "show To pair this device, type this code in Nestor: 000001",
            "say Pairing code: 0, 0, 0, 0, 0, 1. I repeat: 0, 0, 0, 0, 0, 1.",
            "clear",
        ]
    );
    let sent = &core.requests()[0];
    assert_eq!(sent, &json!({"manifest": sent["manifest"].clone()}), "no enrollment code: the manifest alone");
    assert_eq!(
        sent["manifest"]["pairing_methods"],
        json!(["device_code.display", "device_code.speech", "enrollment.provisioning"])
    );
    assert_eq!(tokens.load(&url, "enceinte-salon").as_deref(), Some("nst2_p1"), "the token is kept");
    assert_eq!(core.offers(), ["nst2_p1"]);
}

#[tokio::test]
async fn without_display_or_voice_only_provisioning_is_declared_and_the_device_code_still_pairs() {
    let (url, core) = FakeCore::default().serve().await;
    let (_dir, tokens) = tokens();
    let satellite = builder(&url, &tokens).build().unwrap();
    assert_eq!(satellite.manifest().to_json()["pairing_methods"], json!(["enrollment.provisioning"]));
    satellite.run_until(wait_for(|| !core.offers().is_empty())).await.unwrap();
    assert_eq!(core.offers(), ["nst2_p1"], "paired with the device code read in the log");
}

#[tokio::test]
async fn a_pending_identify_reaches_the_body_and_polling_goes_on() {
    let (url, core) = FakeCore { pending: 3, identify: true, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).body(outputs.clone()).build().unwrap();
    satellite.run_until(wait_for(|| !core.offers().is_empty())).await.unwrap();
    assert_eq!(outputs.noted(), ["identify"]);
    assert!(core.polls.load(Ordering::SeqCst) >= 4, "kept polling until approved");
}

#[tokio::test]
async fn a_configured_enrollment_code_pairs_at_once_without_presenting_a_device_code() {
    let (url, core) = FakeCore { never_approve: true, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).display(outputs.clone()).enrollment(code()).build().unwrap();
    satellite.run_until(wait_for(|| !core.offers().is_empty())).await.unwrap();

    let sent = &core.requests()[0];
    assert_eq!((&sent["enrollment_code"], &sent["method"]), (&json!("K7QF2M9X"), &json!("enrollment.provisioning")));
    assert_eq!(outputs.noted(), ["clear"], "no device code presented");
    assert_eq!(core.offers(), ["nst2_p1"]);
}

#[tokio::test]
async fn a_pairing_uri_brings_its_code_to_its_own_core_only() {
    let (_dir, tokens) = tokens();
    let uri = Enrollment::parse("nestor+http://127.0.0.1:9/?code=K7QF2M9X").unwrap();
    assert!(builder("http://127.0.0.1:9/", &tokens).enrollment(uri.clone()).build().is_ok());
    let elsewhere = builder("http://127.0.0.1:10", &tokens).enrollment(uri).build();
    assert!(matches!(elsewhere, Err(Error::OtherCore { .. })));
    assert!(matches!(builder("nestor.local", &tokens).build(), Err(Error::Protocol(_))), "an http(s) URL");
}

#[tokio::test]
async fn a_refused_enrollment_code_is_said_once_never_sent_again_and_the_device_code_takes_over() {
    let (url, core) = FakeCore { codes: Codes::Refuse, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).display(outputs.clone()).enrollment(code()).build().unwrap();
    satellite.run_until(wait_for(|| !core.offers().is_empty())).await.unwrap();

    assert_eq!(core.coded_requests(), 1, "presented once");
    assert_eq!(
        outputs.noted(),
        [
            "message This enrollment code was already used. Use the device code instead.",
            "show To pair this device, type this code in Nestor: 000002",
            "clear",
        ]
    );
    assert_eq!(core.offers(), ["nst2_p2"]);
}

#[tokio::test]
async fn a_code_typed_while_pending_is_sent_by_its_method_and_pairs() {
    let (url, core) = FakeCore { never_approve: true, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let outputs = Outputs::default();
    let satellite =
        builder(&url, &tokens).display(outputs.clone()).accepts_enrollment(EnrollmentMethod::Input).build().unwrap();
    assert_eq!(
        satellite.manifest().to_json()["pairing_methods"],
        json!(["device_code.display", "enrollment.input", "enrollment.provisioning"])
    );
    let enroller = satellite.enroller();
    assert!(matches!(enroller.enroll("K7QF-2M9X", EnrollmentMethod::Input), Err(Error::NotPairing)), "not started");
    let typing = async {
        wait_for(|| outputs.count("show") == 1).await;
        assert!(enroller.is_pairing());
        enroller.enroll("k7qf 2m9x", EnrollmentMethod::Input).unwrap();
        wait_for(|| !core.offers().is_empty()).await;
    };
    satellite.run_until(typing).await.unwrap();

    assert_eq!(core.enrollments(), [json!({"enrollment_code": "K7QF2M9X", "method": "enrollment.input"})]);
    assert_eq!(core.offers(), ["nst2_p1"]);
    assert_eq!(outputs.noted().last().map(String::as_str), Some("clear"), "the code is no longer shown");
    assert!(!enroller.is_pairing());
}

#[tokio::test]
async fn when_the_device_code_won_the_race_the_token_still_comes() {
    let (url, core) = FakeCore { never_approve: true, codes: Codes::NotPending, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let outputs = Outputs::default();
    let satellite =
        builder(&url, &tokens).display(outputs.clone()).accepts_enrollment(EnrollmentMethod::Input).build().unwrap();
    let enroller = satellite.enroller();
    let scanning = async {
        wait_for(|| outputs.count("show") == 1).await;
        enroller.enroll("nestor+http://127.0.0.1:1/?code=K7QF2M9X", EnrollmentMethod::Input).unwrap_err();
        let uri = format!("{}/?code=K7QF2M9X", url.replace("http://", "nestor+http://"));
        enroller.enroll(&uri, EnrollmentMethod::Input).unwrap();
        wait_for(|| !core.offers().is_empty()).await;
    };
    satellite.run_until(scanning).await.unwrap();
    assert_eq!(core.enrollments().len(), 1);
    assert_eq!(core.offers(), ["nst2_p1"]);
    assert_eq!(outputs.count("message"), 0, "a lost race is no refusal");
}

#[tokio::test]
async fn an_enrollment_code_is_handed_in_only_by_a_declared_method_and_as_a_code() {
    let (url, core) = FakeCore { never_approve: true, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).display(outputs.clone()).build().unwrap();
    let enroller = satellite.enroller();
    let typing = async {
        wait_for(|| outputs.count("show") == 1).await;
        let typed = enroller.enroll("K7QF-2M9X", EnrollmentMethod::Input);
        assert!(matches!(typed, Err(Error::MethodNotDeclared(_))), "{typed:?}");
        let wrong = enroller.enroll("K7QF-2M9U", EnrollmentMethod::Provisioning);
        assert!(matches!(wrong, Err(Error::InvalidEnrollment(_))), "{wrong:?}");
        enroller.enroll("K7QF-2M9X", EnrollmentMethod::Provisioning).unwrap();
        wait_for(|| !core.offers().is_empty()).await;
    };
    satellite.run_until(typing).await.unwrap();
    assert_eq!(core.enrollments().len(), 1);
}

#[tokio::test]
async fn a_code_refused_while_pending_is_said_and_the_device_code_presented_again() {
    let (url, core) = FakeCore { pending: 40, codes: Codes::Refuse, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let outputs = Outputs::default();
    let satellite =
        builder(&url, &tokens).voice(outputs.clone()).accepts_enrollment(EnrollmentMethod::Input).build().unwrap();
    let enroller = satellite.enroller();
    let typing = async {
        wait_for(|| outputs.count("say") == 1).await;
        enroller.enroll("K7QF2M9X", EnrollmentMethod::Input).unwrap();
        wait_for(|| outputs.count("say") == 3).await;
        enroller.enroll("K7QF2M9X", EnrollmentMethod::Input).unwrap(); // typed again
        wait_for(|| !core.offers().is_empty()).await;
    };
    satellite.run_until(typing).await.unwrap();
    assert_eq!(core.enrollments().len(), 1, "a refused code is never sent again");
    assert_eq!(
        outputs.noted()[1..3],
        [
            "say This enrollment code has expired. Use the device code instead.",
            "say Pairing code: 0, 0, 0, 0, 0, 1. I repeat: 0, 0, 0, 0, 0, 1.",
        ]
    );
}

#[tokio::test]
async fn a_refused_enrollment_code_is_kept_and_never_sent_again_after_a_restart() {
    let (url, core) = FakeCore { codes: Codes::Refuse, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let first = builder(&url, &tokens).enrollment(code()).build().unwrap();
    first.run_until(wait_for(|| !core.offers().is_empty())).await.unwrap();
    assert_eq!(core.coded_requests(), 1);

    let file = std::fs::read_to_string(tokens.path()).unwrap();
    assert!(!file.contains("K7QF2M9X") && !file.contains("K7QF-2M9X"), "never in clear: {file}");
    let saved: Value = serde_json::from_str(&file).unwrap();
    assert_eq!(saved["refused"], json!({code().code().digest(): "used"}));
    tokens.forget().unwrap(); // the core revoked the token: the refused codes stay
    assert_eq!(tokens.load(&url, "enceinte-salon"), None);

    let restarted = builder(&url, &tokens).enrollment(code()).build().unwrap();
    restarted.run_until(wait_for(|| core.offers().iter().any(|t| t == "nst2_p3"))).await.unwrap();
    assert_eq!(core.coded_requests(), 1, "the same code, configured again, is not sent again");
    assert_eq!(core.requests().len(), 3, "the restarted device asked for a device code at once");
}

#[tokio::test]
async fn a_speaker_without_a_screen_says_its_code_with_the_bundled_voice_and_stops_once_paired() {
    let (url, core) = FakeCore { pending: 20, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let speaker = RecordingSink::new();
    let satellite = builder(&url, &tokens).sink(speaker.clone()).build().unwrap();
    assert_eq!(
        satellite.manifest().to_json()["pairing_methods"],
        json!(["device_code.speech", "enrollment.provisioning"])
    );
    let said = Mutex::new(0);
    let pairing = async {
        wait_for(|| tokens.load(&url, "enceinte-salon").is_some()).await; // mid-announcement
        *said.lock().unwrap() = speaker.played().len();
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    satellite.run_until(pairing).await.unwrap();
    let said = *said.lock().unwrap();
    assert!(said >= 48_000 / 10, "at least 0.1 s said: {said} samples");
    assert!(said < 48_000 * 6, "approved before the end of the announcement: {said} samples");
    assert_eq!(speaker.played().len(), said, "nothing said once paired");
    assert!(!core.offers().is_empty());
}

#[tokio::test]
async fn a_driver_s_voice_replaces_the_bundled_one() {
    let (url, core) = FakeCore { pending: 3, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let speaker = RecordingSink::new();
    let outputs = Outputs::default();
    let satellite =
        builder(&url, &tokens).language(Language::Fr).sink(speaker.clone()).voice(outputs.clone()).build().unwrap();
    satellite.run_until(wait_for(|| !core.offers().is_empty())).await.unwrap();
    assert_eq!(outputs.noted(), ["say Code d'appairage : 0, 0, 0, 0, 0, 1. Je répète : 0, 0, 0, 0, 0, 1."]);
    assert!(speaker.played().is_empty());
}

#[tokio::test]
async fn the_bundled_voice_is_declared_only_for_a_speaker_without_a_screen() {
    let (_dir, tokens) = tokens();
    let methods = |satellite: Satellite| satellite.manifest().to_json()["pairing_methods"].clone();
    let url = "http://127.0.0.1:9";
    let no_speaker = builder(url, &tokens).build().unwrap();
    assert_eq!(methods(no_speaker), json!(["enrollment.provisioning"]), "no sink: nothing to speak with");
    let with_screen = builder(url, &tokens).sink(RecordingSink::new()).display(Outputs::default()).build().unwrap();
    assert_eq!(methods(with_screen), json!(["device_code.display", "enrollment.provisioning"]));
    let device = Device::new("capteur-entree", "esp32", "entree").unwrap();
    let silent = Manifest::new(device, [AUDIO_CAPTURE]).unwrap();
    let undeclared = Satellite::builder(url, silent, tokens.clone()).sink(RecordingSink::new()).build().unwrap();
    assert_eq!(methods(undeclared), json!(["enrollment.provisioning"]), "audio.playback is not declared");
}

#[tokio::test]
async fn a_rate_limited_device_waits_for_retry_after() {
    let (url, core) = FakeCore { rate_limit_s: Some(1), never_approve: true, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let satellite = builder(&url, &tokens).enrollment(code()).build().unwrap();
    satellite.run_until(wait_for(|| !core.offers().is_empty())).await.unwrap();
    let requests = core.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].0 - requests[0].0 >= Duration::from_secs(1), "waited Retry-After");
    assert_eq!(requests[1].1["enrollment_code"], "K7QF2M9X", "the same code, once allowed");
}

#[tokio::test]
async fn a_core_speaking_another_protocol_stops_the_satellite_and_says_so() {
    let (url, _core) = FakeCore { unsupported: true, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).display(outputs.clone()).build().unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), satellite.run()).await.unwrap();
    assert!(matches!(outcome, Err(Error::ProtocolUnsupported(_))), "{outcome:?}");
    assert_eq!(outputs.noted(), ["message Nestor does not speak this device's protocol: update the device or Nestor."]);
}

#[tokio::test]
async fn shutting_down_while_pending_stops_waiting_for_a_code() {
    let (url, core) = FakeCore { never_approve: true, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let satellite = builder(&url, &tokens).build().unwrap();
    let enroller = satellite.enroller();
    satellite.run_until(wait_for(|| core.polls.load(Ordering::SeqCst) >= 2)).await.unwrap();
    assert!(!enroller.is_pairing());
    assert!(matches!(enroller.enroll("K7QF2M9X", EnrollmentMethod::Provisioning), Err(Error::NotPairing)));
    assert_eq!(tokens.load(&url, "enceinte-salon"), None);
}

#[tokio::test]
async fn a_refused_token_is_forgotten_and_the_device_pairs_again() {
    let (url, core) = FakeCore::default().serve().await;
    let (_dir, tokens) = tokens();
    tokens.save(&url, "enceinte-salon", "nst2_from-another-life").unwrap();
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).display(outputs.clone()).build().unwrap();
    satellite.run_until(wait_for(|| core.offers().len() >= 3)).await.unwrap();

    let offers = core.offers();
    assert_eq!(offers[0], "nst2_from-another-life");
    assert!(offers[1..].iter().all(|t| t == "nst2_p1"), "then the new token, retried: {offers:?}");
    assert_eq!(outputs.count("show"), 1, "paired once");
    assert_eq!(tokens.load(&url, "enceinte-salon").as_deref(), Some("nst2_p1"));
}

#[derive(Clone, Default)]
struct CountingSource {
    opened: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
}

#[async_trait]
impl AudioSource for CountingSource {
    async fn start(&mut self) -> Result<(), AudioError> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn read(&mut self) -> Result<Vec<i16>, AudioError> {
        SilenceSource::new().read().await
    }

    async fn close(&mut self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn an_unreachable_core_is_retried_until_shutdown_and_audio_is_released() {
    let (_dir, tokens) = tokens();
    let source = CountingSource::default();
    let outputs = Outputs::default();
    let satellite = builder("http://127.0.0.1:9", &tokens)
        .source(source.clone())
        .display(outputs.clone())
        .backoff(Duration::from_millis(10), Duration::from_millis(50))
        .build()
        .unwrap();
    satellite.run_until(tokio::time::sleep(Duration::from_millis(300))).await.unwrap();
    let counts = (source.opened.load(Ordering::SeqCst), source.closed.load(Ordering::SeqCst));
    assert_eq!(counts, (1, 1), "opened once, released at shutdown");
    assert!(outputs.noted().is_empty(), "no code without a core");
}

struct BrokenMicrophone;

#[async_trait]
impl AudioSource for BrokenMicrophone {
    async fn start(&mut self) -> Result<(), AudioError> {
        Err(AudioError("no input device".into()))
    }

    async fn read(&mut self) -> Result<Vec<i16>, AudioError> {
        Err(AudioError("never started".into()))
    }
}

#[tokio::test]
async fn a_microphone_that_cannot_open_fails_clearly() {
    let (_dir, tokens) = tokens();
    let satellite = builder("http://127.0.0.1:9", &tokens).source(BrokenMicrophone).build().unwrap();
    assert!(matches!(satellite.run().await, Err(Error::Audio(e)) if e.0 == "no input device"));
}

async fn wait_for(condition: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

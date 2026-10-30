//! What the conformance suite does not see: pairing details, refusals and a core that is down,
//! against a fake core over HTTP.
#![allow(clippy::unwrap_used)] // test helpers fail loudly

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use nestor_satellite::pairing::pair;
use nestor_satellite::{
    AUDIO_CAPTURE, AUDIO_PLAYBACK, AudioError, AudioSource, Device, Error, Manifest, Satellite, SilenceSource,
    TokenStore,
};
use serde_json::{Value, json};

/// A core that hands out pairing tickets, approves each after `pending` polls, and answers offers
/// with `offer_status` (and 401 for any token it did not hand out).
#[derive(Default)]
struct FakeCore {
    tickets: AtomicUsize,
    polls: AtomicUsize,
    pending: usize,
    expire_first: bool,
    refuse_manifest: bool,
    manifests: Mutex<Vec<Value>>,
    offers: Mutex<Vec<String>>,
}

impl FakeCore {
    async fn serve(self) -> (String, Arc<Self>) {
        let core = Arc::new(self);
        let app = Router::new()
            .route("/satellites/pair", post(ask))
            .route("/satellites/pair/{id}", get(poll))
            .route("/satellites/{device}/offer", post(offer))
            .with_state(core.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, core)
    }

    fn offers(&self) -> Vec<String> {
        self.offers.lock().unwrap().clone()
    }
}

async fn ask(State(core): State<Arc<FakeCore>>, Json(manifest): Json<Value>) -> (StatusCode, Json<Value>) {
    core.manifests.lock().unwrap().push(manifest);
    if core.refuse_manifest {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"detail": "unsupported protocol"})));
    }
    let n = core.tickets.fetch_add(1, Ordering::SeqCst) + 1;
    let ticket =
        json!({"pairing_id": format!("p{n}"), "code": format!("00000{n}"), "expires_at": "2026-10-30T12:05:00+00:00"});
    (StatusCode::CREATED, Json(ticket))
}

async fn poll(State(core): State<Arc<FakeCore>>, Path(id): Path<String>) -> Json<Value> {
    if core.expire_first && id == "p1" {
        return Json(json!({"status": "expired"}));
    }
    if core.polls.fetch_add(1, Ordering::SeqCst) < core.pending {
        return Json(json!({"status": "pending"}));
    }
    Json(json!({"status": "approved", "token": format!("nst1_{id}"), "device_id": "enceinte-salon",
                "room": "salon", "capabilities": ["audio.capture"]}))
}

async fn offer(State(core): State<Arc<FakeCore>>, headers: HeaderMap, Json(body): Json<Value>) -> StatusCode {
    assert_eq!(body["type"], "offer");
    assert!(body["sdp"].as_str().unwrap().contains("m=application"), "the data channel is offered");
    let token = headers["authorization"].to_str().unwrap().trim_start_matches("Bearer ").to_owned();
    core.offers.lock().unwrap().push(token.clone());
    if token.starts_with("nst1_p") { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::UNAUTHORIZED }
}

fn manifest() -> Manifest {
    let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon").unwrap();
    Manifest::new(device, [AUDIO_CAPTURE, AUDIO_PLAYBACK]).unwrap()
}

fn recorder() -> (Arc<Mutex<Vec<String>>>, impl Fn(&str) + Send + Sync + 'static) {
    let codes = Arc::new(Mutex::new(Vec::new()));
    let shown = codes.clone();
    (codes, move |code: &str| shown.lock().unwrap().push(code.to_owned()))
}

#[tokio::test]
async fn pairing_shows_the_code_and_returns_the_token_once_approved() {
    let (url, core) = FakeCore { pending: 2, ..Default::default() }.serve().await;
    let (codes, on_code) = recorder();
    let token = pair(&reqwest::Client::new(), &url, &manifest(), &on_code, Duration::from_millis(10)).await.unwrap();
    assert_eq!(token, "nst1_p1");
    assert_eq!(*codes.lock().unwrap(), ["000001"]);
    assert_eq!(core.polls.load(Ordering::SeqCst), 3, "polled until approved");
    assert_eq!(core.manifests.lock().unwrap()[0], manifest().to_json(), "the manifest is sent as is");
}

#[tokio::test]
async fn an_expired_code_is_replaced_by_a_new_one() {
    let (url, _core) = FakeCore { expire_first: true, ..Default::default() }.serve().await;
    let (codes, on_code) = recorder();
    let token = pair(&reqwest::Client::new(), &url, &manifest(), &on_code, Duration::from_millis(10)).await.unwrap();
    assert_eq!(token, "nst1_p2");
    assert_eq!(*codes.lock().unwrap(), ["000001", "000002"]);
}

#[tokio::test]
async fn a_manifest_the_core_refuses_stops_the_satellite() {
    let (url, _core) = FakeCore { refuse_manifest: true, ..Default::default() }.serve().await;
    let dir = tempfile::tempdir().unwrap();
    let satellite = Satellite::builder(url, manifest(), TokenStore::new(dir.path().join("t.json"))).build();
    let outcome = tokio::time::timeout(Duration::from_secs(5), satellite.run()).await.unwrap();
    assert!(matches!(outcome, Err(Error::ManifestRefused(detail)) if detail == "unsupported protocol"));
}

#[tokio::test]
async fn a_refused_token_is_forgotten_and_the_device_pairs_again() {
    let (url, core) = FakeCore::default().serve().await;
    let dir = tempfile::tempdir().unwrap();
    let tokens = TokenStore::new(dir.path().join("t.json"));
    tokens.save(&url, "enceinte-salon", "nst1_from-another-life").unwrap();
    let (codes, on_code) = recorder();
    let satellite = Satellite::builder(url.clone(), manifest(), tokens.clone())
        .on_code(on_code)
        .poll_interval(Duration::from_millis(10))
        .backoff(Duration::from_millis(20), Duration::from_millis(40))
        .build();
    satellite.run_until(wait_for(|| core.offers().len() >= 3)).await.unwrap();

    let offers = core.offers();
    assert_eq!(offers[0], "nst1_from-another-life");
    assert!(offers[1..].iter().all(|t| t == "nst1_p1"), "then the new token, retried: {offers:?}");
    assert_eq!(*codes.lock().unwrap(), ["000001"], "paired once");
    assert_eq!(tokens.load(&url, "enceinte-salon").as_deref(), Some("nst1_p1"));
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
    let dir = tempfile::tempdir().unwrap();
    let source = CountingSource::default();
    let (codes, on_code) = recorder();
    let satellite = Satellite::builder("http://127.0.0.1:9", manifest(), TokenStore::new(dir.path().join("t.json")))
        .source(source.clone())
        .on_code(on_code)
        .backoff(Duration::from_millis(10), Duration::from_millis(50))
        .build();
    satellite.run_until(tokio::time::sleep(Duration::from_millis(300))).await.unwrap();
    let counts = (source.opened.load(Ordering::SeqCst), source.closed.load(Ordering::SeqCst));
    assert_eq!(counts, (1, 1), "opened once, released at shutdown");
    assert!(codes.lock().unwrap().is_empty());
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
    let dir = tempfile::tempdir().unwrap();
    let satellite = Satellite::builder("http://127.0.0.1:9", manifest(), TokenStore::new(dir.path().join("t.json")))
        .source(BrokenMicrophone)
        .build();
    assert!(matches!(satellite.run().await, Err(Error::Audio(e)) if e.0 == "no input device"));
}

async fn wait_for(condition: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

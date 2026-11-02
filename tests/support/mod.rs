//! A fake core over HTTP and WebSocket, and doubles that note what they are asked, shared by the
//! integration tests.
#![allow(dead_code, clippy::unwrap_used)] // each test file uses part of it; helpers fail loudly

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use nestor_satellite::{Body, Display, TextDisplay, TokenStore, Voice};
use serde_json::{Value, json};

/// What the fake core does with enrollment codes.
#[derive(Clone, Copy, Default, PartialEq)]
pub enum Codes {
    /// Accepted.
    #[default]
    Accept,
    /// Refused: already used.
    Refuse,
    /// The pairing no longer waits for a proof: the device code won.
    NotPending,
}

/// A core that hands out device codes, approves a pending pairing after `pending` polls (unless
/// `never_approve`), judges enrollment codes per `codes`, and holds links: it accepts the tokens it
/// handed out (`nst1_p…`) and closes the link with 4401 for any other, welcomes the device with
/// what it declared but `withheld`, sends `script`, and closes each link with the next code of
/// `closes` (if any) `close_after` the welcome. Audio offers are noted and answered 503 (401 when `audio_refused`).
#[derive(Default)]
pub struct FakeCore {
    pub pending: usize,
    pub never_approve: bool,
    pub identify: bool,
    pub codes: Codes,
    pub rate_limit_s: Option<u64>,
    pub unsupported: bool,
    pub withheld: Vec<&'static str>,
    pub script: Vec<Value>,
    pub closes: Mutex<VecDeque<u16>>,
    pub close_after: Duration,
    /// Audio offers are answered 401, as for a revoked token.
    pub audio_refused: bool,
    pub requests: Mutex<Vec<(Instant, Value)>>,
    pub enrollments: Mutex<Vec<Value>>,
    pub approved: Mutex<Vec<String>>,
    pub polls: AtomicUsize,
    /// Every `hello`, accepted or not.
    pub hellos: Mutex<Vec<Value>>,
    /// Every message after an accepted `hello`.
    pub received: Mutex<Vec<Value>>,
    /// The tokens of the audio offers.
    pub offers: Mutex<Vec<String>>,
}

impl FakeCore {
    pub async fn serve(self) -> (String, Arc<Self>) {
        let core = Arc::new(self);
        let app = Router::new()
            .route("/satellites/pairings", post(request))
            .route("/satellites/pairings/{id}", get(poll))
            .route("/satellites/pairings/{id}/enrollment", post(enroll))
            .route("/satellites/{device}/link", any(link))
            .route("/satellites/{device}/audio", post(offer))
            .with_state(core.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, core)
    }

    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().iter().map(|(_, body)| body.clone()).collect()
    }

    pub fn coded_requests(&self) -> usize {
        self.requests().iter().filter(|r| r.get("enrollment_code").is_some()).count()
    }

    pub fn enrollments(&self) -> Vec<Value> {
        self.enrollments.lock().unwrap().clone()
    }

    /// The token of each `hello`.
    pub fn linked(&self) -> Vec<String> {
        self.hellos.lock().unwrap().iter().map(|h| h["payload"]["token"].as_str().unwrap().to_owned()).collect()
    }

    pub fn received(&self) -> Vec<Value> {
        self.received.lock().unwrap().clone()
    }

    pub fn received_of(&self, message_type: &str) -> Vec<Value> {
        self.received().into_iter().filter(|m| m["type"] == message_type).collect()
    }

    pub fn offers(&self) -> Vec<String> {
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
        if core.linked().contains(&format!("nst1_{id}")) {
            return Json(json!({"status": "claimed"}));
        }
        return Json(json!({"status": "approved", "token": format!("nst1_{id}"), "device_id": "enceinte-salon",
                           "room": "salon", "capabilities": ["audio.capture"]}));
    }
    if core.identify && polls == 0 {
        return Json(json!({"status": "pending", "identify": true}));
    }
    Json(json!({"status": "pending"}))
}

async fn offer(State(core): State<Arc<FakeCore>>, headers: HeaderMap, Json(body): Json<Value>) -> StatusCode {
    assert_eq!(body["type"], "offer");
    assert!(body["sdp"].as_str().unwrap().contains("m=audio"), "audio is offered");
    assert!(!body["sdp"].as_str().unwrap().contains("m=application"), "no data channel");
    let token = headers["authorization"].to_str().unwrap().trim_start_matches("Bearer ").to_owned();
    core.offers.lock().unwrap().push(token);
    if core.audio_refused { StatusCode::UNAUTHORIZED } else { StatusCode::SERVICE_UNAVAILABLE }
}

async fn link(State(core): State<Arc<FakeCore>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| linked(core, socket))
}

pub fn envelope(message_type: &str, id: &str, payload: Value) -> Value {
    json!({"type": message_type, "id": id, "at": "2026-10-30T12:00:00+00:00", "payload": payload})
}

async fn send(socket: &mut WebSocket, message: &Value) {
    let _ = socket.send(Message::Text(message.to_string().into())).await;
}

async fn close(socket: &mut WebSocket, code: u16) {
    let _ = socket.send(Message::Close(Some(CloseFrame { code, reason: "".into() }))).await;
    // The device answers the close, then leaves.
    let _ = tokio::time::timeout(Duration::from_secs(2), async { while socket.recv().await.is_some() {} }).await;
}

async fn linked(core: Arc<FakeCore>, mut socket: WebSocket) {
    let Some(Ok(Message::Text(hello))) = socket.recv().await else {
        return;
    };
    let hello: Value = serde_json::from_str(&hello).unwrap();
    core.hellos.lock().unwrap().push(hello.clone());
    let token = hello["payload"]["token"].as_str().unwrap_or_default();
    if hello["type"] != "hello" || !token.starts_with("nst1_p") {
        close(&mut socket, 4401).await;
        return;
    }
    let manifest = &hello["payload"]["manifest"];
    let allowed: BTreeSet<&str> = manifest["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .filter(|name| !core.withheld.contains(name))
        .collect();
    let welcome = json!({"device_id": manifest["device"]["id"], "room": "salon", "capabilities": allowed});
    send(&mut socket, &envelope("welcome", "w-1", welcome)).await;
    for message in &core.script {
        send(&mut socket, message).await;
    }
    let closing = core.closes.lock().unwrap().pop_front();
    let deadline = tokio::time::sleep(core.close_after);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            message = socket.recv() => match message {
                Some(Ok(Message::Text(text))) => core.received.lock().unwrap().push(serde_json::from_str(&text).unwrap()),
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
            () = &mut deadline, if closing.is_some() => {
                close(&mut socket, closing.unwrap_or_default()).await;
                return;
            }
        }
    }
}

/// A display, a voice, a text display and a body that note what they are asked to do.
#[derive(Clone, Default)]
pub struct Outputs(Arc<Mutex<Vec<String>>>);

impl Outputs {
    pub fn noted(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    pub fn note(&self, what: String) {
        self.0.lock().unwrap().push(what);
    }

    pub fn count(&self, prefix: &str) -> usize {
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

impl TextDisplay for Outputs {
    fn show_text(&self, text: &str, last: bool) {
        self.note(format!("text {text} {last}"));
    }
}

impl Body for Outputs {
    fn identify(&self) {
        self.note("identify".into());
    }

    fn behavior(&self, behavior: nestor_satellite::Behavior) {
        self.note(format!("behavior {behavior}"));
    }

    fn connected(&self, up: bool) {
        self.note(format!("connected {up}"));
    }
}

pub fn tokens() -> (tempfile::TempDir, TokenStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::new(dir.path().join("token.json"));
    (dir, store)
}

pub async fn wait_for(condition: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

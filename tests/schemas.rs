//! What the SDK sends and reads, against its copy of the protocol schemas.
#![allow(clippy::unwrap_used)] // test helpers fail loudly

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use jsonschema::{Registry, Validator};
use nestor_satellite::protocol::{IDENTIFY, PERSON_DETECTION, SPEECH_ACTIVITY};
use nestor_satellite::wire;
use nestor_satellite::{
    Behavior, Capability, Device, Enrollment, EnrollmentMethod, Expression, Language, Manifest, PairingMethod,
    Satellite, TokenStore,
};
use serde_json::{Value, json};

fn schemas_in(dir: &Path) -> BTreeMap<String, Value> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, serde_json::from_slice(&fs::read(&path).unwrap()).unwrap())
        })
        .collect()
}

fn vendored() -> BTreeMap<String, Value> {
    schemas_in(&Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas"))
}

fn validator(name: &str) -> Validator {
    let schemas = vendored();
    let mut registry = Registry::new();
    for schema in schemas.values() {
        registry = registry.add(schema["$id"].as_str().unwrap(), schema.clone()).unwrap();
    }
    let registry = registry.prepare().unwrap();
    jsonschema::options().should_validate_formats(true).with_registry(&registry).build(&schemas[name]).unwrap()
}

#[test]
fn the_sdk_knows_every_value_the_core_may_send() {
    let schemas = vendored();
    let enum_of = |name: &str, field: &str| -> Vec<String> {
        schemas[name]["properties"]["payload"]["properties"][field]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    };
    let behaviors: Vec<String> = Behavior::ALL.iter().map(|b| b.as_str().to_owned()).collect();
    let expressions: Vec<String> = Expression::ALL.iter().map(|e| e.as_str().to_owned()).collect();
    assert_eq!(behaviors, enum_of("intent.behavior.json", "behavior"));
    assert_eq!(expressions, enum_of("intent.expression.json", "expression"));
    let to_satellite: Vec<&str> = schemas
        .values()
        .filter(|s| s["x-direction"] == "to_satellite")
        .map(|s| s["properties"]["type"]["const"].as_str().unwrap())
        .collect();
    assert_eq!(
        to_satellite,
        [
            wire::INTENT_BEAT,
            wire::INTENT_BEHAVIOR,
            wire::INTENT_EXPRESSION,
            wire::INTENT_IDENTIFY,
            wire::INTENT_TEXT,
            wire::INTENT_UPDATE,
            wire::WELCOME
        ]
    );
}

#[test]
fn every_pairing_method_but_the_qr_code_can_be_declared_and_the_qr_code_never() {
    let schemas = vendored();
    let protocol = &schemas["manifest.json"]["properties"]["pairing_methods"]["items"]["enum"];
    let mut declarable: Vec<&str> = PairingMethod::ALL.iter().map(|m| m.as_str()).collect();
    declarable.insert(4, "enrollment.qr"); // needs a camera: this SDK drives none
    assert_eq!(protocol, &json!(declarable), "the SDK follows the protocol's methods");
    let qr = json!("enrollment.qr");
    assert!(serde_json::from_value::<PairingMethod>(qr.clone()).is_err(), "not a method of this SDK");
    assert!(serde_json::from_value::<EnrollmentMethod>(qr).is_err(), "no way to hand in a scanned code");
}

#[test]
fn what_the_sdk_sends_follows_the_schemas() {
    let capture = Capability::new("audio.capture").unwrap().with_param("formats", json!(["opus/48000"])).unwrap();
    let detector = Capability::new(PERSON_DETECTION).unwrap().with_param("rate_hz", 10).unwrap();
    let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon").unwrap().with_brand("raspberry").unwrap();
    let identify = Capability::new(IDENTIFY).unwrap();
    let manifest = Manifest::with_capabilities(device, vec![capture, detector, identify])
        .unwrap()
        .with_pairing_methods(PairingMethod::ALL)
        .unwrap();
    validator("manifest.json").validate(&manifest.to_json()).unwrap();
    let sent = [
        wire::hello(&manifest, "nst1_x"),
        wire::ack("intent-1"),
        wire::input_text("Allume la lumière du salon").unwrap(),
        wire::input_text(&"é".repeat(4_000)).unwrap(),
        wire::person_detected(2, 0.75).unwrap(),
        wire::person_detected(0, 1.0).unwrap(),
        wire::speech_activity(true),
        wire::update_status(wire::UpdateState::Current, None).unwrap(),
        wire::update_status(wire::UpdateState::Available, Some("0.6.0")).unwrap(),
        wire::update_status(wire::UpdateState::Installing, Some("0.6.0")).unwrap(),
        wire::update_status(wire::UpdateState::Failed, Some("0.6.0")).unwrap(),
    ];
    for message in sent {
        let name = format!("{}.json", message["type"].as_str().unwrap());
        assert_eq!(
            validator(&name).iter_errors(&message).map(|e| e.to_string()).collect::<Vec<_>>(),
            Vec::<String>::new(),
            "{message}"
        );
    }
}

fn from_core() -> Vec<Value> {
    let at = "2026-10-30T12:00:00.123456+00:00";
    vec![
        json!({"type": "welcome", "id": "m-1", "at": at,
               "payload": {"device_id": "enceinte-salon", "room": "salon", "capabilities": ["audio.capture"]}}),
        json!({"type": "intent.behavior", "id": "m-2", "at": at, "payload": {"behavior": "thinking"}}),
        json!({"type": "intent.expression", "id": "m-3", "at": at, "payload": {"expression": "sigh", "intensity": 0.4}}),
        json!({"type": "intent.expression", "id": "m-4", "at": at, "payload": {"expression": "joy"}}),
        json!({"type": "intent.beat", "id": "m-5", "at": at, "payload": {}}),
        json!({"type": "intent.identify", "id": "m-6", "at": at, "payload": {}}),
        json!({"type": "intent.text", "id": "m-7", "at": at, "payload": {"text": "Je regarde.", "final": false}}),
        json!({"type": "intent.text", "id": "m-8", "at": at, "payload": {"text": "Il fait 21 °C."}}),
        json!({"type": "intent.update", "id": "m-9", "at": at, "payload": {"version": "0.6.0"}}),
    ]
}

#[test]
fn the_sdk_reads_what_the_schemas_allow_the_core_to_send() {
    for message in from_core() {
        let name = format!("{}.json", message["type"].as_str().unwrap());
        assert!(validator(&name).is_valid(&message), "{message}");
        assert!(wire::decode(message.clone()).is_ok(), "{message}");
    }
}

#[test]
fn the_sdk_refuses_what_the_schemas_refuse() {
    type Breakage = (&'static str, fn(&mut Value));
    let breakages: [Breakage; 6] = [
        ("an extra envelope key", |m| m["extra"] = json!(1)),
        ("an empty id", |m| m["id"] = json!("")),
        ("an id too long", |m| m["id"] = json!("x".repeat(65))),
        ("a time that is no time", |m| m["at"] = json!("yesterday")),
        ("an unknown payload field", |m| m["payload"]["extra"] = json!(1)),
        ("a payload that is no object", |m| m["payload"] = json!([])),
    ];
    for original in from_core() {
        let name = format!("{}.json", original["type"].as_str().unwrap());
        for (what, breakage) in breakages {
            let mut message = original.clone();
            breakage(&mut message);
            assert!(!validator(&name).is_valid(&message), "{what}: the schema accepts {message}");
            assert!(wire::decode(message.clone()).is_err(), "{what}: the SDK accepts {message}");
        }
    }
}

/// A core that refuses the first enrollment code, hands out a device code, accepts the enrollment
/// code handed in next and approves; it keeps every body the satellite sends.
#[derive(Default)]
struct Recording {
    requests: Mutex<Vec<Value>>,
    enrollments: Mutex<Vec<Value>>,
}

async fn request(State(core): State<Arc<Recording>>, Json(body): Json<Value>) -> Response {
    let mut requests = core.requests.lock().unwrap();
    requests.push(body);
    if requests.len() == 1 {
        let refusal =
            json!({"error": "enrollment_refused", "detail": "this enrollment code pairs nothing", "reason": "unknown"});
        return (StatusCode::FORBIDDEN, Json(refusal)).into_response();
    }
    let ticket = json!({"pairing_id": "p1", "status": "pending", "expires_at": "2026-10-30T12:10:00+00:00", "device_code": "042917"});
    (StatusCode::CREATED, Json(ticket)).into_response()
}

async fn enroll(State(core): State<Arc<Recording>>, Json(body): Json<Value>) -> Json<Value> {
    core.enrollments.lock().unwrap().push(body);
    Json(json!({"status": "approved", "device_id": "enceinte-salon", "room": "salon", "capabilities": []}))
}

async fn poll(State(core): State<Arc<Recording>>) -> Json<Value> {
    if core.enrollments.lock().unwrap().is_empty() {
        return Json(json!({"status": "pending", "identify": true}));
    }
    Json(json!({"status": "approved", "token": "nst1_x", "device_id": "enceinte-salon", "room": "salon",
                "capabilities": ["device.identify"]}))
}

#[tokio::test]
async fn the_pairing_bodies_the_sdk_sends_follow_the_schemas() {
    let core = Arc::new(Recording::default());
    let app = Router::new()
        .route("/satellites/pairings", post(request))
        .route("/satellites/pairings/{id}/enrollment", post(enroll))
        .route("/satellites/pairings/{id}", get(poll))
        .with_state(core.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon").unwrap();
    let manifest = Manifest::new(device, [IDENTIFY]).unwrap();
    let tokens = TokenStore::new(dir.path().join("t.json"));
    let satellite = Satellite::builder(&url, manifest, tokens.clone())
        .language(Language::En)
        .enrollment(Enrollment::parse("K7QF-2M9X").unwrap())
        .accepts_enrollment(EnrollmentMethod::Input)
        .voice(|_: &str| {})
        .poll_interval(Duration::from_millis(10))
        .build()
        .unwrap();
    let enroller = satellite.enroller();
    let scanning = async {
        while core.requests.lock().unwrap().len() < 2 || !enroller.is_pairing() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        enroller.enroll("ABCD-EFGH", EnrollmentMethod::Input).unwrap();
        while tokens.load(&url, "enceinte-salon").is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), satellite.run_until(scanning)).await.unwrap().unwrap();

    let requests = core.requests.lock().unwrap().clone();
    let enrollments = core.enrollments.lock().unwrap().clone();
    assert_eq!((requests.len(), enrollments.len()), (2, 1));
    for (name, body) in requests
        .iter()
        .map(|b| ("pairing.request.json", b))
        .chain(enrollments.iter().map(|b| ("pairing.enrollment.json", b)))
    {
        assert_eq!(
            validator(name).iter_errors(body).map(|e| e.to_string()).collect::<Vec<_>>(),
            Vec::<String>::new(),
            "{body}"
        );
    }
    assert_eq!(enrollments[0], json!({"enrollment_code": "ABCDEFGH", "method": "enrollment.input"}));
}

#[test]
fn the_pairing_answers_the_sdk_reads_follow_the_schemas() {
    let answers = [
        (
            "pairing.ticket.json",
            json!({"pairing_id": "p1", "status": "pending", "expires_at": "2026-10-30T12:10:00+00:00", "device_code": "042917"}),
        ),
        (
            "pairing.ticket.json",
            json!({"pairing_id": "p1", "status": "approved", "expires_at": "2026-10-30T12:10:00+00:00"}),
        ),
        ("pairing.state.json", json!({"status": "pending", "identify": true})),
        (
            "pairing.state.json",
            json!({"status": "approved", "token": "nst1_x", "device_id": "a", "room": "salon", "capabilities": []}),
        ),
        ("pairing.state.json", json!({"status": "claimed"})),
        ("pairing.error.json", json!({"error": "enrollment_refused", "detail": "d", "reason": "used"})),
        ("pairing.error.json", json!({"error": "too_many_attempts", "detail": "d", "retry_after_s": 2})),
        ("pairing.error.json", json!({"error": "not_pending", "detail": "d"})),
        ("pairing.error.json", json!({"error": "protocol_unsupported", "detail": "d"})),
    ];
    for (name, answer) in answers {
        assert!(validator(name).is_valid(&answer), "{name}: {answer}");
    }
}

#[tokio::test]
async fn what_the_sdk_sends_on_its_link_follows_the_schemas() {
    let script = vec![
        support::envelope("intent.behavior", "i-1", json!({"behavior": "listening"})),
        support::envelope("intent.text", "i-2", json!({"text": "Bonjour.", "final": true})),
    ];
    let (url, core) = support::FakeCore { script, ..Default::default() }.serve().await;
    let (_dir, tokens) = support::tokens();
    tokens.save(&url, "enceinte-salon", "nst1_p0").unwrap();
    let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon").unwrap();
    let manifest = Manifest::new(device, [IDENTIFY, PERSON_DETECTION, SPEECH_ACTIVITY]).unwrap();
    let satellite = Satellite::builder(&url, manifest, tokens)
        .language(Language::En)
        .text_display(|_: &str, _: bool| {})
        .text_input()
        .build()
        .unwrap();
    let observer = satellite.observer();
    let talking = async {
        support::wait_for(|| observer.is_connected()).await;
        observer.input_text("Bonsoir").unwrap();
        observer.person_detected(1, 0.9).unwrap();
        observer.speech_activity(false).unwrap();
        support::wait_for(|| core.received().len() == 5).await;
    };
    tokio::time::timeout(Duration::from_secs(10), satellite.run_until(talking)).await.unwrap().unwrap();

    let sent: Vec<Value> = core.hellos.lock().unwrap().iter().cloned().chain(core.received()).collect();
    let mut types: Vec<&str> = sent.iter().map(|m| m["type"].as_str().unwrap()).collect();
    assert_eq!(types[0], "hello");
    types.sort_unstable();
    assert_eq!(
        types,
        ["ack", "ack", "hello", "input.text", "observation.person.detected", "observation.speech.activity"]
    );
    for message in &sent {
        let name = format!("{}.json", message["type"].as_str().unwrap());
        assert_eq!(
            validator(&name).iter_errors(message).map(|e| e.to_string()).collect::<Vec<_>>(),
            Vec::<String>::new(),
            "{message}"
        );
    }
}

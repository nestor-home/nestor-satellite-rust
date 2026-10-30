//! The SDK's copy of the protocol schemas, and what the SDK sends and reads, against them.
//!
//! When `the_vendored_schemas_are_the_servers` fails after a server change, refresh `schemas/`
//! (see AGENTS.md) and adapt the SDK until every test passes.
#![allow(clippy::unwrap_used)] // test helpers fail loudly

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use jsonschema::{Registry, Validator};
use nestor_satellite::protocol::{AUDIO_CAPTURE, PERSON_DETECTION};
use nestor_satellite::wire::{self, Inbound};
use nestor_satellite::{Behavior, Capability, Device, Expression, Manifest};
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

fn server_copy() -> PathBuf {
    let from_env = std::env::var_os("NESTOR_REPO").map(PathBuf::from);
    from_env
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../nestor"))
        .join("src/nestor/modules/satellites/schemas")
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
fn the_vendored_schemas_are_the_servers() {
    let server = server_copy();
    if !server.is_dir() {
        eprintln!("skipped: no Nestor server at {} (set NESTOR_REPO)", server.display());
        return;
    }
    assert_eq!(schemas_in(&server), vendored(), "refresh schemas/ from the server (AGENTS.md)");
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
    assert_eq!(to_satellite, [wire::INTENT_BEAT, wire::INTENT_BEHAVIOR, wire::INTENT_EXPRESSION, wire::WELCOME]);
}

#[test]
fn what_the_sdk_sends_follows_the_schemas() {
    let capture = Capability::new(AUDIO_CAPTURE).unwrap().with_param("formats", json!(["opus/48000"])).unwrap();
    let detector = Capability::new(PERSON_DETECTION).unwrap().with_param("rate_hz", 10).unwrap();
    let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon").unwrap().with_brand("raspberry").unwrap();
    let manifest = Manifest::with_capabilities(device, vec![capture, detector]).unwrap();
    validator("manifest.json").validate(&manifest.to_json()).unwrap();
    let sent = [
        wire::hello(&manifest),
        wire::ack("intent-1"),
        wire::person_detected(2, 0.75).unwrap(),
        wire::person_detected(0, 1.0).unwrap(),
        wire::speech_activity(true),
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
    ]
}

#[test]
fn the_sdk_reads_what_the_schemas_allow_the_core_to_send() {
    for message in from_core() {
        let name = format!("{}.json", message["type"].as_str().unwrap());
        assert!(validator(&name).is_valid(&message), "{message}");
        assert!(matches!(wire::decode(message.clone()), Ok(Inbound::Nestor(_))), "{message}");
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

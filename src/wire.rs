//! Messages on the link: one JSON envelope `{"type", "id", "at", "payload"}` per WebSocket text frame.
//!
//! The satellite sends `hello` first, then `ack`s, observations, typed text and `update.status`; the
//! core sends `welcome` and `intent.*`. Decoding is as strict as the server's: exactly the four envelope keys,
//! an id of 1 to 64 characters, an ISO 8601 time, and a payload without unknown fields. A message
//! *type* this SDK does not know is not an error, though ([`Message::Unknown`]): a newer core does
//! not break an older driver.

use std::collections::BTreeSet;

use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::protocol::{Behavior, Expression, Manifest, ProtocolError};

/// `hello`: the first message of a link, the device's manifest and its token.
pub const HELLO: &str = "hello";
/// `ack`: an intent was received.
pub const ACK: &str = "ack";
/// `input.text`: what a person typed on the device.
pub const INPUT_TEXT: &str = "input.text";
/// `observation.person.detected`: people seen by the device's own detector.
pub const PERSON_DETECTED: &str = "observation.person.detected";
/// `observation.speech.activity`: voice activity started or stopped.
pub const SPEECH_ACTIVITY: &str = "observation.speech.activity";
/// `update.status`: where the device's own software stands.
pub const UPDATE_STATUS: &str = "update.status";
/// `welcome`: the core's answer to `hello`.
pub const WELCOME: &str = "welcome";
/// `intent.behavior`: the assistant's attitude changed.
pub const INTENT_BEHAVIOR: &str = "intent.behavior";
/// `intent.expression`: an expression to perform now.
pub const INTENT_EXPRESSION: &str = "intent.expression";
/// `intent.beat`: a word is being played.
pub const INTENT_BEAT: &str = "intent.beat";
/// `intent.identify`: a person asks the device to make itself noticed.
pub const INTENT_IDENTIFY: &str = "intent.identify";
/// `intent.text`: the assistant's words, to show.
pub const INTENT_TEXT: &str = "intent.text";
/// `intent.update`: a person asks the device to install a newer version of its software.
pub const INTENT_UPDATE: &str = "intent.update";

/// At most this many characters typed in one `input.text`.
pub const MAX_INPUT_TEXT: usize = 4_000;
/// At most this many characters in one `intent.text`.
pub const MAX_INTENT_TEXT: usize = 16_000;

const MAX_ID: usize = 64;
const MAX_VERSION: usize = 64;

/// Where the device's own software stands (`update.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateState {
    /// Nothing newer was found.
    Current,
    /// A newer version can be installed.
    Available,
    /// It installs a version, then starts again on it.
    Installing,
    /// A version did not install or did not start: it went back to the one it runs.
    Failed,
}

/// A message in its envelope, with a fresh id and the current time.
pub fn envelope(message_type: &str, payload: Value) -> Value {
    json!({
        "type": message_type,
        "id": uuid::Uuid::new_v4().simple().to_string(),
        "at": Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false),
        "payload": payload,
    })
}

/// `hello {manifest, token}`.
pub fn hello(manifest: &Manifest, token: &str) -> Value {
    envelope(HELLO, json!({ "manifest": manifest.to_json(), "token": token }))
}

/// `ack {of}`: the intent `intent_id` was received.
pub fn ack(intent_id: &str) -> Value {
    envelope(ACK, json!({ "of": intent_id }))
}

/// `input.text {text}`: 1 to 4,000 characters, as typed.
pub fn input_text(text: &str) -> Result<Value, ProtocolError> {
    let length = text.chars().count();
    if !(1..=MAX_INPUT_TEXT).contains(&length) {
        return Err(ProtocolError::new(format!("typed text has 1 to {MAX_INPUT_TEXT} characters, not {length}")));
    }
    Ok(envelope(INPUT_TEXT, json!({ "text": text })))
}

/// `observation.person.detected {count, confidence}`; `confidence` is between 0 and 1.
pub fn person_detected(count: u32, confidence: f64) -> Result<Value, ProtocolError> {
    Ok(envelope(PERSON_DETECTED, json!({ "count": count, "confidence": unit(confidence)? })))
}

/// `observation.speech.activity {active}`.
pub fn speech_activity(active: bool) -> Value {
    envelope(SPEECH_ACTIVITY, json!({ "active": active }))
}

/// `update.status {state, version}`: `version` (1 to 64 characters) goes with every state but
/// [`UpdateState::Current`], and only with them.
pub fn update_status(state: UpdateState, version: Option<&str>) -> Result<Value, ProtocolError> {
    match (state, version) {
        (UpdateState::Current, None) => Ok(envelope(UPDATE_STATUS, json!({ "state": state }))),
        (UpdateState::Current, Some(_)) | (_, None) => {
            Err(ProtocolError::new("update.status: a version goes with every state but current, and only with them"))
        }
        (_, Some(version)) => {
            Ok(envelope(UPDATE_STATUS, json!({ "state": state, "version": checked_version(version)? })))
        }
    }
}

fn checked_version(version: &str) -> Result<&str, ProtocolError> {
    let length = version.chars().count();
    if (1..=MAX_VERSION).contains(&length) {
        Ok(version)
    } else {
        Err(ProtocolError::new(format!("a version has 1 to {MAX_VERSION} characters, not {length}")))
    }
}

/// The core's answer to `hello`: where the device is and what it was allowed to do.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Welcome {
    /// The paired device id.
    pub device_id: String,
    /// The paired room.
    pub room: String,
    /// What the household allowed, among what was declared.
    pub capabilities: BTreeSet<String>,
}

/// An intent of the core, to carry out and acknowledge.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// The assistant's attitude changed: show it until the next one.
    Behavior(Behavior),
    /// An expression, sent as the matching audio is played.
    Expression {
        /// Which one.
        expression: Expression,
        /// How strong, between 0 and 1.
        intensity: f64,
    },
    /// A word is being played: a light speech accent.
    Beat,
    /// A person asks the device to make itself noticed for a few seconds.
    Identify,
    /// The assistant's words, to show (plain text, no Markdown).
    Text {
        /// What it says.
        text: String,
        /// The last message of its turn (`final`).
        last: bool,
    },
    /// A person asks the device to install `version` of its software, the newer one it reported,
    /// and start again on it.
    Update {
        /// The version to install.
        version: String,
    },
}

/// A message from the core.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// `welcome`.
    Welcome(Welcome),
    /// `intent.*`.
    Intent(Intent),
    /// A message type this SDK does not know (from a newer core): ignore it.
    Unknown(String),
}

/// A message from the core, with its envelope's id and time.
#[derive(Debug, Clone, PartialEq)]
pub struct Received {
    /// The message id, which an ack refers to.
    pub id: String,
    /// When the core sent it.
    pub at: DateTime<Utc>,
    /// The message.
    pub message: Message,
}

/// Read a message from the core (the text of one WebSocket frame).
pub fn parse(text: &str) -> Result<Received, ProtocolError> {
    let data: Value = serde_json::from_str(text).map_err(|error| ProtocolError::new(format!("not JSON: {error}")))?;
    decode(data)
}

/// Read a message from the core, already parsed as JSON.
pub fn decode(data: Value) -> Result<Received, ProtocolError> {
    let Value::Object(mut data) = data else {
        return Err(ProtocolError::new("a message is a JSON object"));
    };
    let keys: BTreeSet<&str> = data.keys().map(String::as_str).collect();
    if keys != BTreeSet::from(["type", "id", "at", "payload"]) {
        return Err(ProtocolError::new("a message is an object with exactly type, id, at and payload"));
    }
    let message_type = match data.remove("type") {
        Some(Value::String(t)) => t,
        other => {
            return Err(ProtocolError::new(format!("message type {other:?} is not a string")));
        }
    };
    let id = match data.remove("id") {
        Some(Value::String(id)) if !id.is_empty() && id.chars().count() <= MAX_ID => id,
        _ => {
            return Err(ProtocolError::new("message id must be a non-empty string of at most 64 characters"));
        }
    };
    let at = match data.remove("at") {
        Some(Value::String(at)) => parse_time(&at)?,
        other => {
            return Err(ProtocolError::new(format!("message time {other:?} is not an ISO 8601 date-time")));
        }
    };
    let payload = match data.remove("payload") {
        Some(Value::Object(payload)) => payload,
        _ => {
            return Err(ProtocolError::new(format!("{message_type}: the payload must be an object")));
        }
    };
    let message = match message_type.as_str() {
        WELCOME => Message::Welcome(payload_of(&message_type, payload)?),
        INTENT_BEHAVIOR => {
            let p: BehaviorPayload = payload_of(&message_type, payload)?;
            Message::Intent(Intent::Behavior(p.behavior))
        }
        INTENT_EXPRESSION => {
            let p: ExpressionPayload = payload_of(&message_type, payload)?;
            let intensity = unit(p.intensity.unwrap_or(1.0))
                .map_err(|e| ProtocolError::new(format!("{message_type}.intensity: {e}")))?;
            Message::Intent(Intent::Expression { expression: p.expression, intensity })
        }
        INTENT_BEAT => {
            let _: EmptyPayload = payload_of(&message_type, payload)?;
            Message::Intent(Intent::Beat)
        }
        INTENT_IDENTIFY => {
            let _: EmptyPayload = payload_of(&message_type, payload)?;
            Message::Intent(Intent::Identify)
        }
        INTENT_TEXT => {
            let p: TextPayload = payload_of(&message_type, payload)?;
            let length = p.text.chars().count();
            if !(1..=MAX_INTENT_TEXT).contains(&length) {
                return Err(ProtocolError::new(format!(
                    "{message_type}.text has 1 to {MAX_INTENT_TEXT} characters, not {length}"
                )));
            }
            Message::Intent(Intent::Text { text: p.text, last: p.r#final.unwrap_or(true) })
        }
        INTENT_UPDATE => {
            let p: UpdatePayload = payload_of(&message_type, payload)?;
            checked_version(&p.version).map_err(|e| ProtocolError::new(format!("{message_type}.version: {e}")))?;
            Message::Intent(Intent::Update { version: p.version })
        }
        _ => Message::Unknown(message_type),
    };
    Ok(Received { id, at, message })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BehaviorPayload {
    behavior: Behavior,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpressionPayload {
    expression: Expression,
    intensity: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyPayload {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextPayload {
    text: String,
    r#final: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdatePayload {
    version: String,
}

fn payload_of<T: DeserializeOwned>(message_type: &str, payload: Map<String, Value>) -> Result<T, ProtocolError> {
    serde_json::from_value(Value::Object(payload))
        .map_err(|error| ProtocolError::new(format!("{message_type}: {error}")))
}

/// An ISO 8601 date-time, as Python's `datetime.fromisoformat` writes it; without an offset, UTC.
fn parse_time(text: &str) -> Result<DateTime<Utc>, ProtocolError> {
    if let Ok(at) = DateTime::parse_from_rfc3339(text) {
        return Ok(at.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
        .map(|naive| naive.and_utc())
        .map_err(|_| ProtocolError::new(format!("message time {text:?} is not an ISO 8601 date-time")))
}

fn unit(value: f64) -> Result<f64, ProtocolError> {
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(ProtocolError::new(format!("expected a number between 0 and 1, not {value}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(message_type: &str, payload: Value) -> String {
        json!({"type": message_type, "id": "m-1", "at": "2026-10-30T12:00:00.123456+00:00", "payload": payload})
            .to_string()
    }

    fn nestor(text: &str) -> Received {
        parse(text).unwrap()
    }

    #[test]
    fn the_core_s_messages_are_read() {
        let welcome =
            nestor(&message(WELCOME, json!({"device_id": "a", "room": "salon", "capabilities": ["audio.capture"]})));
        assert_eq!(welcome.id, "m-1");
        assert_eq!(
            welcome.message,
            Message::Welcome(Welcome {
                device_id: "a".into(),
                room: "salon".into(),
                capabilities: BTreeSet::from(["audio.capture".to_owned()]),
            })
        );
        assert_eq!(
            nestor(&message(INTENT_BEHAVIOR, json!({"behavior": "thinking"}))).message,
            Message::Intent(Intent::Behavior(Behavior::Thinking))
        );
        assert_eq!(
            nestor(&message(INTENT_EXPRESSION, json!({"expression": "sigh", "intensity": 0.4}))).message,
            Message::Intent(Intent::Expression { expression: Expression::Sigh, intensity: 0.4 })
        );
        assert_eq!(
            nestor(&message(INTENT_EXPRESSION, json!({"expression": "joy"}))).message,
            Message::Intent(Intent::Expression { expression: Expression::Joy, intensity: 1.0 })
        );
        assert_eq!(nestor(&message(INTENT_BEAT, json!({}))).message, Message::Intent(Intent::Beat));
        assert_eq!(nestor(&message(INTENT_IDENTIFY, json!({}))).message, Message::Intent(Intent::Identify));
        assert_eq!(
            nestor(&message(INTENT_TEXT, json!({"text": "Je regarde.", "final": false}))).message,
            Message::Intent(Intent::Text { text: "Je regarde.".into(), last: false })
        );
        assert_eq!(
            nestor(&message(INTENT_UPDATE, json!({"version": "0.6.0"}))).message,
            Message::Intent(Intent::Update { version: "0.6.0".into() })
        );
        assert_eq!(
            nestor(&message(INTENT_TEXT, json!({"text": "Il fait 21 °C."}))).message,
            Message::Intent(Intent::Text { text: "Il fait 21 °C.".into(), last: true }),
            "an unprompted message is the last of its turn"
        );
    }

    #[test]
    fn a_message_type_from_a_newer_core_is_not_an_error() {
        assert_eq!(
            nestor(&message("intent.dance", json!({"style": "waltz"}))).message,
            Message::Unknown("intent.dance".into())
        );
    }

    #[test]
    fn off_contract_messages_are_refused_like_the_server_does() {
        let wrong = [
            "[]".to_owned(),
            "not json".to_owned(),
            message(INTENT_BEHAVIOR, json!({"behavior": "dancing"})),
            message(INTENT_BEHAVIOR, json!({"behavior": "idle", "extra": 1})),
            message(INTENT_BEHAVIOR, json!({})),
            message(INTENT_EXPRESSION, json!({"expression": "joy", "intensity": 1.5})),
            message(INTENT_EXPRESSION, json!({"expression": "joy", "intensity": true})),
            message(INTENT_BEAT, json!({"x": 1})),
            message(INTENT_IDENTIFY, json!({"seconds": 5})),
            message(WELCOME, json!({"device_id": "a", "room": "b"})),
            message(INTENT_TEXT, json!({"text": ""})),
            message(INTENT_TEXT, json!({"text": "é".repeat(16_001)})),
            message(INTENT_TEXT, json!({"text": "ok", "final": "yes"})),
            message(INTENT_TEXT, json!({"text": "ok", "markdown": true})),
            message(INTENT_UPDATE, json!({})),
            message(INTENT_UPDATE, json!({"version": ""})),
            message(INTENT_UPDATE, json!({"version": "1".repeat(65)})),
            message(INTENT_UPDATE, json!({"version": "0.6.0", "url": "https://example.org/x"})),
            json!({"type": "intent.beat", "id": "m", "at": "2026-10-30T12:00:00Z"}).to_string(),
            json!({"type": "intent.beat", "id": "m", "at": "2026-10-30T12:00:00Z", "payload": {}, "x": 1}).to_string(),
            json!({"type": "intent.beat", "id": "", "at": "2026-10-30T12:00:00Z", "payload": {}}).to_string(),
            json!({"type": "intent.beat", "id": "m".repeat(65), "at": "2026-10-30T12:00:00Z", "payload": {}})
                .to_string(),
            json!({"type": "intent.beat", "id": "m", "at": "yesterday", "payload": {}}).to_string(),
            json!({"type": "intent.beat", "id": "m", "at": "2026-10-30T12:00:00Z", "payload": []}).to_string(),
            json!({"type": 3, "id": "m", "at": "2026-10-30T12:00:00Z", "payload": {}}).to_string(),
        ];
        for text in wrong {
            assert!(parse(&text).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn a_time_without_offset_is_utc() {
        let received =
            nestor(&json!({"type": "intent.beat", "id": "m", "at": "2026-10-30T12:00:00", "payload": {}}).to_string());
        assert_eq!(received.at.to_rfc3339(), "2026-10-30T12:00:00+00:00");
    }

    #[test]
    fn what_the_satellite_sends_has_a_fresh_id_and_a_utc_time() -> Result<(), ProtocolError> {
        let (a, b) = (ack("intent-1"), ack("intent-1"));
        assert_ne!(a["id"], b["id"]);
        assert_eq!(a["payload"], json!({"of": "intent-1"}));
        assert!(a["at"].as_str().unwrap().ends_with("+00:00"));
        assert!(person_detected(1, 1.2).is_err());
        assert!(person_detected(1, f64::NAN).is_err());
        assert_eq!(person_detected(2, 0.5).unwrap()["payload"], json!({"count": 2, "confidence": 0.5}));
        assert_eq!(speech_activity(true)["payload"], json!({"active": true}));
        assert_eq!(input_text("Allume le salon")?["payload"], json!({"text": "Allume le salon"}));
        assert!(input_text("").is_err());
        assert!(input_text(&"é".repeat(4_000)).is_ok(), "characters, not bytes");
        assert!(input_text(&"é".repeat(4_001)).is_err());
        assert_eq!(update_status(UpdateState::Current, None)?["payload"], json!({"state": "current"}));
        assert_eq!(
            update_status(UpdateState::Available, Some("0.6.0"))?["payload"],
            json!({"state": "available", "version": "0.6.0"})
        );
        assert!(update_status(UpdateState::Current, Some("0.6.0")).is_err());
        assert!(update_status(UpdateState::Failed, None).is_err());
        assert!(update_status(UpdateState::Installing, Some("")).is_err());
        let manifest = Manifest::new(crate::Device::new("a", "m", "salon")?, ["text.input"])?;
        assert_eq!(hello(&manifest, "nst1_x")["payload"]["token"], "nst1_x");
        Ok(())
    }
}

//! What the satellite does with each message from the core: hand intents to the body and
//! acknowledge them, take note of the welcome, notice the core hanging up.
//!
//! It knows nothing of the transport, so a driver on another profile (a WebSocket) can reuse it.

use std::sync::Arc;

use tracing::{debug, warn};

use crate::body::Body;
use crate::wire::{self, Inbound, Intent, Message, Welcome};

/// What the connection has to do after a message.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Nothing: the message was ignored or needs no answer.
    Nothing,
    /// The core welcomed the device: the link is up.
    Welcomed(Welcome),
    /// Send this reply (an `ack`, as JSON text).
    Reply(String),
    /// The transport said the core left.
    HungUp,
}

/// Turns the core's messages into calls on a [`Body`].
#[derive(Clone)]
pub struct Dispatcher {
    body: Arc<dyn Body>,
}

impl Dispatcher {
    /// A dispatcher to `body`.
    pub fn new(body: Arc<dyn Body>) -> Self {
        Self { body }
    }

    /// Handle one data channel message (its text).
    pub fn handle(&self, text: &str) -> Outcome {
        let received = match wire::parse(text) {
            Ok(Inbound::Nestor(received)) => received,
            Ok(signalling @ Inbound::Signalling { .. }) => {
                if signalling.is_hang_up() {
                    return Outcome::HungUp;
                }
                debug!(?signalling, "transport signalling ignored");
                return Outcome::Nothing;
            }
            Err(error) => {
                warn!(%error, "ignoring an off-contract message from the core");
                return Outcome::Nothing;
            }
        };
        self.body.heartbeat();
        match received.message {
            Message::Unknown(message_type) => {
                debug!(%message_type, "a message type this SDK does not know: ignored");
                Outcome::Nothing
            }
            Message::Welcome(welcome) => Outcome::Welcomed(welcome),
            Message::Intent(intent) => {
                match intent {
                    Intent::Behavior(behavior) => self.body.behavior(behavior),
                    Intent::Expression { expression, intensity } => self.body.expression(expression, intensity),
                    Intent::Beat => self.body.beat(),
                    Intent::Identify => self.body.identify(),
                }
                Outcome::Reply(wire::ack(&received.id).to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::{Value, json};

    use super::*;
    use crate::protocol::{Behavior, Expression};

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
    }

    impl Recorder {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn note(&self, call: String) {
            self.calls.lock().unwrap().push(call);
        }
    }

    impl Body for Recorder {
        fn behavior(&self, behavior: Behavior) {
            self.note(format!("behavior {behavior}"));
        }
        fn expression(&self, expression: Expression, intensity: f64) {
            self.note(format!("expression {expression} {intensity}"));
        }
        fn beat(&self) {
            self.note("beat".into());
        }
        fn heartbeat(&self) {
            self.note("heartbeat".into());
        }
        fn identify(&self) {
            self.note("identify".into());
        }
    }

    fn setup() -> (Arc<Recorder>, Dispatcher) {
        let body = Arc::new(Recorder::default());
        (body.clone(), Dispatcher::new(body))
    }

    fn message(message_type: &str, payload: Value) -> String {
        json!({"type": message_type, "id": "intent-7", "at": "2026-10-30T12:00:00+00:00", "payload": payload})
            .to_string()
    }

    fn acked(outcome: Outcome) -> Value {
        match outcome {
            Outcome::Reply(text) => serde_json::from_str(&text).unwrap(),
            other => panic!("no reply: {other:?}"),
        }
    }

    #[test]
    fn each_intent_reaches_the_body_and_is_acknowledged_by_its_id() {
        let (body, dispatcher) = setup();
        let replies = [
            dispatcher.handle(&message("intent.behavior", json!({"behavior": "speaking"}))),
            dispatcher.handle(&message("intent.expression", json!({"expression": "joy", "intensity": 0.8}))),
            dispatcher.handle(&message("intent.beat", json!({}))),
            dispatcher.handle(&message("intent.identify", json!({}))),
        ];
        for reply in replies {
            let ack = acked(reply);
            assert_eq!((ack["type"].as_str(), &ack["payload"]), (Some("ack"), &json!({"of": "intent-7"})));
        }
        assert_eq!(
            body.calls(),
            [
                "heartbeat",
                "behavior speaking",
                "heartbeat",
                "expression joy 0.8",
                "heartbeat",
                "beat",
                "heartbeat",
                "identify"
            ]
        );
    }

    #[test]
    fn the_welcome_is_reported_and_not_acknowledged() {
        let (_, dispatcher) = setup();
        let outcome = dispatcher
            .handle(&message("welcome", json!({"device_id": "a", "room": "salon", "capabilities": ["audio.capture"]})));
        assert!(matches!(outcome, Outcome::Welcomed(w) if w.room == "salon"));
    }

    #[test]
    fn peer_left_is_a_hang_up_and_other_signalling_is_ignored() {
        let (body, dispatcher) = setup();
        let left = json!({"type": "signalling", "message": {"type": "peerLeft"}}).to_string();
        assert_eq!(dispatcher.handle(&left), Outcome::HungUp);
        let other = json!({"type": "signalling", "message": {"type": "renegotiate"}}).to_string();
        assert_eq!(dispatcher.handle(&other), Outcome::Nothing);
        assert!(body.calls().is_empty(), "signalling is not the core speaking");
    }

    #[test]
    fn an_unknown_type_is_heard_but_not_acknowledged_and_a_malformed_one_is_dropped() {
        let (body, dispatcher) = setup();
        assert_eq!(dispatcher.handle(&message("intent.dance", json!({}))), Outcome::Nothing);
        assert_eq!(dispatcher.handle(&message("intent.behavior", json!({"behavior": "x"}))), Outcome::Nothing);
        assert_eq!(dispatcher.handle("{"), Outcome::Nothing);
        assert_eq!(body.calls(), ["heartbeat"]);
    }
}

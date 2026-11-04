//! What the satellite does with each message from the core: hand intents to the body or the text
//! display and acknowledge them, take note of the welcome.
//!
//! It knows nothing of the transport: a driver with a link of its own can reuse it.

use std::sync::Arc;

use tracing::{debug, warn};

use crate::body::Body;
use crate::text::TextDisplay;
use crate::wire::{self, Intent, Message, Welcome};

/// What the connection has to do after a message.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Nothing: the message was ignored or needs no answer.
    Nothing,
    /// The core welcomed the device: the link is up.
    Welcomed(Welcome),
    /// Send this reply (an `ack`, as JSON text).
    Reply(String),
    /// Send this reply (the `ack`), and install `version` if it is the one this device reported.
    Update {
        /// The `ack`, as JSON text.
        reply: String,
        /// The version asked for.
        version: String,
    },
}

/// Turns the core's messages into calls on a [`Body`] and a [`TextDisplay`].
#[derive(Clone)]
pub struct Dispatcher {
    body: Arc<dyn Body>,
    text: Option<Arc<dyn TextDisplay>>,
}

impl Dispatcher {
    /// A dispatcher to `body`, without a text display: `intent.text` is acknowledged and dropped.
    pub fn new(body: Arc<dyn Body>) -> Self {
        Self { body, text: None }
    }

    /// Show the assistant's words (`intent.text`) on `display`.
    pub fn with_text(mut self, display: Arc<dyn TextDisplay>) -> Self {
        self.text = Some(display);
        self
    }

    /// Handle one message of the link (its text).
    pub fn handle(&self, text: &str) -> Outcome {
        let received = match wire::parse(text) {
            Ok(received) => received,
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
            Message::Intent(Intent::Update { version }) => {
                Outcome::Update { reply: wire::ack(&received.id).to_string(), version }
            }
            Message::Intent(intent) => {
                match intent {
                    Intent::Behavior(behavior) => self.body.behavior(behavior),
                    Intent::Expression { expression, intensity } => self.body.expression(expression, intensity),
                    Intent::Beat => self.body.beat(),
                    Intent::Identify => self.body.identify(),
                    Intent::Text { text, last } => match &self.text {
                        Some(display) => display.show_text(&text, last),
                        None => debug!("intent.text without a text display: not shown"),
                    },
                    Intent::Update { .. } => {} // handled above: the link installs it
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
        let shown = body.clone();
        let display = move |text: &str, last: bool| shown.note(format!("text {text} {last}"));
        (body.clone(), Dispatcher::new(body).with_text(Arc::new(display)))
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
            dispatcher.handle(&message("intent.text", json!({"text": "Je regarde.", "final": false}))),
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
                "identify",
                "heartbeat",
                "text Je regarde. false"
            ]
        );
    }

    #[test]
    fn an_update_is_acknowledged_and_handed_to_the_link_not_the_body() {
        let (body, dispatcher) = setup();
        let Outcome::Update { reply, version } =
            dispatcher.handle(&message("intent.update", json!({"version": "0.6.0"})))
        else {
            panic!("not handed to the link");
        };
        assert_eq!(version, "0.6.0");
        assert_eq!(acked(Outcome::Reply(reply))["payload"], json!({"of": "intent-7"}));
        assert_eq!(body.calls(), ["heartbeat"]);
    }

    #[test]
    fn the_welcome_is_reported_and_not_acknowledged() {
        let (_, dispatcher) = setup();
        let outcome = dispatcher
            .handle(&message("welcome", json!({"device_id": "a", "room": "salon", "capabilities": ["audio.capture"]})));
        assert!(matches!(outcome, Outcome::Welcomed(w) if w.room == "salon"));
    }

    #[test]
    fn without_a_text_display_the_assistant_s_words_are_still_acknowledged() {
        let body = Arc::new(Recorder::default());
        let dispatcher = Dispatcher::new(body.clone());
        let ack = acked(dispatcher.handle(&message("intent.text", json!({"text": "Bonjour."}))));
        assert_eq!(ack["payload"], json!({"of": "intent-7"}));
        assert_eq!(body.calls(), ["heartbeat"]);
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

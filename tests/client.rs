//! What the conformance suite does not see: how pairing presents itself on a display and a voice,
//! enrollment codes handed in, refusals, rate limits, a core that is down, and how the link behaves
//! when the core closes it, against a fake core over HTTP and WebSocket.
#![allow(clippy::unwrap_used)] // test helpers fail loudly

mod support;

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::time::Duration;

use nestor_satellite::{
    Device, Enrollment, EnrollmentMethod, Error, IDENTIFY, Language, Manifest, Satellite, SatelliteBuilder, TokenStore,
};
use serde_json::{Value, json};
use support::{Codes, FakeCore, Outputs, tokens, wait_for};

fn manifest() -> Manifest {
    let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon").unwrap();
    Manifest::new(device, [IDENTIFY]).unwrap()
}

fn builder(url: &str, tokens: &TokenStore) -> SatelliteBuilder {
    Satellite::builder(url, manifest(), tokens.clone())
        .language(Language::En)
        .poll_interval(Duration::from_millis(10))
        .backoff(Duration::from_millis(20), Duration::from_millis(40))
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
    satellite.run_until(wait_for(|| !core.linked().is_empty())).await.unwrap();

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
    assert_eq!(tokens.load(&url, "enceinte-salon").as_deref(), Some("nst1_p1"), "the token is kept");
    assert_eq!(core.linked(), ["nst1_p1"]);
}

#[tokio::test]
async fn without_display_or_voice_only_provisioning_is_declared_and_the_device_code_still_pairs() {
    let (url, core) = FakeCore::default().serve().await;
    let (_dir, tokens) = tokens();
    let satellite = builder(&url, &tokens).build().unwrap();
    assert_eq!(satellite.manifest().to_json()["pairing_methods"], json!(["enrollment.provisioning"]));
    satellite.run_until(wait_for(|| !core.linked().is_empty())).await.unwrap();
    assert_eq!(core.linked(), ["nst1_p1"], "paired with the device code read in the log");
}

#[tokio::test]
async fn a_pending_identify_reaches_the_body_and_polling_goes_on() {
    let (url, core) = FakeCore { pending: 3, identify: true, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).body(outputs.clone()).build().unwrap();
    satellite.run_until(wait_for(|| !core.linked().is_empty())).await.unwrap();
    assert_eq!(outputs.noted()[0], "identify");
    assert_eq!(outputs.count("identify"), 1);
    assert!(core.polls.load(Ordering::SeqCst) >= 4, "kept polling until approved");
}

#[tokio::test]
async fn a_configured_enrollment_code_pairs_at_once_without_presenting_a_device_code() {
    let (url, core) = FakeCore { never_approve: true, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).display(outputs.clone()).enrollment(code()).build().unwrap();
    satellite.run_until(wait_for(|| !core.linked().is_empty())).await.unwrap();

    let sent = &core.requests()[0];
    assert_eq!((&sent["enrollment_code"], &sent["method"]), (&json!("K7QF2M9X"), &json!("enrollment.provisioning")));
    assert_eq!(outputs.noted(), ["clear"], "no device code presented");
    assert_eq!(core.linked(), ["nst1_p1"]);
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
    satellite.run_until(wait_for(|| !core.linked().is_empty())).await.unwrap();

    assert_eq!(core.coded_requests(), 1, "presented once");
    assert_eq!(
        outputs.noted(),
        [
            "message This enrollment code was already used. Use the device code instead.",
            "show To pair this device, type this code in Nestor: 000002",
            "clear",
        ]
    );
    assert_eq!(core.linked(), ["nst1_p2"]);
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
        wait_for(|| !core.linked().is_empty()).await;
    };
    satellite.run_until(typing).await.unwrap();

    assert_eq!(core.enrollments(), [json!({"enrollment_code": "K7QF2M9X", "method": "enrollment.input"})]);
    assert_eq!(core.linked(), ["nst1_p1"]);
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
        wait_for(|| !core.linked().is_empty()).await;
    };
    satellite.run_until(scanning).await.unwrap();
    assert_eq!(core.enrollments().len(), 1);
    assert_eq!(core.linked(), ["nst1_p1"]);
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
        wait_for(|| !core.linked().is_empty()).await;
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
        wait_for(|| !core.linked().is_empty()).await;
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
    first.run_until(wait_for(|| !core.linked().is_empty())).await.unwrap();
    assert_eq!(core.coded_requests(), 1);

    let file = std::fs::read_to_string(tokens.path()).unwrap();
    assert!(!file.contains("K7QF2M9X") && !file.contains("K7QF-2M9X"), "never in clear: {file}");
    let saved: Value = serde_json::from_str(&file).unwrap();
    assert_eq!(saved["refused"], json!({code().code().digest(): "used"}));
    tokens.forget().unwrap(); // the core revoked the token: the refused codes stay
    assert_eq!(tokens.load(&url, "enceinte-salon"), None);

    let restarted = builder(&url, &tokens).enrollment(code()).build().unwrap();
    restarted.run_until(wait_for(|| core.linked().iter().any(|t| t == "nst1_p3"))).await.unwrap();
    assert_eq!(core.coded_requests(), 1, "the same code, configured again, is not sent again");
    assert_eq!(core.requests().len(), 3, "the restarted device asked for a device code at once");
}

#[tokio::test]
async fn a_rate_limited_device_waits_for_retry_after() {
    let (url, core) = FakeCore { rate_limit_s: Some(1), never_approve: true, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let satellite = builder(&url, &tokens).enrollment(code()).build().unwrap();
    satellite.run_until(wait_for(|| !core.linked().is_empty())).await.unwrap();
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
    tokens.save(&url, "enceinte-salon", "nst1_from-another-life").unwrap();
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).display(outputs.clone()).build().unwrap();
    satellite.run_until(wait_for(|| core.linked().len() >= 2)).await.unwrap();

    assert_eq!(core.linked(), ["nst1_from-another-life", "nst1_p1"], "closed with 4401, then paired again");
    assert_eq!(outputs.count("show"), 1, "paired once");
    assert_eq!(tokens.load(&url, "enceinte-salon").as_deref(), Some("nst1_p1"));
}

/// A device already paired, its token kept, for the link tests.
fn paired(url: &str) -> (tempfile::TempDir, TokenStore) {
    let (dir, tokens) = tokens();
    tokens.save(url, "enceinte-salon", "nst1_p0").unwrap();
    (dir, tokens)
}

#[tokio::test]
async fn the_link_opens_with_a_hello_carrying_the_token_and_each_intent_is_acknowledged() {
    let script = vec![
        support::envelope("intent.behavior", "i-1", json!({"behavior": "thinking"})),
        support::envelope("intent.identify", "i-2", json!({})),
        support::envelope("intent.dance", "i-3", json!({"style": "waltz"})),
    ];
    let (url, core) = FakeCore { script, ..Default::default() }.serve().await;
    let (_dir, tokens) = paired(&url);
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).body(outputs.clone()).build().unwrap();
    let observer = satellite.observer();
    satellite.run_until(wait_for(|| core.received_of("ack").len() == 2)).await.unwrap();

    let hello = &core.hellos.lock().unwrap()[0];
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["payload"]["token"], "nst1_p0");
    assert_eq!(hello["payload"]["manifest"]["device"]["id"], "enceinte-salon");
    let acked: Vec<Value> = core.received_of("ack").iter().map(|a| a["payload"]["of"].clone()).collect();
    assert_eq!(acked, [json!("i-1"), json!("i-2")], "an unknown intent is not acknowledged");
    assert_eq!(outputs.noted(), ["connected true", "behavior thinking", "identify", "connected false"]);
    assert_eq!(observer.welcome().unwrap().room, "salon");
    assert_eq!(observer.connections(), 1);
    assert!(!observer.is_connected(), "shut down");
}

#[tokio::test]
async fn the_device_reconnects_with_the_same_token_when_the_core_stops() {
    let closes = VecDeque::from([1001]).into();
    let (url, core) = FakeCore { closes, close_after: Duration::from_millis(50), ..Default::default() }.serve().await;
    let (_dir, tokens) = paired(&url);
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).body(outputs.clone()).build().unwrap();
    let observer = satellite.observer();
    satellite.run_until(wait_for(|| observer.connections() == 2)).await.unwrap();
    assert_eq!(core.linked(), ["nst1_p0", "nst1_p0"]);
    assert_eq!(outputs.count("connected false"), 2, "down after the close, and at shutdown");
    assert!(core.requests().is_empty(), "never paired again");
}

#[tokio::test]
async fn a_device_whose_link_is_replaced_stops() {
    let closes = VecDeque::from([4409]).into();
    let (url, core) = FakeCore { closes, close_after: Duration::from_millis(50), ..Default::default() }.serve().await;
    let (_dir, tokens) = paired(&url);
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).body(outputs.clone()).build().unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), satellite.run()).await.unwrap();
    assert!(matches!(outcome, Err(Error::Replaced)), "{outcome:?}");
    assert_eq!(core.linked().len(), 1, "no reconnection");
    assert_eq!(outputs.noted(), ["connected true", "connected false"]);
    assert_eq!(tokens.load(&url, "enceinte-salon").as_deref(), Some("nst1_p0"), "the token is still good");
    assert_eq!(Language::En.describe(&Error::Replaced), "Another connection of this device took over: this one stops.");
}

#[tokio::test]
async fn a_silent_link_is_lost_and_opened_again() {
    let (url, core) = FakeCore::default().serve().await; // the fake core never pings
    let (_dir, tokens) = paired(&url);
    let satellite = builder(&url, &tokens).link_silence(Duration::from_millis(200)).build().unwrap();
    let observer = satellite.observer();
    satellite.run_until(wait_for(|| observer.connections() == 2)).await.unwrap();
    assert_eq!(core.linked(), ["nst1_p0", "nst1_p0"]);
}

#[tokio::test]
async fn a_text_satellite_sends_what_is_typed_and_shows_the_assistant_s_words() {
    let script = vec![
        support::envelope("intent.text", "t-1", json!({"text": "Je regarde.", "final": false})),
        support::envelope("intent.text", "t-2", json!({"text": "Il fait 21 °C au salon."})),
    ];
    let (url, core) = FakeCore { script, ..Default::default() }.serve().await;
    let (_dir, tokens) = paired(&url);
    let outputs = Outputs::default();
    let satellite = builder(&url, &tokens).text_display(outputs.clone()).text_input().build().unwrap();
    let declared = satellite.manifest().to_json()["capabilities"].clone();
    assert_eq!(declared, json!([{"name": "device.identify"}, {"name": "text.input"}, {"name": "text.display"}]));
    let observer = satellite.observer();
    let typing = async {
        wait_for(|| observer.is_connected()).await;
        assert!(observer.input_text("Quelle température au salon ?").unwrap());
        wait_for(|| !core.received_of("input.text").is_empty() && core.received_of("ack").len() == 2).await;
    };
    satellite.run_until(typing).await.unwrap();

    assert_eq!(core.received_of("input.text")[0]["payload"], json!({"text": "Quelle température au salon ?"}));
    assert_eq!(outputs.noted(), ["text Je regarde. false", "text Il fait 21 °C au salon. true"]);
}

#[tokio::test]
async fn typed_text_the_household_did_not_allow_is_refused_and_nothing_is_sent() {
    let (url, core) = FakeCore { withheld: vec!["text.input"], ..Default::default() }.serve().await;
    let (_dir, tokens) = paired(&url);
    let satellite = builder(&url, &tokens).text_input().build().unwrap();
    let observer = satellite.observer();
    let typing = async {
        wait_for(|| observer.is_connected()).await;
        assert!(matches!(observer.input_text("bonjour"), Err(Error::CapabilityNotAllowed { .. })));
    };
    satellite.run_until(typing).await.unwrap();
    assert!(core.received_of("input.text").is_empty());
}

#[tokio::test]
async fn a_manifest_declaring_text_without_what_carries_it_out_is_refused() {
    let (_dir, tokens) = tokens();
    let device = Device::new("ecran-cuisine", "raspberry-pi-zero", "cuisine").unwrap();
    let manifest = Manifest::new(device, ["text.display"]).unwrap();
    let built = Satellite::builder("http://127.0.0.1:9", manifest.clone(), tokens.clone()).build();
    assert!(matches!(built, Err(Error::NotProvided(c)) if c == "text.display"));
    let given = Satellite::builder("http://127.0.0.1:9", manifest, tokens).text_display(|_: &str, _: bool| {});
    assert_eq!(given.build().unwrap().manifest().to_json()["capabilities"], json!([{"name": "text.display"}]));
}

/// Talking aloud: audio offered once welcomed, when allowed; the device code said by the bundled voice.
#[cfg(feature = "voice")]
mod voice {
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use nestor_satellite::{AUDIO_CAPTURE, AUDIO_PLAYBACK, AudioError, AudioSource, RecordingSink, SilenceSource};

    use super::*;

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
        assert!(!core.linked().is_empty());
    }

    #[tokio::test]
    async fn a_driver_s_voice_replaces_the_bundled_one() {
        let (url, core) = FakeCore { pending: 3, ..Default::default() }.serve().await;
        let (_dir, tokens) = tokens();
        let speaker = RecordingSink::new();
        let outputs = Outputs::default();
        let satellite =
            builder(&url, &tokens).language(Language::Fr).sink(speaker.clone()).voice(outputs.clone()).build().unwrap();
        satellite.run_until(wait_for(|| !core.linked().is_empty())).await.unwrap();
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
        let null = builder(url, &tokens).null_audio().build().unwrap();
        assert_eq!(methods(null), json!(["enrollment.provisioning"]), "no sound card: nobody would hear it");
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

    #[tokio::test]
    async fn audio_is_offered_with_the_token_once_welcomed_and_offered_again_when_it_fails() {
        let (url, core) = FakeCore::default().serve().await;
        let (_dir, tokens) = tokens();
        tokens.save(&url, "enceinte-salon", "nst1_p0").unwrap();
        let satellite = builder(&url, &tokens).null_audio().build().unwrap();
        let declared = satellite.manifest().to_json()["capabilities"].clone();
        assert_eq!(declared, json!([{"name": "device.identify"}, {"name": AUDIO_CAPTURE}, {"name": AUDIO_PLAYBACK}]));
        satellite.run_until(wait_for(|| core.offers().len() >= 2)).await.unwrap();
        assert_eq!(core.offers()[..2], ["nst1_p0", "nst1_p0"], "the link's token; the 503 retried");
        assert_eq!(core.linked(), ["nst1_p0"], "a failed audio does not touch the link");
    }

    #[tokio::test]
    async fn a_token_refused_on_an_audio_offer_is_forgotten_and_the_device_pairs_again() {
        let (url, core) = FakeCore { audio_refused: true, ..Default::default() }.serve().await;
        let (_dir, tokens) = tokens();
        tokens.save(&url, "enceinte-salon", "nst1_p0").unwrap();
        let satellite = builder(&url, &tokens).null_audio().build().unwrap();
        satellite.run_until(wait_for(|| core.linked().len() >= 2)).await.unwrap();
        assert_eq!(core.offers()[0], "nst1_p0");
        assert_eq!(core.linked(), ["nst1_p0", "nst1_p1"], "paired again, then linked with the new token");
    }

    #[tokio::test]
    async fn no_audio_is_offered_when_the_household_allowed_none() {
        let core = FakeCore { withheld: vec![AUDIO_CAPTURE, AUDIO_PLAYBACK], ..Default::default() };
        let (url, core) = core.serve().await;
        let (_dir, tokens) = tokens();
        tokens.save(&url, "enceinte-salon", "nst1_p0").unwrap();
        let satellite = builder(&url, &tokens).null_audio().build().unwrap();
        let observer = satellite.observer();
        let linked = async {
            wait_for(|| observer.is_connected()).await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        satellite.run_until(linked).await.unwrap();
        assert!(core.offers().is_empty(), "{:?}", core.offers());
    }

    #[tokio::test]
    async fn a_manifest_declaring_audio_without_a_device_for_it_is_refused() {
        let (_dir, tokens) = tokens();
        let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon").unwrap();
        let manifest = Manifest::new(device, [AUDIO_CAPTURE]).unwrap();
        let built = Satellite::builder("http://127.0.0.1:9", manifest, tokens).build();
        assert!(matches!(built, Err(Error::NotProvided(c)) if c == AUDIO_CAPTURE));
    }
}

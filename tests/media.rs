//! Watching and calling through a satellite (rules M1 to M7 and A1 of `nestor.satellite/1`), what the
//! conformance suite does not check: sessions the household did not allow, the voice stepping aside
//! during a call, a core that does not talk, the camera on only while something is sent. The fake
//! core never connects a session: it notes the offer and answers it late, which leaves the session
//! open until it ends.
#![allow(clippy::unwrap_used)] // test helpers fail loudly

mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nestor_satellite::{Device, Error, Language, MEDIA_INDICATOR, Manifest, Satellite, SatelliteBuilder, TokenStore};
use serde_json::{Value, json};
use support::{FakeCore, envelope, tokens, wait_for};

fn builder(url: &str, tokens: &TokenStore) -> SatelliteBuilder {
    tokens.save(url, "camera-entree", "nst1_p0").unwrap();
    let device = Device::new("camera-entree", "raspberry-pi-zero", "entree").unwrap();
    Satellite::builder(url, Manifest::new(device, Vec::<&str>::new()).unwrap(), tokens.clone())
        .language(Language::En)
        .backoff(Duration::from_millis(20), Duration::from_millis(40))
}

fn open(id: &str, session: &str, video: bool, audio: bool, max_s: u32) -> Value {
    envelope("intent.media.open", id, json!({"session": session, "video": video, "audio": audio, "max_s": max_s}))
}

fn acked(core: &FakeCore) -> Vec<String> {
    core.received_of("ack").iter().map(|ack| ack["payload"]["of"].as_str().unwrap().to_owned()).collect()
}

/// What the indicator showed, in order.
#[derive(Clone, Default)]
struct Light(Arc<Mutex<Vec<bool>>>);

impl Light {
    fn shown(&self) -> Vec<bool> {
        self.0.lock().unwrap().clone()
    }

    fn indicator(&self) -> impl Fn(bool) + Send + Sync + 'static {
        let light = self.0.clone();
        move |active| light.lock().unwrap().push(active)
    }
}

#[test]
fn the_sdk_listens_on_no_port_it_only_connects_to_the_core() {
    // M4. WebRTC binds the ephemeral ports of its own outgoing connection, nothing else.
    fn sources(dir: &Path) -> Vec<(String, String)> {
        std::fs::read_dir(dir)
            .unwrap()
            .flat_map(|entry| {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    sources(&path)
                } else if path.extension().is_some_and(|e| e == "rs") {
                    vec![(path.display().to_string(), std::fs::read_to_string(&path).unwrap())]
                } else {
                    vec![]
                }
            })
            .collect()
    }
    let serving = ["TcpListener", "UdpSocket::bind", "axum", "hyper", "tiny_http", "warp::"];
    let found: Vec<String> = sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"))
        .into_iter()
        .flat_map(|(path, code)| {
            serving.iter().filter(|s| code.contains(**s)).map(|s| format!("{path}: {s}")).collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn showing_a_session_follows_from_an_indicator_given() {
    let (_dir, tokens) = tokens();
    let satellite = builder("http://127.0.0.1:9", &tokens).media_indicator(|_: bool| {}).build().unwrap();
    assert_eq!(satellite.manifest().to_json()["capabilities"], json!([{"name": MEDIA_INDICATOR}]));
    let device = Device::new("camera-entree", "raspberry-pi-zero", "entree").unwrap();
    for claimed in [MEDIA_INDICATOR, nestor_satellite::VIDEO_CAPTURE] {
        let manifest = Manifest::new(device.clone(), [claimed]).unwrap();
        let built = Satellite::builder("http://127.0.0.1:9", manifest, tokens.clone()).build();
        assert!(matches!(built, Err(Error::NotProvided(c)) if c == claimed));
    }
}

#[tokio::test]
async fn without_a_camera_nor_a_call_every_media_intent_is_acknowledged_and_refused() {
    let script = vec![open("open-1", "s", true, false, 60), open("open-2", "s", false, true, 60)];
    let script = [script, vec![envelope("intent.media.snapshot", "picture-1", json!({}))]].concat();
    let (url, core) = FakeCore { script, ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let light = Light::default();
    let satellite = builder(&url, &tokens).media_indicator(light.indicator()).build().unwrap();
    let observer = satellite.observer();
    let refused = async {
        wait_for(|| acked(&core).len() == 3).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    satellite.run_until(refused).await.unwrap();
    assert!(observer.connections() == 1 && core.media_offers().is_empty(), "nothing offered");
    assert!(core.received_of("media.snapshot").is_empty(), "no picture");
    assert_eq!(light.shown(), Vec::<bool>::new(), "nothing shown");
}

#[tokio::test]
async fn a_core_that_closes_the_link_as_insecure_is_retried() {
    // M7: the core turns away a device that films over plain http; it may be reached over https later.
    let closes = std::collections::VecDeque::from([4403]).into();
    let (url, core) = FakeCore { closes, close_after: Duration::from_millis(50), ..Default::default() }.serve().await;
    let (_dir, tokens) = tokens();
    let satellite = builder(&url, &tokens).build().unwrap();
    let observer = satellite.observer();
    satellite.run_until(wait_for(|| observer.connections() == 2)).await.unwrap();
    assert_eq!(core.linked(), ["nst1_p0", "nst1_p0"], "the same token: it was not refused");
}

/// Calls: a microphone and a speaker (feature `voice`).
#[cfg(feature = "voice")]
mod calls {
    use nestor_satellite::{AUDIO_CAPTURE, AUDIO_PLAYBACK};

    use super::*;

    #[tokio::test]
    async fn the_voice_hangs_up_for_a_call_which_is_shown_and_ends_when_told() {
        let core = FakeCore { media_hold: Duration::from_secs(30), ..Default::default() };
        let (url, core) = core.serve().await;
        let (_dir, tokens) = tokens();
        let light = Light::default();
        let satellite = builder(&url, &tokens).null_audio().media_indicator(light.indicator()).build().unwrap();
        let calling = async {
            wait_for(|| core.offers().len() >= 2).await; // the voice, retried: this core answers 503
            core.send(open("open-1", "call", false, true, 60));
            wait_for(|| core.media_offers().len() == 1).await;
            assert_eq!(light.shown(), [true], "shown before the offer");
            let during = core.offers().len();
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert_eq!(core.offers().len(), during, "no voice while the call lasts");
            core.send(envelope("intent.media.close", "close-1", json!({"session": "call"})));
            wait_for(|| light.shown() == [true, false]).await;
            wait_for(|| core.offers().len() > during).await;
        };
        tokio::time::timeout(Duration::from_secs(10), satellite.run_until(calling)).await.unwrap().unwrap();
        let (token, offer) = &core.media_offers()[0];
        let sdp = offer["sdp"].as_str().unwrap();
        assert_eq!((token.as_str(), &offer["session"]), ("nst1_p0", &json!("call")));
        assert!(sdp.contains("m=audio") && !sdp.contains("m=video"), "a call, without video: {sdp}");
        assert_eq!(acked(&core), ["open-1", "close-1"]);
    }

    #[tokio::test]
    async fn a_call_the_household_did_not_allow_is_never_offered() {
        let script = vec![open("open-1", "call", false, true, 60)];
        let core = FakeCore { withheld: vec![AUDIO_PLAYBACK], script, ..Default::default() };
        let (url, core) = FakeCore { media_hold: Duration::from_secs(30), ..core }.serve().await;
        let (_dir, tokens) = tokens();
        let light = Light::default();
        let satellite = builder(&url, &tokens).null_audio().media_indicator(light.indicator()).build().unwrap();
        let refused = async {
            wait_for(|| acked(&core) == ["open-1"]).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        };
        satellite.run_until(refused).await.unwrap();
        assert!(core.media_offers().is_empty());
        assert_eq!(light.shown(), Vec::<bool>::new());
    }

    #[tokio::test]
    async fn a_core_that_does_not_talk_gets_no_voice_and_its_calls_still_connect() {
        let script = vec![open("open-1", "call", false, true, 60)];
        let core =
            FakeCore { voice: Some(false), script, script_after: Duration::from_millis(300), ..Default::default() };
        let (url, core) = FakeCore { media_hold: Duration::from_secs(30), ..core }.serve().await;
        let (_dir, tokens) = tokens();
        let satellite = builder(&url, &tokens).null_audio().build().unwrap();
        satellite.run_until(wait_for(|| core.media_offers().len() == 1)).await.unwrap();
        assert!(core.offers().is_empty(), "no voice offered (A1): {:?}", core.offers());
        let welcomed = core.hellos.lock().unwrap()[0]["payload"]["manifest"]["capabilities"].clone();
        assert_eq!(welcomed, json!([{"name": AUDIO_CAPTURE}, {"name": AUDIO_PLAYBACK}]));
    }
}

/// Filming: a camera (feature `video`).
#[cfg(feature = "video")]
mod filming {
    use async_trait::async_trait;
    use base64::Engine;
    use nestor_satellite::VIDEO_CAPTURE;
    use nestor_satellite::camera::{Camera, CameraError, Frame, TestPattern};

    use super::*;

    /// Colour bars, noting when the camera is switched on and off.
    #[derive(Clone, Default)]
    struct Lens(Arc<Mutex<Vec<&'static str>>>, Arc<Mutex<TestPattern>>);

    impl Lens {
        fn switched(&self) -> Vec<&'static str> {
            self.0.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Camera for Lens {
        async fn start(&mut self) -> Result<(), CameraError> {
            self.0.lock().unwrap().push("on");
            Ok(())
        }

        async fn read(&mut self) -> Result<Frame, CameraError> {
            let mut pattern = self.1.lock().unwrap().clone();
            let frame = pattern.read().await;
            *self.1.lock().unwrap() = pattern;
            frame
        }

        async fn close(&mut self) {
            self.0.lock().unwrap().push("off");
        }
    }

    #[tokio::test]
    async fn the_camera_is_on_only_for_a_picture_or_a_session_and_each_is_shown() {
        let (url, core) = FakeCore { media_hold: Duration::from_secs(30), ..Default::default() }.serve().await;
        let (_dir, tokens) = tokens();
        let (lens, light) = (Lens::default(), Light::default());
        let satellite = builder(&url, &tokens).camera(lens.clone()).media_indicator(light.indicator()).build().unwrap();
        let declared = satellite.manifest().to_json()["capabilities"].clone();
        assert_eq!(declared, json!([{"name": VIDEO_CAPTURE}, {"name": MEDIA_INDICATOR}]));
        let observer = satellite.observer();
        let filming = async {
            wait_for(|| observer.is_connected()).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(lens.switched().is_empty() && core.media_offers().is_empty(), "nothing unless asked");
            core.send(envelope("intent.media.snapshot", "picture-1", json!({})));
            wait_for(|| !core.received_of("media.snapshot").is_empty()).await;
            wait_for(|| lens.switched() == ["on", "off"]).await;
            core.send(open("open-1", "watch", true, false, 1));
            wait_for(|| core.media_offers().len() == 1).await;
            wait_for(|| lens.switched().len() == 4).await; // ended by its time limit
        };
        tokio::time::timeout(Duration::from_secs(10), satellite.run_until(filming)).await.unwrap().unwrap();
        let picture = &core.received_of("media.snapshot")[0]["payload"];
        assert_eq!(picture["of"], "picture-1");
        let jpeg = base64::engine::general_purpose::STANDARD.decode(picture["jpeg"].as_str().unwrap()).unwrap();
        assert_eq!(jpeg[..2], [0xff, 0xd8], "a JPEG");
        let sdp = core.media_offers()[0].1["sdp"].as_str().unwrap().to_owned();
        assert!(sdp.contains("m=video") && sdp.contains("VP8") && !sdp.contains("m=audio"), "{sdp}");
        assert_eq!(lens.switched(), ["on", "off", "on", "off"]);
        assert_eq!(light.shown(), [true, false, true, false]);
    }
}

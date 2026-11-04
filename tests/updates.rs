//! Updating the device's own software (`device.update`, U1 to U3), against a fake core: what is
//! reported, what is installed on request and what is not, and going back when a new version is
//! not welcomed in time.
#![allow(clippy::unwrap_used)] // test helpers fail loudly

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nestor_satellite::{
    Device, Error, IDENTIFY, Language, Manifest, Satellite, SatelliteBuilder, Software, TokenStore, UPDATE,
    UpdateError, Updater,
};
use serde_json::{Value, json};
use support::{FakeCore, tokens, wait_for};

/// An updater that finds `newer`, notes each version it installs, and fails installing `broken`.
#[derive(Clone, Default)]
struct Shelf {
    newer: Option<&'static str>,
    broken: Option<&'static str>,
    installed: Arc<Mutex<Vec<String>>>,
    checks: Arc<AtomicUsize>,
}

impl Shelf {
    fn installed(&self) -> Vec<String> {
        self.installed.lock().unwrap().clone()
    }
}

#[async_trait]
impl Updater for Shelf {
    async fn newer(&self, _software: &Software) -> Result<Option<String>, UpdateError> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        Ok(self.newer.map(str::to_owned))
    }

    async fn install(&self, software: &Software, version: &str) -> Result<(), UpdateError> {
        assert_eq!(software.name(), "nestor-test-driver", "the driver's own package");
        if self.broken == Some(version) {
            return Err(UpdateError::new("no such version"));
        }
        self.installed.lock().unwrap().push(version.to_owned());
        Ok(())
    }
}

fn manifest(version: &str) -> Manifest {
    let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon").unwrap();
    Manifest::new(device, [IDENTIFY]).unwrap().with_software(Software::new("nestor-test-driver", version).unwrap())
}

/// A paired device running `version`, with `shelf`, that notes when it starts again.
fn satellite(url: &str, tokens: &TokenStore, version: &str, shelf: &Shelf, restarts: &Arc<AtomicUsize>) -> Satellite {
    let restarts = restarts.clone();
    builder(url, tokens, version)
        .updater(shelf.clone())
        .restart(move || {
            restarts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .update_grace(Duration::from_millis(300))
        .build()
        .unwrap()
}

fn builder(url: &str, tokens: &TokenStore, version: &str) -> SatelliteBuilder {
    Satellite::builder(url, manifest(version), tokens.clone())
        .language(Language::En)
        .backoff(Duration::from_millis(20), Duration::from_millis(40))
}

fn paired(url: &str) -> (tempfile::TempDir, TokenStore) {
    let (dir, tokens) = tokens();
    tokens.save(url, "enceinte-salon", "nst1_p0").unwrap();
    (dir, tokens)
}

fn update(version: &str) -> Value {
    support::envelope("intent.update", "u-1", json!({"version": version}))
}

fn statuses(core: &FakeCore) -> Vec<Value> {
    core.received_of("update.status").iter().map(|m| m["payload"].clone()).collect()
}

fn pending(tokens: &TokenStore) -> Option<Value> {
    let path = format!("{}.update.json", tokens.path().display());
    std::fs::read(path).ok().map(|bytes| serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn the_newer_version_it_found_is_reported_installed_when_asked_and_started() {
    let core =
        FakeCore { script: vec![update("0.2.0")], script_after: Duration::from_millis(200), ..Default::default() };
    let (url, core) = core.serve().await;
    let (_dir, tokens) = paired(&url);
    let (shelf, restarts) = (Shelf { newer: Some("0.2.0"), ..Default::default() }, Arc::default());
    let satellite = satellite(&url, &tokens, "0.1.0", &shelf, &restarts);
    assert!(satellite.manifest().declares(UPDATE), "an updater declares device.update");

    satellite.run().await.unwrap();
    wait_for(|| statuses(&core).len() == 2).await; // the core reads the last frames on its own task

    assert_eq!(restarts.load(Ordering::SeqCst), 1, "started again on the new version");
    assert_eq!(shelf.installed(), ["0.2.0"]);
    assert_eq!(
        statuses(&core),
        [json!({"state": "available", "version": "0.2.0"}), json!({"state": "installing", "version": "0.2.0"})]
    );
    assert_eq!(core.received_of("ack")[0]["payload"]["of"], "u-1");
    assert_eq!(pending(&tokens), Some(json!({"from": "0.1.0", "to": "0.2.0", "failed": false})));
}

#[tokio::test]
async fn a_version_it_did_not_report_is_never_installed() {
    for (newer, asked) in [(None, "0.2.0"), (Some("0.2.0"), "0.0.1"), (Some("0.2.0"), "0.3.0")] {
        let core =
            FakeCore { script: vec![update(asked)], script_after: Duration::from_millis(200), ..Default::default() };
        let (url, core) = core.serve().await;
        let (_dir, tokens) = paired(&url);
        let (shelf, restarts) = (Shelf { newer, ..Default::default() }, Arc::default());
        let satellite = satellite(&url, &tokens, "0.1.0", &shelf, &restarts);
        satellite.run_until(wait_for(|| statuses(&core).len() == 2)).await.unwrap();

        assert!(shelf.installed().is_empty(), "{asked} installed");
        assert_eq!(core.received_of("ack").len(), 1, "acknowledged all the same");
        let reported = match newer {
            Some(version) => json!({"state": "available", "version": version}),
            None => json!({"state": "current"}),
        };
        assert_eq!(statuses(&core), [reported.clone(), reported], "it says again where it stands");
        assert_eq!(restarts.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn a_failed_install_is_reported_and_the_link_stays() {
    let core =
        FakeCore { script: vec![update("0.2.0")], script_after: Duration::from_millis(200), ..Default::default() };
    let (url, core) = core.serve().await;
    let (_dir, tokens) = paired(&url);
    let shelf = Shelf { newer: Some("0.2.0"), broken: Some("0.2.0"), ..Default::default() };
    let restarts = Arc::default();
    let satellite = satellite(&url, &tokens, "0.1.0", &shelf, &restarts);
    let observer = satellite.observer();
    satellite.run_until(wait_for(|| statuses(&core).len() == 3)).await.unwrap();

    assert_eq!(statuses(&core)[2], json!({"state": "failed", "version": "0.2.0"}));
    assert_eq!(pending(&tokens), None, "nothing left in progress");
    assert_eq!((restarts.load(Ordering::SeqCst), observer.connections()), (0, 1));
}

#[tokio::test]
async fn a_new_version_welcomed_in_time_keeps_running_and_reports_where_it_stands() {
    let (url, core) = FakeCore::default().serve().await;
    let (_dir, tokens) = paired(&url);
    std::fs::write(format!("{}.update.json", tokens.path().display()), r#"{"from":"0.1.0","to":"0.2.0"}"#).unwrap();
    let (shelf, restarts) = (Shelf::default(), Arc::default());
    let satellite = satellite(&url, &tokens, "0.2.0", &shelf, &restarts);
    let ran = tokio::time::timeout(Duration::from_millis(800), satellite.run()).await;

    assert!(ran.is_err(), "still running past the grace period");
    assert_eq!(statuses(&core), [json!({"state": "current"})]);
    assert_eq!(pending(&tokens), None, "the update is done");
    assert!(shelf.installed().is_empty() && restarts.load(Ordering::SeqCst) == 0);
}

#[tokio::test]
async fn a_new_version_not_welcomed_in_time_goes_back_and_the_old_one_reports_the_failure() {
    let (_dir, tokens) = paired("http://127.0.0.1:9"); // nothing answers: never welcomed
    std::fs::write(format!("{}.update.json", tokens.path().display()), r#"{"from":"0.1.0","to":"0.2.0"}"#).unwrap();
    let (shelf, restarts) = (Shelf::default(), Arc::default());
    satellite("http://127.0.0.1:9", &tokens, "0.2.0", &shelf, &restarts).run().await.unwrap();

    assert_eq!(shelf.installed(), ["0.1.0"], "the version that ran before, installed again");
    assert_eq!(restarts.load(Ordering::SeqCst), 1);
    assert_eq!(pending(&tokens), Some(json!({"from": "0.1.0", "to": "0.2.0", "failed": true})));

    // The old version, started again, against a core that answers.
    let (url, core) = FakeCore::default().serve().await;
    let (_dir, back) = paired(&url);
    std::fs::copy(format!("{}.update.json", tokens.path().display()), format!("{}.update.json", back.path().display()))
        .unwrap();
    let shelf = Shelf { newer: Some("0.2.0"), ..Default::default() };
    let satellite = satellite(&url, &back, "0.1.0", &shelf, &restarts);
    satellite.run_until(wait_for(|| statuses(&core).len() == 2)).await.unwrap();

    assert_eq!(
        statuses(&core),
        [json!({"state": "failed", "version": "0.2.0"}), json!({"state": "available", "version": "0.2.0"})]
    );
    assert_eq!(pending(&back), None, "said once");
}

#[tokio::test]
async fn without_device_update_allowed_nothing_is_reported_nor_installed() {
    let core = FakeCore {
        withheld: vec![UPDATE],
        script: vec![update("0.2.0"), support::envelope("intent.identify", "i-1", json!({}))],
        script_after: Duration::from_millis(100),
        ..Default::default()
    };
    let (url, core) = core.serve().await;
    let (_dir, tokens) = paired(&url);
    let (shelf, restarts) = (Shelf { newer: Some("0.2.0"), ..Default::default() }, Arc::default());
    let satellite = satellite(&url, &tokens, "0.1.0", &shelf, &restarts);
    satellite.run_until(wait_for(|| core.received_of("ack").len() == 2)).await.unwrap();

    assert!(statuses(&core).is_empty());
    assert!(shelf.installed().is_empty());
    assert_eq!(shelf.checks.load(Ordering::SeqCst), 0, "its source is not even asked");
}

#[test]
fn a_manifest_declaring_device_update_without_an_updater_is_refused() {
    let (_dir, tokens) = tokens();
    let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon").unwrap();
    let manifest = Manifest::new(device, [UPDATE]).unwrap();
    let built = Satellite::builder("http://127.0.0.1:9", manifest, tokens).build();
    assert!(matches!(built, Err(Error::NotProvided(capability)) if capability == UPDATE));
}

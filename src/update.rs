//! Updating the device's own software when a person asks (`device.update`, rules U1 to U3).
//!
//! The driver gives an [`Updater`]: where its software comes from and how a version is installed.
//! The core never names a source nor chooses a version: once welcomed, the satellite reports what
//! its updater found (`update.status`), checks again once a day, and installs only that version when
//! asked (`intent.update`), never an older one. It then starts again on it (a [`Restart`]).
//! The new version must be welcomed within a grace period (60 s), or it reinstalls the version that
//! ran before, starts again on it, and that one reports the failure once welcomed.
//!
//! What is pending lives next to the token (`<token>.update.json`): `{"from", "to", "failed"}`.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::protocol::Software;
use crate::wire::{self, UpdateState};

/// How often the updater is asked again while the link is up.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);
/// How long a new version has to be welcomed before going back to the one that ran before.
pub(crate) const GRACE: Duration = Duration::from_secs(60);

/// What an [`Updater`] could not do, in English, for the log.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct UpdateError(pub String);

impl UpdateError {
    /// An error saying `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Where the device's software comes from, and how a version of it is installed. Giving one to the
/// [`SatelliteBuilder`](crate::SatelliteBuilder) declares `device.update`.
#[async_trait]
pub trait Updater: Send + Sync + 'static {
    /// The newest version of `software` at its own source, when it is newer than the one running
    /// (`software.version()`); `None` when the running one is the newest.
    async fn newer(&self, software: &Software) -> Result<Option<String>, UpdateError>;

    /// Install `version` of `software`, so that the next start of this program runs it. Also called
    /// with the version that ran before, when a new one was not welcomed in time.
    async fn install(&self, software: &Software, version: &str) -> Result<(), UpdateError>;
}

/// What starts the newly installed version. By default the process is replaced by the same program
/// with the same arguments, as found when the satellite was built (Unix only; it returns only on
/// failure). A driver under a supervisor may exit instead and let it start the program again.
pub type Restart = Arc<dyn Fn() -> io::Result<()> + Send + Sync>;

/// The program running now, with its arguments, captured before an update replaces it on disk
/// (Linux then names it "(deleted)").
fn reexec_this() -> Restart {
    let program = std::env::current_exe();
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    Arc::new(move || match &program {
        Ok(program) => reexec(program, &arguments),
        Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
    })
}

#[cfg(unix)]
fn reexec(program: &Path, arguments: &[OsString]) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    Err(std::process::Command::new(program).args(arguments).exec())
}

#[cfg(not(unix))]
fn reexec(_program: &Path, _arguments: &[OsString]) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "restarting in place needs Unix: give the builder a restart"))
}

/// The update in progress, kept next to the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Pending {
    /// The version that ran when it was installed.
    from: String,
    /// The version installed.
    to: String,
    /// `to` was not welcomed in time: `from` was installed again.
    #[serde(default)]
    failed: bool,
}

/// The satellite's side of `device.update`: what the updater found, and the update in progress.
pub(crate) struct Updates {
    updater: Arc<dyn Updater>,
    software: Software,
    pending: PathBuf,
    /// The newer version the updater found, the only one ever installed on request.
    found: Mutex<Option<String>>,
    installing: AtomicBool,
    /// A link was welcomed since this run started.
    welcomed: AtomicBool,
    grace: Duration,
    pub(crate) restart: Restart,
}

impl Updates {
    /// `restart` starts the version just installed (by default, the same program re-executed);
    /// `grace` is how long it has to be welcomed.
    pub(crate) fn new(
        updater: Arc<dyn Updater>,
        software: Software,
        token_file: &Path,
        restart: Option<Restart>,
        grace: Duration,
    ) -> Self {
        let mut pending = token_file.as_os_str().to_owned();
        pending.push(".update.json");
        Self {
            updater,
            software,
            pending: PathBuf::from(pending),
            found: Mutex::new(None),
            installing: AtomicBool::new(false),
            welcomed: AtomicBool::new(false),
            grace,
            restart: restart.unwrap_or_else(reexec_this),
        }
    }

    /// A link was welcomed: an update to the running version is confirmed (U3).
    pub(crate) fn welcomed(&self) {
        self.welcomed.store(true, Ordering::SeqCst);
        if let Some(pending) = self.read()
            && !pending.failed
            && pending.to == self.software.version()
        {
            info!(from = %pending.from, to = %pending.to, "the new version was welcomed: update done");
            self.clear();
        }
    }

    /// Once welcomed and allowed `device.update`: say how an update ended, then what the updater
    /// finds, now and once a day (U1). Runs until the link ends.
    pub(crate) async fn report(self: Arc<Self>, outbox: mpsc::UnboundedSender<String>) -> crate::Result<()> {
        if let Some(pending) = self.read() {
            warn!(version = %pending.to, "the update did not take: running {}", self.software.version());
            send(&outbox, UpdateState::Failed, Some(&pending.to));
            self.clear();
        }
        loop {
            self.check(&outbox).await;
            tokio::time::sleep(CHECK_EVERY).await;
        }
    }

    async fn check(&self, outbox: &mpsc::UnboundedSender<String>) {
        let newer = match self.updater.newer(&self.software).await {
            Ok(newer) => newer.filter(|v| v != self.software.version()),
            Err(error) => {
                warn!(%error, "could not check for a newer version");
                return;
            }
        };
        if let Ok(mut found) = self.found.lock() {
            found.clone_from(&newer);
        }
        match newer {
            Some(version) => send(outbox, UpdateState::Available, Some(&version)),
            None => send(outbox, UpdateState::Current, None),
        }
    }

    /// `intent.update {version}` (U2): install it only when it is the newer version found here;
    /// `true` once installed, when the satellite must start again on it.
    pub(crate) async fn install(&self, version: &str, outbox: &mpsc::UnboundedSender<String>) -> bool {
        let found = self.found.lock().ok().and_then(|f| f.clone());
        if found.as_deref() != Some(version) {
            warn!(%version, ?found, "asked to install a version this device did not report: refused");
            match found {
                Some(found) => send(outbox, UpdateState::Available, Some(&found)),
                None => send(outbox, UpdateState::Current, None),
            }
            return false;
        }
        if self.installing.swap(true, Ordering::SeqCst) {
            return false; // already on it
        }
        send(outbox, UpdateState::Installing, Some(version));
        let pending = Pending { from: self.software.version().to_owned(), to: version.to_owned(), failed: false };
        let installed = match self.write(&pending) {
            Ok(()) => self.updater.install(&self.software, version).await,
            Err(error) => Err(UpdateError::new(format!("could not keep the update in progress: {error}"))),
        };
        self.installing.store(false, Ordering::SeqCst);
        match installed {
            Ok(()) => {
                info!(%version, "installed: starting again on it");
                true
            }
            Err(error) => {
                warn!(%error, %version, "the update failed");
                self.clear();
                send(outbox, UpdateState::Failed, Some(version));
                false
            }
        }
    }

    /// When this run is a new version not welcomed within the grace period: install the one that
    /// ran before and return, for the satellite to start again on it. Otherwise never returns.
    pub(crate) async fn unless_welcomed(&self) {
        let Some(pending) = self.read().filter(|p| !p.failed && p.to == self.software.version()) else {
            return std::future::pending().await;
        };
        tokio::time::sleep(self.grace).await;
        if self.welcomed.load(Ordering::SeqCst) {
            return std::future::pending().await;
        }
        warn!(version = %pending.to, back_to = %pending.from, "the new version was not welcomed in time: going back");
        if let Err(error) = self.updater.install(&self.software, &pending.from).await {
            warn!(%error, "could not go back to the version that ran before");
        }
        if let Err(error) = self.write(&Pending { failed: true, ..pending }) {
            warn!(%error, "could not note the failed update");
        }
    }

    fn read(&self) -> Option<Pending> {
        serde_json::from_slice(&fs::read(&self.pending).ok()?).ok()
    }

    fn write(&self, pending: &Pending) -> io::Result<()> {
        fs::write(&self.pending, serde_json::to_vec(pending)?)
    }

    fn clear(&self) {
        if let Err(error) = fs::remove_file(&self.pending)
            && error.kind() != io::ErrorKind::NotFound
        {
            warn!(%error, "could not clear the update in progress");
        }
    }
}

fn send(outbox: &mpsc::UnboundedSender<String>, state: UpdateState, version: Option<&str>) {
    match wire::update_status(state, version) {
        Ok(message) => {
            let _ = outbox.send(message.to_string());
        }
        Err(error) => warn!(%error, "not an update.status"),
    }
}

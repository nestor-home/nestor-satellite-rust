//! Pairing: ask the core, show the code, wait for a household member to confirm it, keep the token.
//!
//! The token is the device's only credential: it is written once, readable by its owner only (mode
//! 600), and forgotten when the core refuses it (revoked, or paired again elsewhere). The file
//! format is the Python SDK's, so a device can switch SDKs without pairing again.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::error::{Error, Result};
use crate::protocol::Manifest;

/// One device's token for one core, in a JSON file.
#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    core: String,
    device_id: String,
    token: String,
}

impl TokenStore {
    /// A store at `path`; a leading `~/` is the user's home.
    pub fn new(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let path = match (path.strip_prefix("~"), home()) {
            (Ok(rest), Some(home)) => home.join(rest),
            _ => path.to_path_buf(),
        };
        Self { path }
    }

    /// Where the token is kept.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The token of `device_id` for `core`, if one was saved (a token for another core or device
    /// is useless here).
    pub fn load(&self, core: &str, device_id: &str) -> Option<String> {
        let saved: Saved = serde_json::from_slice(&fs::read(&self.path).ok()?).ok()?;
        (saved.core == core && saved.device_id == device_id).then_some(saved.token)
    }

    /// Keep the token, readable by its owner only.
    pub fn save(&self, core: &str, device_id: &str, token: &str) -> io::Result<()> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            create_private_dir(parent)?;
        }
        let saved = Saved { core: core.to_owned(), device_id: device_id.to_owned(), token: token.to_owned() };
        let mut file = open_private(&self.path)?;
        file.write_all(&serde_json::to_vec(&saved)?)?;
        restrict(&self.path) // an older file may have been more open
    }

    /// Forget the token (nothing to do when there is none).
    pub fn forget(&self) -> io::Result<()> {
        match fs::remove_file(&self.path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

#[cfg(unix)]
fn open_private(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().write(true).create(true).truncate(true).open(path)
}

#[cfg(unix)]
fn restrict(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// What the core answers a pairing request: the code to show, the id to poll.
#[derive(Debug, Clone, Deserialize)]
pub struct PairingTicket {
    /// The id to poll.
    pub pairing_id: String,
    /// The 6-digit code the person confirms in Nestor.
    pub code: String,
    /// When the code expires (ISO 8601).
    pub expires_at: String,
}

#[derive(Deserialize)]
struct PairingState {
    status: String,
    token: Option<String>,
}

/// Print the code for the person to confirm: the default way of showing it.
pub fn show_code(code: &str) {
    println!("Pairing code: {code} — confirm it in Nestor (admin, or `nestor satellites approve {code}`)");
}

/// Pair until a household member confirms a code; a code that expires is replaced by a new one.
///
/// `core` is the core's base URL (`http://nestor.local:7860`). The token comes once: save it.
pub async fn pair(
    http: &Client,
    core: &str,
    manifest: &Manifest,
    on_code: &(dyn Fn(&str) + Send + Sync),
    poll: Duration,
) -> Result<String> {
    loop {
        let response = http.post(format!("{core}/satellites/pair")).json(&manifest.to_json()).send().await?;
        if response.status() == StatusCode::UNPROCESSABLE_ENTITY {
            return Err(Error::ManifestRefused(detail(response).await));
        }
        let ticket: PairingTicket = success(response).await?.json().await?;
        on_code(&ticket.code);
        loop {
            let response = http.get(format!("{core}/satellites/pair/{}", ticket.pairing_id)).send().await?;
            let state: PairingState = success(response).await?.json().await?;
            match (state.status.as_str(), state.token) {
                ("approved", Some(token)) => return Ok(token),
                ("pending", _) => tokio::time::sleep(poll).await,
                (status, _) => {
                    // expired, or claimed by someone else: start again
                    info!(%status, "pairing ended without a token, asking again");
                    break;
                }
            }
        }
    }
}

pub(crate) async fn success(response: Response) -> Result<Response> {
    if response.status().is_success() {
        Ok(response)
    } else {
        let status = response.status().as_u16();
        Err(Error::Core { status, detail: detail(response).await })
    }
}

async fn detail(response: Response) -> String {
    let text = response.text().await.unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_owned))
        .unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_kept_for_its_core_and_device_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path().join("nested/token.json"));
        assert_eq!(store.load("http://core", "a"), None);
        store.save("http://core", "a", "nst1_secret").unwrap();
        assert_eq!(store.load("http://core", "a").as_deref(), Some("nst1_secret"));
        assert_eq!(store.load("http://other", "a"), None);
        assert_eq!(store.load("http://core", "b"), None);
        store.forget().unwrap();
        store.forget().unwrap();
        assert_eq!(store.load("http://core", "a"), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_token_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.json");
        fs::write(&path, "{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        TokenStore::new(&path).save("c", "a", "t").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_file_that_is_not_a_token_is_no_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.json");
        fs::write(&path, "not json").unwrap();
        assert_eq!(TokenStore::new(&path).load("c", "a"), None);
    }

    #[test]
    fn a_path_under_home_is_expanded() {
        if let Some(home) = home() {
            assert_eq!(TokenStore::new("~/x.json").path(), home.join("x.json"));
        }
    }
}

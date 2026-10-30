//! The bundled voice (`voice/`) is a copy of nestor-satellite-python's: every SDK says the device
//! code with the same clips. `../nestor-satellite-python`, or `NESTOR_SATELLITE_PYTHON`; skipped when
//! there is none, unless `NESTOR_VOICE=required` (CI).
#![allow(clippy::unwrap_used)] // test helpers fail loudly

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Every file under `dir`, by its path relative to it.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|name| name.to_string_lossy().starts_with('.')) {
                continue; // .DS_Store and the like
            }
            if path.is_dir() {
                pending.push(path);
            } else {
                let name = path.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
                found.insert(name, fs::read(&path).unwrap());
            }
        }
    }
    found
}

#[test]
fn the_bundled_voice_is_the_python_sdk_s() {
    let required = std::env::var("NESTOR_VOICE").is_ok_and(|v| v == "required");
    let python = std::env::var_os("NESTOR_SATELLITE_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../nestor-satellite-python"))
        .join("src/nestor_satellite/voice");
    if !python.is_dir() {
        assert!(!required, "NESTOR_VOICE=required, but there is no {}", python.display());
        eprintln!("skipped: no nestor-satellite-python at {} (set NESTOR_SATELLITE_PYTHON)", python.display());
        return;
    }
    let ours = files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("voice"));
    let theirs = files(&python);
    let names = |files: &BTreeMap<String, Vec<u8>>| files.keys().cloned().collect::<Vec<_>>();
    assert_eq!(names(&ours), names(&theirs), "copy voice/ from nestor-satellite-python (AGENTS.md)");
    for (name, bytes) in &ours {
        assert!(bytes == &theirs[name], "{name} differs: copy voice/ from nestor-satellite-python (AGENTS.md)");
    }
}

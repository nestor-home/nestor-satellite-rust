//! The `nestor-satellite` binary against the Nestor server's black-box conformance suite, as a
//! robot-less speaker (`scripts/conformance.py`).
//!
//! Needs the server's Python environment: `NESTOR_PYTHON`, or `../nestor/.venv/bin/python`.
//! Skipped when there is none, unless `NESTOR_CONFORMANCE=required` (CI).
#![cfg(feature = "cli")]
#![allow(clippy::unwrap_used)] // test helpers fail loudly

use std::path::{Path, PathBuf};
use std::process::Command;

fn python() -> Option<PathBuf> {
    let candidate = std::env::var_os("NESTOR_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../nestor/.venv/bin/python"));
    candidate.exists().then_some(candidate)
}

#[test]
fn the_binary_passes_the_servers_conformance_suite_as_a_speaker() {
    let required = std::env::var("NESTOR_CONFORMANCE").is_ok_and(|v| v == "required");
    let (Some(python), Some(binary)) = (python(), option_env!("CARGO_BIN_EXE_nestor-satellite")) else {
        assert!(!required, "NESTOR_CONFORMANCE=required, but no Nestor Python environment was found");
        eprintln!("skipped: no Nestor Python environment (set NESTOR_PYTHON)");
        return;
    };
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/conformance.py");
    let output = Command::new(python).arg(script).arg(binary).output().unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    println!("{report}");
    assert!(output.status.success(), "{report}\n{}", String::from_utf8_lossy(&output.stderr));
    for check in [
        "PASS  reconnects with the same token after a hang-up",
        "PASS  pairs with the enrollment code of NESTOR_PAIRING_URI",
        "PASS  presents a refused enrollment code (used) only once",
        "PASS  waits for Retry-After when rate-limited",
        "PASS  keeps polling after a pending identify",
    ] {
        assert!(report.contains(check), "{check}: not run\n{report}");
    }
}

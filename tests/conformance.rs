//! The `nestor-satellite` binary against the Nestor server's black-box conformance suite, as a
//! robot-less speaker: the published server's `nestor satellites conformance`, run through `uvx`,
//! at the version pinned in `.nestor-version`.
//!
//! Skipped when `uvx` is not on the PATH, unless `NESTOR_CONFORMANCE=required` (CI).
#![cfg(feature = "cli")]
#![allow(clippy::unwrap_used)] // test helpers fail loudly

use std::process::Command;

const SERVER_VERSION: &str = include_str!("../.nestor-version");

fn has_uvx() -> bool {
    Command::new("uvx").arg("--version").output().is_ok_and(|output| output.status.success())
}

#[test]
fn the_binary_passes_the_servers_conformance_suite_as_a_speaker() {
    let required = std::env::var("NESTOR_CONFORMANCE").is_ok_and(|v| v == "required");
    let (true, Some(binary)) = (has_uvx(), option_env!("CARGO_BIN_EXE_nestor-satellite")) else {
        assert!(!required, "NESTOR_CONFORMANCE=required, but uvx is not on the PATH");
        eprintln!("skipped: no uvx to run the server's conformance suite");
        return;
    };
    let server = format!("nestor-home[voice]=={}", SERVER_VERSION.trim());
    let output = Command::new("uvx")
        .args(["--from", &server, "nestor", "satellites", "conformance", "--deadline", "15", "--"])
        .args([binary, "run", "--core", "{url}", "--room", "salon", "--id", "enceinte-salon"])
        .args(["--model", "conformance", "--token-file", "{state}/token.json", "--null-audio", "--verbose"])
        .env("LC_ALL", "en_US.UTF-8")
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    println!("{report}");
    assert!(output.status.success(), "{report}\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(!report.lines().any(|line| line.starts_with("FAIL")), "{report}");
    for check in [
        "PASS  reconnects with the same token after a hang-up (W4)",
        "PASS  pairs with the enrollment code of NESTOR_PAIRING_URI, no code typed (P3, P4)",
        "PASS  presents a refused enrollment code (used) only once (P5)",
        "PASS  waits for Retry-After when rate-limited (P6)",
        "PASS  keeps polling after a pending identify (I1)",
    ] {
        assert!(report.lines().any(|line| line == check), "{check}: not run\n{report}");
    }
}

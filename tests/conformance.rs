//! The `nestor-satellite` binary against the Nestor server's black-box conformance suite: the
//! published server's `nestor satellites conformance`, run through `uvx`, at the version pinned in
//! `.nestor-version` (or from `NESTOR_CONFORMANCE_FROM`, a `uvx --from` source such as
//! `'../nestor[voice]'`, to test against a local server). Once as a robot-less speaker, once as a
//! text satellite in a terminal.
//!
//! Skipped when `uvx` is not on the PATH, unless `NESTOR_CONFORMANCE=required` (CI).
#![cfg(feature = "cli")]
#![allow(clippy::unwrap_used)] // test helpers fail loudly

use std::process::Command;

const SERVER_VERSION: &str = include_str!("../.nestor-version");

fn has_uvx() -> bool {
    Command::new("uvx").arg("--version").output().is_ok_and(|output| output.status.success())
}

/// The suite's report on the binary run with `options`; `None` when it cannot run here.
fn conformance(options: &[&str]) -> Option<String> {
    let required = std::env::var("NESTOR_CONFORMANCE").is_ok_and(|v| v == "required");
    let (true, Some(binary)) = (has_uvx(), option_env!("CARGO_BIN_EXE_nestor-satellite")) else {
        assert!(!required, "NESTOR_CONFORMANCE=required, but uvx is not on the PATH");
        eprintln!("skipped: no uvx to run the server's conformance suite");
        return None;
    };
    let server = std::env::var("NESTOR_CONFORMANCE_FROM")
        .unwrap_or_else(|_| format!("nestor-home[voice]=={}", SERVER_VERSION.trim()));
    let output = Command::new("uvx")
        .args(["--from", &server, "nestor", "satellites", "conformance", "--deadline", "15", "--"])
        .args([binary, "run", "--core", "{url}", "--room", "salon", "--id", "enceinte-salon"])
        .args(["--model", "conformance", "--token-file", "{state}/token.json", "--verbose"])
        .args(options)
        .env("LC_ALL", "en_US.UTF-8")
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&output.stdout).into_owned();
    println!("{report}");
    assert!(output.status.success(), "{report}\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(!report.lines().any(|line| line.starts_with("FAIL")), "{report}");
    for check in [
        "PASS  opens its link with its token, a hello first, as the paired device (L1)",
        "PASS  reconnects with the same token after the core closes its link (L5)",
        "PASS  forgets a refused token and pairs again (L2, P8)",
        "PASS  pairs with the enrollment code of NESTOR_PAIRING_URI, no code typed (P3, P4)",
        "PASS  presents a refused enrollment code (used) only once (P5)",
        "PASS  waits for Retry-After when rate-limited (P6)",
        "PASS  keeps polling after a pending identify (I1)",
    ] {
        assert!(report.lines().any(|line| line == check), "{check}: not run\n{report}");
    }
    Some(report)
}

#[test]
fn the_binary_passes_the_servers_conformance_suite_as_a_speaker() {
    let Some(report) = conformance(&["--null-audio"]) else {
        return;
    };
    for check in [
        "PASS  receives the core's audio (A1, A2)",
        "PASS  sends its microphone audio (A1, A2)",
        "PASS  offers no audio when audio is not allowed (A1)",
    ] {
        assert!(report.lines().any(|line| line == check), "{check}: not run\n{report}");
    }
}

#[test]
fn the_binary_passes_the_servers_conformance_suite_as_a_text_satellite() {
    let Some(report) = conformance(&["--text-only"]) else {
        return;
    };
    assert!(report.lines().any(|line| line.starts_with("PASS  acknowledges intent.text")), "{report}");
    assert!(!report.contains("audio"), "no audio checked: {report}");
}

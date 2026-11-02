//! `nestor-satellite run`: this machine's microphone and speaker as a satellite of a Nestor core, or
//! its terminal (`--text`, `--text-only`: lines typed on stdin, answers printed on stdout).
//!
//!     nestor-satellite run --core http://nestor.local:8471 --room salon
//!     NESTOR_PAIRING_URI=nestor+http://nestor.local:8471/?code=K7QF2M9X nestor-satellite run --room salon
//!     nestor-satellite run --core http://nestor.local:8471 --room bureau --text-only
//!
//! Useful on a Raspberry Pi with a USB speakerphone. The first run pairs with the enrollment code
//! it is given (`--pairing-uri`, `--pairing-code`, or their environment variables), else says a
//! device code through the speaker and logs it, to type in Nestor; the token (and the enrollment
//! codes refused, as digests) is then kept in `~/.config/nestor-satellite/<id>.json`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use nestor_satellite::devices::{Microphone, Speaker, device_names};
use nestor_satellite::{
    Device, Enrollment, EnrollmentCode, Error, IDENTIFY, Language, Manifest, Observer, PairingUri, Satellite,
    TEXT_INPUT, TokenStore,
};
use tracing_subscriber::EnvFilter;

/// Run a Nestor satellite on this machine.
#[derive(Parser)]
#[command(name = "nestor-satellite", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once
enum Command {
    /// Run a microphone + speaker satellite, or a text one in this terminal.
    Run(Run),
    /// List the audio devices.
    Devices,
}

#[derive(clap::Args)]
struct Run {
    /// The core's voice server, e.g. http://nestor.local:8471 (default: the pairing URI's).
    #[arg(long, env = "NESTOR_CORE")]
    core: Option<String>,
    /// The pairing URI made in Nestor (nestor+http://host:port/?code=K7QF2M9X): the core and an
    /// enrollment code.
    #[arg(long, env = "NESTOR_PAIRING_URI", conflicts_with = "pairing_code")]
    pairing_uri: Option<PairingUri>,
    /// An enrollment code made in Nestor (K7QF-2M9X), the core given by --core.
    #[arg(long, env = "NESTOR_PAIRING_CODE")]
    pairing_code: Option<EnrollmentCode>,
    /// The language it speaks to people: en or fr (default: from LANG, else en).
    #[arg(long, value_parser = parse_language)]
    language: Option<Language>,
    /// The room the satellite is in (a slug: salon, cuisine…).
    #[arg(long)]
    room: String,
    /// This device's id (default: the host name, as a slug).
    #[arg(long)]
    id: Option<String>,
    /// The device model it declares.
    #[arg(long, default_value = "rust-satellite")]
    model: String,
    /// Its maker, as a slug (optional).
    #[arg(long)]
    brand: Option<String>,
    /// Where the token is kept (default: ~/.config/nestor-satellite/<id>.json).
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// Talk in writing too: lines typed on stdin are sent, the assistant's words printed on stdout.
    #[arg(long)]
    text: bool,
    /// Talk in writing only: no microphone, no speaker, no WebRTC.
    #[arg(long, conflicts_with_all = ["text", "null_audio", "input_device", "output_device", "ice_servers"])]
    text_only: bool,
    /// The input device (part of its name; default: the system's).
    #[arg(long)]
    input_device: Option<String>,
    /// The output device (part of its name; default: the system's).
    #[arg(long)]
    output_device: Option<String>,
    /// STUN/TURN server, only needed beyond the local network (repeatable).
    #[arg(long = "ice-server")]
    ice_servers: Vec<String>,
    /// No sound card: send silence and drop the core's voice (conformance runs, CI).
    #[arg(long)]
    null_audio: bool,
    /// Log more, down to each step of the link (`RUST_LOG` takes precedence).
    #[arg(short, long)]
    verbose: bool,
}

fn default_id() -> String {
    let host = gethostname::gethostname().to_string_lossy().to_lowercase();
    let first = host.split('.').next().unwrap_or_default();
    let slug: String =
        first.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' }).collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "satellite" } else { slug };
    slug.chars().take(64).collect()
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Devices => match device_names() {
            Ok((inputs, outputs)) => {
                println!("inputs:");
                inputs.iter().for_each(|name| println!("  {name}"));
                println!("outputs:");
                outputs.iter().for_each(|name| println!("  {name}"));
                ExitCode::SUCCESS
            }
            Err(error) => fail(&error),
        },
        Command::Run(run) => {
            init_logging(run.verbose);
            let language = run.language.unwrap_or_else(Language::from_env);
            let satellite = match satellite(run, language) {
                Ok(satellite) => satellite,
                Err(error) => return fail(&error),
            };
            if satellite.manifest().declares(TEXT_INPUT) {
                type_lines(satellite.observer());
            }
            match satellite.run_until(shutdown()).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(&language.describe(&error)),
            }
        }
    }
}

fn satellite(run: Run, language: Language) -> Result<Satellite, String> {
    let id = run.id.unwrap_or_else(default_id);
    let mut device = Device::new(&id, run.model, run.room).map_err(|e| e.to_string())?;
    if let Some(brand) = run.brand {
        device = device.with_brand(brand).map_err(|e| e.to_string())?;
    }
    // No screen: it pairs with an enrollment code, or a device code said through the speaker (bundled
    // clips; not with --null-audio nor --text-only) and read in its log. Text and audio are declared
    // from what is given below.
    let manifest = Manifest::new(device, [IDENTIFY]).map_err(|e| e.to_string())?;
    let tokens = match run.token_file {
        Some(path) => TokenStore::new(path),
        None => TokenStore::new(format!("~/.config/nestor-satellite/{id}.json")),
    };
    let enrollment: Option<Enrollment> = run.pairing_uri.map(Into::into).or(run.pairing_code.map(Into::into));
    let core = match (run.core, enrollment.as_ref().and_then(Enrollment::core)) {
        (Some(core), _) => core,
        (None, Some(core)) => core.to_owned(),
        (None, None) => return Err("give the core's address (--core, NESTOR_CORE) or a pairing URI".to_owned()),
    };
    let mut builder = Satellite::builder(core, manifest, tokens).language(language);
    if let Some(enrollment) = enrollment {
        builder = builder.enrollment(enrollment);
    }
    if run.text || run.text_only {
        builder = builder.text_input().text_display(|text: &str, _last: bool| println!("{text}"));
    }
    if !run.text_only {
        builder = builder.ice_servers(run.ice_servers);
        builder = if run.null_audio {
            builder.null_audio()
        } else {
            let microphone = run.input_device.map_or_else(Microphone::new, Microphone::named);
            let speaker = run.output_device.map_or_else(Speaker::new, Speaker::named);
            builder.source(microphone).sink(speaker)
        };
    }
    builder.build().map_err(|error: Error| language.describe(&error))
}

/// Each line typed on stdin, sent as typed text; the satellite goes on after the end of stdin.
fn type_lines(observer: Observer) {
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match observer.input_text(line) {
                Ok(true) => {}
                Ok(false) => tracing::warn!("not connected to the core: the line was not sent"),
                Err(error) => tracing::warn!(%error, "the line was not sent"),
            }
        }
    });
}

fn parse_language(tag: &str) -> Result<Language, String> {
    Language::parse(tag).ok_or_else(|| format!("{tag:?}: the languages are en and fr"))
}

fn init_logging(verbose: bool) {
    // webrtc-rs logs every unreachable candidate as an error: the SDK reports what matters.
    // The device code and pairing steps are logged at info: they are how a headless device is paired.
    let default = if verbose { "off,nestor_satellite=debug" } else { "off,nestor_satellite=info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
}

/// Ctrl-C, or SIGTERM (systemd, the conformance suite) on Unix.
async fn shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn fail(error: &dyn std::fmt::Display) -> ExitCode {
    eprintln!("nestor-satellite: {error}");
    ExitCode::FAILURE
}

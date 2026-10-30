//! `nestor-satellite run`: this machine's microphone and speaker as a satellite of a Nestor core.
//!
//!     nestor-satellite run --core http://nestor.local:7860 --room salon
//!
//! Useful on a Raspberry Pi with a USB speakerphone. The first run shows a pairing code to confirm
//! in Nestor; the token is then kept in `~/.config/nestor-satellite/<id>.json`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use nestor_satellite::devices::{Microphone, Speaker, device_names};
use nestor_satellite::{AUDIO_CAPTURE, AUDIO_PLAYBACK, Device, Manifest, Satellite, SilenceSource, TokenStore};
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
    /// Run a microphone + speaker satellite.
    Run(Run),
    /// List the audio devices.
    Devices,
}

#[derive(clap::Args)]
struct Run {
    /// The core's voice server, e.g. http://nestor.local:7860.
    #[arg(long, env = "NESTOR_CORE")]
    core: String,
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
    /// Log more (`RUST_LOG` takes precedence).
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
            match satellite(run) {
                Ok(satellite) => match satellite.run_until(shutdown()).await {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => fail(&error),
                },
                Err(error) => fail(&*error),
            }
        }
    }
}

fn satellite(run: Run) -> Result<Satellite, Box<dyn std::error::Error>> {
    let id = run.id.unwrap_or_else(default_id);
    let mut device = Device::new(&id, run.model, run.room)?;
    if let Some(brand) = run.brand {
        device = device.with_brand(brand)?;
    }
    let manifest = Manifest::new(device, [AUDIO_CAPTURE, AUDIO_PLAYBACK])?;
    let tokens = match run.token_file {
        Some(path) => TokenStore::new(path),
        None => TokenStore::new(format!("~/.config/nestor-satellite/{id}.json")),
    };
    let builder = Satellite::builder(run.core, manifest, tokens).ice_servers(run.ice_servers);
    Ok(if run.null_audio {
        builder.source(SilenceSource::new()).build() // the core's voice is decoded and dropped
    } else {
        let microphone = run.input_device.map_or_else(Microphone::new, Microphone::named);
        let speaker = run.output_device.map_or_else(Speaker::new, Speaker::named);
        builder.source(microphone).sink(speaker).build()
    })
}

fn init_logging(verbose: bool) {
    // webrtc-rs logs every unreachable candidate as an error: the SDK reports what matters.
    let default = if verbose { "off,nestor_satellite=info" } else { "off,nestor_satellite=warn" };
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

fn fail(error: &dyn std::error::Error) -> ExitCode {
    eprintln!("nestor-satellite: {error}");
    ExitCode::FAILURE
}

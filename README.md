# nestor-satellite

Rust SDK to write **satellites** for [Nestor](https://github.com/nestor-home/nestor): devices —
a Raspberry Pi speaker, a robot, a screen — that pair with a Nestor core, stream audio over WebRTC,
carry out the core's intents and report what they observe, following the `nestor.satellite/2`
protocol. It is the Rust twin of [nestor-satellite-python](https://github.com/nestor-home/nestor-satellite-python)
and does not depend on the Nestor server.

```rust
use nestor_satellite::devices::{Microphone, Speaker};
use nestor_satellite::{
    AUDIO_CAPTURE, AUDIO_PLAYBACK, Behavior, Body, Device, IDENTIFY, Manifest, Satellite, TokenStore,
};

struct Led;
impl Body for Led {
    fn behavior(&self, behavior: Behavior) {
        println!("attitude: {behavior}"); // light the ring, from a task of its own
    }
    fn identify(&self) {
        println!("blink"); // a person looks for this device: blink for a few seconds
    }
}

#[tokio::main]
async fn main() -> nestor_satellite::Result<()> {
    let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon")?;
    let manifest = Manifest::new(device, [AUDIO_CAPTURE, AUDIO_PLAYBACK, IDENTIFY])?;
    let tokens = TokenStore::new("~/.config/nestor-satellite/enceinte-salon.json");
    let satellite = Satellite::builder("http://nestor.local:7860", manifest, tokens).body(Led);
    satellite.source(Microphone::new()).sink(Speaker::new()).build()?.run().await // pairs once, then reconnects
}
```

## Pairing

A person proves they mean to pair the device, one of two ways; the first proof accepted wins:

- **an enrollment code**, made in the Nestor admin as a pairing URI
  (`nestor+http://nestor.local:7860/?code=K7QF2M9X`) or a code alone (`K7QF-2M9X`). Configured with
  `.enrollment(Enrollment::parse(…)?)`, or in `NESTOR_PAIRING_URI` (`NESTOR_PAIRING_CODE` with the
  core's address given separately), it is sent with the first request and the device is paired at
  once (`enrollment.provisioning`, always declared). While the device waits, a code typed on it is
  handed in with an `Enroller` (`enrollment.input`, declared with `.accepts_enrollment(…)`). This SDK
  drives no camera: `enrollment.qr` is never declared;
- **a device code**, six digits the device presents and the person types in the admin: on a
  `Display` (`device_code.display`), aloud (`device_code.speech`, digits one by one, said twice),
  and always in the log. A speaker without a screen (`audio.playback` declared, a sink given) says
  it by itself, offline, with recorded clips in English or French (`voice/`, the same as the other
  SDKs', CC BY 4.0: see `voice/LICENSE`); a `Voice` of the driver's replaces them. A device with
  neither screen nor voice says in its log that an enrollment code is the way to pair it.

A refused enrollment code is said once (log, display, voice) and never sent again, even after a
restart: the `TokenStore` keeps its SHA-256 (never the code). The device code takes over. Messages to people are in English or French (`.language(…)`, else `LANG`). A body that
can make the device noticed (`Body::identify`, capability `device.identify`) helps a person tell
devices apart. The token is kept in the `TokenStore` (mode 600) and forgotten if the core refuses
it; the refused codes stay.

A screen that shows its code and takes codes typed on its keypad:

```rust
use nestor_satellite::{AUDIO_PLAYBACK, Device, Display, EnrollmentMethod, IDENTIFY, Manifest, Satellite, TokenStore};

struct Screen;
impl Display for Screen {
    fn show_code(&self, text: &str, code: &str) {
        println!("{text}\n    {code}"); // draw it large, from a task of its own
    }
    fn show_message(&self, text: &str) {
        println!("{text}"); // a refused enrollment code, in the device's language
    }
    fn clear(&self) {
        println!("paired");
    }
}

#[tokio::main]
async fn main() -> nestor_satellite::Result<()> {
    let device = Device::new("ecran-cuisine", "raspberry-pi-zero", "cuisine")?;
    let manifest = Manifest::new(device, [AUDIO_PLAYBACK, IDENTIFY])?;
    let tokens = TokenStore::new("~/.config/nestor-satellite/ecran-cuisine.json");
    let satellite = Satellite::builder("http://nestor.local:7860", manifest, tokens)
        .display(Screen) // declares device_code.display
        .accepts_enrollment(EnrollmentMethod::Input) // declares enrollment.input
        .build()?;
    let keypad = satellite.enroller();
    std::thread::spawn(move || {
        for typed in std::io::stdin().lines().map_while(Result::ok) {
            if let Err(error) = keypad.enroll(&typed, EnrollmentMethod::Input) {
                eprintln!("{error}"); // not a code, or the device is not waiting for one
            }
        }
    });
    satellite.run().await
}
```

## The command

With nothing to write, a microphone + speaker satellite (`nestor-satellite devices` lists the sound
cards). Headless, it pairs with the pairing URI made in the admin:

```bash
cargo install --git https://github.com/nestor-home/nestor-satellite-rust --features cli
NESTOR_PAIRING_URI='nestor+http://nestor.local:7860/?code=K7QF2M9X' nestor-satellite run --room salon
# or
nestor-satellite run --room salon --pairing-uri 'nestor+http://nestor.local:7860/?code=K7QF2M9X'
```

Without a code, `nestor-satellite run --core http://nestor.local:7860 --room salon` says a device
code through the speaker and logs it, to type in the admin. `--language fr` speaks French.

Building needs Rust 1.91+, a C compiler and `cmake` (libopus is built from source); on Linux the
`audio` feature also needs ALSA (`apt install libasound2-dev pkg-config`).

## Features

- default: protocol types, wire codec, pairing (codes, pairing URIs, display and voice, enroller)
  and token store, the WebRTC link (webrtc-rs, Opus), the `Body` trait, observations,
  `AudioSource`/`AudioSink` and their test doubles, the English and French catalogue;
- `audio`: `devices::{Microphone, Speaker}` through cpal;
- `cli`: the `nestor-satellite` command (implies `audio`).

## Development

```bash
cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features
```

The tests check the vendored schemas against the server's (`../nestor`) and run the server's
black-box conformance suite against the `nestor-satellite` binary when the server's Python
environment is there (`../nestor/.venv`); see [AGENTS.md](AGENTS.md).

## License

MIT

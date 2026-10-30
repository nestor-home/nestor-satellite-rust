# nestor-satellite

Rust SDK to write **satellites** for [Nestor](https://github.com/nestor-home/nestor): devices —
a Raspberry Pi speaker, a robot, a screen — that pair with a Nestor core, stream audio over WebRTC,
carry out the core's intents and report what they observe, following the `nestor.satellite/1`
protocol. It is the Rust twin of [nestor-satellite-python](https://github.com/nestor-home/nestor-satellite-python)
and does not depend on the Nestor server.

```rust
use nestor_satellite::devices::{Microphone, Speaker};
use nestor_satellite::{AUDIO_CAPTURE, AUDIO_PLAYBACK, Behavior, Body, Device, Manifest, Satellite, TokenStore};

struct Led;
impl Body for Led {
    fn behavior(&self, behavior: Behavior) {
        println!("attitude: {behavior}"); // light the ring, from a task of its own
    }
}

#[tokio::main]
async fn main() -> nestor_satellite::Result<()> {
    let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon")?;
    let manifest = Manifest::new(device, [AUDIO_CAPTURE, AUDIO_PLAYBACK])?;
    let tokens = TokenStore::new("~/.config/nestor-satellite/enceinte-salon.json");
    let satellite = Satellite::builder("http://nestor.local:7860", manifest, tokens).body(Led);
    satellite.source(Microphone::new()).sink(Speaker::new()).build().run().await // pairs once, then reconnects
}
```

Or, with nothing to write, a microphone + speaker satellite (`nestor-satellite devices` lists the
sound cards):

```bash
cargo install --git https://github.com/nestor-home/nestor-satellite-rust --features cli
nestor-satellite run --core http://nestor.local:7860 --room salon
```

Building needs Rust 1.91+, a C compiler and `cmake` (libopus is built from source); on Linux the
`audio` feature also needs ALSA (`apt install libasound2-dev pkg-config`).

## Features

- default: protocol types, wire codec, pairing and token store, the WebRTC link (webrtc-rs, Opus),
  the `Body` trait, observations, `AudioSource`/`AudioSink` and their test doubles;
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

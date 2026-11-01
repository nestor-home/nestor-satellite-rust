# nestor-satellite

Rust SDK to write **satellites** for [Nestor](https://nestorhome.dev): devices (a Raspberry Pi
speaker, a robot, a screen) that pair with a Nestor server, stream audio over WebRTC, carry out its
intents and report what they observe, following the `nestor.satellite/2` protocol. It does not
depend on the Nestor server.

**Documentation:** [nestorhome.dev/docs/sdks/rust](https://nestorhome.dev/docs/sdks/rust/)
(features, pairing options, the command, conformance); the API on
[docs.rs](https://docs.rs/nestor-satellite).

```bash
cargo add nestor-satellite --features audio
```

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

The command, with nothing to write (a microphone and a speaker):

```bash
cargo install nestor-satellite --features cli
NESTOR_PAIRING_URI='nestor+http://nestor.local:7860/?code=K7QF2M9X' nestor-satellite run --room salon
```

Building needs Rust 1.91+, a C compiler and `cmake`; on Linux the `audio` feature also needs ALSA
(`apt install libasound2-dev pkg-config`). The recorded voice that says device codes is shared
under CC BY 4.0: see [`voice/LICENSE`](voice/LICENSE).

## Development

```bash
cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features
```

The tests run the published server's black-box conformance suite against the `nestor-satellite`
binary when `uvx` is on the PATH (server version in `.nestor-version`); see [AGENTS.md](AGENTS.md).

## Releases

release-please (`release.yml`) opens the release pull request; merging it tags `vX.Y.Z`, creates
the GitHub Release and publishes the crate. The tag push runs `binaries.yml`, made by
[dist](https://opensource.axo.dev/cargo-dist/) from `dist-workspace.toml`, which builds the binaries
and attaches them to that release. dist always writes its workflow as `release.yml`: after changing
`dist-workspace.toml`, comment out `allow-dirty`, run `dist generate`, move the new `release.yml`
to `binaries.yml` (named "Binaries"), restore `release.yml` with `git checkout`, and put
`allow-dirty` back; `dist plan` must pass.

## License

MIT

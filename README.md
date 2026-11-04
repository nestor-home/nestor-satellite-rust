# nestor-satellite

Rust SDK to write **satellites** for [Nestor](https://nestorhome.dev): devices (a screen and a
keyboard, a Raspberry Pi speaker, a robot) that pair with a Nestor server, hold one WebSocket link to
it, carry out its intents, report what they observe and talk with the assistant in writing, aloud or
both, following the `nestor.satellite/1` protocol. It does not depend on the Nestor server.

**Documentation:** [nestorhome.dev/docs/sdks/rust](https://nestorhome.dev/docs/sdks/rust/)
(features, pairing options, the command, conformance); the API on
[docs.rs](https://docs.rs/nestor-satellite).

| Feature | Adds                                                                                    |
| ------- | --------------------------------------------------------------------------------------- |
| (none)  | pairing, the link, intents, observations, text (`text.input`, `text.display`)           |
| `voice` | WebRTC audio (Opus) with your own `AudioSource`/`AudioSink`, the device code said aloud |
| `audio` | `voice` with the machine's microphone and speaker (cpal)                                |
| `cli`   | the `nestor-satellite` command                                                          |

Capabilities follow what the satellite is given: a text display declares `text.display`,
`text_input()` declares `text.input`, a microphone `audio.capture`, a speaker `audio.playback`.

A screen with a keyboard, no WebRTC:

```rust
use nestor_satellite::{Device, IDENTIFY, Manifest, Satellite, TokenStore};

#[tokio::main]
async fn main() -> nestor_satellite::Result<()> {
    let device = Device::new("ecran-cuisine", "raspberry-pi-zero", "cuisine")?;
    let tokens = TokenStore::new("~/.config/nestor-satellite/ecran-cuisine.json");
    let satellite = Satellite::builder("http://nestor.local:8471", Manifest::new(device, [IDENTIFY])?, tokens)
        .text_display(|text: &str, _last: bool| println!("{text}"))
        .text_input()
        .build()?;
    let keyboard = satellite.observer();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            let _ = keyboard.input_text(&line); // false while not connected
        }
    });
    satellite.run().await // pairs once, then reconnects
}
```

A speaker (`cargo add nestor-satellite --features audio`):

```rust
use nestor_satellite::devices::{Microphone, Speaker};
use nestor_satellite::{Behavior, Body, Device, IDENTIFY, Manifest, Satellite, TokenStore};

struct Led;
impl Body for Led {
    fn behavior(&self, behavior: Behavior) {
        println!("attitude: {behavior}"); // light the ring, from a task of its own
    }
}

#[tokio::main]
async fn main() -> nestor_satellite::Result<()> {
    let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon")?;
    let tokens = TokenStore::new("~/.config/nestor-satellite/enceinte-salon.json");
    let satellite = Satellite::builder("http://nestor.local:8471", Manifest::new(device, [IDENTIFY])?, tokens);
    satellite.body(Led).source(Microphone::new()).sink(Speaker::new()).build()?.run().await
}
```

A driver that updates itself when a person asks in Nestor gives an `Updater` (where its package
comes from, how a version is installed) and names its package in the manifest:
`Manifest::new(device, [IDENTIFY])?.with_software(Software::new("my-driver", env!("CARGO_PKG_VERSION"))?)`,
then `.updater(my_updater)` on the builder. It reports newer versions once connected and once a day,
installs only the one it found, starts again on it, and goes back if the new one is not welcomed
within 60 s.

The command, with nothing to write (a microphone and a speaker, or the terminal):

```bash
cargo install nestor-satellite --features cli
NESTOR_PAIRING_URI='nestor+http://nestor.local:8471/?code=K7QF2M9X' nestor-satellite run --room salon
nestor-satellite run --core http://nestor.local:8471 --room bureau --text-only   # lines in, answers out
```

Building needs Rust 1.91+ and a C compiler; the `voice` feature also needs `cmake`, and on Linux
the `audio` feature ALSA (`apt install libasound2-dev pkg-config`). The recorded voice that says device
codes is shared under CC BY 4.0: see [`voice/LICENSE`](voice/LICENSE).

## Development

```bash
cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features
cargo clippy --all-targets -- -D warnings && cargo test   # without voice
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

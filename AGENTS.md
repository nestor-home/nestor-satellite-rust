# AGENTS.md

`nestor-satellite` is the Rust SDK for Nestor satellite drivers. This file is for coding agents:
read it before changing anything.

## Commands

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

All three must pass; CI runs them on stable Rust, checks the MSRV (1.91: webrtc-rs needs it) and
runs `cargo audit`. Building needs a C compiler and `cmake` (libopus is bundled), and on Linux ALSA
headers for the `audio` feature (`libasound2-dev pkg-config`). Add dependencies with `cargo add`,
pin them to a full version, and commit `Cargo.lock`.

## Layout

```text
src/
├── protocol.rs   values of nestor.satellite/1: Device, Sdk, Capability, Manifest, Behavior, Expression
├── wire.rs       the JSON envelope: messages the satellite sends, strict decoding of what the core sends
├── pairing.rs    pair with a code, TokenStore (file mode 600, same format as the Python SDK)
├── client.rs     Satellite: WebRTC connection, hello/welcome, acks, Observer, Backoff, reconnection
├── dispatch.rs   Dispatcher: one data channel message → Body call + ack (transport-agnostic)
├── body.rs       Body: what a driver implements to carry out intents (no-op defaults)
├── audio.rs      AudioSource / AudioSink traits, SilenceSource / RecordingSink doubles
├── media.rs      Opus encode/decode, real-time pacing, WebRTC tracks
├── devices.rs    Microphone / Speaker through cpal (feature `audio`)
└── bin/nestor-satellite.rs   the CLI (feature `cli`)
schemas/          JSON Schemas copied from the server (do not edit by hand)
scripts/conformance.py        runs the server's conformance suite against the binary
examples/led.rs   the README's driver, compiled by clippy
tests/            schemas, fake-core client tests, conformance
```

## Rules

1. **Never depend on the server.** The server owns the protocol; this crate follows it. The Nestor
   repository is only used by tests, next to this one (`../nestor`, or `NESTOR_REPO`).
2. **Refreshing the schemas.** `schemas/` is a copy of
   `../nestor/src/nestor/modules/satellites/schemas/`; `tests/schemas.rs` fails when they drift.
   To refresh: `cp ../nestor/src/nestor/modules/satellites/schemas/*.json schemas/`, then adapt
   `protocol.rs`/`wire.rs` until `cargo test` passes. Every message the SDK sends is validated
   against these schemas, and the decoder must refuse what they refuse.
3. **Be lenient in what you read, strict in what you send.** An unknown message type from a newer
   core is ignored; a malformed known one is logged and dropped; constructors validate what is sent.
4. **The network loop never waits for the device.** `Body` methods return at once; a driver
   animates in its own task or thread. Physical safety lives in the driver, never in the link.
5. **Secrets:** the token is written with mode 600, never logged, and forgotten when the core
   refuses it (401).
6. **WebRTC interop.** The core is aiortc (Pipecat SmallWebRTC). The offer is sent with
   `a=setup:active` (see `dtls_client` in `client.rs`): without it neither side opens SCTP and the
   data channel never comes up. Keep that test and the conformance run green.

## Conformance

The server's black-box suite (`nestor.adapters.channels.voice.satellites.conformance`) runs a fake
core, launches the binary as a subprocess (robot-less speaker: `audio.capture` + `audio.playback`,
`--null-audio`), approves the pairing and checks pairing, hello, acks, audio both ways and
reconnection:

```bash
(cd ../nestor && uv sync --extra voice)
cargo test --all-features --test conformance -- --nocapture
# or by hand:
cargo build --features cli && ../nestor/.venv/bin/python scripts/conformance.py target/debug/nestor-satellite
```

The test is skipped without the server's environment (`NESTOR_PYTHON` overrides the interpreter);
`NESTOR_CONFORMANCE=required` (CI) makes that a failure.

## Code conventions

- Edition 2024, `rustfmt.toml` (width 120). English only. `unsafe` is forbidden; no `unwrap`,
  `expect` or `panic` in library code (clippy denies them; tests may).
- Errors: `thiserror` (`Error`, `ProtocolError`, `AudioError`); logs: `tracing`, never `println!`
  except `show_code` and the CLI.
- Async: tokio; explicit timeouts (HTTP, ICE gathering, welcome); every task of a connection is
  aborted and the peer connection closed when it ends; `run_until` releases the audio devices.
- Keep the repository lean: no Markdown beyond README.md and this file.

## Tests

- Test behaviour, not implementation: one behaviour per test, named as a sentence.
- Hardware is replaced by doubles (`SilenceSource`, `RecordingSink`); the core by an axum fake
  (`tests/client.rs`) or the server's own suite (`tests/conformance.rs`).
- A real microphone, speaker or Raspberry Pi is not exercised by the tests: say so when it matters.

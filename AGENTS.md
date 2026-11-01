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
runs `cargo audit` (also weekly; clean as of 31/10/2026, no ignored advisory: an advisory that cannot
be fixed goes in `.cargo/audit.toml` with its reason). Building needs a C compiler and `cmake` (libopus is bundled), and on Linux ALSA
headers for the `audio` feature (`libasound2-dev pkg-config`). Add dependencies with `cargo add`,
pin them to a full version, and commit `Cargo.lock`.

## Layout

```text
src/
├── protocol.rs   values of nestor.satellite/1: Device, Sdk, Capability, Manifest, PairingMethod, Behavior…
├── wire.rs       the JSON envelope: messages the satellite sends, strict decoding of what the core sends
├── codes.rs      enrollment codes (Crockford base32, normalised) and pairing URIs (nestor+http…)
├── pairing.rs    the pairing flow (device code, enrollment codes, refusals, Retry-After), Display, Voice,
│                 Enroller, TokenStore (file mode 600, same format as the Python SDK)
├── i18n.rs       Language: what is said to people, from the Fluent catalogue i18n/{en,fr}.ftl
├── speech.rs     the device code said through the sink with the bundled clips (voice/), cancellable
├── client.rs     Satellite: builder (pairing methods derived from outputs), WebRTC connection,
│                 hello/welcome, acks, Observer, Backoff, reconnection
├── dispatch.rs   Dispatcher: one data channel message → Body call + ack (transport-agnostic)
├── body.rs       Body: what a driver implements to carry out intents (no-op defaults)
├── audio.rs      AudioSource / AudioSink traits, SilenceSource / RecordingSink doubles
├── media.rs      Opus encode/decode, real-time pacing, WebRTC tracks
├── devices.rs    Microphone / Speaker through cpal (feature `audio`)
└── bin/nestor-satellite.rs   the CLI (feature `cli`)
schemas/          JSON Schemas copied from the server (do not edit by hand)
voice/            recorded clips of the spoken device code, copied from nestor-satellite-python
examples/         the drivers the README and the documentation show (led.rs, screen.rs), compiled by clippy
tests/            schemas, fake-core client tests, conformance
```

## Rules

1. **Never depend on the server.** The server owns the protocol; this crate follows it. The server
   is only used by the conformance test, as the published `nestor-home` package (`.nestor-version`).
2. **Refreshing the schemas.** `schemas/` is a copy of the schemas of the `nestor-home` release in
   `.nestor-version`. The standard itself is https://nestorhome.dev/docs/satellites/protocol/ (rules N1,
   P1-P8, I1-I3, W1-W4). To refresh:
   `uvx --from nestor-home=="$(cat .nestor-version)" nestor satellites schemas --out schemas/`, then adapt
   `protocol.rs`/`wire.rs` until `cargo test` passes. Every message the SDK sends is validated
   against these schemas, and the decoder must refuse what they refuse.
3. **Be lenient in what you read, strict in what you send.** An unknown message type from a newer
   core is ignored; a malformed known one is logged and dropped; constructors validate what is sent.
4. **The network loop never waits for the device.** `Body` methods return at once; a driver
   animates in its own task or thread. Physical safety lives in the driver, never in the link.
5. **Secrets:** the token is written with mode 600, never logged, and forgotten when the core
   refuses it (401). An enrollment code is never logged either (`EnrollmentCode`'s `Debug` hides
   it); the device code is, always: it is how a headless device is paired.
6. **WebRTC interop.** The core is aiortc (Pipecat SmallWebRTC). The offer is sent with
   `a=setup:active` (see `dtls_client` in `client.rs`): without it neither side opens SCTP and the
   data channel never comes up. Keep that test and the conformance run green.
7. **Pairing never fails silently.** Every refusal is logged; a refused enrollment code is never
   sent again; the device code takes over. Only `protocol_unsupported` and `invalid_request` stop
   the satellite. A pairing method is declared only when the driver gave what carries it out.
8. **People read sentences from the catalogue.** Anything shown, said or logged for people (codes,
   refusals, hints, errors) goes through `Language::say`/`describe`: add the sentence to both
   `src/i18n/en.ftl` and `fr.ftl` (a test checks they match), short, no em-dash. Logs for developers
   stay in English in the code.

9. **The bundled voice is the Python SDK's.** `voice/` is a byte-identical copy of
   `../nestor-satellite-python/src/nestor_satellite/voice/` (recorded by its `scripts/voice.py`,
   CC BY 4.0). Each piece of
   `pairing-code-spoken` between the digits has a clip: change that sentence only with new clips,
   then `cp -R ../nestor-satellite-python/src/nestor_satellite/voice/* voice/` and list new files in
   `speech.rs`. It is declared (`device_code.speech`) only for a sink given, `audio.playback`
   declared, and no display nor driver `Voice`; it stops when pairing ends, however it ends.
10. **Refused enrollment codes outlive the process.** The `TokenStore` file keeps their SHA-256
   (`refused`), even when the token is forgotten; a code is never written or logged in clear.
11. **No QR code.** This SDK drives no camera: `enrollment.qr` is not a `PairingMethod` here, so no
   path declares it.

## Conformance

The server's black-box suite (`nestor satellites conformance`, in the published `nestor-home`
package) runs a fake core and launches the binary once per scenario (robot-less speaker: `audio.capture` +
`audio.playback` + `device.identify`, `--null-audio`): `{url}` is the core, `{state}` a fresh folder
for the token, and the enrollment code comes in `NESTOR_PAIRING_URI`. It checks pairing with a
device code and with an enrollment code, refused codes, Retry-After, identify, hello, acks, audio
both ways and reconnection:

```bash
cargo test --all-features --test conformance -- --nocapture
# or by hand:
cargo build --features cli
uvx --from "nestor-home[voice]==$(cat .nestor-version)" nestor satellites conformance --deadline 15 -- \
  target/debug/nestor-satellite run --core {url} --room salon --id enceinte-salon --model conformance \
  --token-file {state}/token.json --null-audio --verbose
```

The server's version is pinned in `.nestor-version`, the one place to bump it (deliberately, after
reading the server's changelog). Against a local checkout of the server, replace the `--from`
argument by `'../nestor[voice]'`. The test checks the exit status (0), that no line starts with
`FAIL`, and a few named checks; it is skipped when `uvx` is not on the PATH, and
`NESTOR_CONFORMANCE=required` (CI) makes that a failure.

## Code conventions

- Edition 2024, `rustfmt.toml` (width 120). English only. `unsafe` is forbidden; no `unwrap`,
  `expect` or `panic` in library code (clippy denies them; tests may).
- Errors: `thiserror` (`Error`, `ProtocolError`, `AudioError`); logs: `tracing`, never `println!`
  except in the CLI and the examples.
- Async: tokio; explicit timeouts (HTTP, ICE gathering, welcome); every task of a connection is
  aborted and the peer connection closed when it ends; `run_until` releases the audio devices.
- Keep the repository lean: no Markdown beyond README.md and this file.

## Tests

- Test behaviour, not implementation: one behaviour per test, named as a sentence.
- Hardware is replaced by doubles (`SilenceSource`, `RecordingSink`, a display/voice/body that
  notes what it is asked); the core by an axum fake (`tests/client.rs`, `tests/schemas.rs`) or the
  server's own suite (`tests/conformance.rs`).
- A real microphone, speaker or Raspberry Pi is not exercised by the tests: say so when it matters.

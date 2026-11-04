# AGENTS.md

`nestor-satellite` is the Rust SDK for Nestor satellite drivers. This file is for coding agents:
read it before changing anything.

## Commands

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo clippy --all-targets -- -D warnings && cargo test   # without `voice`: no WebRTC
```

All must pass; CI runs them on stable Rust, checks that the default build has no webrtc, checks the
MSRV (1.91: webrtc-rs needs it) and
runs `cargo audit` (also weekly; clean as of 31/10/2026, no ignored advisory: an advisory that cannot
be fixed goes in `.cargo/audit.toml` with its reason). Building needs a C compiler and `cmake` (libopus is bundled), and on Linux ALSA
headers for the `audio` feature (`libasound2-dev pkg-config`).

Features: none by default (pairing, the WebSocket link, intents, observations, text); `voice` adds
WebRTC audio (`webrtc`, `rtc`, `opus`) and the bundled voice; `audio` adds cpal devices on top of
`voice`; `cli` is the command, on top of `audio`. Nothing of `voice` may leak into the default build. Add dependencies with `cargo add`,
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
├── client.rs     Satellite: builder (capabilities and pairing methods derived from what it is given),
│                 the WebSocket link (hello/welcome, acks, close codes, silence), Observer, Backoff
├── dispatch.rs   Dispatcher: one link message → Body or TextDisplay call + ack (transport-agnostic)
├── body.rs       Body: what a driver implements to carry out intents (no-op defaults)
├── text.rs       TextDisplay: where the assistant's words are shown (text.display)
├── update.rs     Updater: the driver's own software, reported and installed on request (device.update)
├── voice.rs      WebRTC audio offered once welcomed, offered again while the link is up (feature `voice`)
├── voiceless.rs  `voice` without the feature: nothing to open, no device code said (keeps `cfg` out of the
│                 link and pairing)
├── audio.rs      AudioSource / AudioSink traits, SilenceSource / RecordingSink doubles (feature `voice`)
├── media.rs      Opus encode/decode, real-time pacing, WebRTC tracks (feature `voice`)
├── speech.rs     the device code said through the sink with the bundled clips (voice/) (feature `voice`)
├── devices.rs    Microphone / Speaker through cpal (feature `audio`)
└── bin/nestor-satellite.rs   the CLI (feature `cli`): a speaker, or a text satellite in a terminal
schemas/          JSON Schemas copied from the server (do not edit by hand)
voice/            recorded clips of the spoken device code, copied from nestor-satellite-python
examples/         the drivers the README and the documentation show (led.rs, screen.rs), compiled by clippy
tests/            schemas, fake-core client tests (support/: the fake core, HTTP and WebSocket), conformance
```

## Rules

1. **Never depend on the server.** The server owns the protocol; this crate follows it. The server
   is only used by the conformance test, as the published `nestor-home` package (`.nestor-version`).
2. **Refreshing the schemas.** `schemas/` is a copy of the schemas of the `nestor-home` release in
   `.nestor-version`. The standard itself is https://nestorhome.dev/docs/satellites/protocol/ (rules N1,
   P1-P8, I1-I3, L1-L5, T1-T4, U1-U3, A1-A4). To refresh:
   `uvx --from nestor-home=="$(cat .nestor-version)" nestor satellites schemas --out schemas/`, then adapt
   `protocol.rs`/`wire.rs` until `cargo test` passes. Every message the SDK sends is validated
   against these schemas, and the decoder must refuse what they refuse.
3. **Be lenient in what you read, strict in what you send.** An unknown message type from a newer
   core is ignored; a malformed known one is logged and dropped; constructors validate what is sent.
4. **The network loop never waits for the device.** `Body` methods return at once; a driver
   animates in its own task or thread. Physical safety lives in the driver, never in the link.
5. **Secrets:** the token is written with mode 600, never logged, and forgotten when the core
   refuses it (link closed with 4401, or 401 on an audio offer). An enrollment code is never logged
   either (`EnrollmentCode`'s `Debug` hides it); the device code is, always: it is how a headless
   device is paired.
6. **One link, audio apart.** Every message goes over the WebSocket link, never over WebRTC (no data
   channel). Audio is offered only when `welcome` allows `audio.capture` or `audio.playback`, lives
   with the link, and pairing and the link never wait for it. 4409 (replaced) stops the satellite;
   1001, 4400 and a silent link (60 s) reconnect. The core is aiortc (Pipecat SmallWebRTC): keep
   the conformance run green.
7. **Pairing never fails silently.** Every refusal is logged; a refused enrollment code is never
   sent again; the device code takes over. Only `protocol_unsupported` and `invalid_request` stop
   the satellite (and, once paired, a link replaced with 4409). A pairing method is declared only when the driver gave what carries it out.
8. **People read sentences from the catalogue.** Anything shown, said or logged for people (codes,
   refusals, hints, errors) goes through `Language::say`/`describe`: add the sentence to both
   `src/i18n/en.ftl` and `fr.ftl` (a test checks they match), short, no em-dash. Logs for developers
   stay in English in the code.

9. **The bundled voice is the Python SDK's.** `voice/` is a byte-identical copy of
   `../nestor-satellite-python/src/nestor_satellite/voice/` (recorded by its `scripts/voice.py`,
   CC BY 4.0). Each piece of
   `pairing-code-spoken` between the digits has a clip: change that sentence only with new clips,
   then `cp -R ../nestor-satellite-python/src/nestor_satellite/voice/* voice/` and list new files in
   `speech.rs`. It needs the `voice` feature, and is declared (`device_code.speech`) only for a
   sink given (not `null_audio`) and no display nor driver `Voice`; it stops when pairing ends,
   however it ends.
10. **Refused enrollment codes outlive the process.** The `TokenStore` file keeps their SHA-256
   (`refused`), even when the token is forgotten; a code is never written or logged in clear.
11. **No QR code.** This SDK drives no camera: `enrollment.qr` is not a `PairingMethod` here, so no
   path declares it.
12. **Capabilities follow what is given.** `text.display` from a `TextDisplay`, `text.input` from
   `text_input()`, `audio.capture` from a source, `audio.playback` from a sink, `device.update` from
   an `Updater`. A manifest declaring one of them without it is refused (`Error::NotProvided`).
13. **The core never chooses what is installed.** An `Updater` asks the software's own source; the
   satellite installs only the newer version it found and reported, never one the core names, never
   an older one. A new version not welcomed within the grace period (60 s) reinstalls the one that ran
   before. The crate ships no updater: a driver gives one for the way its software is installed.

## Conformance

The server's black-box suite (`nestor satellites conformance`, in the published `nestor-home`
package) runs a fake core and launches the binary once per scenario, twice over: as a robot-less
speaker (`audio.capture` + `audio.playback` + `device.identify`, `--null-audio`) and as a text
satellite (`text.input` + `text.display` + `device.identify`, `--text-only`). `{url}` is the core,
`{state}` a fresh folder for the token, and the enrollment code comes in `NESTOR_PAIRING_URI`. It
checks pairing with a device code and with an enrollment code, refused codes, Retry-After,
identify, the link and its hello, acks, text, audio both ways, no audio when not allowed,
reconnection and a refused token:

```bash
cargo test --all-features --test conformance -- --nocapture --test-threads=1
# or by hand:
cargo build --features cli
uvx --from "nestor-home[voice]==$(cat .nestor-version)" nestor satellites conformance --deadline 15 -- \
  target/debug/nestor-satellite run --core {url} --room salon --id enceinte-salon --model conformance \
  --token-file {state}/token.json --null-audio --verbose
```

The server's version is pinned in `.nestor-version`, the one place to bump it (deliberately, after
reading the server's changelog). Against a local checkout of the server, set
`NESTOR_CONFORMANCE_FROM='../nestor[voice]'` (or replace the `--from` argument by hand). The test checks the exit status (0), that no line starts with
`FAIL`, and a few named checks; it is skipped when `uvx` is not on the PATH, and
`NESTOR_CONFORMANCE=required` (CI) makes that a failure.

## Code conventions

- Edition 2024, `rustfmt.toml` (width 120). English only. `unsafe` is forbidden; no `unwrap`,
  `expect` or `panic` in library code (clippy denies them; tests may).
- Errors: `thiserror` (`Error`, `ProtocolError`, `AudioError`); logs: `tracing`, never `println!`
  except in the CLI and the examples.
- Async: tokio; explicit timeouts (HTTP, opening the link, welcome, link silence, ICE gathering);
  every task of a link (audio) is aborted and the peer connection closed when it ends; `run_until`
  releases the audio devices.
- Keep the repository lean: no Markdown beyond README.md and this file.

## Tests

- Test behaviour, not implementation: one behaviour per test, named as a sentence.
- Hardware is replaced by doubles (`SilenceSource`, `RecordingSink`, a display/voice/body that
  notes what it is asked); the core by an axum fake (`tests/client.rs`, `tests/schemas.rs`) or the
  server's own suite (`tests/conformance.rs`).
- A real microphone, speaker or Raspberry Pi is not exercised by the tests: say so when it matters.

//! The device code said aloud by the SDK itself (`device_code.speech`), offline: short recorded
//! clips played one after another through the speaker.
//!
//! The sentence is the catalogue's `pairing-code-spoken`. Formatted with a placeholder for the
//! digits, it splits into pieces of text and digit slots: each piece of text, and each digit, has a
//! clip recorded once (`voice/`, a copy of nestor-satellite-python's, recorded by its
//! `scripts/voice.py`), listed with the pauses in `voice/index.json`. The Python and TypeScript SDKs
//! ship the same clips and read them the same way. A language without clips is said in English.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use fluent_bundle::FluentValue;
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{error, warn};

use crate::audio::{AudioSink, frame_samples};
use crate::i18n::Language;

/// The sentence of the catalogue that says the device code.
pub(crate) const MESSAGE: &str = "pairing-code-spoken";
/// Where the digits go, when the sentence is cut into clips.
const SLOT: &str = "\u{0}";
/// Around a piece of text: punctuation and Fluent's isolation marks.
const STRIP: &[char] = &['.', ',', ';', ':', '!', '?', '…', '\u{2068}', '\u{2069}'];
/// How much audio the sink holds ahead of real time: a stop is heard at once.
const AHEAD: Duration = Duration::from_millis(100);

/// `voice/index.json`: the clips of each language and the pauses between them.
#[derive(Debug, Deserialize)]
pub(crate) struct Index {
    pauses_ms: Pauses,
    languages: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct Pauses {
    after_text: u32,
    between_digits: u32,
    after_digits: u32,
}

/// One step of an announcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// A digit, or a piece of the sentence: its key in the index.
    Clip(String),
    /// Silence, in milliseconds.
    Pause(u32),
}

macro_rules! embedded {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_bytes!(concat!("../voice/", $path)).as_slice())),*]
    };
}

/// Every clip of `voice/`, by its path in the index.
const CLIPS: &[(&str, &[u8])] = embedded!(
    "en/0.wav",
    "en/1.wav",
    "en/2.wav",
    "en/3.wav",
    "en/4.wav",
    "en/5.wav",
    "en/6.wav",
    "en/7.wav",
    "en/8.wav",
    "en/9.wav",
    "en/pairing-code.wav",
    "en/i-repeat.wav",
    "fr/0.wav",
    "fr/1.wav",
    "fr/2.wav",
    "fr/3.wav",
    "fr/4.wav",
    "fr/5.wav",
    "fr/6.wav",
    "fr/7.wav",
    "fr/8.wav",
    "fr/9.wav",
    "fr/code-d-appairage.wav",
    "fr/je-repete.wav",
);

const INDEX: &str = include_str!("../voice/index.json");

/// The bundled index.
pub(crate) fn index() -> Option<&'static Index> {
    static PARSED: OnceLock<Option<Index>> = OnceLock::new();
    PARSED
        .get_or_init(|| {
            serde_json::from_str(INDEX).map_err(|error| error!(%error, "voice/index.json does not parse")).ok()
        })
        .as_ref()
}

impl Index {
    /// The language the code is said in: `language` when it has clips, else English.
    pub(crate) fn voiced(&self, language: Language) -> Language {
        if self.languages.contains_key(language.tag()) { language } else { Language::En }
    }

    /// What to play to say `code` in `language` (English when it has no clips): clips and the
    /// pauses between them.
    pub(crate) fn announcement(&self, language: Language, code: &str) -> Vec<Step> {
        let language = self.voiced(language);
        let pauses = self.pauses_ms;
        let sentence = language.say(MESSAGE, &[("digits", FluentValue::from(SLOT))]);
        let parts: Vec<&str> = sentence.split(SLOT).collect();
        let mut steps = Vec::new();
        for (position, text) in parts.iter().enumerate() {
            let key = key(text);
            if !key.is_empty() {
                steps.extend([Step::Clip(key.to_owned()), Step::Pause(pauses.after_text)]);
            }
            if position + 1 < parts.len() {
                for digit in code.chars() {
                    steps.extend([Step::Clip(digit.to_string()), Step::Pause(pauses.between_digits)]);
                }
                if let Some(last) = steps.last_mut() {
                    *last = Step::Pause(pauses.after_digits);
                }
            }
        }
        while matches!(steps.last(), Some(Step::Pause(_))) {
            steps.pop();
        }
        steps
    }

    /// The clip of `text` in `language` (English when it has no clips), as recorded.
    fn clip(&self, language: Language, text: &str) -> Option<&'static [u8]> {
        let path = self.languages.get(self.voiced(language).tag())?.get(text)?;
        CLIPS.iter().find(|(embedded, _)| embedded == path).map(|(_, bytes)| *bytes)
    }
}

/// A piece of the sentence as the index names its clip: without the spaces and punctuation around it.
fn key(text: &str) -> &str {
    text.trim().trim_matches(STRIP).trim()
}

/// A 16-bit mono PCM WAV file: its rate and samples.
pub(crate) fn decode(wav: &[u8]) -> Result<(u32, Vec<i16>), String> {
    if wav.get(..4) != Some(b"RIFF") || wav.get(8..12) != Some(b"WAVE") {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut rest = wav.get(12..).unwrap_or_default();
    let mut rate = None;
    while rest.len() >= 8 {
        let (header, body) = rest.split_at(8);
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let chunk = body.get(..size).ok_or("a chunk runs past the end of the file")?;
        match &header[..4] {
            b"fmt " => {
                let field = |at: usize| chunk.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
                let (format, channels, bits) = (field(0), field(2), field(14));
                if (format, channels, bits) != (Some(1), Some(1), Some(16)) {
                    return Err("not 16-bit mono PCM".into());
                }
                rate = chunk.get(4..8).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            }
            b"data" => {
                let rate = rate.ok_or("the data comes before its format")?;
                return Ok((rate, chunk.as_chunks::<2>().0.iter().map(|s| i16::from_le_bytes(*s)).collect()));
            }
            _ => {}
        }
        rest = body.get(size + size % 2..).unwrap_or_default(); // chunks are padded to an even size
    }
    Err("no audio data".into())
}

/// `pcm` at `from` Hz, linearly resampled to `to` Hz.
pub(crate) fn resample(pcm: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || from == 0 {
        return pcm.to_vec();
    }
    let length = (pcm.len() as u64 * u64::from(to) / u64::from(from)) as usize;
    let step = f64::from(from) / f64::from(to);
    (0..length)
        .map(|i| {
            let position = i as f64 * step;
            let at = position as usize;
            let (a, b) = match (pcm.get(at), pcm.get(at + 1)) {
                (Some(&a), Some(&b)) => (a, b),
                (Some(&a), None) => (a, a),
                _ => (0, 0),
            };
            let fraction = position - at as f64;
            (f64::from(a) + (f64::from(b) - f64::from(a)) * fraction).round() as i16
        })
        .collect()
}

/// Says a device code through the speaker, in real time, in the background. A new code replaces
/// the one being said; [`stop`](Self::stop), [`hush`](Self::hush) or dropping it cuts it.
pub(crate) struct SpokenCode {
    sink: Arc<Mutex<Box<dyn AudioSink>>>,
    language: Language,
    /// Bumped by every new code and every stop: a task writes only while it is the current one.
    current: Arc<AtomicU64>,
    task: StdMutex<Option<JoinHandle<()>>>,
}

impl SpokenCode {
    pub(crate) fn new(sink: Arc<Mutex<Box<dyn AudioSink>>>, language: Language) -> Self {
        Self { sink, language, current: Arc::new(AtomicU64::new(0)), task: StdMutex::new(None) }
    }

    /// Start saying `code` (cutting what was being said).
    pub(crate) fn say(&self, code: &str) {
        let generation = self.stop();
        let Some(index) = index() else { return };
        let steps = index.announcement(self.language, code);
        let task = tokio::spawn(play(
            index,
            self.language,
            steps,
            self.sink.clone(),
            Turn { current: self.current.clone(), generation },
        ));
        if let Ok(mut slot) = self.task.lock() {
            *slot = Some(task);
        }
    }

    /// Cut what is being said: nothing more is written to the sink. The new generation.
    pub(crate) fn stop(&self) -> u64 {
        let generation = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(task) = self.task.lock().ok().and_then(|mut slot| slot.take()) {
            task.abort();
        }
        generation
    }

    /// [`stop`](Self::stop), and wait for a write in flight to end.
    pub(crate) async fn hush(&self) {
        self.stop();
        drop(self.sink.lock().await);
    }
}

impl Drop for SpokenCode {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Whether a playing task is still the one that should be heard.
struct Turn {
    current: Arc<AtomicU64>,
    generation: u64,
}

impl Turn {
    fn is_current(&self) -> bool {
        self.current.load(Ordering::SeqCst) == self.generation
    }
}

async fn play(
    index: &'static Index,
    language: Language,
    steps: Vec<Step>,
    sink: Arc<Mutex<Box<dyn AudioSink>>>,
    turn: Turn,
) {
    let rate = sink.lock().await.sample_rate();
    let chunk = frame_samples(rate).max(1);
    let started = Instant::now();
    let mut played: u64 = 0;
    for step in steps {
        let pcm = match step {
            Step::Pause(ms) => vec![0; (u64::from(rate) * u64::from(ms) / 1000) as usize],
            Step::Clip(text) => match index.clip(language, &text).map(decode) {
                Some(Ok((clip_rate, pcm))) => resample(&pcm, clip_rate, rate),
                Some(Err(problem)) => {
                    error!(clip = text, problem, "a recorded clip does not decode");
                    continue;
                }
                None => {
                    error!(clip = text, language = language.tag(), "no recorded clip: run scripts/voice.py");
                    continue;
                }
            },
        };
        for piece in pcm.chunks(chunk) {
            let due = started + Duration::from_secs_f64(played as f64 / f64::from(rate));
            if due > Instant::now() + AHEAD {
                tokio::time::sleep_until(due - AHEAD).await;
            }
            let mut sink = sink.lock().await;
            if !turn.is_current() {
                return;
            }
            if let Err(error) = sink.write(piece).await {
                warn!(%error, "the device code could not be said");
                return;
            }
            played += piece.len() as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::audio::RecordingSink;

    fn clips(steps: &[Step]) -> Vec<&str> {
        steps
            .iter()
            .filter_map(|s| match s {
                Step::Clip(text) => Some(text.as_str()),
                Step::Pause(_) => None,
            })
            .collect()
    }

    #[test]
    fn a_code_is_said_with_its_digits_one_by_one_twice_between_the_sentence_s_clips() {
        let index = index().unwrap();
        let steps = index.announcement(Language::En, "042917");
        let digits = ["0", "4", "2", "9", "1", "7"];
        let mut expected = vec!["Pairing code"];
        expected.extend(digits);
        expected.push("I repeat");
        expected.extend(digits);
        assert_eq!(clips(&steps), expected);
        assert_eq!(
            steps[0..4],
            [Step::Clip("Pairing code".into()), Step::Pause(250), Step::Clip("0".into()), Step::Pause(350)]
        );
        assert_eq!(steps[12..15], [Step::Clip("7".into()), Step::Pause(800), Step::Clip("I repeat".into())]);
        assert_eq!(steps.last(), Some(&Step::Clip("7".into())), "no silence at the end");
    }

    #[test]
    fn a_code_is_said_in_french_with_the_french_clips() {
        let steps = index().unwrap().announcement(Language::Fr, "100000");
        let said = clips(&steps);
        assert_eq!((said[0], said[7]), ("Code d'appairage", "Je répète"));
        assert_eq!(said.len(), 14);
    }

    #[test]
    fn a_language_without_clips_is_said_in_english() {
        let english_only: Index = serde_json::from_value(serde_json::json!({
            "sample_rate": 16000,
            "pauses_ms": {"after_text": 1, "between_digits": 2, "after_digits": 3},
            "languages": {"en": {"Pairing code": "en/pairing-code.wav", "1": "en/1.wav"}},
        }))
        .unwrap();
        assert_eq!(english_only.voiced(Language::Fr), Language::En);
        assert_eq!(clips(&english_only.announcement(Language::Fr, "1")), ["Pairing code", "1", "I repeat", "1"]);
        assert_eq!(english_only.clip(Language::Fr, "1"), index().unwrap().clip(Language::En, "1"));
    }

    #[test]
    fn every_piece_of_every_language_has_a_clip_that_decodes_as_16_bit_mono() {
        let index = index().unwrap();
        for language in Language::ALL {
            assert_eq!(index.voiced(language), language, "{language:?} has clips");
            for step in index.announcement(language, "0123456789") {
                if let Step::Clip(text) = step {
                    let clip = index.clip(language, &text).unwrap_or_else(|| panic!("{language:?}: {text}"));
                    let (rate, pcm) = decode(clip).unwrap();
                    assert_eq!(rate, 16_000);
                    assert!(pcm.len() > 1_600, "{text}: at least 0.1 s");
                }
            }
        }
    }

    #[test]
    fn every_clip_of_the_index_is_embedded_and_every_embedded_clip_is_in_the_index() {
        let index = index().unwrap();
        let mut listed: Vec<&str> =
            index.languages.values().flat_map(|clips| clips.values().map(String::as_str)).collect();
        let mut embedded: Vec<&str> = CLIPS.iter().map(|(path, _)| *path).collect();
        listed.sort_unstable();
        embedded.sort_unstable();
        assert_eq!(listed, embedded);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("voice");
        let mut on_disk: Vec<String> = ["en", "fr"]
            .iter()
            .flat_map(|language| {
                std::fs::read_dir(root.join(language))
                    .unwrap()
                    .map(move |entry| format!("{language}/{}", entry.unwrap().file_name().to_string_lossy()))
            })
            .collect();
        on_disk.sort_unstable();
        assert_eq!(on_disk, embedded);
    }

    #[test]
    fn a_wav_file_that_is_not_16_bit_mono_pcm_is_refused() {
        assert!(decode(b"not a wav file at all").is_err());
        let mut stereo = CLIPS[0].1.to_vec();
        stereo[22] = 2; // channels
        assert!(decode(&stereo).is_err());
    }

    #[test]
    fn resampling_keeps_the_duration_and_the_shape() {
        let ramp: Vec<i16> = (0..160).map(|i| i * 100).collect();
        let up = resample(&ramp, 16_000, 48_000);
        assert_eq!(up.len(), 480);
        assert_eq!((up[0], up[3], up[4]), (0, 100, 133));
        assert_eq!(resample(&ramp, 16_000, 16_000), ramp);
        assert_eq!(resample(&ramp, 16_000, 8_000).len(), 80);
    }

    #[tokio::test(start_paused = true)]
    async fn the_code_is_played_in_real_time_keeping_at_most_100_ms_ahead() {
        let recording = RecordingSink::new();
        let sink: Arc<Mutex<Box<dyn AudioSink>>> = Arc::new(Mutex::new(Box::new(recording.clone())));
        let speech = SpokenCode::new(sink, Language::En);
        speech.say("042917");
        tokio::time::sleep(Duration::from_secs(1)).await;
        let played = recording.played().len() as f64 / 48_000.0;
        assert!((1.0..=1.15).contains(&played), "{played} s played after 1 s");
        tokio::time::sleep(Duration::from_secs(60)).await;
        let whole = recording.played().len() as f64 / 48_000.0;
        assert!((6.0..20.0).contains(&whole), "{whole} s in all");
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_code_cuts_the_one_being_said_and_a_stop_cuts_it_at_once() {
        let recording = RecordingSink::new();
        let sink: Arc<Mutex<Box<dyn AudioSink>>> = Arc::new(Mutex::new(Box::new(recording.clone())));
        let speech = SpokenCode::new(sink, Language::En);
        speech.say("111111");
        tokio::time::sleep(Duration::from_millis(500)).await;
        speech.say("222222");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let cut = recording.played().len() as f64 / 48_000.0;
        assert!(cut <= 1.25, "one voice at a time: {cut} s played in 1 s");
        speech.hush().await;
        let said = recording.played().len();
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(recording.played().len(), said, "nothing written after the stop");
    }
}

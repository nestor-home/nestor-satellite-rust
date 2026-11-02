//! Without the `voice` feature, `voice` is this module: no audio device to open, no device code said,
//! so the link and pairing hold no WebRTC nor Opus and no `cfg` of their own.

use std::sync::Arc;

use crate::error::Result;
use crate::i18n::Language;

/// Nothing to talk aloud with.
#[derive(Default)]
pub(crate) struct VoiceSetup {}

impl VoiceSetup {
    /// No microphone, no speaker.
    pub(crate) fn given(&self) -> (bool, bool) {
        (false, false)
    }

    /// No speaker to say the device code through.
    pub(crate) fn audible(&self) -> bool {
        false
    }

    pub(crate) async fn open(&mut self) -> Result<Audio> {
        Ok(Audio)
    }
}

/// No audio device is open.
pub(crate) struct Audio;

impl Audio {
    pub(crate) fn spoken_code(&self, _language: Language) -> Option<Arc<SpokenCode>> {
        None
    }

    pub(crate) async fn close(&self) {}
}

/// The device code is never said with the bundled voice: no value of this type exists.
pub(crate) enum SpokenCode {}

impl SpokenCode {
    pub(crate) fn say(&self, _code: &str) {
        match *self {}
    }

    pub(crate) async fn hush(&self) {
        match *self {}
    }
}

//! Without the `voice` feature, `voice` is this module: no audio device to open, no device code said,
//! so the link and pairing hold no WebRTC nor Opus and no `cfg` of their own.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::error::Result;
use crate::i18n::Language;
use crate::sessions::{Hear, Means, MediaLink};

/// Nothing to talk aloud with.
#[derive(Default)]
pub(crate) struct VoiceSetup {}

impl VoiceSetup {
    /// No microphone, no speaker, no camera.
    pub(crate) fn given(&self) -> (bool, bool, bool) {
        (false, false, false)
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

    /// No voice.
    pub(crate) fn hear(
        &self,
        _link: &MediaLink,
        _microphone: bool,
        _refused: mpsc::UnboundedSender<()>,
    ) -> Option<Hear> {
        None
    }

    /// Nothing to film or call with.
    pub(crate) fn media(&self, _link: &MediaLink, _films: bool, _calls: bool) -> Means {
        Means { films: false, calls: false, connect: None, still: None }
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

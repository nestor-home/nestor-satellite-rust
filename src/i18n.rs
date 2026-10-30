//! What the satellite says to people (its device code, a refused enrollment code, an error), in
//! their language: a Fluent catalogue embedded in the crate (`src/i18n/*.ftl`), English and French.
//!
//! The language is chosen with [`SatelliteBuilder::language`](crate::SatelliteBuilder::language),
//! else from `LANG`, else English.

use std::sync::OnceLock;

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use tracing::error;
use unic_langid::LanguageIdentifier;

use crate::error::Error;

/// A language the satellite speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Language {
    /// English, the default.
    #[default]
    En,
    /// French.
    Fr,
}

impl Language {
    /// Every language of the catalogue.
    pub const ALL: [Self; 2] = [Self::En, Self::Fr];

    /// The language of a locale or language tag (`fr`, `fr-FR`, `fr_FR.UTF-8`); `None` when the
    /// catalogue does not have it.
    pub fn parse(tag: &str) -> Option<Self> {
        let primary = tag.split(['_', '-', '.', '@']).next().unwrap_or_default().to_ascii_lowercase();
        match primary.as_str() {
            "en" => Some(Self::En),
            "fr" => Some(Self::Fr),
            _ => None,
        }
    }

    /// The language of `LANG`, else English.
    pub fn from_env() -> Self {
        std::env::var("LANG").ok().and_then(|lang| Self::parse(&lang)).unwrap_or_default()
    }

    /// Its tag (`en`).
    pub fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Fr => "fr",
        }
    }

    /// An error, as said to people: the pairing and link errors a person can act on are translated,
    /// others are given as the SDK words them (English).
    pub fn describe(self, error: &Error) -> String {
        let id = match error {
            Error::ProtocolUnsupported(_) => "error-protocol-unsupported",
            Error::PairingRefused(_) => "error-pairing-refused",
            Error::NotPairing => "error-not-pairing",
            Error::MethodNotDeclared(_) => "error-method-not-declared",
            Error::InvalidEnrollment(_) => "error-enrollment-code",
            Error::OtherCore { .. } => "error-other-core",
            Error::TokenRefused => "error-token-refused",
            Error::Http(_) => "error-unreachable",
            other => return other.to_string(),
        };
        self.say(id, &[])
    }

    /// The sentence `id` of the catalogue, with its arguments.
    pub(crate) fn say(self, id: &str, args: &[(&str, FluentValue<'_>)]) -> String {
        let bundle = bundle(self);
        let Some(pattern) = bundle.get_message(id).and_then(|message| message.value()) else {
            error!(id, language = self.tag(), "a sentence is missing from the catalogue");
            return id.to_owned();
        };
        let mut fluent_args = FluentArgs::new();
        for (name, value) in args {
            fluent_args.set(*name, value.clone());
        }
        let mut errors = Vec::new();
        let text = bundle.format_pattern(pattern, Some(&fluent_args), &mut errors);
        if !errors.is_empty() {
            error!(id, ?errors, "a sentence of the catalogue could not be formatted");
        }
        text.into_owned()
    }
}

const SOURCES: [(Language, &str); 2] =
    [(Language::En, include_str!("i18n/en.ftl")), (Language::Fr, include_str!("i18n/fr.ftl"))];

fn bundle(language: Language) -> &'static FluentBundle<FluentResource> {
    static BUNDLES: OnceLock<[FluentBundle<FluentResource>; 2]> = OnceLock::new();
    let [en, fr] = BUNDLES.get_or_init(|| SOURCES.map(|(language, source)| load(language, source)));
    match language {
        Language::En => en,
        Language::Fr => fr,
    }
}

fn load(language: Language, source: &str) -> FluentBundle<FluentResource> {
    let locale: LanguageIdentifier = language.tag().parse().unwrap_or_default();
    let mut bundle = FluentBundle::new_concurrent(vec![locale]);
    bundle.set_use_isolating(false); // no Unicode isolation marks around values: these go to logs and voices too
    let resource = FluentResource::try_new(source.to_owned()).unwrap_or_else(|(resource, errors)| {
        error!(language = language.tag(), ?errors, "the catalogue does not parse");
        resource
    });
    if let Err(errors) = bundle.add_resource(resource) {
        error!(language = language.tag(), ?errors, "the catalogue defines a sentence twice");
    }
    bundle
}

/// A device code's digits one by one, as said aloud: `042917` → `0, 4, 2, 9, 1, 7`.
pub(crate) fn digits(code: &str) -> String {
    code.chars().map(String::from).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(source: &str) -> Vec<&str> {
        assert!(FluentResource::try_new(source.to_owned()).is_ok(), "the catalogue parses");
        source
            .lines()
            .filter_map(|line| line.split_once(" = "))
            .map(|(id, _)| id)
            .filter(|id| !id.starts_with(' '))
            .collect()
    }

    #[test]
    fn every_language_has_every_sentence_and_parses() {
        let english = ids(SOURCES[0].1);
        for (language, source) in SOURCES {
            assert_eq!(ids(source), english, "{language:?}");
        }
    }

    #[test]
    fn the_language_comes_from_a_locale_and_defaults_to_english() {
        assert_eq!(Language::parse("fr_FR.UTF-8"), Some(Language::Fr));
        assert_eq!(Language::parse("en-GB"), Some(Language::En));
        assert_eq!(Language::parse("FR"), Some(Language::Fr));
        assert_eq!(Language::parse("de_DE"), None);
        assert_eq!(Language::parse("C"), None);
        assert_eq!(Language::default(), Language::En);
    }

    #[test]
    fn sentences_are_said_in_the_chosen_language_with_their_values() {
        assert_eq!(Language::En.say("pairing-code-shown", &[]), "To pair this device, type this code in Nestor:");
        let spoken = [("digits", FluentValue::from(digits("042917")))];
        assert_eq!(
            Language::Fr.say("pairing-code-spoken", &spoken),
            "Mon code d'appairage est 0, 4, 2, 9, 1, 7. Je répète : 0, 4, 2, 9, 1, 7."
        );
        let wait = [("seconds", FluentValue::from(2.0))];
        assert_eq!(Language::En.say("pairing-rate-limited", &wait), "Too many wrong codes. Trying again in 2 s.");
        let reason = [("reason", FluentValue::from("used"))];
        assert!(Language::Fr.say("pairing-refused", &reason).starts_with("Ce code d'enrôlement a déjà servi."));
        let other = [("reason", FluentValue::from("from-a-newer-core"))];
        assert!(Language::En.say("pairing-refused", &other).starts_with("This enrollment code is not valid."));
    }

    #[test]
    fn no_sentence_uses_an_em_dash() {
        for (language, source) in SOURCES {
            assert!(!source.contains('—'), "{language:?}");
        }
    }

    #[test]
    fn digits_are_said_one_by_one() {
        assert_eq!(digits("042917"), "0, 4, 2, 9, 1, 7");
    }

    #[test]
    fn errors_people_can_act_on_are_translated() {
        assert_eq!(Language::Fr.describe(&Error::NotPairing), "Cet appareil n'attend pas de code.");
        assert_eq!(Language::En.describe(&Error::Timeout("x")), "timed out: x");
    }
}

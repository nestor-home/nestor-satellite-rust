//! Enrollment codes and pairing URIs, as a person hands them to a device.
//!
//! An enrollment code is 8 symbols of Crockford's base32 (`K7QF2M9X`), shown grouped by four
//! (`K7QF-2M9X`). Reading it forgives case, dashes, spaces, and the letters Crockford maps to digits
//! (`O` → `0`, `I` and `L` → `1`). A pairing URI carries the core's address with the code:
//! `nestor+http://nestor.local:8471/?code=K7QF2M9X`.

use std::fmt;
use std::str::FromStr;

use reqwest::Url;

use crate::protocol::ProtocolError;

/// Crockford's base32: no `I`, `L`, `O` or `U`.
pub const ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// An enrollment code has this many symbols.
pub const ENROLLMENT_LENGTH: usize = 8;

/// An enrollment code, in its canonical form (8 symbols, upper case, no dash).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct EnrollmentCode(String);

impl EnrollmentCode {
    /// The code a person typed or scanned (`k7qf-2m9x` → `K7QF2M9X`); an error when it cannot be one.
    pub fn parse(text: &str) -> Result<Self, ProtocolError> {
        let code: String = text
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .map(|c| match c.to_ascii_uppercase() {
                'O' => '0',
                'I' | 'L' => '1',
                other => other,
            })
            .collect();
        if code.chars().count() != ENROLLMENT_LENGTH || !code.chars().all(|c| ALPHABET.contains(c)) {
            return Err(ProtocolError::new("an enrollment code is 8 letters and digits, like K7QF-2M9X"));
        }
        Ok(Self(code))
    }

    /// The canonical form, as sent to the core.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// As shown to people: `K7QF-2M9X`.
    pub fn grouped(&self) -> String {
        format!("{}-{}", &self.0[..4], &self.0[4..])
    }

    /// The SHA-256 of the canonical form, in lowercase hex: how a refused code is kept on disk
    /// (never in clear).
    pub fn digest(&self) -> String {
        let digest = ring::digest::digest(&ring::digest::SHA256, self.0.as_bytes());
        digest.as_ref().iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

impl fmt::Debug for EnrollmentCode {
    /// A code is a secret while it lives: it is never logged.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EnrollmentCode(…)")
    }
}

impl FromStr for EnrollmentCode {
    type Err = ProtocolError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

/// A pairing URI: the core's base URL and the enrollment code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingUri {
    core: String,
    code: EnrollmentCode,
}

impl PairingUri {
    /// `nestor+http://nestor.local:8471/?code=K7QF2M9X` → `http://nestor.local:8471` and `K7QF2M9X`.
    pub fn parse(uri: &str) -> Result<Self, ProtocolError> {
        let wrong = || ProtocolError::new("a pairing URI looks like nestor+http://host:port/?code=K7QF2M9X");
        let uri = uri.trim();
        let (scheme, rest) = uri.split_once("://").ok_or_else(wrong)?;
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "nestor+http" => "http",
            "nestor+https" => "https",
            _ => return Err(wrong()),
        };
        let url = Url::parse(&format!("{scheme}://{rest}")).map_err(|_| wrong())?;
        let codes: Vec<String> = url.query_pairs().filter(|(key, _)| key == "code").map(|(_, v)| v.into()).collect();
        let [code] = codes.as_slice() else {
            return Err(wrong());
        };
        Ok(Self { core: base_url(url.as_str()).map_err(|_| wrong())?, code: EnrollmentCode::parse(code)? })
    }

    /// The core's base URL (`http://nestor.local:8471`).
    pub fn core(&self) -> &str {
        &self.core
    }

    /// The enrollment code.
    pub fn code(&self) -> &EnrollmentCode {
        &self.code
    }
}

impl FromStr for PairingUri {
    type Err = ProtocolError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

/// An enrollment code as a person hands it over: a pairing URI (with the core's address) or the
/// code alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrollment {
    code: EnrollmentCode,
    core: Option<String>,
}

impl Enrollment {
    /// A pairing URI (`nestor+http…`) or an enrollment code (`K7QF-2M9X`).
    pub fn parse(text: &str) -> Result<Self, ProtocolError> {
        if text.trim().to_ascii_lowercase().starts_with("nestor+") {
            Ok(PairingUri::parse(text)?.into())
        } else {
            Ok(EnrollmentCode::parse(text)?.into())
        }
    }

    /// The enrollment code.
    pub fn code(&self) -> &EnrollmentCode {
        &self.code
    }

    /// The core's base URL, when it came in a pairing URI.
    pub fn core(&self) -> Option<&str> {
        self.core.as_deref()
    }
}

impl From<PairingUri> for Enrollment {
    fn from(uri: PairingUri) -> Self {
        Self { code: uri.code, core: Some(uri.core) }
    }
}

impl From<EnrollmentCode> for Enrollment {
    fn from(code: EnrollmentCode) -> Self {
        Self { code, core: None }
    }
}

impl FromStr for Enrollment {
    type Err = ProtocolError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

/// A core's base URL as this SDK compares and stores it: `scheme://host[:port][/path]`, without a
/// trailing slash, query or fragment.
pub(crate) fn base_url(text: &str) -> Result<String, ProtocolError> {
    let wrong = || ProtocolError::new(format!("the core's address {text:?} must be an http(s) URL"));
    let url = Url::parse(text.trim()).map_err(|_| wrong())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(wrong());
    }
    Ok(format!("{}{}", url.origin().ascii_serialization(), url.path().trim_end_matches('/')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_enrollment_code_forgives_case_dashes_spaces_and_look_alike_letters() {
        for typed in ["K7QF2M9X", "k7qf-2m9x", " K7QF 2M9X ", "k7qf-2m9x\n"] {
            assert_eq!(EnrollmentCode::parse(typed).unwrap().as_str(), "K7QF2M9X", "{typed:?}");
        }
        assert_eq!(EnrollmentCode::parse("oil0-ILOO").unwrap().as_str(), "01101100");
        assert_eq!(EnrollmentCode::parse("K7QF2M9X").unwrap().grouped(), "K7QF-2M9X");
    }

    #[test]
    fn a_code_s_digest_is_the_sha256_of_its_canonical_form() {
        let digest = EnrollmentCode::parse("k7qf-2m9x").unwrap().digest();
        assert_eq!(digest, "af9f1438361c898452f3da3b89fa0541991ac63a6b120e08cb9c41c2c06f7e86");
        assert_eq!(EnrollmentCode::parse("K7QF2M9X").unwrap().digest(), digest);
    }

    #[test]
    fn what_is_not_eight_crockford_symbols_is_no_enrollment_code() {
        for wrong in ["", "K7QF2M9", "K7QF2M9XY", "K7QF2M9U", "K7QF_2M9X", "K7QF2M9É", "042917"] {
            assert!(EnrollmentCode::parse(wrong).is_err(), "{wrong:?}");
        }
    }

    #[test]
    fn an_enrollment_code_is_never_logged() {
        let code = EnrollmentCode::parse("K7QF2M9X").unwrap();
        assert!(!format!("{code:?} {:?}", Enrollment::from(code.clone())).contains("K7QF"));
    }

    #[test]
    fn a_pairing_uri_gives_the_core_s_base_url_and_the_code() {
        let uri = PairingUri::parse("nestor+http://nestor.local:8471/?code=K7QF2M9X").unwrap();
        assert_eq!((uri.core(), uri.code().as_str()), ("http://nestor.local:8471", "K7QF2M9X"));
        let uri = PairingUri::parse("NESTOR+HTTPS://nestor.example.org/satellites-proxy/?code=k7qf-2m9x").unwrap();
        assert_eq!((uri.core(), uri.code().as_str()), ("https://nestor.example.org/satellites-proxy", "K7QF2M9X"));
        let uri = PairingUri::parse("nestor+http://127.0.0.1:9/?code=K7QF%202M9X").unwrap();
        assert_eq!((uri.core(), uri.code().as_str()), ("http://127.0.0.1:9", "K7QF2M9X"));
    }

    #[test]
    fn a_uri_without_the_nestor_scheme_or_a_single_valid_code_is_refused() {
        for wrong in [
            "http://nestor.local:8471/?code=K7QF2M9X",
            "nestor+ftp://nestor.local/?code=K7QF2M9X",
            "nestor+http://nestor.local:8471/",
            "nestor+http://nestor.local:8471/?code=K7QF2M9X&code=K7QF2M9X",
            "nestor+http://nestor.local:8471/?code=nope",
            "nestor+http:///?code=K7QF2M9X",
        ] {
            assert!(PairingUri::parse(wrong).is_err(), "{wrong}");
        }
    }

    #[test]
    fn an_enrollment_is_a_pairing_uri_or_a_code_alone() {
        let from_uri = Enrollment::parse("nestor+http://nestor.local:8471/?code=K7QF2M9X").unwrap();
        assert_eq!(from_uri.core(), Some("http://nestor.local:8471"));
        let alone = Enrollment::parse("K7QF-2M9X").unwrap();
        assert_eq!((alone.core(), alone.code().as_str()), (None, "K7QF2M9X"));
    }

    #[test]
    fn a_core_s_address_is_compared_without_trailing_slash() {
        assert_eq!(base_url("http://Nestor.local:8471/").unwrap(), "http://nestor.local:8471");
        assert_eq!(base_url("https://h/proxy/").unwrap(), "https://h/proxy");
        assert!(base_url("nestor.local:8471").is_err());
        assert!(base_url("ftp://h").is_err());
    }
}

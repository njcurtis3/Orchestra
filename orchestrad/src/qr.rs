//! A pairing offer drawn as a terminal QR code.
//!
//! The text is the offer JSON the core builds; it carries the pairing secret, so it goes
//! to the terminal that asked and nowhere else (not a log, not the journal).

use qrcode::render::unicode::Dense1x2;
use qrcode::{EcLevel, QrCode};

/// The offer does not fit in a QR code at error-correction level M.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TooLarge;

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the pairing offer is too large for a QR code")
    }
}

impl std::error::Error for TooLarge {}

/// Two rows of modules per text line, with a quiet zone. Modules are drawn light on the
/// terminal's dark background, the usual look for `qrencode -t ansiutf8`; a phone scanner
/// reads that as is.
pub fn render(offer: &str) -> Result<String, TooLarge> {
    let code = QrCode::with_error_correction_level(offer.as_bytes(), EcLevel::M)
        .map_err(|_| TooLarge)?;
    Ok(code
        .render::<Dense1x2>()
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .quiet_zone(true)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let offer = r#"{"v":1,"name":"host","secret":"abc"}"#;
        assert_eq!(render(offer).unwrap(), render(offer).unwrap());
    }

    #[test]
    fn has_a_quiet_zone_and_several_lines() {
        let out = render("hello").unwrap();
        assert!(out.lines().count() > 8);
        let first = out.lines().next().unwrap();
        assert!(first.chars().all(|c| c == '█'), "top row is quiet zone: {first:?}");
    }

    #[test]
    fn grows_with_payload() {
        let small = render(&"a".repeat(20)).unwrap();
        let large = render(&"a".repeat(400)).unwrap();
        assert!(large.lines().count() > small.lines().count());
        assert!(large.lines().next().unwrap().chars().count() > small.lines().next().unwrap().chars().count());
    }

    #[test]
    fn refuses_5000_bytes() {
        assert_eq!(render(&"a".repeat(5000)), Err(TooLarge));
    }
}

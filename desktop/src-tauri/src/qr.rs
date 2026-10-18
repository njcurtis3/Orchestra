//! The pairing QR, drawn in Rust so the secret it carries reaches the webview only as an
//! image. The SVG uses presentation attributes and nothing else: the page's CSP has no
//! `unsafe-inline` for styles, so a `style` attribute or element would be blocked.

use qrcode::render::svg;
use qrcode::{EcLevel, QrCode};

/// `text` as an SVG string, error correction M.
pub fn svg_of(text: &str) -> Result<String, String> {
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M)
        .map_err(|_| "could not make the pairing code".to_string())?;
    Ok(code
        .render::<svg::Color>()
        .min_dimensions(280, 280)
        .quiet_zone(true)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_svg_has_no_style_script_or_link() {
        let svg = svg_of(r#"{"v":1,"host_name":"Desk","addrs":["127.0.0.1:47800"]}"#).unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains(r#"fill="#));
        for banned in ["style", "<script", "href", "onload", "<image", "<foreignObject"] {
            assert!(!svg.contains(banned), "svg contains {banned}");
        }
    }

    #[test]
    fn qr_svg_is_deterministic_and_grows_with_the_payload() {
        let short = svg_of("a").unwrap();
        assert_eq!(short, svg_of("a").unwrap());
        let long = svg_of(&"x".repeat(400)).unwrap();
        assert!(long.len() > short.len());
    }

    #[test]
    fn qr_svg_refuses_a_payload_too_big_for_a_code() {
        assert!(svg_of(&"x".repeat(5000)).is_err());
    }
}

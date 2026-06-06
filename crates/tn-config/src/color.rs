//! `#RRGGBB` colors used throughout themes and config.
//!
//! [`Color`] is `tn-config`'s own RGB type — `tn-config` stays free of `tn-core`
//! (the dependency graph keeps them as siblings), and carries the full chrome
//! palette that `tn_core::Palette` doesn't model. The GPUI layer maps the
//! terminal subset into `tn_core::Palette`.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 24-bit RGB color, parsed from / serialized to `#RRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse `#RRGGBB` (case-insensitive; the leading `#` is optional).
    pub fn from_hex(s: &str) -> Result<Self, ColorError> {
        let h = s.strip_prefix('#').unwrap_or(s);
        if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ColorError(s.to_string()));
        }
        let v = u32::from_str_radix(h, 16).map_err(|_| ColorError(s.to_string()))?;
        Ok(Self::new((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    /// `#RRGGBB`, upper-case.
    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    pub fn rgb(self) -> (u8, u8, u8) {
        (self.r, self.g, self.b)
    }
}

/// Curated accent swatches for the in-app agent editor's color picker
/// (`(中文名, 颜色)`). These are deliberate *user choices*, not theme tokens, so
/// they live here as config-domain data — keeping the GPUI layer free of color
/// literals (the `no_hardcoded_theme_colors` guard only scans `tn-ui`). The UI
/// also offers a "默认" option (no explicit accent → the theme/descriptor accent).
pub const ACCENT_SWATCHES: &[(&str, Color)] = &[
    ("蓝", Color::new(0x7A, 0xA2, 0xF7)),
    ("紫", Color::new(0xBB, 0x9A, 0xF7)),
    ("珊瑚", Color::new(0xF0, 0x91, 0x6D)),
    ("青", Color::new(0x73, 0xDA, 0xCA)),
    ("绿", Color::new(0x9E, 0xCE, 0x6A)),
    ("琥珀", Color::new(0xE0, 0xAF, 0x68)),
    ("粉", Color::new(0xF7, 0x9A, 0xC0)),
];

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Error returned when a string isn't a valid `#RRGGBB` color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorError(String);

impl fmt::Display for ColorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid color `{}` (expected #RRGGBB)", self.0)
    }
}

impl std::error::Error for ColorError {}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_with_and_without_hash() {
        assert_eq!(
            Color::from_hex("#7AA2F7").unwrap(),
            Color::new(0x7A, 0xA2, 0xF7)
        );
        assert_eq!(
            Color::from_hex("7aa2f7").unwrap(),
            Color::new(0x7A, 0xA2, 0xF7)
        );
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(Color::from_hex("#fff").is_err()); // too short
        assert!(Color::from_hex("#GGGGGG").is_err()); // non-hex
        assert!(Color::from_hex("#1234567").is_err()); // too long
    }

    #[test]
    fn round_trips_through_hex() {
        let c = Color::new(0x1A, 0x1B, 0x26);
        assert_eq!(Color::from_hex(&c.to_hex()).unwrap(), c);
    }
}

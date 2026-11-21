//! Text encodings and the dBase language-driver (LDID) mapping.

use std::fmt;

use crate::codepages::{CP437_HIGH, CP850_HIGH, CP860_HIGH, CP1252_HIGH};

/// Character encoding of text fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Encoding {
    /// Pick from the language-driver byte at header offset 29 (see
    /// [`Encoding::from_ldid`]). Never returned as a resolved encoding.
    #[default]
    Auto,
    /// ISO-8859-1: every byte maps to the code point of the same value.
    Latin1,
    /// IBM PC code page 437 (US MS-DOS).
    Cp437,
    /// IBM PC code page 850 (multilingual MS-DOS). DATASUS SIH files use it.
    Cp850,
    /// IBM PC code page 860 (Portuguese MS-DOS).
    Cp860,
    /// Windows-1252. The five undefined bytes (0x81, 0x8D, 0x8F, 0x90, 0x9D)
    /// map to the C1 control of the same value, like the WHATWG decoder, so
    /// decoding never fails and is lossless.
    Cp1252,
    /// UTF-8. Invalid sequences are a hard error.
    Utf8,
}

impl Encoding {
    /// Resolve `Auto` from a language-driver ID. The table is the xBase LDID
    /// table as shipped by dbfread 2.0.7 (`codepages.py`, from Ethan Furman's
    /// `dbf`), restricted to the code pages this crate implements:
    ///
    /// * CP437: 0x01, 0x09, 0x0B, 0x0D, 0x0F, 0x11, 0x15, 0x18, 0x19, 0x1B
    /// * CP850: 0x02, 0x0A, 0x0E, 0x10, 0x12, 0x14, 0x16, 0x1A, 0x1D, 0x25, 0x37
    /// * CP860: 0x24 (dbfread: `('cp860', 'Portuguese OEM')`)
    /// * CP1252: 0x03, 0x57, 0x58, 0x59
    ///
    /// **Everything else falls back to Latin-1**: 0x00 ("no driver", the
    /// most common value in DATASUS files), unknown IDs, and IDs of code
    /// pages this crate does not implement (e.g. 0x26 = CP866, 0x4D = CP936,
    /// 0xC8 = CP1250). Latin-1 never fails and maps each byte to the code
    /// point of the same value, so the original bytes stay recoverable; pass
    /// an explicit encoding when the LDID is known to be wrong or missing.
    pub fn from_ldid(ldid: u8) -> Encoding {
        match ldid {
            0x01 | 0x09 | 0x0B | 0x0D | 0x0F | 0x11 | 0x15 | 0x18 | 0x19 | 0x1B => Encoding::Cp437,
            0x02 | 0x0A | 0x0E | 0x10 | 0x12 | 0x14 | 0x16 | 0x1A | 0x1D | 0x25 | 0x37 => {
                Encoding::Cp850
            }
            0x24 => Encoding::Cp860,
            0x03 | 0x57..=0x59 => Encoding::Cp1252,
            _ => Encoding::Latin1,
        }
    }

    /// `self`, or the LDID-derived encoding if `self` is `Auto`.
    pub fn resolve(self, ldid: u8) -> Encoding {
        match self {
            Encoding::Auto => Encoding::from_ldid(ldid),
            e => e,
        }
    }

    /// A lowercase name (`"cp850"`, `"latin1"`, ...).
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Auto => "auto",
            Encoding::Latin1 => "latin1",
            Encoding::Cp437 => "cp437",
            Encoding::Cp850 => "cp850",
            Encoding::Cp860 => "cp860",
            Encoding::Cp1252 => "cp1252",
            Encoding::Utf8 => "utf8",
        }
    }

    /// Parse a name as accepted by [`Encoding::name`], plus common aliases
    /// (`iso-8859-1`, `latin-1`, `windows-1252`, `utf-8`, ...),
    /// case-insensitively.
    pub fn from_name(name: &str) -> Option<Encoding> {
        let n: String = name
            .chars()
            .filter(|c| !matches!(c, '-' | '_' | ' '))
            .map(|c| c.to_ascii_lowercase())
            .collect();
        Some(match n.as_str() {
            "auto" => Encoding::Auto,
            "latin1" | "iso88591" | "l1" => Encoding::Latin1,
            "cp437" | "ibm437" | "437" => Encoding::Cp437,
            "cp850" | "ibm850" | "850" => Encoding::Cp850,
            "cp860" | "ibm860" | "860" => Encoding::Cp860,
            "cp1252" | "windows1252" | "1252" => Encoding::Cp1252,
            "utf8" => Encoding::Utf8,
            _ => return None,
        })
    }

    fn high_table(self) -> Option<&'static [char; 128]> {
        match self {
            Encoding::Cp437 => Some(&CP437_HIGH),
            Encoding::Cp850 => Some(&CP850_HIGH),
            Encoding::Cp860 => Some(&CP860_HIGH),
            Encoding::Cp1252 => Some(&CP1252_HIGH),
            _ => None,
        }
    }

    /// Decode `bytes` to UTF-8. Pure-ASCII input is borrowed without
    /// copying; otherwise the text is built in `scratch` (reused, so no
    /// allocation once it has grown). `Auto` is treated as Latin-1.
    pub fn decode<'a>(
        self,
        bytes: &'a [u8],
        scratch: &'a mut String,
    ) -> Result<&'a str, DecodeError> {
        if bytes.is_ascii() {
            // ASCII is valid UTF-8, so this cannot fail.
            return std::str::from_utf8(bytes).map_err(|_| DecodeError { valid_up_to: 0 });
        }
        scratch.clear();
        match self {
            Encoding::Utf8 => {
                return std::str::from_utf8(bytes).map_err(|e| DecodeError {
                    valid_up_to: e.valid_up_to(),
                });
            }
            Encoding::Latin1 | Encoding::Auto => {
                scratch.extend(bytes.iter().map(|&b| b as char));
            }
            other => {
                let t = other.high_table().expect("code page table");
                scratch.extend(bytes.iter().map(|&b| {
                    if b < 0x80 {
                        b as char
                    } else {
                        t[(b - 0x80) as usize]
                    }
                }));
            }
        }
        Ok(scratch.as_str())
    }
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Invalid UTF-8 in a field read with [`Encoding::Utf8`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError {
    /// Length of the valid prefix, in bytes.
    pub valid_up_to: usize,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid UTF-8 after {} valid bytes", self.valid_up_to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(e: Encoding, b: &[u8]) -> String {
        let mut s = String::new();
        e.decode(b, &mut s).unwrap().to_owned()
    }

    #[test]
    fn spot_checks_against_python_codecs() {
        assert_eq!(dec(Encoding::Cp850, &[0xA0]), "á");
        assert_eq!(dec(Encoding::Cp850, &[0x87]), "ç");
        assert_eq!(dec(Encoding::Cp850, &[0xA3]), "ú");
        assert_eq!(dec(Encoding::Cp850, &[0x80]), "Ç");
        assert_eq!(dec(Encoding::Cp850, &[0xA7]), "º");
        assert_eq!(dec(Encoding::Cp850, &[0xC6, 0xE4]), "ãõ");
        assert_eq!(dec(Encoding::Cp850, b"AN\xa0LISE"), "ANáLISE");
        assert_eq!(dec(Encoding::Cp437, &[0x80, 0x9E, 0xE1]), "Ç₧ß");
        assert_eq!(dec(Encoding::Cp1252, &[0x80]), "€");
        // CP860: bytes from Python's `"ANÁLISE ... Nº".encode("cp860")`.
        assert_eq!(dec(Encoding::Cp860, b"AN\x86LISE"), "ANÁLISE");
        assert_eq!(dec(Encoding::Cp860, b"CIR\x96RGICA"), "CIRÚRGICA");
        assert_eq!(dec(Encoding::Cp860, b"INFORMA\x80\x8eO"), "INFORMAÇÃO");
        assert_eq!(dec(Encoding::Cp860, b"N\xa7"), "Nº");
        assert_eq!(dec(Encoding::Cp860, &[0x84, 0x94, 0x8F, 0x99]), "ãõÂÕ");
        assert_eq!(
            dec(Encoding::Cp1252, &[0x81, 0x8D, 0x8F, 0x90, 0x9D]),
            "\u{81}\u{8D}\u{8F}\u{90}\u{9D}"
        );
        assert_eq!(dec(Encoding::Cp1252, &[0xE7, 0xE3]), "çã");
        assert_eq!(dec(Encoding::Latin1, b"N\xba"), "Nº");
        assert_eq!(dec(Encoding::Utf8, "ação".as_bytes()), "ação");
    }

    #[test]
    fn every_byte_decodes_to_one_char() {
        for e in [
            Encoding::Latin1,
            Encoding::Cp437,
            Encoding::Cp850,
            Encoding::Cp860,
            Encoding::Cp1252,
        ] {
            for b in 0..=255u8 {
                assert_eq!(dec(e, &[b]).chars().count(), 1, "{e} {b:#x}");
            }
        }
    }

    #[test]
    fn ascii_is_borrowed() {
        let mut s = String::new();
        let input = b"HELLO";
        let out = Encoding::Cp850.decode(input, &mut s).unwrap();
        assert_eq!(out.as_ptr(), input.as_ptr());
    }

    #[test]
    fn invalid_utf8() {
        let mut s = String::new();
        assert_eq!(
            Encoding::Utf8.decode(b"ab\xff", &mut s),
            Err(DecodeError { valid_up_to: 2 })
        );
    }

    #[test]
    fn ldid() {
        assert_eq!(Encoding::from_ldid(0x02), Encoding::Cp850);
        assert_eq!(Encoding::from_ldid(0x01), Encoding::Cp437);
        assert_eq!(Encoding::from_ldid(0x03), Encoding::Cp1252);
        assert_eq!(Encoding::from_ldid(0x57), Encoding::Cp1252);
        assert_eq!(Encoding::from_ldid(0x00), Encoding::Latin1);
        assert_eq!(Encoding::from_ldid(0x35), Encoding::Latin1);
        assert_eq!(Encoding::from_ldid(0x24), Encoding::Cp860);
        assert_eq!(Encoding::from_ldid(0x26), Encoding::Latin1);
        assert_eq!(Encoding::from_ldid(0xFF), Encoding::Latin1);
        assert_eq!(Encoding::from_name("CP860"), Some(Encoding::Cp860));
        assert_eq!(Encoding::from_name("ibm-860"), Some(Encoding::Cp860));
        assert_eq!(Encoding::Cp860.name(), "cp860");
        assert_eq!(Encoding::Auto.resolve(0x02), Encoding::Cp850);
        assert_eq!(Encoding::Utf8.resolve(0x02), Encoding::Utf8);
        assert_eq!(Encoding::from_name("ISO-8859-1"), Some(Encoding::Latin1));
        assert_eq!(Encoding::from_name("windows-1252"), Some(Encoding::Cp1252));
        assert_eq!(Encoding::from_name("klingon"), None);
    }
}

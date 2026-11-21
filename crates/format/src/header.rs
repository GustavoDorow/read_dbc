//! The dBase III table header.

use std::io::{self, Read};

use crate::dcl::read_some;
use crate::encoding::Encoding;
use crate::error::{Error, Result};

/// One field descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Field name: descriptor bytes 0..11 up to the first NUL, decoded with
    /// the resolved encoding.
    pub name: String,
    /// The ASCII type byte (`b'C'`, `b'N'`, `b'D'`, ...).
    pub field_type: u8,
    /// Width in bytes (descriptor byte 16).
    pub length: usize,
    /// Decimal count (descriptor byte 17).
    pub decimals: u8,
    /// Byte offset of the field inside a record (the deletion flag is at
    /// offset 0, so the first field starts at 1).
    pub offset: usize,
}

impl Field {
    /// The type byte as a `char`.
    pub fn type_char(&self) -> char {
        self.field_type as char
    }
}

/// The parsed table header (the first `H` bytes of the file).
#[derive(Debug, Clone)]
pub struct Header {
    /// Version byte (offset 0).
    pub version: u8,
    /// Last-update date as stored (offsets 1..4: years since 1900, month, day).
    pub last_update: [u8; 3],
    /// Declared record count `N` (offset 4). Untrusted until the data has
    /// been read to the end.
    pub record_count: u32,
    /// Header length `H` (offset 8).
    pub header_len: u16,
    /// Record length `R`, including the deletion flag (offset 10).
    pub record_len: u16,
    /// Language-driver ID (offset 29).
    pub ldid: u8,
    /// The byte at `H - 1` (0x0D, or 0x00 in some DATASUS files).
    pub terminator: u8,
    /// Field descriptors in record order.
    pub fields: Vec<Field>,
}

/// Version bytes of dBase III-compatible tables, which share the 32-byte
/// header and 32-byte field-descriptor layout:
/// 0x02 FoxBASE, 0x03 dBase III/FoxBASE+ without memo, 0x30/0x31/0x32
/// Visual FoxPro, 0x43/0x63/0x8E/0xCB dBase IV SQL tables, 0x83 dBase III
/// with memo, 0x8B dBase IV with memo, 0xF5 FoxPro 2 with memo, 0xFB
/// FoxBASE. dBase 7 (0x04, 48-byte descriptors) is rejected.
pub const SUPPORTED_VERSIONS: &[u8] = &[
    0x02, 0x03, 0x30, 0x31, 0x32, 0x43, 0x63, 0x83, 0x8B, 0x8E, 0xCB, 0xF5, 0xFB,
];

/// Read until `buf` is full or the input ends; returns the byte count.
pub(crate) fn read_full<R: Read + ?Sized>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match read_some(r, &mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

/// Read exactly `buf.len()` bytes that are part of the header area ending
/// at file offset `end`; a short read is [`Error::TruncatedHeader`].
pub(crate) fn read_header_bytes<R: Read>(
    r: &mut R,
    buf: &mut [u8],
    start: u64,
    end: u64,
) -> Result<()> {
    let got = read_full(r, buf)?;
    if got < buf.len() {
        return Err(Error::TruncatedHeader {
            expected: end,
            got: start + got as u64,
        });
    }
    Ok(())
}

impl Header {
    /// Read and validate the header from `r` (positioned at file offset 0).
    /// Returns the header and its raw bytes (needed for the DBC CRC).
    ///
    /// Reads exactly `H` bytes (at most 64 KiB) and nothing else, so it is
    /// cheap enough for a query planner: combine it with
    /// [`resolve_schema`](crate::resolve_schema) to learn the output columns
    /// without opening a [`DbcReader`](crate::DbcReader). `encoding` is used
    /// only to decode field names (resolved from the LDID when `Auto`).
    pub fn read<R: Read>(r: &mut R, encoding: Encoding) -> Result<(Header, Vec<u8>)> {
        let mut fixed = [0u8; 32];
        read_header_bytes(r, &mut fixed, 0, 32)?;
        let version = fixed[0];
        if !SUPPORTED_VERSIONS.contains(&version) {
            return Err(Error::UnsupportedVersion { version });
        }
        let header_len = u16::from_le_bytes([fixed[8], fixed[9]]);
        let h = header_len as usize;
        // 32 fixed bytes + at least one 32-byte descriptor + terminator.
        if h < 65 {
            return Err(Error::InvalidHeader {
                offset: 8,
                message: format!("header length {h} is too small for any field descriptor"),
            });
        }
        let mut raw = vec![0u8; h];
        raw[..32].copy_from_slice(&fixed);
        read_header_bytes(r, &mut raw[32..], 32, h as u64)?;
        let header = Self::parse(&raw, encoding)?;
        Ok((header, raw))
    }

    /// Parse a complete raw header (exactly `H` bytes).
    pub fn parse(raw: &[u8], encoding: Encoding) -> Result<Header> {
        if raw.len() < 32 {
            return Err(Error::TruncatedHeader {
                expected: 32,
                got: raw.len() as u64,
            });
        }
        let version = raw[0];
        if !SUPPORTED_VERSIONS.contains(&version) {
            return Err(Error::UnsupportedVersion { version });
        }
        let record_count = u32::from_le_bytes(raw[4..8].try_into().unwrap());
        let header_len = u16::from_le_bytes([raw[8], raw[9]]);
        let record_len = u16::from_le_bytes([raw[10], raw[11]]);
        let ldid = raw[29];
        let h = header_len as usize;
        if raw.len() != h {
            return Err(Error::InvalidHeader {
                offset: 8,
                message: format!("header length field says {h} bytes, got {}", raw.len()),
            });
        }
        if h < 65 {
            return Err(Error::InvalidHeader {
                offset: 8,
                message: format!("header length {h} is too small for any field descriptor"),
            });
        }
        let enc = encoding.resolve(ldid);
        // Descriptors run from byte 32 in 32-byte steps until a 0x0D
        // terminator byte, or until the slot at H - 1 is reached.
        let mut fields = Vec::new();
        let mut pos = 32usize;
        let mut offset = 1usize;
        let terminator;
        loop {
            if raw[pos] == 0x0D {
                terminator = 0x0D;
                break;
            }
            if pos == h - 1 {
                let b = raw[pos];
                if b != 0x00 {
                    return Err(Error::BadTerminator {
                        offset: pos as u64,
                        byte: b,
                    });
                }
                terminator = b;
                break;
            }
            if pos + 32 > h - 1 {
                return Err(Error::InvalidHeader {
                    offset: pos as u64,
                    message: format!(
                        "header length {h} is not 32 + 32 * fields + 1 (descriptor at {pos} overruns the terminator)"
                    ),
                });
            }
            let d = &raw[pos..pos + 32];
            let name_end = d[..11].iter().position(|&b| b == 0).unwrap_or(11);
            let mut scratch = String::new();
            let name = enc
                .decode(&d[..name_end], &mut scratch)
                .map(str::to_owned)
                .unwrap_or_else(|_| String::from_utf8_lossy(&d[..name_end]).into_owned());
            let field_type = d[11];
            let length = d[16] as usize;
            let decimals = d[17];
            if name.is_empty() {
                return Err(Error::InvalidHeader {
                    offset: pos as u64,
                    message: format!("field descriptor {} has an empty name", fields.len() + 1),
                });
            }
            if length == 0 {
                return Err(Error::InvalidHeader {
                    offset: pos as u64 + 16,
                    message: format!("field {name:?} has zero length"),
                });
            }
            fields.push(Field {
                name,
                field_type,
                length,
                decimals,
                offset,
            });
            offset += length;
            pos += 32;
        }
        if fields.is_empty() {
            return Err(Error::NoFields);
        }
        if offset as u64 != record_len as u64 {
            return Err(Error::RecordLengthMismatch {
                declared: record_len,
                computed: offset as u64,
            });
        }
        Ok(Header {
            version,
            last_update: [raw[1], raw[2], raw[3]],
            record_count,
            header_len,
            record_len,
            ldid,
            terminator,
            fields,
        })
    }

    /// Size in bytes of the declared record area (`N * R`).
    pub fn data_len(&self) -> u64 {
        self.record_count as u64 * self.record_len as u64
    }
}

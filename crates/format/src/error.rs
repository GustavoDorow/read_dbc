//! The crate's error type.

use std::fmt;
use std::io;

use crate::dcl::BlastError;

/// Everything that can go wrong while reading a `.dbc` or `.dbf` file.
///
/// Offsets are byte positions in the *file* for header, compressed-stream
/// and trailing-data errors, and positions in the *logical DBF* (the stored
/// header followed by the decompressed record area, i.e. the equivalent
/// `.dbf` file) for record and value errors. Record indexes are 0-based
/// physical record numbers (deleted records count).
#[derive(Debug)]
#[non_exhaustive]
#[allow(missing_docs)] // variant fields are described by the variant docs
pub enum Error {
    /// The underlying reader failed.
    Io(io::Error),
    /// The file ends inside the header (or the CRC field that follows it).
    TruncatedHeader { expected: u64, got: u64 },
    /// The version byte is not a dBase III-compatible table.
    UnsupportedVersion { version: u8 },
    /// The header is structurally invalid.
    InvalidHeader { offset: u64, message: String },
    /// The header terminator at `H - 1` is neither 0x0D nor 0x00.
    BadTerminator { offset: u64, byte: u8 },
    /// The record length in the header disagrees with the field widths.
    RecordLengthMismatch { declared: u16, computed: u64 },
    /// The header declares no fields.
    NoFields,
    /// An invalid reader option (type override, projection, ...).
    InvalidOption(String),
    /// The compressed stream is invalid (blast error). `offset` is the file
    /// offset of the compressed byte being decoded when it was detected.
    Dcl { error: BlastError, offset: u64 },
    /// Whole bytes follow the end of the compressed stream (or, for plain
    /// `.dbf`, the data). Never observed in DATASUS files.
    TrailingData { offset: u64 },
    /// The record data ends before the declared number of records.
    TruncatedRecords {
        record: u64,
        declared: u64,
        offset: u64,
    },
    /// The record data continues past the declared records, or the byte
    /// after them is not the 0x1A end-of-file marker.
    ExcessData {
        offset: u64,
        byte: u8,
        after_eof_marker: bool,
    },
    /// The stored CRC-32 does not match the header plus decompressed data.
    CrcMismatch { stored: u32, computed: u32 },
    /// A record's deletion flag is neither `' '` nor `'*'`.
    BadDeletionFlag { record: u64, offset: u64, byte: u8 },
    /// A field value cannot be converted to its column type.
    InvalidValue {
        record: u64,
        field: String,
        offset: u64,
        raw: Vec<u8>,
        reason: String,
    },
    /// A previous call returned an error; the reader cannot continue.
    Poisoned,
}

/// Show raw bytes as an escaped byte string (`b"AB\x00"`).
pub(crate) fn show_bytes(raw: &[u8]) -> String {
    let mut s = String::from("b\"");
    for &b in raw {
        match b {
            b'"' => s.push_str("\\\""),
            b'\\' => s.push_str("\\\\"),
            0x20..=0x7E => s.push(b as char),
            _ => s.push_str(&format!("\\x{b:02x}")),
        }
    }
    s.push('"');
    s
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::TruncatedHeader { expected, got } => {
                write!(
                    f,
                    "file too short: header needs {expected} bytes but only {got} are present"
                )
            }
            Error::UnsupportedVersion { version } => {
                write!(
                    f,
                    "unsupported dBase version byte 0x{version:02x} (expected a dBase III-compatible table)"
                )
            }
            Error::InvalidHeader { offset, message } => {
                write!(f, "invalid DBF header at byte {offset}: {message}")
            }
            Error::BadTerminator { offset, byte } => write!(
                f,
                "invalid DBF header: terminator at byte {offset} is 0x{byte:02x} (expected 0x0d or 0x00)"
            ),
            Error::RecordLengthMismatch { declared, computed } => write!(
                f,
                "invalid DBF header: record length is {declared} but the fields add up to {computed} (including the deletion flag)"
            ),
            Error::NoFields => f.write_str("invalid DBF header: no field descriptors"),
            Error::InvalidOption(msg) => f.write_str(msg),
            Error::Dcl { error, offset } => {
                write!(
                    f,
                    "corrupt compressed data at file offset {offset}: {error}"
                )
            }
            Error::TrailingData { offset } => {
                write!(
                    f,
                    "unexpected trailing bytes at file offset {offset} after the end of the data"
                )
            }
            Error::TruncatedRecords {
                record,
                declared,
                offset,
            } => write!(
                f,
                "truncated data: the header declares {declared} records but the data ends in record {record} (at logical offset {offset})"
            ),
            Error::ExcessData {
                offset,
                byte,
                after_eof_marker,
            } => {
                if *after_eof_marker {
                    write!(
                        f,
                        "data continues after the 0x1a end-of-file marker (byte 0x{byte:02x} at logical offset {offset}); the header's record count is wrong or the file is corrupt"
                    )
                } else {
                    write!(
                        f,
                        "data continues past the declared records (byte 0x{byte:02x} at logical offset {offset}, expected end of data or the 0x1a marker); the header's record count is wrong or the file is corrupt"
                    )
                }
            }
            Error::CrcMismatch { stored, computed } => write!(
                f,
                "CRC-32 mismatch: the file stores 0x{stored:08x} but the header and decompressed data give 0x{computed:08x}; the file is corrupt"
            ),
            Error::BadDeletionFlag {
                record,
                offset,
                byte,
            } => write!(
                f,
                "corrupt record {record}: deletion flag is 0x{byte:02x} at logical offset {offset} (expected ' ' or '*')"
            ),
            Error::InvalidValue {
                record,
                field,
                offset,
                raw,
                reason,
            } => write!(
                f,
                "record {record}, field {field:?} (logical offset {offset}): {reason}: {}",
                show_bytes(raw)
            ),
            Error::Poisoned => f.write_str("the reader already failed; it cannot continue"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

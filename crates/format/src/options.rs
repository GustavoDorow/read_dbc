//! Reader options.

use crate::encoding::Encoding;
use crate::schema::ColumnType;

/// Options for [`DbcReader`](crate::DbcReader).
#[derive(Debug, Clone)]
pub struct ReadOptions {
    /// Text encoding. Default [`Encoding::Auto`] (from the LDID byte).
    pub encoding: Encoding,
    /// Read every field as text (C rules: trailing spaces/NULs stripped,
    /// everything else preserved). Also allows field types that have no
    /// typed mapping. Per-field [`types`](Self::types) still apply on top.
    pub all_varchar: bool,
    /// Yield deleted (`'*'`) records too; the sink's
    /// [`BatchSink::deleted`](crate::BatchSink::deleted) receives the flag.
    pub include_deleted: bool,
    /// Compare the stored CRC-32 of a `.dbc` at the end of the data
    /// (default true). Ignored for plain `.dbf`.
    pub verify_crc: bool,
    /// Per-field type overrides: `(field name, type)`, the name matched
    /// ASCII case-insensitively. Supported: any field → `Varchar`;
    /// `C` → `Date` (strict `YYYYMMDD`) and `C` → `BigInt` (strict integer);
    /// and any field → its natural type.
    pub types: Vec<(String, ColumnType)>,
}

impl Default for ReadOptions {
    fn default() -> Self {
        ReadOptions {
            encoding: Encoding::Auto,
            all_varchar: false,
            include_deleted: false,
            verify_crc: true,
            types: Vec::new(),
        }
    }
}

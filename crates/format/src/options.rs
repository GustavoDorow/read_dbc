//! Reader options.

use crate::encoding::Encoding;
use crate::schema::ColumnType;

/// Select a DBF field for a type override by header name or position.
#[derive(Debug, Clone)]
pub enum TypeOverrideField {
    /// The original field name, matched ASCII case-insensitively and required to be unique.
    Name(String),
    /// The zero-based position in the header, before projection.
    Index(usize),
}

impl From<String> for TypeOverrideField {
    fn from(name: String) -> Self {
        Self::Name(name)
    }
}

impl From<&str> for TypeOverrideField {
    fn from(name: &str) -> Self {
        Self::Name(name.into())
    }
}

/// Options for [`DbcReader`](crate::DbcReader).
#[derive(Debug, Clone)]
pub struct ReadOptions {
    /// Text encoding. Default [`Encoding::Auto`] (from the LDID byte).
    pub encoding: Encoding,
    /// Read every field as text (C rules: trailing spaces/NULs stripped,
    /// everything else preserved). Per-field [`types`](Self::types) still
    /// apply on top. Columns chosen this way have no
    /// [`Column::fallback`](crate::Column::fallback).
    pub all_varchar: bool,
    /// Yield deleted (`'*'`) records too; the sink's
    /// [`BatchSink::deleted`](crate::BatchSink::deleted) receives the flag.
    pub include_deleted: bool,
    /// Compare the stored CRC-32 of a `.dbc` at the end of the data
    /// (default true). Ignored for plain `.dbf`.
    pub verify_crc: bool,
    /// Per-field type overrides by original name or zero-based index.
    /// Supported: any field → `Varchar`;
    /// `C` → `Date` (strict `YYYYMMDD`; blank or `00000000` → NULL) and
    /// `C` → `BigInt` (strict integer);
    /// and any field → its natural type.
    pub types: Vec<(TypeOverrideField, ColumnType)>,
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

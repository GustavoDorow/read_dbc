//! `read_dbc_format`: a streaming, dependency-free reader for DATASUS `.dbc`
//! files (and plain dBase III `.dbf` files).
//!
//! # The format
//!
//! A `.dbc` file is a dBase III table whose record area is compressed:
//!
//! | Bytes | Content |
//! |---|---|
//! | `[0, H)` | The dBase header, verbatim: version (0), record count `N` (u32le @4), header length `H` (u16le @8), record length `R` (u16le @10), language-driver ID (@29), 32-byte field descriptors from 32, terminator at `H-1` (0x0D, or 0x00 in some DATASUS files) |
//! | `[H, H+4)` | CRC-32 (IEEE/zlib), little-endian, of header bytes `[0, H)` followed by the *entire* decompressed stream (including an optional trailing 0x1A) |
//! | `H+4 ..` | A PKWare DCL "implode" stream (literal-mode byte, dictionary byte, tokens, end code) that decompresses to exactly `N*R` bytes, or `N*R + 1` with a final 0x1A |
//!
//! Each record is a deletion flag (`' '` live, `'*'` deleted) followed by the
//! fixed-width fields. See `docs/research/dbc-format.md` in the repository.
//!
//! # Streaming design and memory bound
//!
//! [`DbcReader`] reads from any [`std::io::Read`] (no `Seek`), strictly
//! front to back. It never reads the whole file or the whole decompressed
//! data: the [`DclReader`] decoder keeps a 32 KiB output ring (history
//! window + staging; DCL needs at most 4 KiB of history) and a 32 KiB input
//! buffer, and the table reader keeps the raw header (`H` ≤ 64 KiB), one
//! record (`R` ≤ 64 KiB) and a reusable text scratch buffer. Memory is
//! therefore independent of the file size. The header's record count is
//! untrusted and never used to size an allocation.
//!
//! Rows are pulled in batches with [`DbcReader::read_batch`] into a
//! caller-provided [`BatchSink`]. Each call does bounded work (at most
//! `max(max_rows, 2048)` physical records), so a batch can be short or
//! even empty before the end: loop until [`DbcReader::is_finished`]. Only projected columns are decoded
//! ([`DbcReader::set_projection`]); text is handed out as `&str` borrowed
//! from the record buffer (pure ASCII, no copy) or from a reused scratch
//! string (code-page conversion), so no per-value allocation happens.
//!
//! # Integrity guarantees
//!
//! Reading fails loudly instead of returning partial data. Every error is a
//! typed [`Error`] with file/logical offsets, record index and field name
//! where applicable. Checked:
//!
//! * header: length, version byte ([`SUPPORTED_VERSIONS`]), terminator,
//!   descriptor layout, `R` = 1 + sum of field widths, at least one field;
//! * the DCL stream: every blast error ([`BlastError`]);
//! * the record data: each deletion flag is `' '` or `'*'`; exactly `N`
//!   records, then nothing or a single 0x1A; the DCL end code is present and
//!   no bytes follow it (never observed in DATASUS files);
//! * the CRC-32 (unless [`ReadOptions::verify_crc`] is off).
//!
//! **The CRC can only be known at the end of the stream, so the end checks
//! run as part of reading the last declared record**: the call that would
//! hand out the last record (in [`DbcReader::read_batch`],
//! [`DbcReader::next_raw_record`] or [`DbcReader::finish`]) first verifies
//! the end of the stream and the CRC, and returns the error instead if they
//! fail. A batch containing the last row is therefore never returned `Ok`
//! from a corrupt file, and [`DbcReader::is_finished`] becomes true only
//! after successful verification. Once a call fails the reader is poisoned.
//!
//! Batches before the last one are handed out before the CRC is known, so
//! a consumer that stops early (a `LIMIT` query, a streaming pipeline) can
//! receive rows from a file that later fails verification. Only a read to
//! the end (or [`DbcReader::finish`]) proves the file intact.
//!
//! # Type mapping
//!
//! Decided from the header alone (see [`ColumnType`]):
//!
//! * `C` → VARCHAR: trailing spaces and NULs stripped (dbfread's rule),
//!   leading bytes kept; blank → `""` (not NULL).
//! * `N`/`F` without decimals, width ≤ 18 → BIGINT; width 19–38 →
//!   DECIMAL(width, 0). With decimals → DECIMAL(width, decimals), parsed
//!   exactly (no floating point). Blank → NULL; anything else that is not a
//!   plain decimal number is an error. Width > 38 is an error at open.
//! * `D` → DATE from `YYYYMMDD`; blank, all-zero or invalid → NULL.
//! * `L` → BOOLEAN (`TtYy`/`FfNn`, `?`/blank → NULL).
//! * Other types are an error at open unless [`ReadOptions::all_varchar`].
//!
//! Per-field overrides ([`ReadOptions::types`]): any field → VARCHAR,
//! `C` → DATE (strict `YYYYMMDD`, blank → NULL), `C` → BIGINT.
//!
//! # Schema without reading data
//!
//! [`Header::read`] reads only the header (`H` bytes) and
//! [`resolve_schema`] maps it to columns with the same rules
//! [`DbcReader`] uses, for planners that must declare columns cheaply.
//!
//! # Example
//!
//! ```no_run
//! use read_dbc_format::{ColumnBuffers, DbcReader, ReadOptions};
//!
//! let file = std::fs::File::open("RDAC0801.dbc")?;
//! let mut reader = DbcReader::new(file, ReadOptions::default())?;
//! let mut buffers = ColumnBuffers::new(reader.columns().len());
//! while !reader.is_finished() {
//!     reader.read_batch(2048, &mut buffers)?;
//! }
//! println!("{:?}", reader.summary());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod codepages;
mod crc32;
pub mod dcl;
mod encoding;
mod error;
mod header;
mod options;
mod reader;
mod schema;
pub mod values;

pub use crc32::{Crc32, crc32};
pub use dcl::{BlastError, DclError, DclReader};
pub use encoding::{DecodeError, Encoding};
pub use error::{Error, Result};
pub use header::{Field, Header, SUPPORTED_VERSIONS};
pub use options::ReadOptions;
pub use reader::{
    BatchSink, ColumnBuffers, CrcStatus, DbcReader, Format, MIN_RECORDS_PER_BATCH, OwnedValue,
    Progress, RawRecord, Summary, Value,
};
pub use schema::{Column, ColumnType, resolve_schema};

/// Version of this crate, as declared in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

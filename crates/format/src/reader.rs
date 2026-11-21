//! The streaming table reader.

use std::io::Read;

use crate::crc32::Crc32;
use crate::dcl::{DclError, DclReader, read_some};
use crate::encoding::Encoding;
use crate::error::{Error, Result};
use crate::header::{Field, Header, read_header_bytes};
use crate::options::ReadOptions;
use crate::schema::{Column, Decoder, resolve_schema};
use crate::values::{
    parse_date_field, parse_i64, parse_logical, parse_scaled, parse_text_date, trim_text,
};

/// A decoded field value. Text borrows from the reader's buffers and is
/// valid only during the [`BatchSink::value`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value<'a> {
    /// SQL NULL.
    Null,
    /// Text, already decoded to UTF-8.
    Text(&'a str),
    /// A [`ColumnType::BigInt`](crate::ColumnType::BigInt) value.
    BigInt(i64),
    /// A [`ColumnType::Decimal`](crate::ColumnType::Decimal) value with
    /// `width <= 18`, as an integer scaled by `10^scale`.
    Decimal64(i64),
    /// A [`ColumnType::Decimal`](crate::ColumnType::Decimal) value with
    /// `width > 18`, as an integer scaled by `10^scale`.
    Decimal128(i128),
    /// A date, as days since 1970-01-01.
    Date(i32),
    /// A boolean.
    Boolean(bool),
}

/// Receives decoded values from [`DbcReader::read_batch`].
///
/// Values arrive row by row; within a row, in projection order. `column` is
/// the position in the projection (not the field index) and `row` is the
/// row's index within the current batch (starting at 0 for every call).
pub trait BatchSink {
    /// Store one value.
    fn value(&mut self, column: usize, row: usize, value: Value<'_>);
    /// Called once per row, after its values, when
    /// [`ReadOptions::include_deleted`] is set.
    fn deleted(&mut self, row: usize, deleted: bool) {
        let _ = (row, deleted);
    }
}

/// An owned copy of a [`Value`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // mirrors `Value`
pub enum OwnedValue {
    Null,
    Text(String),
    BigInt(i64),
    Decimal64(i64),
    Decimal128(i128),
    Date(i32),
    Boolean(bool),
}

impl From<Value<'_>> for OwnedValue {
    fn from(v: Value<'_>) -> Self {
        match v {
            Value::Null => OwnedValue::Null,
            Value::Text(s) => OwnedValue::Text(s.to_owned()),
            Value::BigInt(x) => OwnedValue::BigInt(x),
            Value::Decimal64(x) => OwnedValue::Decimal64(x),
            Value::Decimal128(x) => OwnedValue::Decimal128(x),
            Value::Date(x) => OwnedValue::Date(x),
            Value::Boolean(x) => OwnedValue::Boolean(x),
        }
    }
}

/// A simple [`BatchSink`] that appends owned values per projected column
/// (allocates per text value; meant for tests and small tools).
#[derive(Debug, Clone, Default)]
pub struct ColumnBuffers {
    /// One vector per projected column.
    pub columns: Vec<Vec<OwnedValue>>,
    /// Deletion flags, filled only with `include_deleted`.
    pub deleted: Vec<bool>,
}

impl ColumnBuffers {
    /// Buffers for `n` projected columns.
    pub fn new(n: usize) -> Self {
        ColumnBuffers {
            columns: vec![Vec::new(); n],
            deleted: Vec::new(),
        }
    }
}

impl BatchSink for ColumnBuffers {
    fn value(&mut self, column: usize, _row: usize, value: Value<'_>) {
        self.columns[column].push(value.into());
    }
    fn deleted(&mut self, _row: usize, deleted: bool) {
        self.deleted.push(deleted);
    }
}

/// The container format being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// DATASUS `.dbc`: header, CRC-32, PKWare DCL record area.
    Dbc,
    /// Plain uncompressed `.dbf`.
    Dbf,
}

/// Outcome of the CRC check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrcStatus {
    /// The stored CRC-32 matched (value shown).
    Verified(u32),
    /// `verify_crc` was off.
    NotChecked,
    /// Plain `.dbf` (no CRC).
    NotApplicable,
}

/// Facts established once the whole file was read and verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// Physical records read (equals the declared count).
    pub records: u64,
    /// Records flagged as deleted.
    pub deleted: u64,
    /// Whether the data ended with the 0x1A end-of-file marker.
    pub eof_marker: bool,
    /// CRC verification result.
    pub crc: CrcStatus,
}

/// Position information for progress reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Physical records read so far.
    pub records_read: u64,
    /// Records declared in the header.
    pub records_declared: u64,
    /// Bytes obtained from the underlying reader so far (file position,
    /// including read-ahead buffering).
    pub input_bytes: u64,
}

impl Progress {
    /// Fraction of records read, in `[0, 1]`.
    pub fn fraction(&self) -> f64 {
        if self.records_declared == 0 {
            1.0
        } else {
            (self.records_read as f64 / self.records_declared as f64).min(1.0)
        }
    }
}

/// One physical record (including deleted ones), as raw bytes.
#[derive(Debug, Clone, Copy)]
pub struct RawRecord<'a> {
    /// 0-based physical record index.
    pub index: u64,
    /// Whether the deletion flag is `'*'`.
    pub deleted: bool,
    /// The whole record, starting with the deletion flag.
    pub bytes: &'a [u8],
    fields: &'a [Field],
}

impl<'a> RawRecord<'a> {
    /// Raw bytes of field `i` (index into [`Header::fields`]), or `None`
    /// if there is no such field.
    pub fn field(&self, i: usize) -> Option<&'a [u8]> {
        let f = self.fields.get(i)?;
        Some(&self.bytes[f.offset..f.offset + f.length])
    }

    /// Number of fields (valid indexes for [`field`](Self::field)).
    pub fn field_count(&self) -> usize {
        self.fields.len()
    }
}

/// Read-ahead buffer for plain `.dbf` input (records are often a few dozen
/// bytes, so reading them one by one from a file would be slow).
struct DbfSource<R> {
    inner: R,
    buf: Box<[u8]>,
    pos: usize,
    len: usize,
    /// Total bytes obtained from `inner`.
    count: u64,
}

const DBF_BUF: usize = 1 << 16;

impl<R: Read> DbfSource<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos == self.len {
            // Large reads bypass the buffer.
            if out.len() >= self.buf.len() {
                let n = read_some(&mut self.inner, out)?;
                self.count += n as u64;
                return Ok(n);
            }
            let n = read_some(&mut self.inner, &mut self.buf)?;
            self.count += n as u64;
            self.pos = 0;
            self.len = n;
        }
        let n = (self.len - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

enum Body<R> {
    Dbc(DclReader<R>),
    Dbf(DbfSource<R>),
}

impl<R: Read> Body<R> {
    /// Fill `buf` as far as the data allows; returns the byte count.
    fn fill(&mut self, buf: &mut [u8], data_start: u64) -> Result<usize> {
        let mut got = 0;
        while got < buf.len() {
            let n = match self {
                Body::Dbc(d) => d
                    .read_decoded(&mut buf[got..])
                    .map_err(|e| dcl_error(e, data_start))?,
                Body::Dbf(r) => r.read(&mut buf[got..])?,
            };
            if n == 0 {
                break;
            }
            got += n;
        }
        Ok(got)
    }
}

fn dcl_error(e: DclError, data_start: u64) -> Error {
    match e {
        DclError::Blast { error, offset } => Error::Dcl {
            error,
            offset: data_start + offset,
        },
        DclError::TrailingData { offset } => Error::TrailingData {
            offset: data_start + offset,
        },
        DclError::Io(e) => Error::Io(e),
    }
}

enum Phase {
    Records,
    Done(Summary),
    Failed,
}

/// The least number of physical records one
/// [`read_batch`](DbcReader::read_batch) call may examine (it examines up
/// to `max(max_rows, MIN_RECORDS_PER_BATCH)`).
pub const MIN_RECORDS_PER_BATCH: usize = 2048;

/// Streaming reader for `.dbc` (and plain `.dbf`) tables.
///
/// See the [crate documentation](crate) for the format, the memory bound
/// and the integrity guarantees.
pub struct DbcReader<R> {
    format: Format,
    header: Header,
    columns: Vec<Column>,
    projection: Vec<usize>,
    encoding: Encoding,
    include_deleted: bool,
    body: Body<R>,
    /// File offset where the record data (or DCL stream) starts.
    data_start: u64,
    stored_crc: Option<u32>,
    crc: Option<Crc32>,
    record: Vec<u8>,
    scratch: String,
    records_read: u64,
    deleted_count: u64,
    phase: Phase,
}

impl<R: Read> DbcReader<R> {
    /// Open a DATASUS `.dbc` stream: reads and validates the header and the
    /// stored CRC, and resolves the output schema. No record data is read.
    pub fn new(mut reader: R, options: ReadOptions) -> Result<Self> {
        let (header, raw) = Header::read(&mut reader, options.encoding)?;
        let h = header.header_len as u64;
        let mut crc_bytes = [0u8; 4];
        read_header_bytes(&mut reader, &mut crc_bytes, h, h + 4)?;
        let stored = u32::from_le_bytes(crc_bytes);
        let crc = options.verify_crc.then(|| {
            let mut c = Crc32::new();
            c.update(&raw);
            c
        });
        Self::build(
            header,
            Format::Dbc,
            Body::Dbc(DclReader::new(reader)),
            h + 4,
            Some(stored),
            crc,
            &options,
        )
    }

    /// Open a plain uncompressed `.dbf` stream (no format auto-detection).
    pub fn new_dbf(mut reader: R, options: ReadOptions) -> Result<Self> {
        let (header, _) = Header::read(&mut reader, options.encoding)?;
        let h = header.header_len as u64;
        let body = Body::Dbf(DbfSource {
            inner: reader,
            buf: vec![0u8; DBF_BUF].into_boxed_slice(),
            pos: 0,
            len: 0,
            count: 0,
        });
        Self::build(header, Format::Dbf, body, h, None, None, &options)
    }

    fn build(
        header: Header,
        format: Format,
        body: Body<R>,
        data_start: u64,
        stored_crc: Option<u32>,
        crc: Option<Crc32>,
        options: &ReadOptions,
    ) -> Result<Self> {
        let columns = resolve_schema(&header, options)?;
        let encoding = options.encoding.resolve(header.ldid);
        let record = vec![0u8; header.record_len as usize];
        Ok(DbcReader {
            format,
            projection: (0..columns.len()).collect(),
            columns,
            encoding,
            include_deleted: options.include_deleted,
            body,
            data_start,
            stored_crc,
            crc,
            record,
            scratch: String::new(),
            records_read: 0,
            deleted_count: 0,
            phase: Phase::Records,
            header,
        })
    }

    /// The parsed header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The output columns, one per field, in field order.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The resolved text encoding (never [`Encoding::Auto`]).
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// The container format.
    pub fn format(&self) -> Format {
        self.format
    }

    /// The CRC-32 stored in a `.dbc` file.
    pub fn stored_crc(&self) -> Option<u32> {
        self.stored_crc
    }

    /// The current projection (indexes into [`columns`](Self::columns)).
    pub fn projection(&self) -> &[usize] {
        &self.projection
    }

    /// Decode only these columns (indexes into [`columns`](Self::columns)),
    /// in this order, in later [`read_batch`](Self::read_batch) calls. An
    /// empty projection decodes nothing but still counts rows. Unprojected
    /// fields are never parsed, so their values cannot cause errors; the
    /// whole stream is still decompressed and verified.
    pub fn set_projection(&mut self, columns: &[usize]) -> Result<()> {
        if let Some(&bad) = columns.iter().find(|&&c| c >= self.columns.len()) {
            return Err(Error::InvalidOption(format!(
                "projection index {bad} out of range ({} columns)",
                self.columns.len()
            )));
        }
        self.projection = columns.to_vec();
        Ok(())
    }

    /// Progress information.
    pub fn progress(&self) -> Progress {
        let input_bytes = self.data_start
            + match &self.body {
                Body::Dbc(d) => d.compressed_read(),
                Body::Dbf(r) => r.count,
            };
        Progress {
            records_read: self.records_read,
            records_declared: self.header.record_count as u64,
            input_bytes,
        }
    }

    /// True once every record was read and the end of the data verified.
    /// This, not a short or empty batch, is the end-of-data signal for
    /// [`read_batch`](Self::read_batch).
    pub fn is_finished(&self) -> bool {
        matches!(self.phase, Phase::Done(_))
    }

    /// The verification summary, once [`is_finished`](Self::is_finished).
    pub fn summary(&self) -> Option<&Summary> {
        match &self.phase {
            Phase::Done(s) => Some(s),
            _ => None,
        }
    }

    /// Decode up to `max_rows` rows into `sink`, decoding only the projected
    /// columns. Deleted records are skipped unless `include_deleted`.
    ///
    /// Returns the number of rows written. **A short batch, even `Ok(0)`,
    /// does not mean the end of the data**: each call examines at most
    /// `max(max_rows, 2048)` physical records ([`MIN_RECORDS_PER_BATCH`]),
    /// so that a run of deleted records cannot make one call scan the whole
    /// file, and `max_rows == 0` does nothing. Loop until
    /// [`is_finished`](Self::is_finished):
    ///
    /// ```no_run
    /// # use read_dbc_format::{ColumnBuffers, DbcReader, ReadOptions};
    /// # let mut reader = DbcReader::new(std::io::empty(), ReadOptions::default())?;
    /// # let mut sink = ColumnBuffers::new(reader.columns().len());
    /// while !reader.is_finished() {
    ///     let rows = reader.read_batch(2048, &mut sink)?;
    ///     // hand `rows` rows downstream (possibly 0)
    /// }
    /// # Ok::<(), read_dbc_format::Error>(())
    /// ```
    ///
    /// The call that consumes the last declared record checks the end of
    /// the stream before returning (end code, optional 0x1A, no extra data,
    /// CRC), so it returns `Ok` only if the file is intact, and
    /// `is_finished()` is true right after it. Earlier batches are returned
    /// before the CRC can be known.
    ///
    /// On `Err`, values already written to `sink` during this call must be
    /// discarded; the reader is then unusable ([`Error::Poisoned`]).
    pub fn read_batch<S: BatchSink + ?Sized>(
        &mut self,
        max_rows: usize,
        sink: &mut S,
    ) -> Result<usize> {
        let mut rows = 0;
        let mut budget = max_rows.max(MIN_RECORDS_PER_BATCH);
        while rows < max_rows && budget > 0 {
            budget -= 1;
            let deleted = match self.advance()? {
                Some(d) => d,
                None => break,
            };
            if deleted && !self.include_deleted {
                continue;
            }
            if let Err(e) = self.emit_row(rows, deleted, sink) {
                self.phase = Phase::Failed;
                return Err(e);
            }
            rows += 1;
        }
        Ok(rows)
    }

    /// The next physical record as raw bytes, deleted or not (regardless of
    /// `include_deleted`). `Ok(None)` only after the end was verified; the
    /// last record is returned only after verification succeeded.
    pub fn next_raw_record(&mut self) -> Result<Option<RawRecord<'_>>> {
        Ok(self.advance()?.map(|deleted| RawRecord {
            index: self.records_read - 1,
            deleted,
            bytes: &self.record,
            fields: &self.header.fields,
        }))
    }

    /// Read (without decoding values) and verify the rest of the file.
    pub fn finish(&mut self) -> Result<Summary> {
        while self.advance()?.is_some() {}
        Ok(self.summary().expect("finished").clone())
    }

    fn emit_row<S: BatchSink + ?Sized>(
        &mut self,
        row: usize,
        deleted: bool,
        sink: &mut S,
    ) -> Result<()> {
        let rec = &self.record[..];
        for (j, &ci) in self.projection.iter().enumerate() {
            let col = &self.columns[ci];
            let f = &self.header.fields[col.field_index];
            let raw = &rec[f.offset..f.offset + f.length];
            match decode_value(col.decoder, raw, self.encoding, &mut self.scratch) {
                Ok(v) => sink.value(j, row, v),
                Err(reason) => {
                    let record = self.records_read - 1;
                    return Err(Error::InvalidValue {
                        record,
                        field: f.name.clone(),
                        offset: self.header.header_len as u64
                            + record * self.header.record_len as u64
                            + f.offset as u64,
                        raw: raw.to_vec(),
                        reason: reason.to_owned(),
                    });
                }
            }
        }
        if self.include_deleted {
            sink.deleted(row, deleted);
        }
        Ok(())
    }

    /// Read the next physical record into `self.record`. Returns its
    /// deletion flag, or `None` when the data is exhausted and verified.
    fn advance(&mut self) -> Result<Option<bool>> {
        match self.phase {
            Phase::Records => {}
            Phase::Done(_) => return Ok(None),
            Phase::Failed => return Err(Error::Poisoned),
        }
        let r = self.advance_inner();
        if r.is_err() {
            self.phase = Phase::Failed;
        }
        r
    }

    fn advance_inner(&mut self) -> Result<Option<bool>> {
        let declared = self.header.record_count as u64;
        if self.records_read >= declared {
            self.finish_stream()?;
            return Ok(None);
        }
        let idx = self.records_read;
        let rlen = self.header.record_len as u64;
        let got = self.body.fill(&mut self.record, self.data_start)?;
        if got < self.record.len() {
            return Err(Error::TruncatedRecords {
                record: idx,
                declared,
                offset: self.header.header_len as u64 + idx * rlen + got as u64,
            });
        }
        if let Some(c) = self.crc.as_mut() {
            c.update(&self.record);
        }
        let deleted = match self.record[0] {
            b' ' => false,
            b'*' => true,
            byte => {
                return Err(Error::BadDeletionFlag {
                    record: idx,
                    offset: self.header.header_len as u64 + idx * rlen,
                    byte,
                });
            }
        };
        self.records_read += 1;
        self.deleted_count += deleted as u64;
        if self.records_read == declared {
            // Verify before the last record is handed out.
            self.finish_stream()?;
        }
        Ok(Some(deleted))
    }

    /// After the declared records: optional 0x1A, then the end of the data
    /// (DCL end code with nothing after it), then the CRC.
    fn finish_stream(&mut self) -> Result<()> {
        let end = self.header.header_len as u64 + self.header.data_len();
        let mut tail = [0u8; 2];
        let got = self.body.fill(&mut tail, self.data_start)?;
        if got >= 1 && tail[0] != 0x1A {
            return Err(Error::ExcessData {
                offset: end,
                byte: tail[0],
                after_eof_marker: false,
            });
        }
        if got == 2 {
            return Err(Error::ExcessData {
                offset: end + 1,
                byte: tail[1],
                after_eof_marker: true,
            });
        }
        if let Body::Dbc(d) = &mut self.body {
            d.check_end().map_err(|e| dcl_error(e, self.data_start))?;
        }
        let crc = match (self.format, self.crc.as_mut()) {
            (Format::Dbf, _) => crate::CrcStatus::NotApplicable,
            (Format::Dbc, None) => crate::CrcStatus::NotChecked,
            (Format::Dbc, Some(c)) => {
                c.update(&tail[..got]);
                let computed = c.finalize();
                let stored = self.stored_crc.expect("dbc has a stored crc");
                if computed != stored {
                    return Err(Error::CrcMismatch { stored, computed });
                }
                crate::CrcStatus::Verified(computed)
            }
        };
        self.phase = Phase::Done(Summary {
            records: self.records_read,
            deleted: self.deleted_count,
            eof_marker: got == 1,
            crc,
        });
        Ok(())
    }
}

/// Decode one field according to its column decoder.
fn decode_value<'a>(
    decoder: Decoder,
    raw: &'a [u8],
    encoding: Encoding,
    scratch: &'a mut String,
) -> std::result::Result<Value<'a>, &'static str> {
    Ok(match decoder {
        Decoder::Text => Value::Text(
            encoding
                .decode(trim_text(raw), scratch)
                .map_err(|_| "invalid UTF-8")?,
        ),
        Decoder::Int64 => parse_i64(raw)?.map_or(Value::Null, Value::BigInt),
        Decoder::Decimal { scale, width } => match parse_scaled(raw, scale as u32, width as u32)? {
            None => Value::Null,
            Some(v) if width <= 18 => Value::Decimal64(v as i64),
            Some(v) => Value::Decimal128(v),
        },
        Decoder::DateField => parse_date_field(raw).map_or(Value::Null, Value::Date),
        Decoder::Logical => parse_logical(raw)?.map_or(Value::Null, Value::Boolean),
        Decoder::TextDate => parse_text_date(raw)?.map_or(Value::Null, Value::Date),
        Decoder::TextInt64 => parse_i64(trim_text(raw))?.map_or(Value::Null, Value::BigInt),
    })
}

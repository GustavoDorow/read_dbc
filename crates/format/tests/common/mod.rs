//! Shared helpers for the integration tests.
#![allow(dead_code)]

use std::io::{self, Read};
use std::path::PathBuf;

use read_dbc_format::dcl::testing::encode_literals;
use read_dbc_format::{BatchSink, DbcReader, Error, ReadOptions, Value, crc32};

pub const FIXTURES: [&str; 5] = ["RDAC0801", "RDRR1501", "RDAP2401", "RDSC2501", "RDAC2601"];

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// True when `READ_DBC_REQUIRE_FIXTURES=1`: missing fixtures fail the
/// tests instead of skipping them (for CI).
pub fn fixtures_required() -> bool {
    std::env::var("READ_DBC_REQUIRE_FIXTURES").is_ok_and(|v| v == "1")
}

fn present(p: PathBuf, what: &str) -> Option<PathBuf> {
    if p.exists() {
        return Some(p);
    }
    let msg = format!(
        "{what} {} is missing; run scripts/fixtures/build-reference.sh to create it",
        p.display()
    );
    if fixtures_required() {
        panic!("READ_DBC_REQUIRE_FIXTURES=1 but {msg}");
    }
    eprintln!("SKIP: {msg} (set READ_DBC_REQUIRE_FIXTURES=1 to fail instead)");
    None
}

/// Path of `test/data/<name>.<ext>`, or `None` (with a `SKIP:` message) if
/// the fixture is missing. Panics instead when [`fixtures_required`].
pub fn fixture(name: &str, ext: &str) -> Option<PathBuf> {
    present(
        repo_root().join("test/data").join(format!("{name}.{ext}")),
        "fixture",
    )
}

/// Path of `test/reference/<name>.<kind>.jsonl.gz`; see [`fixture`].
pub fn reference(name: &str, kind: &str) -> Option<PathBuf> {
    present(
        repo_root()
            .join("test/reference")
            .join(format!("{name}.{kind}.jsonl.gz")),
        "reference",
    )
}

/// Reader that hands out at most one byte per call.
pub struct OneByte<R>(pub R);
impl<R: Read> Read for OneByte<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(1);
        self.0.read(&mut buf[..n])
    }
}

/// A sink that ignores values (counts them).
#[derive(Default)]
pub struct CountSink(pub u64);
impl BatchSink for CountSink {
    fn value(&mut self, _c: usize, _r: usize, _v: Value<'_>) {
        self.0 += 1;
    }
}

/// Read every row with all columns; returns rows read or the first error.
pub fn drain(bytes: &[u8], opts: ReadOptions) -> Result<u64, Error> {
    let mut r = DbcReader::new(bytes, opts)?;
    let mut sink = CountSink::default();
    let mut rows = 0u64;
    while !r.is_finished() {
        rows += r.read_batch(1000, &mut sink)? as u64;
    }
    assert!(r.summary().is_some());
    Ok(rows)
}

/// Field spec for synthetic tables: (name, type, length, decimals).
pub type FieldSpec<'a> = (&'a str, u8, u8, u8);

/// Build a plain DBF. `records` hold the full record bytes (flag included).
pub fn build_dbf(fields: &[FieldSpec], records: &[Vec<u8>], ldid: u8, eof: bool) -> Vec<u8> {
    let h = 32 + 32 * fields.len() + 1;
    let r: usize = 1 + fields.iter().map(|f| f.2 as usize).sum::<usize>();
    let mut out = vec![0u8; 32];
    out[0] = 0x03;
    out[1..4].copy_from_slice(&[124, 1, 2]);
    out[4..8].copy_from_slice(&(records.len() as u32).to_le_bytes());
    out[8..10].copy_from_slice(&(h as u16).to_le_bytes());
    out[10..12].copy_from_slice(&(r as u16).to_le_bytes());
    out[29] = ldid;
    for (name, ty, len, dec) in fields {
        let mut d = [0u8; 32];
        d[..name.len()].copy_from_slice(name.as_bytes());
        d[11] = *ty;
        d[16] = *len;
        d[17] = *dec;
        out.extend_from_slice(&d);
    }
    out.push(0x0D);
    for rec in records {
        assert_eq!(rec.len(), r, "record length");
        out.extend_from_slice(rec);
    }
    if eof {
        out.push(0x1A);
    }
    out
}

/// Turn a DBF into a DBC: header, CRC-32, literal-only DCL stream.
pub fn dbf_to_dbc(dbf: &[u8]) -> Vec<u8> {
    let h = u16::from_le_bytes([dbf[8], dbf[9]]) as usize;
    let mut out = dbf[..h].to_vec();
    out.extend_from_slice(&crc32(dbf).to_le_bytes());
    out.extend_from_slice(&encode_literals(&dbf[h..]));
    out
}

/// Concatenate a record from a flag byte and field values (padded by caller).
pub fn rec(flag: u8, parts: &[&[u8]]) -> Vec<u8> {
    let mut v = vec![flag];
    for p in parts {
        v.extend_from_slice(p);
    }
    v
}

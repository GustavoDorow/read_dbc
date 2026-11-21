//! Parity tests against the real DATASUS SIH fixtures and their lossless
//! references. Skipped (with a `SKIP:` message) when `test/data` is absent,
//! unless `READ_DBC_REQUIRE_FIXTURES=1`, which makes them fail instead.

mod common;

use std::collections::HashMap;
use std::io::{BufRead, BufReader};

use common::*;
use read_dbc_format::{
    BatchSink, ColumnType, CrcStatus, DbcReader, Encoding, OwnedValue, ReadOptions, Value, dcl,
};
use serde_json::Value as Json;

fn read_jsonl(path: &std::path::Path) -> impl Iterator<Item = serde_json::Map<String, Json>> {
    let f = std::fs::File::open(path).unwrap();
    BufReader::new(flate2::read::GzDecoder::new(f))
        .lines()
        .map(|l| match serde_json::from_str(&l.unwrap()).unwrap() {
            Json::Object(m) => m,
            other => panic!("not an object: {other}"),
        })
}

fn latin1(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| u8::try_from(c as u32).expect("reference string is not latin-1"))
        .collect()
}

/// (a) The decompressed stream equals datasus-core's blast output and the
/// reconstructed DBF; the CRC verifies.
#[test]
fn decompressed_stream_matches_datasus_core_and_crc() {
    let mut checked = 0;
    for name in FIXTURES {
        let Some(path) = fixture(name, "dbc") else {
            continue;
        };
        let bytes = std::fs::read(&path).unwrap();
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;

        let ours = dcl::decompress_all(&bytes[h + 4..]).unwrap();
        let mut theirs = Vec::new();
        datasus_core::blast::blast(&bytes[h + 4..], &mut theirs, usize::MAX).unwrap();
        assert_eq!(ours.len(), theirs.len(), "{name}: length");
        assert!(
            ours == theirs,
            "{name}: decompressed bytes differ from datasus-core"
        );

        if let Some(dbf) = fixture(name, "dbf") {
            let dbf = std::fs::read(dbf).unwrap();
            assert_eq!(&dbf[..h], &bytes[..h], "{name}: header");
            assert!(
                dbf[h..] == ours[..],
                "{name}: differs from the reference .dbf"
            );
        }

        let mut r = DbcReader::new(&bytes[..], ReadOptions::default()).unwrap();
        let summary = r.finish().unwrap();
        let stored = u32::from_le_bytes(bytes[h..h + 4].try_into().unwrap());
        assert_eq!(summary.crc, CrcStatus::Verified(stored), "{name}");
        assert_eq!(summary.records, r.header().record_count as u64);
        assert_eq!(r.encoding(), Encoding::Cp850, "{name}: LDID 0x02");
        eprintln!(
            "{name}: {} bytes decompressed, crc {stored:08x} ok, eof marker {}",
            ours.len(),
            summary.eof_marker
        );
        checked += 1;
    }
    eprintln!("checked {checked} fixtures");
}

/// (b) Every record and field equals the raw reference (bytes, untrimmed).
#[test]
fn raw_records_match_reference() {
    for name in FIXTURES {
        let (Some(path), Some(refp)) = (fixture(name, "dbc"), reference(name, "raw")) else {
            continue;
        };
        let file = std::fs::File::open(path).unwrap();
        let mut r = DbcReader::new(file, ReadOptions::default()).unwrap();
        let names: Vec<String> = r.header().fields.iter().map(|f| f.name.clone()).collect();
        let mut expected = read_jsonl(&refp);
        let (mut records, mut values) = (0u64, 0u64);
        while let Some(rec) = r.next_raw_record().unwrap() {
            let exp = expected
                .next()
                .unwrap_or_else(|| panic!("{name}: more records than reference"));
            let flag = exp["_deleted"].as_str().unwrap();
            assert_eq!(latin1(flag), [rec.bytes[0]], "{name} record {}", rec.index);
            assert_eq!(rec.deleted, flag == "*");
            assert_eq!(exp.len(), names.len() + 1, "{name}: field count");
            for (i, fname) in names.iter().enumerate() {
                let want = latin1(exp[fname].as_str().unwrap());
                assert_eq!(
                    rec.field(i).unwrap(),
                    &want[..],
                    "{name} record {} field {fname}",
                    rec.index
                );
                values += 1;
            }
            assert_eq!(rec.field_count(), names.len());
            assert!(rec.field(names.len()).is_none());
            records += 1;
        }
        assert!(
            expected.next().is_none(),
            "{name}: fewer records than reference"
        );
        assert!(r.is_finished());
        eprintln!("{name}: {records} records, {values} raw field values equal");
    }
}

struct RowSink {
    rows: Vec<Vec<OwnedValue>>,
    ncols: usize,
}
impl BatchSink for RowSink {
    fn value(&mut self, column: usize, _row: usize, value: Value<'_>) {
        if column == 0 {
            self.rows.push(Vec::with_capacity(self.ncols));
        }
        self.rows.last_mut().unwrap().push(value.into());
    }
}

fn format_scaled(v: i128, scale: u8) -> String {
    let neg = v < 0;
    let digits = v.unsigned_abs().to_string();
    let s = scale as usize;
    let digits = if digits.len() <= s {
        format!("{}{digits}", "0".repeat(s + 1 - digits.len()))
    } else {
        digits
    };
    let (int, frac) = digits.split_at(digits.len() - s);
    let body = if s == 0 {
        int.to_string()
    } else {
        format!("{int}.{frac}")
    };
    if neg { format!("-{body}") } else { body }
}

/// (c) Typed values (Latin-1) equal dbfread's.
#[test]
fn typed_values_match_dbfread() {
    for name in FIXTURES {
        let (Some(path), Some(refp)) = (fixture(name, "dbc"), reference(name, "dbfread")) else {
            continue;
        };
        let file = std::fs::File::open(path).unwrap();
        let opts = ReadOptions {
            encoding: Encoding::Latin1,
            ..Default::default()
        };
        let mut r = DbcReader::new(file, opts).unwrap();
        let cols = r.columns().to_vec();
        let mut sink = RowSink {
            rows: Vec::new(),
            ncols: cols.len(),
        };
        let mut expected = read_jsonl(&refp);
        let mut counts: HashMap<&'static str, u64> = HashMap::new();
        let mut row_index = 0u64;
        while !r.is_finished() {
            sink.rows.clear();
            r.read_batch(997, &mut sink).unwrap();
            for row in &sink.rows {
                let exp = expected
                    .next()
                    .unwrap_or_else(|| panic!("{name}: more rows than dbfread"));
                for (col, got) in cols.iter().zip(row) {
                    let pair = exp[&col.name].as_array().unwrap();
                    let (ty, payload) = (pair[0].as_str().unwrap(), &pair[1]);
                    let ctx = format!(
                        "{name} row {row_index} field {}: {got:?} vs {pair:?}",
                        col.name
                    );
                    let kind = match (ty, got, col.column_type) {
                        ("NoneType", OwnedValue::Null, _) => "null",
                        ("str", OwnedValue::Text(s), _) => {
                            assert_eq!(s, payload.as_str().unwrap(), "{ctx}");
                            "text"
                        }
                        ("int", OwnedValue::BigInt(v), ColumnType::BigInt) => {
                            assert_eq!(v.to_string(), payload.as_str().unwrap(), "{ctx}");
                            "int"
                        }
                        ("int", OwnedValue::Decimal64(v), ColumnType::Decimal { scale, .. }) => {
                            let want: i128 = payload.as_str().unwrap().parse().unwrap();
                            assert_eq!(*v as i128, want * 10i128.pow(scale as u32), "{ctx}");
                            "decimal(int)"
                        }
                        ("float", OwnedValue::Decimal64(v), ColumnType::Decimal { scale, .. }) => {
                            let f: f64 = payload.as_str().unwrap().parse().unwrap();
                            assert_eq!(
                                format_scaled(*v as i128, scale),
                                format!("{f:.*}", scale as usize),
                                "{ctx}"
                            );
                            "decimal(float)"
                        }
                        _ => panic!("type mismatch: {ctx}"),
                    };
                    *counts.entry(kind).or_default() += 1;
                }
                row_index += 1;
            }
        }
        assert!(expected.next().is_none(), "{name}: fewer rows than dbfread");
        let mut c: Vec<_> = counts.into_iter().collect();
        c.sort();
        eprintln!("{name}: {row_index} rows; compared values by kind: {c:?}");
    }
}

/// Feeding the input one byte at a time gives the same records.
#[test]
fn one_byte_input_gives_identical_records() {
    let Some(path) = fixture("RDAC0801", "dbc") else {
        return;
    };
    let bytes = std::fs::read(path).unwrap();
    let mut a = DbcReader::new(&bytes[..], ReadOptions::default()).unwrap();
    let mut b = DbcReader::new(OneByte(&bytes[..]), ReadOptions::default()).unwrap();
    let mut n = 0;
    loop {
        let ra = a.next_raw_record().unwrap().map(|r| r.bytes.to_vec());
        let rb = b.next_raw_record().unwrap().map(|r| r.bytes.to_vec());
        assert_eq!(ra, rb, "record {n}");
        if ra.is_none() {
            break;
        }
        n += 1;
    }
    assert_eq!(a.summary(), b.summary());
    assert_eq!(b.progress().input_bytes, bytes.len() as u64);
}

/// The CP850 text in RDSC2501 decodes as expected with Auto encoding.
#[test]
fn cp850_text_in_rdsc2501() {
    let Some(path) = fixture("RDSC2501", "dbc") else {
        return;
    };
    let mut r = DbcReader::new(std::fs::File::open(path).unwrap(), ReadOptions::default()).unwrap();
    let idx = r
        .columns()
        .iter()
        .position(|c| c.name == "AUD_JUST")
        .unwrap();
    r.set_projection(&[idx]).unwrap();
    let mut buf = read_dbc_format::ColumnBuffers::new(1);
    while !r.is_finished() {
        r.read_batch(4096, &mut buf).unwrap();
    }
    let texts: Vec<&str> = buf.columns[0]
        .iter()
        .filter_map(|v| match v {
            OwnedValue::Text(s) if !s.is_ascii() => Some(s.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 55);
    assert!(
        texts.contains(&"ANáLISE DO PRONTUáRIO"),
        "{:?}",
        &texts[..3]
    );
    assert!(texts.iter().any(|t| t.contains("CIRúRGICA COMPATíVEL")));
}

/// A missing fixture is skipped locally but fails under
/// `READ_DBC_REQUIRE_FIXTURES=1`.
#[test]
fn missing_fixture_policy() {
    let got = std::panic::catch_unwind(|| fixture("DOES_NOT_EXIST", "dbc"));
    if fixtures_required() {
        assert!(got.is_err(), "a missing fixture must fail when required");
    } else {
        assert_eq!(got.unwrap(), None);
    }
}

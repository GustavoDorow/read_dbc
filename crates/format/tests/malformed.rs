//! Corrupted inputs must fail with a useful error, never succeed silently.

mod common;

use common::*;
use read_dbc_format::dcl::testing::encode_literals;
use read_dbc_format::{BlastError, DbcReader, Error, ReadOptions, crc32};

fn must_fail(label: &str, bytes: &[u8]) -> Error {
    match drain(bytes, ReadOptions::default()) {
        Ok(rows) => panic!("{label}: read {rows} rows without an error"),
        Err(e) => {
            eprintln!("{label}: {e}");
            assert!(!e.to_string().is_empty());
            e
        }
    }
}

/// A small synthetic table (3 records) as (dbf, dbc).
fn synthetic(eof: bool) -> (Vec<u8>, Vec<u8>) {
    let fields = [("NAME", b'C', 5, 0), ("VAL", b'N', 6, 2)];
    let recs = vec![
        rec(b' ', &[b"ALICE", b"  1.50"]),
        rec(b'*', &[b"BOB  ", b" -2.00"]),
        rec(b' ', &[b"     ", b"      "]),
    ];
    let dbf = build_dbf(&fields, &recs, 0x00, eof);
    let dbc = dbf_to_dbc(&dbf);
    (dbf, dbc)
}

fn with_crc(dbf: &[u8], data: &[u8]) -> Vec<u8> {
    // Rebuild a DBC from a header and an arbitrary decompressed stream,
    // with a correct CRC, so that only the structural problem remains.
    let h = u16::from_le_bytes([dbf[8], dbf[9]]) as usize;
    let mut whole = dbf[..h].to_vec();
    whole.extend_from_slice(data);
    let mut out = dbf[..h].to_vec();
    out.extend_from_slice(&crc32(&whole).to_le_bytes());
    out.extend_from_slice(&encode_literals(data));
    out
}

#[test]
fn synthetic_valid_baseline() {
    for eof in [false, true] {
        let (dbf, dbc) = synthetic(eof);
        assert_eq!(drain(&dbc, ReadOptions::default()).unwrap(), 2);
        let mut r = DbcReader::new_dbf(&dbf[..], ReadOptions::default()).unwrap();
        let s = r.finish().unwrap();
        assert_eq!((s.records, s.deleted, s.eof_marker), (3, 1, eof));
    }
}

#[test]
fn truncations() {
    let mut cases = vec![("synthetic", synthetic(true).1)];
    if let Some(p) = fixture("RDAC0801", "dbc") {
        cases.push(("RDAC0801", std::fs::read(p).unwrap()));
    }
    for (name, bytes) in cases {
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        let len = bytes.len();
        let cuts = [
            ("empty", 0),
            ("in fixed header", 10),
            ("in descriptors", 40),
            ("before terminator", h - 1),
            ("at end of header", h),
            ("in CRC", h + 2),
            ("at stream start", h + 4),
            ("after mode byte", h + 5),
            ("early stream", h + 6),
            ("mid-stream", h + 4 + (len - h - 4) / 2),
            ("last byte missing", len - 1),
        ];
        for (label, cut) in cuts {
            let e = must_fail(
                &format!("{name} truncated {label} ({cut}/{len})"),
                &bytes[..cut],
            );
            if cut < h + 4 {
                assert!(matches!(e, Error::TruncatedHeader { .. }), "{label}: {e:?}");
            } else {
                assert!(matches!(e, Error::Dcl { .. }), "{label}: {e:?}");
            }
        }
    }
}

#[test]
fn flipped_compressed_bytes() {
    let mut cases = vec![("synthetic", synthetic(true).1)];
    if let Some(p) = fixture("RDAC0801", "dbc") {
        cases.push(("RDAC0801", std::fs::read(p).unwrap()));
    }
    for (name, bytes) in cases {
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        let start = h + 4;
        let span = bytes.len() - start;
        // The final byte is excluded: its bits after the end code are
        // padding that blast (and this decoder) ignores, so flipping them is
        // undetectable by design. The byte before it is included.
        let mut positions: Vec<usize> = (0..span - 1)
            .step_by((span / 40).max(1))
            .map(|o| start + o)
            .collect();
        positions.push(bytes.len() - 2);
        let mut kinds = std::collections::BTreeMap::new();
        for pos in positions {
            for mask in [0x01u8, 0x80] {
                let mut b = bytes.clone();
                b[pos] ^= mask;
                let e = must_fail(&format!("{name} flip {mask:#04x} at {pos}"), &b);
                let kind = format!("{e:?}")
                    .split([' ', '{', '('])
                    .next()
                    .unwrap()
                    .to_string();
                *kinds.entry(kind).or_insert(0) += 1;
            }
        }
        eprintln!("{name}: error kinds {kinds:?}");
    }
}

#[test]
fn header_record_count_changed() {
    let mut cases = vec![
        ("synthetic", synthetic(true).1),
        ("synthetic-noeof", synthetic(false).1),
    ];
    if let Some(p) = fixture("RDAC0801", "dbc") {
        cases.push(("RDAC0801", std::fs::read(p).unwrap()));
    }
    for (name, bytes) in cases {
        let n = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        for (label, new_n) in [
            ("N+1", n + 1),
            ("N-1", n - 1),
            ("N+1000", n + 1000),
            ("N=0", 0),
        ] {
            let mut b = bytes.clone();
            b[4..8].copy_from_slice(&new_n.to_le_bytes());
            let e = must_fail(&format!("{name} {label}"), &b);
            assert!(
                matches!(e, Error::TruncatedRecords { .. } | Error::ExcessData { .. }),
                "{name} {label}: {e:?}"
            );
            // Even without CRC verification, the count mismatch is caught.
            let opts = ReadOptions {
                verify_crc: false,
                ..Default::default()
            };
            assert!(drain(&b, opts).is_err(), "{name} {label} without crc");
        }
    }
}

#[test]
fn header_corruptions() {
    let (_, dbc) = synthetic(true);
    let h = u16::from_le_bytes([dbc[8], dbc[9]]) as usize;

    let mut b = dbc.clone();
    b[h - 1] = 0x41;
    assert!(matches!(
        must_fail("bad terminator", &b),
        Error::BadTerminator { byte: 0x41, .. }
    ));

    // 0x00 terminator is accepted (but changes the CRC).
    let mut b = dbc.clone();
    b[h - 1] = 0x00;
    assert!(matches!(
        must_fail("0x00 terminator, stale crc", &b),
        Error::CrcMismatch { .. }
    ));

    let mut b = dbc.clone();
    b[0] = 0x04;
    assert!(matches!(
        must_fail("dBase 7 version", &b),
        Error::UnsupportedVersion { version: 4 }
    ));

    let mut b = dbc.clone();
    b[10] += 1;
    assert!(matches!(
        must_fail("record length", &b),
        Error::RecordLengthMismatch { .. }
    ));

    let mut b = dbc.clone();
    b[8] += 32; // H grows by one descriptor: the terminator slot moves into data
    // The descriptors still end at the old 0x0D, so the header parses; the
    // extra 32 bytes swallow the CRC and the start of the DCL stream, whose
    // "dictionary byte" (file offset H' + 4 + 1) is then garbage.
    let new_h = h as u64 + 32;
    assert!(matches!(
        must_fail("header length", &b),
        Error::Dcl { error: BlastError::BadDictSize, offset } if offset == new_h + 4 + 2
    ));

    let mut b = dbc.clone();
    b[8..10].copy_from_slice(&33u16.to_le_bytes());
    assert!(matches!(
        must_fail("tiny header length", &b),
        Error::InvalidHeader { .. }
    ));

    // Zero fields: H = 33 is rejected as too small; build a header whose
    // first descriptor slot holds the terminator instead.
    let mut b = dbc.clone();
    b[32] = 0x0D;
    let e = must_fail("no fields", &b);
    assert!(matches!(e, Error::NoFields), "{e:?}");

    let mut b = dbc.clone();
    b[h + 1] ^= 0xFF; // stored CRC
    assert!(matches!(
        must_fail("stored crc", &b),
        Error::CrcMismatch { .. }
    ));
}

#[test]
fn record_area_problems() {
    let (dbf, _) = synthetic(true);
    let h = u16::from_le_bytes([dbf[8], dbf[9]]) as usize;
    let data = &dbf[h..];

    let mut d = data.to_vec();
    d[12] = b'X'; // flag of record 1 (R = 12)
    assert!(matches!(
        must_fail("bad flag", &with_crc(&dbf, &d)),
        Error::BadDeletionFlag {
            record: 1,
            byte: b'X',
            ..
        }
    ));

    let mut d = data.to_vec();
    d.push(b'!');
    assert!(matches!(
        must_fail("byte after 0x1a", &with_crc(&dbf, &d)),
        Error::ExcessData {
            after_eof_marker: true,
            ..
        }
    ));

    let mut d = data[..data.len() - 1].to_vec();
    d.push(b'Z');
    assert!(matches!(
        must_fail("non-0x1a extra byte", &with_crc(&dbf, &d)),
        Error::ExcessData {
            after_eof_marker: false,
            byte: b'Z',
            ..
        }
    ));

    let d = &data[..data.len() - 5];
    assert!(matches!(
        must_fail("short stream", &with_crc(&dbf, d)),
        Error::TruncatedRecords { record: 2, .. }
    ));

    let mut b = with_crc(&dbf, data);
    b.extend_from_slice(b"\0\0");
    assert!(matches!(
        must_fail("trailing bytes", &b),
        Error::TrailingData { .. }
    ));

    // Plain DBF: extra bytes and truncation.
    let mut b = dbf.clone();
    b.push(b'x');
    assert!(
        DbcReader::new_dbf(&b[..], ReadOptions::default())
            .unwrap()
            .finish()
            .is_err()
    );
    let b = &dbf[..dbf.len() - 3];
    assert!(matches!(
        DbcReader::new_dbf(b, ReadOptions::default())
            .unwrap()
            .finish(),
        Err(Error::TruncatedRecords { .. })
    ));
}

#[test]
fn reader_is_poisoned_after_error() {
    let (_, mut dbc) = synthetic(true);
    let h = u16::from_le_bytes([dbc[8], dbc[9]]) as usize;
    dbc[h] ^= 1;
    let mut r = DbcReader::new(&dbc[..], ReadOptions::default()).unwrap();
    let mut sink = CountSink::default();
    assert!(matches!(
        r.read_batch(100, &mut sink),
        Err(Error::CrcMismatch { .. })
    ));
    assert!(matches!(r.read_batch(100, &mut sink), Err(Error::Poisoned)));
    assert!(r.next_raw_record().is_err());
    assert!(!r.is_finished());
}

#[test]
fn last_record_withheld_until_verified() {
    let (_, mut dbc) = synthetic(true);
    let h = u16::from_le_bytes([dbc[8], dbc[9]]) as usize;
    dbc[h] ^= 1;
    let mut r = DbcReader::new(&dbc[..], ReadOptions::default()).unwrap();
    assert!(r.next_raw_record().unwrap().is_some());
    assert!(r.next_raw_record().unwrap().is_some());
    // The third (last) record would be returned only after the CRC check.
    assert!(matches!(
        r.next_raw_record(),
        Err(Error::CrcMismatch { .. })
    ));
}

#[test]
fn arbitrary_garbage_never_panics() {
    let (_, dbc) = synthetic(true);
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..2000 {
        let mut b = dbc.clone();
        for _ in 0..3 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let i = (state as usize) % b.len();
            b[i] = (state >> 32) as u8;
        }
        let _ = drain(&b, ReadOptions::default());
        let _ = DbcReader::new_dbf(&b[..], ReadOptions::default()).and_then(|mut r| r.finish());
    }
}

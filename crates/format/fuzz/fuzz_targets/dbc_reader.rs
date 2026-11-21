//! Arbitrary bytes through the table reader. Must never panic or loop
//! forever, and must uphold the reader's contract (poisoned after an error,
//! `is_finished` only after a verified end).
//!
//! Input layout: `[sel, mode, override, ...bytes]`.
//!
//! * `sel` picks the encoding, `all_varchar`, `include_deleted`,
//!   `verify_crc`, the projection and the batch size.
//! * `mode & 3`: 0 = `bytes` is a `.dbc`; 1 = a plain `.dbf`; 2 or 3 =
//!   `bytes` is a `.dbf` that is wrapped into a `.dbc` with a *correct* CRC
//!   (literal-only DCL), so mutations of field values reach the value
//!   parsers instead of stopping at the CRC check.
//! * `mode & 4`: interleave `next_raw_record` (touching every field) with
//!   `read_batch`.
//! * `override`: when non-zero, a per-field type override (field index and
//!   target type chosen from its bits) built from the parsed header.
#![no_main]

use libfuzzer_sys::fuzz_target;
use read_dbc_format::dcl::testing::encode_literals;
use read_dbc_format::{
    BatchSink, ColumnType, DbcReader, Encoding, Error, Header, ReadOptions, Value, crc32,
};

struct Sink;
impl BatchSink for Sink {
    fn value(&mut self, _c: usize, _r: usize, v: Value<'_>) {
        std::hint::black_box(v);
    }
}

const TYPES: [ColumnType; 5] = [
    ColumnType::Varchar,
    ColumnType::BigInt,
    ColumnType::Date,
    ColumnType::Boolean,
    ColumnType::Decimal {
        width: 10,
        scale: 2,
    },
];

/// Wrap a plain DBF into a DBC with a correct CRC.
fn wrap(dbf: &[u8]) -> Option<Vec<u8>> {
    let h = u16::from_le_bytes([*dbf.get(8)?, *dbf.get(9)?]) as usize;
    if dbf.len() < h {
        return None;
    }
    let mut out = dbf[..h].to_vec();
    out.extend_from_slice(&crc32(dbf).to_le_bytes());
    out.extend_from_slice(&encode_literals(&dbf[h..]));
    Some(out)
}

fn run(bytes: &[u8], dbc: bool, sel: u8, mode: u8, ovr: u8) {
    let mut opts = ReadOptions {
        encoding: [
            Encoding::Auto,
            Encoding::Latin1,
            Encoding::Utf8,
            Encoding::Cp850,
        ][(sel & 3) as usize],
        all_varchar: sel & 4 != 0,
        include_deleted: sel & 8 != 0,
        verify_crc: sel & 16 != 0,
        types: Vec::new(),
    };
    if ovr != 0
        && let Ok((header, _)) = Header::read(&mut &bytes[..], opts.encoding)
    {
        let i = (ovr >> 3) as usize % header.fields.len();
        opts.types
            .push((header.fields[i].name.clone(), TYPES[(ovr & 7) as usize % 5]));
    }
    let reader = if dbc {
        DbcReader::new(bytes, opts)
    } else {
        DbcReader::new_dbf(bytes, opts)
    };
    let Ok(mut r) = reader else { return };
    match sel >> 5 & 3 {
        1 => {
            let _ = r.set_projection(&[0]);
        }
        2 => {
            let _ = r.set_projection(&[]);
        }
        _ => {}
    }
    let batch = [1, 7, 64, 3000][(sel >> 7) as usize * 2 + (mode >> 3 & 1) as usize];
    let raw = mode & 4 != 0;
    let mut toggle = false;
    while !r.is_finished() {
        toggle = !toggle;
        let res = if raw && toggle {
            r.next_raw_record().map(|rec| {
                if let Some(rec) = rec {
                    for i in 0..rec.field_count() {
                        std::hint::black_box(rec.field(i).unwrap());
                    }
                    assert!(rec.field(rec.field_count()).is_none());
                }
            })
        } else {
            r.read_batch(batch, &mut Sink).map(drop)
        };
        if res.is_err() {
            assert!(!r.is_finished());
            assert!(matches!(r.read_batch(1, &mut Sink), Err(Error::Poisoned)));
            return;
        }
    }
    assert!(r.summary().is_some());
}

fuzz_target!(|data: &[u8]| {
    let [sel, mode, ovr, bytes @ ..] = data else {
        return;
    };
    match mode & 3 {
        0 => run(bytes, true, *sel, *mode, *ovr),
        1 => run(bytes, false, *sel, *mode, *ovr),
        _ => {
            if let Some(dbc) = wrap(bytes) {
                run(&dbc, true, *sel, *mode, *ovr);
            }
        }
    }
});

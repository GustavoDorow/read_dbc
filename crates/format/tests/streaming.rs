//! Streaming behaviour: I/O errors and misbehaving readers, bounded work per
//! `read_batch` call, and schema resolution from the header alone.

mod common;

use std::io::{self, Read};

use common::*;
use read_dbc_format::{
    ColumnBuffers, ColumnType, DbcReader, Error, Header, MIN_RECORDS_PER_BATCH, ReadOptions,
    resolve_schema,
};

/// A table of `n` records (`C(4)` + `N(5)`), record `i` deleted when
/// `deleted(i)`.
fn table(n: usize, deleted: impl Fn(usize) -> bool) -> Vec<u8> {
    let fields = [("NAME", b'C', 4, 0), ("NUM", b'N', 5, 0)];
    let recs: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            let name = format!("{:04}", i % 10_000);
            let num = format!("{:5}", i % 100_000);
            rec(
                if deleted(i) { b'*' } else { b' ' },
                &[name.as_bytes(), num.as_bytes()],
            )
        })
        .collect();
    build_dbf(&fields, &recs, 0x00, true)
}

/// Hands out `data` in chunks of at most `chunk`, then fails with a
/// distinctive error.
struct FailAfter<'a> {
    data: &'a [u8],
    chunk: usize,
}
impl Read for FailAfter<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.data.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "vfs: connection reset by peer (test 91c2)",
            ));
        }
        let n = buf.len().min(self.chunk).min(self.data.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

fn expect_original_io_error(e: Error) {
    match e {
        Error::Io(e) => {
            assert_eq!(e.kind(), io::ErrorKind::ConnectionReset, "{e}");
            assert_eq!(e.to_string(), "vfs: connection reset by peer (test 91c2)");
        }
        other => panic!("expected the original I/O error, got {other:?}"),
    }
}

/// The inner reader fails after ~20,000 bytes while decoded bytes are still
/// pending: the original error (kind and message) must surface through
/// `read_batch`, not a generic "previous read failed".
#[test]
fn mid_stream_io_error_surfaces_unchanged() {
    let dbf = table(3000, |_| false);
    for dbc in [true, false] {
        let bytes = if dbc { dbf_to_dbc(&dbf) } else { dbf.clone() };
        assert!(bytes.len() > 25_000);
        for chunk in [usize::MAX, 4096, 777] {
            let inner = FailAfter {
                data: &bytes[..20_000],
                chunk,
            };
            let mut r = if dbc {
                DbcReader::new(inner, ReadOptions::default())
            } else {
                DbcReader::new_dbf(inner, ReadOptions::default())
            }
            .unwrap();
            let mut buf = ColumnBuffers::new(2);
            let mut rows = 0;
            let err = loop {
                match r.read_batch(100, &mut buf) {
                    Ok(n) => rows += n,
                    Err(e) => break e,
                }
                assert!(!r.is_finished());
            };
            assert!(rows > 1000, "dbc={dbc} chunk={chunk}: {rows} rows");
            expect_original_io_error(err);
            assert!(matches!(r.read_batch(100, &mut buf), Err(Error::Poisoned)));
        }
    }
}

/// Same through `next_raw_record` and `finish`.
#[test]
fn mid_stream_io_error_via_raw_records_and_finish() {
    let bytes = dbf_to_dbc(&table(3000, |_| false));
    let inner = || FailAfter {
        data: &bytes[..20_000],
        chunk: usize::MAX,
    };
    let mut r = DbcReader::new(inner(), ReadOptions::default()).unwrap();
    let err = loop {
        match r.next_raw_record() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("unexpected end"),
            Err(e) => break e,
        }
    };
    expect_original_io_error(err);
    let mut r = DbcReader::new(inner(), ReadOptions::default()).unwrap();
    expect_original_io_error(r.finish().unwrap_err());
}

/// Claims to have read more bytes than the buffer holds, after handing out
/// `honest` bytes correctly.
struct Liar<'a> {
    data: &'a [u8],
    honest: usize,
}
impl Read for Liar<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.honest == 0 {
            return Ok(buf.len() + 1);
        }
        let n = buf.len().min(self.honest).min(self.data.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        self.honest -= n;
        Ok(n)
    }
}

/// A broken inner `Read` is an `InvalidData` error, never a panic, whether
/// it lies while the header, the CRC field or the data is read.
#[test]
fn lying_reader_is_an_error_not_a_panic() {
    let dbf = table(500, |_| false);
    let dbc = dbf_to_dbc(&dbf);
    for (is_dbc, bytes) in [(true, &dbc), (false, &dbf)] {
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        for honest in [0, 10, 32, h - 1, h, h + 2, h + 4, h + 100, 5000] {
            let inner = Liar {
                data: bytes,
                honest,
            };
            let opened = if is_dbc {
                DbcReader::new(inner, ReadOptions::default())
            } else {
                DbcReader::new_dbf(inner, ReadOptions::default())
            };
            let err = match opened {
                Err(e) => e,
                Ok(mut r) => {
                    let mut buf = ColumnBuffers::new(2);
                    loop {
                        match r.read_batch(64, &mut buf) {
                            Ok(_) => assert!(!r.is_finished(), "honest={honest}"),
                            Err(e) => break e,
                        }
                    }
                }
            };
            match err {
                Error::Io(e) => assert_eq!(
                    e.kind(),
                    io::ErrorKind::InvalidData,
                    "dbc={is_dbc} honest={honest}: {e}"
                ),
                other => panic!("dbc={is_dbc} honest={honest}: {other:?}"),
            }
        }
    }
}

/// Short reads and random `Interrupted` errors from the inner reader give
/// exactly the same rows and summary as reading from a slice.
#[test]
fn interrupted_and_short_reads_are_transparent() {
    struct Choppy<'a> {
        data: &'a [u8],
        state: u64,
    }
    impl Choppy<'_> {
        fn rand(&mut self) -> u64 {
            self.state ^= self.state << 13;
            self.state ^= self.state >> 7;
            self.state ^= self.state << 17;
            self.state
        }
    }
    impl Read for Choppy<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.rand() % 3 == 0 {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            let n = (1 + self.rand() as usize % 300)
                .min(buf.len())
                .min(self.data.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }
    let dbf = table(2500, |i| i % 7 == 3);
    let dbc = dbf_to_dbc(&dbf);
    for (is_dbc, bytes) in [(true, &dbc), (false, &dbf)] {
        fn open<'a>(is_dbc: bool, r: Box<dyn Read + 'a>) -> DbcReader<Box<dyn Read + 'a>> {
            if is_dbc {
                DbcReader::new(r, ReadOptions::default()).unwrap()
            } else {
                DbcReader::new_dbf(r, ReadOptions::default()).unwrap()
            }
        }
        let mut expected = ColumnBuffers::new(2);
        let mut a = open(is_dbc, Box::new(&bytes[..]));
        while !a.is_finished() {
            a.read_batch(333, &mut expected).unwrap();
        }
        for seed in 1..=5u64 {
            let mut b = open(
                is_dbc,
                Box::new(Choppy {
                    data: bytes,
                    state: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15),
                }),
            );
            let mut got = ColumnBuffers::new(2);
            while !b.is_finished() {
                b.read_batch(333, &mut got).unwrap();
            }
            assert_eq!(got.columns, expected.columns, "dbc={is_dbc} seed={seed}");
            assert_eq!(b.summary(), a.summary());
            assert_eq!(b.progress().input_bytes, bytes.len() as u64);
        }
        assert_eq!(
            expected.columns[0].len(),
            2500 - (0..2500).filter(|i| i % 7 == 3).count()
        );
    }
}

/// With every record deleted, `read_batch(1, ..)` examines a bounded number
/// of records per call instead of the whole file.
#[test]
fn work_per_batch_is_bounded() {
    let n = 5 * MIN_RECORDS_PER_BATCH + 17;
    let dbc = dbf_to_dbc(&table(n, |_| true));
    let mut r = DbcReader::new(&dbc[..], ReadOptions::default()).unwrap();
    let mut buf = ColumnBuffers::new(2);
    let mut calls = 0;
    while !r.is_finished() {
        let before = r.progress().records_read;
        assert_eq!(r.read_batch(1, &mut buf).unwrap(), 0);
        let examined = r.progress().records_read - before;
        assert!(examined <= MIN_RECORDS_PER_BATCH as u64, "{examined}");
        calls += 1;
    }
    assert_eq!(calls, 6);
    assert!(buf.columns[0].is_empty());
    assert_eq!(r.summary().unwrap().deleted, n as u64);

    // A large max_rows raises the budget to max_rows.
    let mut r = DbcReader::new(&dbc[..], ReadOptions::default()).unwrap();
    assert_eq!(
        r.read_batch(3 * MIN_RECORDS_PER_BATCH, &mut buf).unwrap(),
        0
    );
    assert_eq!(r.progress().records_read, 3 * MIN_RECORDS_PER_BATCH as u64);
    assert!(!r.is_finished());
}

/// The bound never lets a caller skip the end-of-stream checks: the call
/// that consumes the last record fails on a bad CRC, even when it returns
/// no rows.
#[test]
fn bounded_batches_still_verify_the_end() {
    let n = 2 * MIN_RECORDS_PER_BATCH;
    let mut dbc = dbf_to_dbc(&table(n, |_| true));
    let h = u16::from_le_bytes([dbc[8], dbc[9]]) as usize;
    dbc[h] ^= 1; // stored CRC
    let mut r = DbcReader::new(&dbc[..], ReadOptions::default()).unwrap();
    let mut buf = ColumnBuffers::new(2);
    assert_eq!(r.read_batch(1, &mut buf).unwrap(), 0);
    assert!(!r.is_finished());
    assert!(matches!(
        r.read_batch(1, &mut buf),
        Err(Error::CrcMismatch { .. })
    ));
    assert!(!r.is_finished());

    // Live rows: a batch ending exactly at the last record verifies it.
    let mut dbc = dbf_to_dbc(&table(10, |_| false));
    let mut r = DbcReader::new(&dbc[..], ReadOptions::default()).unwrap();
    assert_eq!(r.read_batch(10, &mut buf).unwrap(), 10);
    assert!(r.is_finished());
    dbc[h] ^= 1;
    let mut r = DbcReader::new(&dbc[..], ReadOptions::default()).unwrap();
    assert!(matches!(
        r.read_batch(10, &mut buf),
        Err(Error::CrcMismatch { .. })
    ));
}

/// `read_batch(0)` does nothing: `Ok(0)`, no record consumed, not finished.
#[test]
fn zero_row_batch_is_a_no_op() {
    let dbc = dbf_to_dbc(&table(3, |_| false));
    let mut r = DbcReader::new(&dbc[..], ReadOptions::default()).unwrap();
    let mut buf = ColumnBuffers::new(2);
    for _ in 0..3 {
        assert_eq!(r.read_batch(0, &mut buf).unwrap(), 0);
    }
    assert_eq!(r.progress().records_read, 0);
    assert!(!r.is_finished());
    assert_eq!(r.read_batch(5, &mut buf).unwrap(), 3);
    assert!(r.is_finished());
    assert_eq!(r.read_batch(0, &mut buf).unwrap(), 0);
}

/// `Header::read` + `resolve_schema` give the reader's columns while
/// reading only the header bytes.
#[test]
fn schema_from_header_only() {
    struct Counted<'a>(&'a [u8], usize);
    impl Read for Counted<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.0.read(buf)?;
            self.1 += n;
            Ok(n)
        }
    }
    let fields = [
        ("TXT", b'C', 8, 0),
        ("DT", b'C', 8, 0),
        ("N", b'N', 5, 0),
        ("V", b'N', 10, 2),
        ("D", b'D', 8, 0),
    ];
    let dbf = build_dbf(&fields, &[], 0x02, false);
    let dbc = dbf_to_dbc(&dbf);
    let opts = ReadOptions {
        types: vec![("dt".into(), ColumnType::Date)],
        ..Default::default()
    };
    let mut src = Counted(&dbc, 0);
    // Reads go through `&mut` so the byte count is visible afterwards.
    let (header, raw) = Header::read(&mut src, opts.encoding).unwrap();
    assert_eq!(raw.len(), header.header_len as usize);
    assert_eq!(src.1, header.header_len as usize);
    let cols = resolve_schema(&header, &opts).unwrap();
    let r = DbcReader::new(&dbc[..], opts.clone()).unwrap();
    assert_eq!(cols, r.columns());
    assert_eq!(
        cols.iter().map(|c| c.column_type).collect::<Vec<_>>(),
        [
            ColumnType::Varchar,
            ColumnType::Date,
            ColumnType::BigInt,
            ColumnType::Decimal {
                width: 10,
                scale: 2
            },
            ColumnType::Date
        ]
    );
    // Same errors as opening a reader.
    let bad = ReadOptions {
        types: vec![("nope".into(), ColumnType::Date)],
        ..Default::default()
    };
    assert!(matches!(
        resolve_schema(&header, &bad),
        Err(Error::InvalidOption(_))
    ));
    assert!(DbcReader::new(&dbc[..], bad).is_err());
}

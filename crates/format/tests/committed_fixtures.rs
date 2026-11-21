//! Reads every committed fixture in `test/fixtures/` and checks the values
//! and errors documented in `test/fixtures/README.md`. These files are in
//! the repository, so nothing here is skipped.

#[path = "support/sha256.rs"]
mod sha256;

use std::path::PathBuf;

use read_dbc_format::{
    ColumnBuffers, ColumnType, CrcStatus, DbcReader, Encoding, Error, FallbackReason, Header,
    OwnedValue as V, ReadOptions, Summary, resolve_schema, values::days_from_civil,
};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test/fixtures")
}

fn bytes(name: &str) -> Vec<u8> {
    std::fs::read(dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

struct Read {
    names: Vec<String>,
    types: Vec<ColumnType>,
    fallback: Vec<Option<FallbackReason>>,
    encoding: Encoding,
    buf: ColumnBuffers,
    summary: Summary,
}

fn read_with(name: &str, opts: ReadOptions) -> Result<Read, Error> {
    let b = bytes(name);
    let mut r = if name.ends_with(".dbf") {
        DbcReader::new_dbf(&b[..], opts)?
    } else {
        DbcReader::new(&b[..], opts)?
    };
    let mut buf = ColumnBuffers::new(r.columns().len());
    while !r.is_finished() {
        r.read_batch(3, &mut buf)?;
    }
    Ok(Read {
        names: r.columns().iter().map(|c| c.name.clone()).collect(),
        types: r.columns().iter().map(|c| c.column_type).collect(),
        fallback: r.columns().iter().map(|c| c.fallback).collect(),
        encoding: r.encoding(),
        buf,
        summary: r.summary().unwrap().clone(),
    })
}

fn read(name: &str) -> Read {
    read_with(name, ReadOptions::default()).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn t(s: &str) -> V {
    V::Text(s.into())
}
fn i(x: i64) -> V {
    V::BigInt(x)
}
fn d64(x: i64) -> V {
    V::Decimal64(x)
}
fn d128(x: i128) -> V {
    V::Decimal128(x)
}
fn date(y: i32, m: u32, d: u32) -> V {
    V::Date(days_from_civil(y, m, d))
}
fn b(x: bool) -> V {
    V::Boolean(x)
}
const N: V = V::Null;

#[test]
fn sha256sums_cover_every_fixture() {
    assert_eq!(
        sha256::sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let sums = String::from_utf8(bytes("SHA256SUMS")).unwrap();
    let mut listed = Vec::new();
    for line in sums.lines() {
        let (hash, name) = line.split_once("  ").expect("sha256sum format");
        assert_eq!(sha256::sha256_hex(&bytes(name)), hash, "{name}");
        listed.push(name.to_string());
    }
    let mut on_disk: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".dbc") || n.ends_with(".dbf"))
        .collect();
    on_disk.sort();
    listed.sort();
    assert_eq!(
        on_disk, listed,
        "every fixture must be listed in SHA256SUMS"
    );
    assert!(listed.contains(&"RDAC0801.dbc".to_string()));
}

fn check_types(r: &Read) {
    assert_eq!(
        r.names,
        ["TXT", "N10", "N12", "N20", "N38", "DT", "FLAG", "DTC"]
    );
    assert_eq!(
        r.types,
        [
            ColumnType::Varchar,
            ColumnType::BigInt,
            ColumnType::Decimal {
                width: 12,
                scale: 2
            },
            ColumnType::Decimal {
                width: 20,
                scale: 0
            },
            ColumnType::Decimal {
                width: 38,
                scale: 4
            },
            ColumnType::Date,
            ColumnType::Boolean,
            ColumnType::Varchar,
        ]
    );
    assert!(r.fallback.iter().all(Option::is_none));
    assert_eq!(r.encoding, Encoding::Latin1);
    let c = &r.buf.columns;
    assert_eq!(
        c[0],
        [
            t("0012"),
            t("ABC"),
            t(""),
            t("A B"),
            t("  LEAD"),
            t("Z"),
            t("NUL"),
            t("12345678")
        ]
    );
    assert_eq!(
        c[1],
        [
            i(0),
            i(-42),
            N,
            i(9_999_999_999),
            i(-999_999_999),
            i(7),
            i(5),
            i(0)
        ]
    );
    assert_eq!(
        c[2],
        [
            d64(0),
            d64(-50),
            N,
            d64(99_999_999_999),
            d64(1230),
            d64(-100),
            d64(50),
            d64(0)
        ]
    );
    assert_eq!(
        c[3],
        [
            d128(0),
            d128(12_345_678_901_234_567_890),
            N,
            d128(-9_999_999_999_999_999_999),
            d128(99_999_999_999_999_999_999),
            d128(-1),
            N,
            d128(0)
        ]
    );
    assert_eq!(
        c[4],
        [
            d128(0),
            d128(1_234_567_890_123_456_789_012_345_678_901_234_567),
            N,
            d128(-999_999_999_999_999_999_999_999_999_999_999_999),
            d128(1),
            d128(-15_000),
            N,
            d128(10_000)
        ]
    );
    assert_eq!(
        c[5],
        [
            date(2023, 1, 15),
            N,
            N,
            N,
            date(1970, 1, 1),
            date(2999, 12, 31),
            N,
            date(2000, 2, 29)
        ]
    );
    assert_eq!(
        c[6],
        [
            b(true),
            b(false),
            b(true),
            b(false),
            N,
            N,
            b(true),
            b(false)
        ]
    );
    assert_eq!(
        c[7],
        [
            t("20240229"),
            t("00000000"),
            t(""),
            t("19991231"),
            t("20000101"),
            t("19700101"),
            t("20240101"),
            t("20231231")
        ]
    );
}

#[test]
fn types_dbc_and_plain_dbf() {
    let r = read("types.dbc");
    check_types(&r);
    assert_eq!(r.summary.records, 8);
    assert!(r.summary.eof_marker);
    assert!(matches!(r.summary.crc, CrcStatus::Verified(_)));
    let p = read("plain.dbf");
    check_types(&p);
    assert_eq!(p.summary.crc, CrcStatus::NotApplicable);

    // C → DATE override: "00000000" and blank are NULL.
    let opts = ReadOptions {
        types: vec![("DTC".into(), ColumnType::Date)],
        ..Default::default()
    };
    for name in ["types.dbc", "plain.dbf"] {
        let r = read_with(name, opts.clone()).unwrap();
        assert_eq!(r.types[7], ColumnType::Date);
        assert_eq!(
            r.buf.columns[7],
            [
                date(2024, 2, 29),
                N,
                N,
                date(1999, 12, 31),
                date(2000, 1, 1),
                date(1970, 1, 1),
                date(2024, 1, 1),
                date(2023, 12, 31)
            ]
        );
    }
    // all_varchar keeps the raw text (trailing spaces stripped).
    let r = read_with(
        "types.dbc",
        ReadOptions {
            all_varchar: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(r.types.iter().all(|&t| t == ColumnType::Varchar));
    assert_eq!(r.buf.columns[2][4], t("  12.3"));
    assert_eq!(r.buf.columns[5][3], t("20230230"));
}

#[test]
fn encodings() {
    let words = ["ANÁLISE", "CIRÚRGICA", "SEM INFORMAÇÃO", "Nº"];
    for (name, enc) in [
        ("cp850.dbc", Encoding::Cp850),
        ("latin1.dbc", Encoding::Latin1),
        ("cp1252.dbc", Encoding::Cp1252),
        ("cp860.dbc", Encoding::Cp860),
    ] {
        let r = read(name);
        assert_eq!(r.encoding, enc, "{name}");
        assert_eq!(r.types, [ColumnType::BigInt, ColumnType::Varchar]);
        let mut expect: Vec<V> = words.iter().map(|w| t(w)).collect();
        if name == "cp1252.dbc" {
            expect.push(t("€ 10"));
        }
        assert_eq!(r.buf.columns[1], expect, "{name}");
        let ids: Vec<V> = (1..=expect.len() as i64).map(i).collect();
        assert_eq!(r.buf.columns[0], ids);
        // Strict UTF-8 rejects the single-byte text.
        let e = read_with(
            name,
            ReadOptions {
                encoding: Encoding::Utf8,
                ..Default::default()
            },
        );
        assert!(matches!(e, Err(Error::InvalidValue { .. })), "{name}");
    }
    // The same bytes read as CP850 differ from CP860.
    let r = read_with(
        "cp860.dbc",
        ReadOptions {
            encoding: Encoding::Cp850,
            ..Default::default()
        },
    )
    .unwrap();
    assert_ne!(r.buf.columns[1][0], t("ANÁLISE"));
}

#[test]
fn deleted_records() {
    let r = read("deleted.dbc");
    assert_eq!(r.buf.columns[0], [i(1), i(3), i(4)]);
    assert_eq!(r.buf.columns[1], [t("ONE"), t("THREE"), t("FOUR")]);
    assert_eq!((r.summary.records, r.summary.deleted), (5, 2));
    let r = read_with(
        "deleted.dbc",
        ReadOptions {
            include_deleted: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.buf.columns[0], [i(1), i(2), i(3), i(4), i(5)]);
    assert_eq!(r.buf.deleted, [false, true, false, false, true]);
}

#[test]
fn eof_marker_and_empty() {
    for (name, eof) in [("eof_marker.dbc", true), ("no_eof_marker.dbc", false)] {
        let r = read(name);
        assert_eq!(r.summary.eof_marker, eof, "{name}");
        assert_eq!(r.buf.columns[0], [i(1), i(2), i(3)]);
        assert_eq!(r.buf.columns[1], [t("A"), t("B"), t("C")]);
    }
    let r = read("empty.dbc");
    assert_eq!(r.names, ["ID", "NAME"]);
    assert_eq!(r.types, [ColumnType::BigInt, ColumnType::Varchar]);
    assert_eq!(r.summary.records, 0);
    assert!(r.summary.eof_marker);
    assert!(r.buf.columns.iter().all(Vec::is_empty));
}

#[test]
fn fallback_columns() {
    let r = read("fallback.dbc");
    assert_eq!(r.names, ["ID", "MEMO", "ODD", "NAME", "D6"]);
    assert_eq!(
        r.types,
        [
            ColumnType::BigInt,
            ColumnType::Varchar,
            ColumnType::Varchar,
            ColumnType::Varchar,
            ColumnType::Varchar
        ]
    );
    assert_eq!(
        r.fallback,
        [
            None,
            Some(FallbackReason::UnsupportedType),
            Some(FallbackReason::DecimalsNotBelowWidth),
            None,
            Some(FallbackReason::DateWidth)
        ]
    );
    assert_eq!(r.buf.columns[0], [i(1), i(2)]);
    assert_eq!(r.buf.columns[1], [t("         1"), t("")]);
    assert_eq!(r.buf.columns[2], [t("12.3"), t("")]);
    assert_eq!(r.buf.columns[3], [t("ab"), t("cd")]);
    assert_eq!(r.buf.columns[4], [t("202401"), t("")]);
}

#[test]
fn duplicate_names() {
    let r = read("dupnames.dbc");
    assert_eq!(r.names, ["A", "A"]);
    assert_eq!(r.types, [ColumnType::Varchar, ColumnType::BigInt]);
    assert_eq!(r.buf.columns[0], [t("x"), t("y")]);
    assert_eq!(r.buf.columns[1], [i(1), i(2)]);
    let e = read_with(
        "dupnames.dbc",
        ReadOptions {
            types: vec![("A".into(), ColumnType::Varchar)],
            ..Default::default()
        },
    );
    assert!(matches!(e, Err(Error::InvalidOption(_))));
}

#[test]
fn layouts() {
    let a = read("layout_a.dbc");
    assert_eq!(a.names, ["UF", "CODE", "QTY", "NAME"]);
    assert_eq!(a.types[2], ColumnType::BigInt);
    assert_eq!(a.buf.columns[0], [t("AC"), t("AM")]);
    assert_eq!(a.buf.columns[1], [t("12345"), t("00001")]);
    assert_eq!(a.buf.columns[2], [i(100), i(-5)]);
    assert_eq!(a.buf.columns[3], [t("ALPHA"), t("BETA")]);

    let b = read("layout_b.dbc");
    assert_eq!(b.names, ["UF", "CODE", "EXTRA", "QTY", "NAME"]);
    assert_eq!(b.types[3], ColumnType::Decimal { width: 7, scale: 2 });
    assert_eq!(b.buf.columns[0], [t("PA"), t("RR")]);
    assert_eq!(b.buf.columns[1], [t("12345678"), t("00000002")]);
    assert_eq!(b.buf.columns[2], [t("X1"), t("")]);
    assert_eq!(b.buf.columns[3], [d64(123_456), d64(-50)]);
    assert_eq!(b.buf.columns[4], [t("GAMMA"), t("DELTA")]);

    let widths = |name: &str| {
        let (h, _) = Header::read(&mut &bytes(name)[..], Encoding::Auto).unwrap();
        h.fields
            .iter()
            .map(|f| (f.name.clone(), f.length, f.decimals))
            .collect::<Vec<_>>()
    };
    assert_eq!(widths("layout_a.dbc")[1], ("CODE".into(), 5, 0));
    assert_eq!(widths("layout_b.dbc")[1], ("CODE".into(), 8, 0));
}

#[test]
fn corrupt_files_fail() {
    let e = |name: &str| match read_with(name, ReadOptions::default()) {
        Ok(_) => panic!("{name} read without error"),
        Err(e) => e,
    };
    // Opening succeeds; the CRC error comes with the last row.
    let b = bytes("bad_crc.dbc");
    let mut r = DbcReader::new(&b[..], ReadOptions::default()).unwrap();
    let mut buf = ColumnBuffers::new(2);
    assert!(matches!(
        r.read_batch(10, &mut buf),
        Err(Error::CrcMismatch { .. })
    ));
    assert!(matches!(e("bad_crc.dbc"), Error::CrcMismatch { .. }));
    // ... and is not checked with verify_crc off.
    assert!(
        read_with(
            "bad_crc.dbc",
            ReadOptions {
                verify_crc: false,
                ..Default::default()
            }
        )
        .is_ok()
    );

    let err = e("truncated.dbc");
    assert!(
        matches!(err, Error::Dcl { .. } | Error::TruncatedRecords { .. }),
        "{err:?}"
    );
    let err = e("bad_header.dbc");
    assert!(matches!(err, Error::RecordLengthMismatch { .. }), "{err:?}");
    let err = e("bad_flag.dbc");
    assert!(
        matches!(
            err,
            Error::BadDeletionFlag {
                record: 3,
                byte: b'X',
                ..
            }
        ),
        "{err:?}"
    );
    let err = e("trailing_garbage.dbc");
    assert!(matches!(err, Error::TrailingData { .. }), "{err:?}");
    let err = e("bad_number.dbc");
    assert!(
        matches!(&err, Error::InvalidValue { record: 3, field, raw, .. } if field == "ID" && raw == b"ABC"),
        "{err:?}"
    );
    // The bad value is only an error if ID is decoded: CRC and structure
    // are valid.
    let b = bytes("bad_number.dbc");
    let mut r = DbcReader::new(&b[..], ReadOptions::default()).unwrap();
    r.set_projection(&[1]).unwrap();
    let mut buf = ColumnBuffers::new(1);
    while !r.is_finished() {
        r.read_batch(10, &mut buf).unwrap();
    }
    assert_eq!(buf.columns[0], [t("A"), t("B"), t("C"), t("D")]);
}

#[test]
fn real_datasus_file() {
    let r = read("RDAC0801.dbc");
    assert_eq!(r.summary.records, 2087);
    assert_eq!(r.summary.deleted, 0);
    assert!(r.summary.eof_marker);
    assert_eq!(r.summary.crc, CrcStatus::Verified(0x9b94_505a));
    assert_eq!(r.encoding, Encoding::Cp850);
    assert_eq!(r.names.len(), 86);
    assert!(r.fallback.iter().all(Option::is_none));
    let (h, _) = Header::read(&mut &bytes("RDAC0801.dbc")[..], Encoding::Auto).unwrap();
    let cols = resolve_schema(&h, &ReadOptions::default()).unwrap();
    assert_eq!(cols.len(), 86);
    assert_eq!(h.fields.iter().filter(|f| f.field_type == b'C').count(), 55);
    assert_eq!(h.fields.iter().filter(|f| f.field_type == b'N').count(), 31);
    assert!(r.buf.columns.iter().all(|c| c.len() == 2087));
}

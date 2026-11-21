//! Type mapping, overrides, encodings, projection and deleted records on
//! synthetic tables (both `.dbf` and `.dbc` containers).

mod common;

use common::*;
use read_dbc_format::{
    ColumnBuffers, ColumnType, DbcReader, Encoding, Error, OwnedValue as V, ReadOptions,
};

fn read_all(
    bytes: &[u8],
    dbc: bool,
    opts: ReadOptions,
) -> Result<(Vec<ColumnType>, ColumnBuffers), Error> {
    let mut r = if dbc {
        DbcReader::new(bytes, opts)?
    } else {
        DbcReader::new_dbf(bytes, opts)?
    };
    let types = r.columns().iter().map(|c| c.column_type).collect();
    let mut buf = ColumnBuffers::new(r.columns().len());
    while !r.is_finished() {
        r.read_batch(2, &mut buf)?;
    }
    Ok((types, buf))
}

/// Read both as .dbf and as .dbc and check they agree.
fn both(dbf: &[u8], opts: ReadOptions) -> Result<(Vec<ColumnType>, ColumnBuffers), Error> {
    let a = read_all(dbf, false, opts.clone());
    let b = read_all(&dbf_to_dbc(dbf), true, opts);
    match (&a, &b) {
        (Ok(x), Ok(y)) => {
            assert_eq!(x.0, y.0);
            assert_eq!(x.1.columns, y.1.columns);
        }
        (Err(x), Err(y)) => assert_eq!(x.to_string(), y.to_string()),
        _ => panic!("dbf and dbc disagree: {a:?} vs {b:?}"),
    }
    a
}

fn t(s: &str) -> V {
    V::Text(s.into())
}

const FIELDS: [FieldSpec; 9] = [
    ("NAME", b'C', 6, 0),
    ("CODE", b'C', 4, 0),
    ("QTY", b'N', 5, 0),
    ("VAL", b'N', 10, 2),
    ("BIG", b'N', 25, 0),
    ("HUGE", b'N', 30, 4),
    ("FLT", b'F', 8, 3),
    ("DT", b'D', 8, 0),
    ("OK", b'L', 1, 0),
];

fn sample() -> Vec<u8> {
    let recs = vec![
        rec(
            b' ',
            &[
                b"  ab  ",
                b"0012",
                b"   42",
                b"   -123.45",
                b"1234567890123456789012345",
                b"    -12345678901234567890.1234",
                b"   1.5  ",
                b"20240229",
                b"T",
            ],
        ),
        rec(
            b' ',
            &[
                b"      ",
                b"\0\0\0\0",
                b"     ",
                b"          ",
                b"                         ",
                b"                              ",
                b"        ",
                b"        ",
                b" ",
            ],
        ),
        rec(
            b'*',
            &[
                b"DEL   ",
                b"0000",
                b"    1",
                b"      1.00",
                b"                        1",
                b"                             1",
                b"       1",
                b"00000000",
                b"?",
            ],
        ),
        rec(
            b' ',
            &[
                b"x \0 \0 ",
                b" 1  ",
                b"  -0 ",
                b"       .5 ",
                b"                       -0",
                b"                         0.000",
                b"  -0.125",
                b"20231301",
                b"n",
            ],
        ),
    ];
    build_dbf(&FIELDS, &recs, 0x00, true)
}

#[test]
fn natural_types() {
    let (types, buf) = both(&sample(), ReadOptions::default()).unwrap();
    assert_eq!(
        types,
        vec![
            ColumnType::Varchar,
            ColumnType::Varchar,
            ColumnType::BigInt,
            ColumnType::Decimal {
                width: 10,
                scale: 2
            },
            ColumnType::Decimal {
                width: 25,
                scale: 0
            },
            ColumnType::Decimal {
                width: 30,
                scale: 4
            },
            ColumnType::Decimal { width: 8, scale: 3 },
            ColumnType::Date,
            ColumnType::Boolean,
        ]
    );
    let c = &buf.columns;
    assert_eq!(c[0], vec![t("  ab"), t(""), t("x")]);
    assert_eq!(c[1], vec![t("0012"), t(""), t(" 1")]);
    assert_eq!(c[2], vec![V::BigInt(42), V::Null, V::BigInt(0)]);
    assert_eq!(c[3], vec![V::Decimal64(-12345), V::Null, V::Decimal64(50)]);
    assert_eq!(
        c[4],
        vec![
            V::Decimal128(1234567890123456789012345),
            V::Null,
            V::Decimal128(0)
        ]
    );
    assert_eq!(
        c[5],
        vec![
            V::Decimal128(-123456789012345678901234),
            V::Null,
            V::Decimal128(0)
        ]
    );
    assert_eq!(c[6], vec![V::Decimal64(1500), V::Null, V::Decimal64(-125)]);
    assert_eq!(c[7], vec![V::Date(19_782), V::Null, V::Null]);
    assert_eq!(c[8], vec![V::Boolean(true), V::Null, V::Boolean(false)]);
    assert!(buf.deleted.is_empty());
}

#[test]
fn include_deleted_and_projection() {
    let opts = ReadOptions {
        include_deleted: true,
        ..Default::default()
    };
    let dbf = sample();
    let mut r = DbcReader::new_dbf(&dbf[..], opts).unwrap();
    r.set_projection(&[8, 0, 0]).unwrap();
    let mut buf = ColumnBuffers::new(3);
    while !r.is_finished() {
        r.read_batch(3, &mut buf).unwrap();
    }
    assert_eq!(buf.deleted, vec![false, false, true, false]);
    assert_eq!(
        buf.columns[0],
        vec![V::Boolean(true), V::Null, V::Null, V::Boolean(false)]
    );
    assert_eq!(buf.columns[1], vec![t("  ab"), t(""), t("DEL"), t("x")]);
    assert_eq!(buf.columns[1], buf.columns[2]);
    let s = r.summary().unwrap();
    assert_eq!((s.records, s.deleted), (4, 1));
    assert!(r.set_projection(&[9]).is_err());
}

#[test]
fn projection_skips_bad_values() {
    let fields = [("A", b'C', 2, 0), ("N", b'N', 3, 0)];
    let dbf = build_dbf(&fields, &[rec(b' ', &[b"ok", b"x!y"])], 0, false);
    let e = both(&dbf, ReadOptions::default()).unwrap_err();
    let msg = e.to_string();
    assert!(
        matches!(&e, Error::InvalidValue { record: 0, field, offset, raw, .. }
        if field == "N" && *offset == 32 + 64 + 1 + 3 && raw == b"x!y"),
        "{e:?}"
    );
    assert!(
        msg.contains("record 0") && msg.contains("\"N\"") && msg.contains("x!y"),
        "{msg}"
    );
    // Projecting only A never parses N.
    let dbc = dbf_to_dbc(&dbf);
    let mut r = DbcReader::new(&dbc[..], ReadOptions::default()).unwrap();
    r.set_projection(&[0]).unwrap();
    let mut buf = ColumnBuffers::new(1);
    assert_eq!(r.read_batch(10, &mut buf).unwrap(), 1);
    assert_eq!(buf.columns[0], vec![t("ok")]);
    // An empty projection still counts rows.
    let mut r = DbcReader::new(&dbc[..], ReadOptions::default()).unwrap();
    r.set_projection(&[]).unwrap();
    let mut buf = ColumnBuffers::new(0);
    assert_eq!(r.read_batch(10, &mut buf).unwrap(), 1);
    assert_eq!(r.read_batch(10, &mut buf).unwrap(), 0);
    assert!(r.is_finished());
}

#[test]
fn numeric_errors() {
    for bad in [
        &b"1.234"[..],
        b"1e5  ",
        b"1,50 ",
        b"*****",
        b"- 1  ",
        b"12 3 ",
    ] {
        let fields = [("V", b'N', 5, 2)];
        let dbf = build_dbf(&fields, &[rec(b' ', &[bad])], 0, false);
        let e = both(&dbf, ReadOptions::default()).unwrap_err();
        assert!(matches!(e, Error::InvalidValue { .. }), "{bad:?}: {e:?}");
    }
}

#[test]
fn all_varchar() {
    let opts = ReadOptions {
        all_varchar: true,
        ..Default::default()
    };
    let (types, buf) = both(&sample(), opts).unwrap();
    assert!(types.iter().all(|&t| t == ColumnType::Varchar));
    assert_eq!(buf.columns[2], vec![t("   42"), t(""), t("  -0")]);
    assert_eq!(buf.columns[3], vec![t("   -123.45"), t(""), t("       .5")]);
    assert_eq!(buf.columns[7], vec![t("20240229"), t(""), t("20231301")]);
}

#[test]
fn fallback_to_varchar() {
    use read_dbc_format::{FallbackReason as FR, Header, resolve_schema};
    // (field spec, raw value, expected fallback)
    let mut cases: Vec<(FieldSpec, Vec<u8>, FR)> = Vec::new();
    for &ty in b"MBGIT@Y0PV?" {
        cases.push((("X", ty, 4, 0), b"\x01ab ".to_vec(), FR::UnsupportedType));
    }
    cases.push((("X", b'D', 6, 0), b"202401".to_vec(), FR::DateWidth));
    cases.push((("X", b'D', 10, 0), b"2024-01-01".to_vec(), FR::DateWidth));
    cases.push((("X", b'N', 39, 0), [b' '; 39].to_vec(), FR::NumericTooWide));
    cases.push((("X", b'F', 40, 2), [b'9'; 40].to_vec(), FR::NumericTooWide));
    cases.push((
        ("X", b'N', 4, 9),
        b"12.3".to_vec(),
        FR::DecimalsNotBelowWidth,
    ));
    cases.push((
        ("X", b'N', 4, 4),
        b"  .5".to_vec(),
        FR::DecimalsNotBelowWidth,
    ));
    cases.push((
        ("X", b'F', 3, 200),
        b"abc".to_vec(),
        FR::DecimalsNotBelowWidth,
    ));
    for (spec, raw, reason) in cases {
        let fields = [("ID", b'N', 2, 0), spec, ("Z", b'C', 2, 0)];
        let mut blank = vec![b' '; spec.2 as usize];
        blank[0] = 0;
        let dbf = build_dbf(
            &fields,
            &[
                rec(b' ', &[b" 1", &raw, b"ok"]),
                rec(b' ', &[b" 2", &blank, b"  "]),
            ],
            0x02,
            false,
        );
        let ctx = format!("{} {:?}", spec.1 as char, spec);
        let (header, _) = Header::read(&mut &dbf[..], Encoding::Auto).unwrap();
        let cols = resolve_schema(&header, &ReadOptions::default()).unwrap();
        assert_eq!(cols[1].column_type, ColumnType::Varchar, "{ctx}");
        assert_eq!(cols[1].fallback, Some(reason), "{ctx}");
        assert_eq!(cols[0].fallback, None);
        assert_eq!(cols[2].fallback, None);
        let (types, buf) = both(&dbf, ReadOptions::default()).unwrap();
        assert_eq!(types[0], ColumnType::BigInt);
        // C rules: raw bytes, trailing spaces/NULs stripped, file encoding.
        let mut s = String::new();
        let expect = Encoding::Cp850
            .decode(read_dbc_format::values::trim_text(&raw), &mut s)
            .unwrap()
            .to_owned();
        assert_eq!(buf.columns[1], vec![t(&expect), t("")], "{ctx}");
        // all_varchar and a VARCHAR override are the caller's choice: no
        // fallback reported.
        let opts = ReadOptions {
            all_varchar: true,
            ..Default::default()
        };
        let cols = resolve_schema(&header, &opts).unwrap();
        assert!(cols.iter().all(|c| c.fallback.is_none()));
        let opts = ReadOptions {
            types: vec![("X".into(), ColumnType::Varchar)],
            ..Default::default()
        };
        assert_eq!(resolve_schema(&header, &opts).unwrap()[1].fallback, None);
        // A typed override of a fallback field is rejected.
        let opts = ReadOptions {
            types: vec![("X".into(), ColumnType::BigInt)],
            ..Default::default()
        };
        assert!(matches!(
            resolve_schema(&header, &opts),
            Err(Error::InvalidOption(_))
        ));
    }
    assert_eq!(
        FR::DecimalsNotBelowWidth.to_string(),
        "numeric field decimal count is not smaller than its width"
    );

    // Edges that keep their natural type.
    for (spec, ty) in [
        (
            ("X", b'N', 38, 37),
            ColumnType::Decimal {
                width: 38,
                scale: 37,
            },
        ),
        (
            ("X", b'N', 38, 0),
            ColumnType::Decimal {
                width: 38,
                scale: 0,
            },
        ),
        (
            ("X", b'N', 2, 1),
            ColumnType::Decimal { width: 2, scale: 1 },
        ),
        (("X", b'D', 8, 0), ColumnType::Date),
    ] {
        let dbf = build_dbf(&[spec], &[], 0, false);
        let (types, _) = both(&dbf, ReadOptions::default()).unwrap();
        assert_eq!(types, vec![ty], "{spec:?}");
    }

    // A zero-width field is still a hard error.
    let dbf = build_dbf(&[("X", b'M', 0, 0)], &[], 0, false);
    assert!(matches!(
        both(&dbf, ReadOptions::default()),
        Err(Error::InvalidHeader { .. })
    ));
}

#[test]
fn type_overrides() {
    let fields = [
        ("NASC", b'C', 8, 0),
        ("IDADE", b'C', 3, 0),
        ("VAL", b'N', 6, 2),
        ("Z", b'C', 2, 0),
    ];
    let recs = vec![
        rec(b' ', &[b"19250818", b"042", b"  1.50", b"ab"]),
        rec(b' ', &[b"        ", b"   ", b"      ", b"  "]),
        rec(b' ', &[b"00000000", b"007", b"  2.00", b"cd"]),
    ];
    let dbf = build_dbf(&fields, &recs, 0, false);
    let opts = ReadOptions {
        types: vec![
            ("nasc".into(), ColumnType::Date),
            ("Idade".into(), ColumnType::BigInt),
            ("VAL".into(), ColumnType::Varchar),
            ("z".into(), ColumnType::Varchar),
        ],
        ..Default::default()
    };
    let (types, buf) = both(&dbf, opts.clone()).unwrap();
    assert_eq!(
        types,
        vec![
            ColumnType::Date,
            ColumnType::BigInt,
            ColumnType::Varchar,
            ColumnType::Varchar
        ]
    );
    // "00000000" is NULL, like blank.
    assert_eq!(buf.columns[0], vec![V::Date(-16_207), V::Null, V::Null]);
    assert_eq!(buf.columns[1], vec![V::BigInt(42), V::Null, V::BigInt(7)]);
    assert_eq!(buf.columns[2], vec![t("  1.50"), t(""), t("  2.00")]);

    // Overrides win over all_varchar.
    let (types, _) = both(
        &dbf,
        ReadOptions {
            all_varchar: true,
            ..opts
        },
    )
    .unwrap();
    assert_eq!(types[0], ColumnType::Date);
    assert_eq!(types[2], ColumnType::Varchar);

    // Invalid C → DATE is a hard error with context.
    let bad = build_dbf(
        &fields,
        &[rec(b' ', &[b"20241301", b"1  ", b"  1.00", b"  "])],
        0,
        false,
    );
    let e = both(
        &bad,
        ReadOptions {
            types: vec![("NASC".into(), ColumnType::Date)],
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        matches!(&e, Error::InvalidValue { field, raw, .. } if field == "NASC" && raw == b"20241301"),
        "{e:?}"
    );
    // Any other invalid date (not the all-zero filler) stays an error.
    for bad_date in [&b"20230230"[..], b"0000000 ", b"00000101", b"2023023X"] {
        let bad = build_dbf(
            &fields,
            &[rec(b' ', &[bad_date, b"1  ", b"  1.00", b"  "])],
            0,
            false,
        );
        let e = both(
            &bad,
            ReadOptions {
                types: vec![("NASC".into(), ColumnType::Date)],
                ..Default::default()
            },
        );
        assert!(matches!(e, Err(Error::InvalidValue { .. })), "{bad_date:?}");
    }
    let e = both(
        &bad,
        ReadOptions {
            types: vec![("Z".into(), ColumnType::BigInt)],
            ..Default::default()
        },
    );
    assert!(e.is_ok(), "blank C → BIGINT is NULL");
    let bad2 = build_dbf(
        &fields,
        &[rec(b' ', &[b"20240101", b"1x ", b"  1.00", b"  "])],
        0,
        false,
    );
    let e = both(
        &bad2,
        ReadOptions {
            types: vec![("IDADE".into(), ColumnType::BigInt)],
            ..Default::default()
        },
    );
    assert!(matches!(e, Err(Error::InvalidValue { .. })));

    // Unknown field, unsupported conversion, duplicate override.
    for types in [
        vec![("NOPE".to_string(), ColumnType::Date)],
        vec![("VAL".to_string(), ColumnType::Date)],
        vec![("NASC".to_string(), ColumnType::Boolean)],
        vec![
            ("NASC".to_string(), ColumnType::Date),
            ("nasc".to_string(), ColumnType::Varchar),
        ],
    ] {
        let e = both(
            &dbf,
            ReadOptions {
                types: types.clone(),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(e, Error::InvalidOption(_)), "{types:?}: {e:?}");
    }
}

#[test]
fn encodings() {
    let fields = [("T", b'C', 7, 0)];
    let recs = vec![
        rec(b' ', &[b"AN\xa0LISE"]),
        rec(b' ', &[b"N\xba     "]),
        rec(b' ', &[b"\x80      "]),
    ];
    let cases = [
        (
            0x02,
            Encoding::Auto,
            Encoding::Cp850,
            ["ANáLISE", "N║", "Ç"],
        ),
        (
            0x00,
            Encoding::Auto,
            Encoding::Latin1,
            ["AN\u{a0}LISE", "Nº", "\u{80}"],
        ),
        (
            0x57,
            Encoding::Auto,
            Encoding::Cp1252,
            ["AN\u{a0}LISE", "Nº", "€"],
        ),
        (
            0x01,
            Encoding::Auto,
            Encoding::Cp437,
            ["ANáLISE", "N║", "Ç"],
        ),
        (
            0x02,
            Encoding::Latin1,
            Encoding::Latin1,
            ["AN\u{a0}LISE", "Nº", "\u{80}"],
        ),
    ];
    for (ldid, req, resolved, want) in cases {
        let dbf = build_dbf(&fields, &recs, ldid, false);
        let r = DbcReader::new_dbf(
            &dbf[..],
            ReadOptions {
                encoding: req,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(r.encoding(), resolved);
        let (_, buf) = both(
            &dbf,
            ReadOptions {
                encoding: req,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            buf.columns[0],
            want.iter().map(|s| t(s)).collect::<Vec<_>>(),
            "ldid {ldid:#x}"
        );
    }
    // UTF-8: valid passes, invalid is a hard error.
    let ok = build_dbf(&fields, &[rec(b' ', &["ação".as_bytes(), b" "])], 0, false);
    let (_, buf) = both(
        &ok,
        ReadOptions {
            encoding: Encoding::Utf8,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(buf.columns[0], vec![t("ação")]);
    let bad = build_dbf(&fields, &recs[..1], 0, false);
    let e = both(
        &bad,
        ReadOptions {
            encoding: Encoding::Utf8,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(matches!(e, Error::InvalidValue { .. }), "{e:?}");
}

#[test]
fn header_metadata_and_progress() {
    let dbf = sample();
    let dbc = dbf_to_dbc(&dbf);
    let mut r = DbcReader::new(&dbc[..], ReadOptions::default()).unwrap();
    let h = r.header();
    assert_eq!(
        (
            h.version,
            h.record_count,
            h.header_len,
            h.ldid,
            h.terminator
        ),
        (0x03, 4, 32 + 9 * 32 + 1, 0, 0x0D)
    );
    assert_eq!(h.fields[3].name, "VAL");
    assert_eq!(
        (
            h.fields[3].type_char(),
            h.fields[3].length,
            h.fields[3].decimals,
            h.fields[3].offset
        ),
        ('N', 10, 2, 16)
    );
    assert_eq!(r.progress().records_read, 0);
    let mut buf = ColumnBuffers::new(9);
    assert_eq!(r.read_batch(1, &mut buf).unwrap(), 1);
    let p = r.progress();
    assert_eq!((p.records_read, p.records_declared), (1, 4));
    assert!(p.fraction() > 0.0 && p.fraction() < 1.0);
    while !r.is_finished() {
        r.read_batch(1, &mut buf).unwrap();
    }
    assert_eq!(r.progress().fraction(), 1.0);
    assert_eq!(r.progress().input_bytes, dbc.len() as u64);
}

#[test]
fn empty_table() {
    let fields = [("A", b'C', 1, 0)];
    for eof in [false, true] {
        let dbf = build_dbf(&fields, &[], 0, eof);
        let (_, buf) = both(&dbf, ReadOptions::default()).unwrap();
        assert!(buf.columns[0].is_empty());
    }
}

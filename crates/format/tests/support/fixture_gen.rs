//! Definitions of the synthetic fixtures committed in `test/fixtures/`.
//!
//! Shared (via `#[path]`) by the `make_fixtures` example, which writes
//! them, and by `tests/fixtures_up_to_date.rs`, which checks that the
//! committed files are exactly what this code generates. Requires the
//! `writer` feature. Output is fully deterministic.
#![allow(dead_code)]

use read_dbc_format::Encoding;
use read_dbc_format::writer::{DclOptions, FieldDef, Table};

use super::sha256::sha256_hex;

/// The one real DATASUS file kept in `test/fixtures/` (copied, not
/// generated): SIH/RD, Acre, January 2008.
pub const REAL_FILE: &str = "RDAC0801.dbc";
/// Its SHA-256 (same as in `scripts/fixtures/SHA256SUMS`).
pub const REAL_FILE_SHA256: &str =
    "fde119624f5ba8400fdd25f086be6180bb25440c141dbf9ae634b7f417d0f28d";

/// One generated file.
pub struct Fixture {
    pub name: &'static str,
    pub bytes: Vec<u8>,
    /// DCL parameters (`None` for `.dbf` and for files that are not a
    /// complete valid stream description).
    pub dcl: Option<DclOptions>,
}

fn f(name: &str, ty: u8, len: u8, dec: u8) -> FieldDef {
    FieldDef::new(name, ty, len, dec)
}

/// Encode `s` in a single-byte encoding by inverting its decoder.
pub fn encode_text(s: &str, enc: Encoding) -> Vec<u8> {
    let mut scratch = String::new();
    s.chars()
        .map(|c| {
            (0..=255u8)
                .find(|&b| {
                    enc.decode(&[b], &mut scratch)
                        .is_ok_and(|d| d.chars().eq([c]))
                })
                .unwrap_or_else(|| panic!("{c:?} is not encodable in {enc}"))
        })
        .collect()
}

const RAW4K: DclOptions = DclOptions {
    coded_literals: false,
    dict_bits: 6,
    matching: true,
};
const CODED1K: DclOptions = DclOptions {
    coded_literals: true,
    dict_bits: 4,
    matching: true,
};
const RAW2K: DclOptions = DclOptions {
    coded_literals: false,
    dict_bits: 5,
    matching: true,
};
const CODED4K: DclOptions = DclOptions {
    coded_literals: true,
    dict_bits: 6,
    matching: true,
};
const CODED2K: DclOptions = DclOptions {
    coded_literals: true,
    dict_bits: 5,
    matching: true,
};
const RAW1K: DclOptions = DclOptions {
    coded_literals: false,
    dict_bits: 4,
    matching: true,
};
const LITERALS: DclOptions = DclOptions {
    coded_literals: false,
    dict_bits: 6,
    matching: false,
};

/// `types.dbc` / `plain.dbf`.
pub fn types_table() -> Table {
    let mut t = Table::new(vec![
        f("TXT", b'C', 8, 0),
        f("N10", b'N', 10, 0),
        f("N12", b'N', 12, 2),
        f("N20", b'N', 20, 0),
        f("N38", b'N', 38, 4),
        f("DT", b'D', 8, 0),
        f("FLAG", b'L', 1, 0),
        f("DTC", b'C', 8, 0),
    ]);
    let rows: [[&[u8]; 8]; 8] = [
        [
            b"0012",
            b"0",
            b"0.00",
            b"0",
            b"0.0000",
            b"20230115",
            b"T",
            b"20240229",
        ],
        [
            b"ABC   ",
            b"-42",
            b"-0.50",
            b"12345678901234567890",
            b"123456789012345678901234567890123.4567",
            b"",
            b"F",
            b"00000000",
        ],
        [b"", b"", b"", b"", b"", b"00000000", b"Y", b""],
        [
            b"A B",
            b"9999999999",
            b"999999999.99",
            b"-9999999999999999999",
            b"-99999999999999999999999999999999.9999",
            b"20230230",
            b"N",
            b"19991231",
        ],
        [
            b"  LEAD",
            b"-999999999",
            b"12.3      ",
            b"99999999999999999999",
            b"0.0001",
            b"19700101",
            b"?",
            b"20000101",
        ],
        [
            b"Z",
            b"7",
            b"-1",
            b"-1",
            b"-1.5",
            b"29991231",
            b"",
            b"19700101",
        ],
        [b"NUL\0\0", b"+5", b".5", b"", b"", b"", b"t", b"20240101"],
        [
            b"12345678",
            b"  -0",
            b"0",
            b"0",
            b"1",
            b"20000229",
            b"f",
            b"20231231",
        ],
    ];
    for r in rows {
        t.push(false, &r);
    }
    t
}

fn words_table(ldid: u8, enc: Encoding, words: &[&str]) -> Table {
    let mut t = Table::new(vec![f("ID", b'N', 2, 0), f("TXT", b'C', 16, 0)]);
    t.ldid = ldid;
    for (i, w) in words.iter().enumerate() {
        let id = (i + 1).to_string();
        t.push(false, &[id.as_bytes(), &encode_text(w, enc)]);
    }
    t
}

/// The accented words used by the encoding fixtures.
pub const WORDS: [&str; 4] = ["ANÁLISE", "CIRÚRGICA", "SEM INFORMAÇÃO", "Nº"];

fn small_table() -> Table {
    let mut t = Table::new(vec![f("ID", b'N', 3, 0), f("NAME", b'C', 4, 0)]);
    for (id, name) in [(&b"1"[..], &b"A"[..]), (b"2", b"B"), (b"3", b"C")] {
        t.push(false, &[id, name]);
    }
    t
}

/// Every synthetic fixture, in a fixed order.
pub fn fixtures() -> Vec<Fixture> {
    let mut out = Vec::new();
    let mut dbc = |name: &'static str, t: &Table, o: DclOptions| {
        out.push(Fixture {
            name,
            bytes: t.to_dbc(&o),
            dcl: Some(o),
        });
    };

    let types = types_table();
    dbc("types.dbc", &types, RAW4K);

    dbc(
        "cp850.dbc",
        &words_table(0x02, Encoding::Cp850, &WORDS),
        CODED1K,
    );
    dbc(
        "latin1.dbc",
        &words_table(0x00, Encoding::Latin1, &WORDS),
        RAW2K,
    );
    let mut w1252 = WORDS.to_vec();
    w1252.push("€ 10");
    dbc(
        "cp1252.dbc",
        &words_table(0x03, Encoding::Cp1252, &w1252),
        CODED4K,
    );
    dbc(
        "cp860.dbc",
        &words_table(0x24, Encoding::Cp860, &WORDS),
        RAW1K,
    );

    let mut del = Table::new(vec![f("ID", b'N', 3, 0), f("NAME", b'C', 6, 0)]);
    for (d, id, name) in [
        (false, &b"1"[..], &b"ONE"[..]),
        (true, b"2", b"TWO"),
        (false, b"3", b"THREE"),
        (false, b"4", b"FOUR"),
        (true, b"5", b"FIVE"),
    ] {
        del.push(d, &[id, name]);
    }
    dbc("deleted.dbc", &del, LITERALS);

    let mut eof = small_table();
    dbc("eof_marker.dbc", &eof, CODED2K);
    eof.eof_marker = false;
    dbc("no_eof_marker.dbc", &eof, CODED2K);

    let empty = Table::new(vec![f("ID", b'N', 3, 0), f("NAME", b'C', 4, 0)]);
    dbc("empty.dbc", &empty, RAW4K);

    let mut fb = Table::new(vec![
        f("ID", b'N', 3, 0),
        f("MEMO", b'M', 10, 0),
        f("ODD", b'N', 4, 9),
        f("NAME", b'C', 5, 0),
        f("D6", b'D', 6, 0),
    ]);
    fb.push(false, &[b"1", b"         1", b"12.3", b"ab", b"202401"]);
    fb.push(false, &[b"2", b"", b"", b"cd", b""]);
    dbc("fallback.dbc", &fb, RAW4K);

    let mut dup = Table::new(vec![f("A", b'C', 3, 0), f("A", b'N', 3, 0)]);
    dup.push(false, &[b"x", b"1"]);
    dup.push(false, &[b"y", b"2"]);
    dbc("dupnames.dbc", &dup, RAW4K);

    let mut la = Table::new(vec![
        f("UF", b'C', 2, 0),
        f("CODE", b'C', 5, 0),
        f("QTY", b'N', 5, 0),
        f("NAME", b'C', 10, 0),
    ]);
    la.push(false, &[b"AC", b"12345", b"100", b"ALPHA"]);
    la.push(false, &[b"AM", b"00001", b"-5", b"BETA"]);
    dbc("layout_a.dbc", &la, RAW4K);
    let mut lb = Table::new(vec![
        f("UF", b'C', 2, 0),
        f("CODE", b'C', 8, 0),
        f("EXTRA", b'C', 4, 0),
        f("QTY", b'N', 7, 2),
        f("NAME", b'C', 10, 0),
    ]);
    lb.push(false, &[b"PA", b"12345678", b"X1", b"1234.56", b"GAMMA"]);
    lb.push(false, &[b"RR", b"00000002", b"", b"-0.5", b"DELTA"]);
    dbc("layout_b.dbc", &lb, RAW4K);

    out.push(Fixture {
        name: "plain.dbf",
        bytes: types.to_dbf(),
        dcl: None,
    });

    // Corrupt variants.
    let good = small_table().to_dbc(&RAW4K);
    let h = 32 + 32 * 2 + 1;
    let mut bad_crc = good.clone();
    for b in &mut bad_crc[h..h + 4] {
        *b ^= 0xFF;
    }
    out.push(Fixture {
        name: "bad_crc.dbc",
        bytes: bad_crc,
        dcl: Some(RAW4K),
    });

    let lit = types.to_dbc(&LITERALS);
    let th = 32 + 32 * types.fields.len() + 1;
    let cut = th + 4 + (lit.len() - th - 4) / 2;
    out.push(Fixture {
        name: "truncated.dbc",
        bytes: lit[..cut].to_vec(),
        dcl: None,
    });

    let mut bh = small_table();
    bh.record_len = Some(bh.computed_record_len() as u16 + 1);
    out.push(Fixture {
        name: "bad_header.dbc",
        bytes: bh.to_dbc(&RAW4K),
        dcl: Some(RAW4K),
    });

    let mut bf = small_table();
    bf.push_raw(b"X  4D   ".to_vec());
    out.push(Fixture {
        name: "bad_flag.dbc",
        bytes: bf.to_dbc(&RAW4K),
        dcl: Some(RAW4K),
    });

    let mut tg = good.clone();
    tg.extend_from_slice(b"\0\0JUNK");
    out.push(Fixture {
        name: "trailing_garbage.dbc",
        bytes: tg,
        dcl: None,
    });

    let mut bn = small_table();
    bn.push(false, &[b"ABC", b"D"]);
    out.push(Fixture {
        name: "bad_number.dbc",
        bytes: bn.to_dbc(&RAW4K),
        dcl: Some(RAW4K),
    });
    out
}

fn dcl_label(o: &Option<DclOptions>) -> String {
    match o {
        None => "—".into(),
        Some(o) => format!(
            "{}, {} KiB{}",
            if o.coded_literals { "coded" } else { "raw" },
            1 << (o.dict_bits - 4),
            if o.matching { "" } else { ", literals only" }
        ),
    }
}

/// All output files: the fixtures, `README.md` and `SHA256SUMS`. When the
/// real file's bytes are given (it must match [`REAL_FILE_SHA256`]), it is
/// listed in both.
pub fn generate(real_file: Option<&[u8]>) -> Vec<(String, Vec<u8>)> {
    let fx = fixtures();
    let mut sums = String::new();
    let mut table = String::from(
        "| File | Bytes | DCL (literals, dictionary) | SHA-256 |\n|---|---:|---|---|\n",
    );
    for x in &fx {
        let h = sha256_hex(&x.bytes);
        sums.push_str(&format!("{h}  {}\n", x.name));
        table.push_str(&format!(
            "| `{}` | {} | {} | `{}…` |\n",
            x.name,
            x.bytes.len(),
            dcl_label(&x.dcl),
            &h[..16]
        ));
    }
    if let Some(real) = real_file {
        let h = sha256_hex(real);
        assert_eq!(h, REAL_FILE_SHA256, "{REAL_FILE} has the wrong hash");
        sums.push_str(&format!("{h}  {REAL_FILE}\n"));
        table.push_str(&format!(
            "| `{REAL_FILE}` | {} | raw, 4 KiB (DATASUS) | `{}…` |\n",
            real.len(),
            &h[..16]
        ));
    }
    let readme = README.replace("{{TABLE}}", &table);
    let mut files: Vec<(String, Vec<u8>)> = fx
        .into_iter()
        .map(|x| (x.name.to_string(), x.bytes))
        .collect();
    files.push(("README.md".into(), readme.into_bytes()));
    files.push(("SHA256SUMS".into(), sums.into_bytes()));
    files
}

const README: &str = r#"# Committed test fixtures

Small files for tests that must run in CI without the downloaded
`test/data/`. **Everything except `RDAC0801.dbc` is generated**; do not edit
by hand. Regenerate (deterministic, byte-identical) with:

```bash
cargo run -p read_dbc_format --example make_fixtures --features writer -- test/fixtures
```

The definitions live in `crates/format/tests/support/fixture_gen.rs`.
`cargo test -p read_dbc_format --features writer` fails if the committed
files differ from what it generates, and `tests/committed_fixtures.rs`
(always run) checks every file below against the values documented here.
Verify the hashes with `cd test/fixtures && sha256sum -c SHA256SUMS`.

All generated files are dBase III (version 0x03), header terminator 0x0D,
update date 2024-01-02, with a 0x1A end-of-file marker unless noted.
Numeric values are padded right-aligned and the rest left-aligned with
spaces. "DCL" gives the literal mode and dictionary size of the compressed
stream (DATASUS itself uses raw, 4 KiB); the generator's encoder is a
simple greedy matcher.

{{TABLE}}
## Valid files

### `types.dbc` (and `plain.dbf`)

LDID 0x00 (Latin-1). `plain.dbf` is the same table as an uncompressed
`.dbf` (identical bytes to the decompressed `types.dbc`). 8 rows, columns:

| Field | dBase | Column type |
|---|---|---|
| TXT | C(8) | VARCHAR |
| N10 | N(10,0) | BIGINT |
| N12 | N(12,2) | DECIMAL(12,2) |
| N20 | N(20,0) | DECIMAL(20,0) (128-bit) |
| N38 | N(38,4) | DECIMAL(38,4) (128-bit) |
| DT | D(8) | DATE |
| FLAG | L(1) | BOOLEAN |
| DTC | C(8) | VARCHAR; DATE with a type override |

Raw values (`␣` = space, `\0` = NUL; numbers as stored, before padding) and
the expected result:

| Row | TXT | N10 | N12 | N20 | N38 | DT | FLAG | DTC (as DATE) |
|---|---|---|---|---|---|---|---|---|
| 0 | `0012` → `0012` | `0` → 0 | `0.00` → 0.00 | `0` → 0 | `0.0000` → 0.0000 | `20230115` → 2023-01-15 | `T` → true | `20240229` → 2024-02-29 |
| 1 | `ABC␣␣␣` → `ABC` | `-42` → -42 | `-0.50` → -0.50 | `12345678901234567890` | `123456789012345678901234567890123.4567` | blank → NULL | `F` → false | `00000000` → NULL |
| 2 | blank → `""` | blank → NULL | blank → NULL | blank → NULL | blank → NULL | `00000000` → NULL | `Y` → true | blank → NULL |
| 3 | `A␣B` → `A B` | `9999999999` | `999999999.99` | `-9999999999999999999` | `-99999999999999999999999999999999.9999` | `20230230` (invalid) → NULL | `N` → false | `19991231` → 1999-12-31 |
| 4 | `␣␣LEAD` → `␣␣LEAD` | `-999999999` | `12.3␣␣␣␣␣␣` → 12.30 | `99999999999999999999` | `0.0001` | `19700101` → 1970-01-01 | `?` → NULL | `20000101` → 2000-01-01 |
| 5 | `Z` | `7` | `-1` → -1.00 | `-1` | `-1.5` → -1.5000 | `29991231` → 2999-12-31 | blank → NULL | `19700101` → 1970-01-01 |
| 6 | `NUL\0\0` → `NUL` | `+5` → 5 | `.5` → 0.50 | blank → NULL | blank → NULL | blank → NULL | `t` → true | `20240101` → 2024-01-01 |
| 7 | `12345678` | `-0` → 0 | `0` → 0.00 | `0` | `1` → 1.0000 | `20000229` → 2000-02-29 | `f` → false | `20231231` → 2023-12-31 |

Without an override DTC is plain text: row 1 is `00000000`, row 2 is `""`.

### Encodings: `cp850.dbc`, `latin1.dbc`, `cp1252.dbc`, `cp860.dbc`

Fields `ID` N(2,0) → BIGINT and `TXT` C(16) → VARCHAR. Rows 1–4 are
`ANÁLISE`, `CIRÚRGICA`, `SEM INFORMAÇÃO`, `Nº`, each encoded in the file's
code page; `cp1252.dbc` has a fifth row `€ 10` (byte 0x80).

| File | LDID | Encoding chosen by `auto` |
|---|---|---|
| `cp850.dbc` | 0x02 | CP850 |
| `latin1.dbc` | 0x00 | Latin-1 |
| `cp1252.dbc` | 0x03 | CP1252 |
| `cp860.dbc` | 0x24 | CP860 |

Read with the wrong encoding, the text is different but still valid (all
four are single-byte code pages); with `utf8` the non-ASCII rows fail.

### `deleted.dbc`

`ID` N(3,0), `NAME` C(6); 5 records, records 2 and 5 (the last one) are
deleted (`*`). Default read: IDs 1, 3, 4 (`ONE`, `THREE`, `FOUR`). With
deleted records included: IDs 1–5 with deleted flags
false, true, false, false, true. Literal-only DCL stream.

### `eof_marker.dbc` / `no_eof_marker.dbc`

`ID` N(3,0), `NAME` C(4); rows (1, `A`), (2, `B`), (3, `C`). Identical
except that only the first ends its data with 0x1A.

### `empty.dbc`

`ID` N(3,0) → BIGINT, `NAME` C(4) → VARCHAR; 0 records (the data is only
the 0x1A marker). Schema is known, no rows.

### `fallback.dbc`

| Field | dBase | Column type | Row 1 | Row 2 |
|---|---|---|---|---|
| ID | N(3,0) | BIGINT | 1 | 2 |
| MEMO | M(10) | VARCHAR (fallback: unsupported type) | `␣␣␣␣␣␣␣␣␣1` | `""` |
| ODD | N(4,9) | VARCHAR (fallback: decimals ≥ width) | `12.3` | `""` |
| NAME | C(5) | VARCHAR | `ab` | `cd` |
| D6 | D(6) | VARCHAR (fallback: date width ≠ 8) | `202401` | `""` |

### `dupnames.dbc`

Two fields both named `A`: C(3) → VARCHAR and N(3,0) → BIGINT; rows
(`x`, 1), (`y`, 2). A type override naming `A` is ambiguous (error).

### `layout_a.dbc` / `layout_b.dbc`

Same "family" with a schema change, for `union_by_name`:

| Field | layout_a | layout_b |
|---|---|---|
| UF | C(2) | C(2) |
| CODE | C(5) | C(8) (widened) |
| EXTRA | — | C(4) (added, before QTY) |
| QTY | N(5,0) → BIGINT | N(7,2) → DECIMAL(7,2) |
| NAME | C(10) | C(10) |

layout_a rows: (`AC`, `12345`, 100, `ALPHA`), (`AM`, `00001`, -5, `BETA`).
layout_b rows: (`PA`, `12345678`, `X1`, 1234.56, `GAMMA`),
(`RR`, `00000002`, `""`, -0.50, `DELTA`).

## Corrupt files (every one must make a full read fail)

| File | Defect | Expected error |
|---|---|---|
| `bad_crc.dbc` | 3-row table (`ID` N(3,0), `NAME` C(4)); stored CRC bytes inverted | CRC mismatch, raised with the last row (opening succeeds) |
| `truncated.dbc` | `types.dbc` (literal-only stream) cut in the middle of the stream | corrupt/truncated compressed data |
| `bad_header.dbc` | 3-row table; header record length is one more than the fields add up to | invalid header at open (record length mismatch) |
| `bad_flag.dbc` | 3 valid rows, then a 4th record with deletion flag `X` (valid CRC) | bad deletion flag in record 3 |
| `trailing_garbage.dbc` | the valid 3-row `.dbc` followed by 6 extra bytes | trailing bytes after the compressed stream |
| `bad_number.dbc` | 3 valid rows, then ID = `ABC` (valid CRC) | invalid value in record 3, field `ID` (only if `ID` is read) |

## Real file

`RDAC0801.dbc` is a public DATASUS file (SIH/RD, Acre, January 2008;
114,153 bytes; 2,087 records, 86 fields: 55 C and 31 N; LDID 0x02 = CP850;
raw literals, 4 KiB dictionary; ends with 0x1A; stored CRC 0x9b94505a).
Copied from the download listed in `scripts/fixtures/SHA256SUMS`.
"#;

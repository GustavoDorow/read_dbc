# Committed test fixtures

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

| File | Bytes | DCL (literals, dictionary) | SHA-256 |
|---|---:|---|---|
| `types.dbc` | 515 | raw, 4 KiB | `275cfa77e96bb4a3…` |
| `cp850.dbc` | 153 | coded, 1 KiB | `f2ba26c6050ebeec…` |
| `latin1.dbc` | 158 | raw, 2 KiB | `53b44efb83380c03…` |
| `cp1252.dbc` | 161 | coded, 4 KiB | `fc200306e37e600c…` |
| `cp860.dbc` | 158 | raw, 1 KiB | `4bd04671d0298833…` |
| `deleted.dbc` | 163 | raw, 4 KiB, literals only | `62d18ff24297cb87…` |
| `eof_marker.dbc` | 120 | coded, 2 KiB | `b513c019855102e2…` |
| `no_eof_marker.dbc` | 118 | coded, 2 KiB | `fdd02793122c5588…` |
| `empty.dbc` | 107 | raw, 4 KiB | `c07346f5bb948ed3…` |
| `fallback.dbc` | 234 | raw, 4 KiB | `dd197ccdb12a5c84…` |
| `dupnames.dbc` | 117 | raw, 4 KiB | `e7463093856990da…` |
| `layout_a.dbc` | 208 | raw, 4 KiB | `3c834a951405553e…` |
| `layout_b.dbc` | 251 | raw, 4 KiB | `aa951469f7a35d3c…` |
| `promote.dbc` | 176 | raw, 4 KiB | `2d99e9f35cfaf4c6…` |
| `reserved_names.dbc` | 126 | raw, 4 KiB | `82f72d8e43fc9c23…` |
| `promote.dbf` | 164 | — | `22eb04f61ebcbcad…` |
| `plain.dbf` | 1138 | — | `a308fa273b8daa02…` |
| `bad_crc.dbc` | 121 | raw, 4 KiB | `babc172c7f746c20…` |
| `truncated.dbc` | 773 | — | `d2ff2600f924fdee…` |
| `bad_header.dbc` | 121 | raw, 4 KiB | `69e2ecf3f647cad5…` |
| `bad_flag.dbc` | 127 | raw, 4 KiB | `8452b82afac25e82…` |
| `trailing_garbage.dbc` | 127 | — | `f11d9a9577260f34…` |
| `bad_number.dbc` | 127 | raw, 4 KiB | `da3d393e8703f825…` |
| `RDAC0801.dbc` | 114153 | raw, 4 KiB (DATASUS) | `fde119624f5ba840…` |

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
(`x`, 1), (`y`, 2). Use index 1 in the Rust reader or the renamed output
column `A_1` in SQL to override the second field.

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

### `promote.dbc` / `promote.dbf`

The glob `promote.db?` matches both formats. The columns `DT`, `QTY`, and
`FLAG` are DATE, BIGINT, and BOOLEAN in the DBC. Its rows are
(2024-02-29, -42, true), (1970-01-01, 7, false), and (NULL, NULL, NULL).
The DBF uses C(8), C(5), and N(3,0), with rows
(`20240229`, `00042`, -3) and (`""`, `""`, NULL). The SQL union promotes all
three columns to VARCHAR.

### `reserved_names.dbc`

One row with `FILENAME` = `original` and `_DELETED` = `active` (both C(8)).
SQL rejects `filename := true` and `include_deleted := true` on this file.

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

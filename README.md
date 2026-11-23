# read_dbc

A DuckDB extension written in Rust (MIT-licensed, targeting DuckDB v2.0.1) for querying DATASUS `.dbc` files directly in SQL. DATASUS distributes Brazil's public health microdata (SIH, SIA, CNES, SIM, SINASC) as `.dbc` files — dBase III tables whose record area is compressed with PKWare DCL "implode" and guarded by a little-endian IEEE CRC-32. `read_dbc` streams records straight from `.dbc` (or plain `.dbf`) into DuckDB vectors with bounded memory, projection pushdown, code-page decoding, exact decimals, and CRC-32 verification, without a separate convert-to-DBF or convert-to-Parquet step.

> **Status:** `read_dbc` is not yet published in the DuckDB Community Extensions catalog. Build locally and load the unsigned binary for now:

```shell
git clone --recurse-submodules https://github.com/GustavoDorow/read_dbc
cd read_dbc
make configure && make release
duckdb -unsigned
```

```sql
LOAD 'build/release/read_dbc.duckdb_extension';

-- Once published to the Community Extensions catalog:
-- INSTALL read_dbc FROM community;
-- LOAD read_dbc;

SELECT MUNIC_RES, count(*) AS admissions, sum(VAL_TOT) AS total_brl
FROM read_dbc('RDSC2501.dbc')
GROUP BY MUNIC_RES
ORDER BY total_brl DESC
LIMIT 10;
```

## Why

Working with DATASUS `.dbc` files normally starts with a conversion pipeline: decompress `.dbc` to an uncompressed `.dbf` (roughly 9–10x larger on disk) with `blast-dbf`, `pyreaddbc`, `datasus-dbc`, or TabWin, then parse the `.dbf` in Python (`dbfread`, `pandas`, `PySUS`) or convert to Parquet before querying. That adds temporary disk writes, upfront full-file latency before a single row is readable, and gigabytes of Python/pandas RAM on large state-month files. Existing open-source `.dbc` readers also skip the stored CRC-32, and reading SIH files with a Latin-1 default silently corrupts CP850 accented text.

`read_dbc` decodes the DCL stream on the fly using fixed buffers (32 KiB output ring, 32 KiB input buffer, one record buffer), parses only projected columns, preserves exact decimal precision (`BIGINT` and `DECIMAL`), respects the header's code page, and verifies the CRC-32 checksum.

## Usage

```sql
read_dbc(
    files,
    encoding := 'auto',
    all_varchar := false,
    include_deleted := false,
    verify_crc := true,
    types := NULL,
    union_by_name := false,
    filename := false
)
```

Named parameters accept either `:=` or `=`.

| Option | Default | Meaning |
|---|---|---|
| `files` | *(required)* | File path, glob pattern, or `VARCHAR[]` list of paths/globs. Globs expand through DuckDB's filesystem (matches sorted, list order preserved; an empty list or unmatched pattern is an error). Paths ending in `.dbf` (case-insensitive) are read as plain uncompressed DBF. |
| `encoding` | `'auto'` | `'auto'` selects the code page from the DBF header's language-driver byte (LDID, byte 29; unknown falls back to Latin-1). Explicit values: `'latin1'` / `'iso-8859-1'`, `'cp437'`, `'cp850'`, `'cp860'`, `'cp1252'` / `'windows-1252'`, `'utf8'` (strict). |
| `all_varchar` | `false` | Read every field as `VARCHAR` (trailing spaces and NULs stripped, leading bytes kept). `types` overrides still apply on top. |
| `include_deleted` | `false` | Include dBase deleted records (`*`) and append a `_deleted BOOLEAN` column. Errors if the file already has a `_deleted` column. |
| `verify_crc` | `true` | Verify the little-endian IEEE CRC-32 stored in `.dbc` files. Set to `false` for third-party `.dbc` files written with a zero placeholder checksum (such as those created by R `read.dbc`). |
| `types` | `NULL` | `STRUCT` or `MAP` mapping output column name (case-insensitive) to `VARCHAR` / `TEXT` / `STRING`, `BIGINT` / `INT64` / `LONG`, `DATE`, `BOOLEAN` / `BOOL`, or `DECIMAL(w,s)` / `NUMERIC(w,s)`. Allows: any field → `VARCHAR`, `C` → `DATE` (strict `YYYYMMDD`), `C` → `BIGINT` (strict integer), or any field → its natural type. |
| `union_by_name` | `false` | Combine files with differing schemas (see below). |
| `filename` | `false` | Append a `filename VARCHAR` column holding the expanded path. Errors if the file already has a `filename` column. |

**Replacement scan.** Any `FROM` path or glob ending in `.dbc` (case-insensitive) invokes `read_dbc` automatically. Plain `.dbf` paths are not intercepted so they do not conflict with other DBF extensions.

**Duplicate column names.** If a DBF header repeats a field name (case-insensitively), later occurrences are renamed `NAME_1`, `NAME_2`, ... (matching DuckDB's `read_csv`). Use the renamed output names in `types`, e.g. `types := {'A_1': 'VARCHAR'}`.

**Multi-file schemas.** Without `union_by_name`, every file must have the same column names (case-insensitive) and types in the same order (field widths may differ). With `union_by_name := true`, columns are matched case-insensitively in first-seen order and spelling, missing columns are `NULL`, `BIGINT` and `DECIMAL` promote to the smallest exact `DECIMAL` holding both (`BIGINT` counts as 19 integer digits; an error if more than 38 digits are required, solvable with `types` or `all_varchar`), and any other type mix promotes to `VARCHAR` rendered like a SQL cast.

### Examples

```sql
-- Replacement scan (.dbc only; globs supported)
SELECT count(*) FROM 'test/fixtures/RDAC0801.dbc';

-- Convert DATASUS C(8) text dates (YYYYMMDD) directly to DATE
SELECT min(DT_INTER), max(DT_INTER)
FROM read_dbc('test/fixtures/RDAC0801.dbc', types := {'DT_INTER': 'DATE'});

-- Aggregate SIH hospital admissions by municipality (projection parses only 2 of 113 columns)
SELECT MUNIC_RES, count(*) AS admissions, sum(VAL_TOT) AS total_brl
FROM read_dbc('RDSC2501.dbc')
GROUP BY MUNIC_RES
ORDER BY total_brl DESC
LIMIT 10;

-- Union across years where SIH added columns (86 cols in 2008, 113 in 2015-2025, 114 in 2026)
SELECT filename, count(*) AS admissions, sum(VAL_TOT) AS total_brl
FROM read_dbc('data/RD*.dbc', union_by_name := true, filename := true)
GROUP BY filename
ORDER BY filename;

-- Explicit file list + MAP syntax for types
SELECT DT_INTER, N_AIH, VAL_TOT
FROM read_dbc(
    ['RDAC0801.dbc', 'data/RD24*.dbc'],
    union_by_name := true,
    types := MAP {'DT_INTER': 'DATE', 'N_AIH': 'VARCHAR'}
)
LIMIT 5;
```

## Types and encodings

Column types come strictly from the DBF header — no data sampling is performed.

| dBase field | DuckDB type | Parsing rules |
|---|---|---|
| `C` | `VARCHAR` | Trailing spaces and NULs stripped, leading bytes kept; blank → `""` (not `NULL`). |
| `N` / `F` (decimals = 0, width ≤ 18) | `BIGINT` | Blank (spaces or NULs) → `NULL`; anything other than a plain signed decimal integer is an error. |
| `N` / `F` (width 19–38, or 0 < decimals < width) | `DECIMAL(width, decimals)` | Exact integer-scaled parsing (`i64` up to width 18, `i128` for 19–38; no floating point); blank → `NULL`. |
| `D` (width 8) | `DATE` | `YYYYMMDD`; blank, `00000000`, or invalid calendar date → `NULL`. |
| `L` | `BOOLEAN` | `T`/`t`/`Y`/`y` → `true`, `F`/`f`/`N`/`n` → `false`, `?` or blank → `NULL`; anything else is an error. |
| Unsupported / fallback (`M`, `B`, `G`, `I`, `T`, `@`, `Y`, VFP `0`, unknown type bytes; `D` with width ≠ 8; `N`/`F` wider than 38 or with decimals ≥ width) | `VARCHAR` | Falls back to `C` rules using the file's encoding; never fails open. |

**Blank vs `NULL` and strictness.** Blank `C` (and fallback `VARCHAR`) fields produce `""`, whereas blank `N`, `F`, `D`, and `L` fields produce `NULL`. A `C` → `DATE` override via `types` is stricter than a native `D` field: blank and `00000000` yield `NULL`, while any other non-calendar string raises an error. Compared to `dbfread` 2.0.7, `read_dbc` parses exact decimals instead of `float`, rejects `*` overflow padding, commas, scientific notation, excess fractional digits, and non-space ASCII whitespace in numerics, treats NUL-only numerics and invalid `D` dates as `NULL`, and decodes LDID `0x00` as Latin-1 rather than ASCII.

### Character encodings (`encoding := 'auto'`)

| Header byte 29 (LDID) | Encoding selected |
|---|---|
| `0x01`, `0x09`, `0x0B`, `0x0D`, `0x0F`, `0x11`, `0x15`, `0x18`, `0x19`, `0x1B` | `cp437` |
| `0x02`, `0x0A`, `0x0E`, `0x10`, `0x12`, `0x14`, `0x16`, `0x1A`, `0x1D`, `0x25`, `0x37` | `cp850` |
| `0x24` | `cp860` (Portuguese MS-DOS) |
| `0x03`, `0x57`, `0x58`, `0x59` | `cp1252` |
| Anything else (including `0x00`) | `latin1` (never fails; preserves every byte losslessly) |

**SIH CP850 finding.** Every sampled DATASUS SIH file declares LDID `0x02` (`cp850`). Reading SIH files as Latin-1 — the common default in Python and R pipelines — garbles Portuguese accented text. In `RDSC2501.dbc`, 55 `AUD_JUST` values carry CP850 high bytes (`0x87` → `ç`, `0xA0` → `á`, `0xA1` → `í`, `0xA3` → `ú`), such as `DESSCRIçaO CIRúRGICA COMPATíVEL COM O PROCEDIMENTO` and `ANáLISE DO PRONTUáRIO`; decoded as Latin-1, those bytes turn into C1 control characters (`0x87`), non-breaking spaces (`0xA0`), `¡`, and `£`. Likewise, uppercase CP850 words in `test/fixtures/cp850.dbc` (`ANÁLISE`, `CIRÚRGICA`, `Nº`) read as `ANµLISE`, `CIRéRGICA`, and `N§` under Latin-1. Other DATASUS families declare different LDIDs (e.g. `0x00` in CNES/ST, SIM/DO, and SINASC/DN; `0x03` in SIA/PA; `0x58` in CNES/LT), which `encoding := 'auto'` resolves per file.

## Integrity guarantees

Format research documented in [docs/research/dbc-format.md](docs/research/dbc-format.md) established that the 4 bytes at offset `H` (between the `H`-byte DBF header and the PKWare DCL stream at `H+4`) are a little-endian IEEE CRC-32 (`CRC-32/ISO-HDLC`) over the unmodified stored header `[0, H)` followed by every decompressed byte through DCL EOS, including any `0x1A` EOF byte. This matched all 11 sampled DATASUS FTP downloads (10 distinct contents across five families, 1994–2024), both upstream `blast-dbf` fixtures, and static inspection of DATASUS's official TabWin 4.15 `dbf2dbc.exe` and `IMPBORL.DLL`.

`read_dbc` validates the file at every layer and names the offending file in every error:

- **Header errors (raised at bind, before scanning):** truncated header, unsupported dBase version byte, terminator at `H-1` other than `0x0D` or `0x00`, declared record length disagreeing with the sum of field widths, zero fields, or zero-width fields.
- **Compressed stream errors:** any PKWare DCL / `blast` error (invalid literal mode, invalid dictionary code, back-reference past start of output, unexpected EOF before EOS) and any trailing bytes after DCL EOS.
- **Record framing errors:** deletion flag other than `' '` (`0x20`) or `'*'` (`0x2A`), fewer than `N` records, or excess decompressed bytes beyond `N` records and an optional single `0x1A` marker.
- **CRC-32 mismatch:** checked at end-of-stream *before* the final batch of records is handed to DuckDB (when `verify_crc := true`).
- **Value errors:** malformed `N`/`F`, `L`, strict `types` overrides (`C` → `DATE` or `BIGINT`), or invalid UTF-8 under `encoding := 'utf8'`, reporting the file, 0-based physical record index, field name, and logical DBF byte offset. Unprojected fields are not parsed.

**Streaming caveat (`LIMIT` / early stop).** Because `read_dbc` streams batches without buffering the whole file, batches prior to the last one are returned to DuckDB before the end of the stream (and therefore before the CRC-32) is reached. A query that stops early — such as `SELECT * FROM read_dbc(...) LIMIT 10` — can return rows from a file whose CRC fails later. A full scan always checks the CRC before emitting the final batch.

## Correctness

### Parity with `dbfread` 2.0.7

Checked against `dbfread` 2.0.7 (`encoding='iso-8859-1'`) using the release build on DuckDB 2.0.1 ([scripts/parity/RESULTS.md](scripts/parity/RESULTS.md)).

**DATASUS SIH/RD (5 files, 7,637,058 values compared, 0 mismatches):**

| File | Rows | Cols | Values compared | Mismatches |
|---|---:|---:|---:|---:|
| `RDAC0801.dbc` (2008-01, AC) | 2,087 | 86 | 179,482 | 0 |
| `RDRR1501.dbc` (2015-01, RR) | 2,759 | 113 | 311,767 | 0 |
| `RDAP2401.dbc` (2024-01, AP) | 4,790 | 113 | 541,270 | 0 |
| `RDSC2501.dbc` (2025-01, SC) | 53,631 | 113 | 6,060,303 | 0 |
| `RDAC2601.dbc` (2026-01, AC) | 4,774 | 114 | 544,236 | 0 |
| `union_by_name` of all 5 | 68,041 | 114 | rows = sum, values = per-file reads | 0 |

**Other DATASUS families (5 files, 1,664,019 values compared, 0 mismatches):**

| File | Family | Rows | Cols | Values compared | Mismatches | LDID | Auto encoding |
|---|---|---:|---:|---:|---:|---|---|
| `STAC2401.dbc` | CNES/ST | 1,374 | 208 | 285,792 | 0 | `0x00` | `latin1` |
| `LTAP2401.dbc` | CNES/LT | 120 | 28 | 3,360 | 0 | `0x58` | `cp1252` |
| `DORR1996.dbc` | SIM/DO | 1,029 | 40 | 41,160 | 0 | `0x00` | `latin1` |
| `DNAC2024.dbc` | SINASC/DN | 13,101 | 61 | 799,161 | 0 | `0x00` | `latin1` |
| `PAAP0801.dbc` | SIA/PA | 9,899 | 54 | 534,546 | 0 | `0x03` | `cp1252` |

`STAC2401.dbc` uses a `0x00` header terminator, which `read_dbc` accepts natively (the scratch DBF had to normalize that byte to `0x0D` for `dbfread`).

### Fuzzing and differential testing

- **Fuzzing ([docs/fuzzing.md](docs/fuzzing.md)):** Two `cargo-fuzz` targets under `crates/format/fuzz/` ran under AddressSanitizer (`rustc 1.102.0-nightly`, AMD Ryzen 5 5600): `dbc_reader` (~19 min total, 180,000+ executions in the final 2-min run, 1,232 edges / 4,399 features) and `dcl` (~27 min total, 456,767 executions in the final 10-min run, 182 edges / 604 features). Zero crashes, timeouts, or library OOMs (an initial OOM in the `dcl` harness was caused by calling the test helper `decompress_all` into an unbounded `Vec`; switching the harness to stream like the extension resolved it).
- **Differential and mutation testing:** As recorded in [docs/fuzzing.md](docs/fuzzing.md), earlier independent runs checked the Rust DCL decoder differentially against Mark Adler's reference `blast.c` (~143,000 generated streams, 0 mismatches) and ran ~3.2 million CRC-preserving mutations of real files (0 panics).

## Performance

Measured on AMD Ryzen 5 5600 (6C/12T), 31.2 GiB RAM, NVMe SSD (btrfs; temp files on disk, not tmpfs), Omarchy Linux (`x86_64`), DuckDB 2.0.1 (`1.5.6` for PySUS), 5 interleaved repetitions per workflow in fresh OS processes with warm page cache. Values below are **medians**; full methodology, minimums, and raw CSV/JSON data are in [docs/benchmarks.md](docs/benchmarks.md).

### Large file: `RDSP2501.dbc` (238,144 rows × 113 cols; 17.9 MiB `.dbc` / 159.4 MiB `.dbf`)

| Workflow | First row | Total | Peak RSS | Disk written |
|---|---:|---:|---:|---:|
| `read_dbc` stream (`SELECT *`, 2,048-row Arrow batches) | 98.7 ms | 1.03 s | 168.1 MiB | 0 B |
| `read_dbc` materialise (`CREATE TABLE AS SELECT *`) | 1.30 s | 1.30 s | 361.4 MiB | 0 B |
| `read_dbc` `count(*)` | 279.3 ms | 279.3 ms | 86.1 MiB | 0 B |
| `read_dbc` `GROUP BY` (`MUNIC_RES, sum(VAL_TOT)`) | 338.6 ms | 338.6 ms | 114.1 MiB | 0 B |
| `datasus` CLI → Parquet → DuckDB stream (cold) | 1.46 s | 1.70 s | 310.9 MiB | 9.1 MiB |
| `datasus` CLI → Parquet → DuckDB table (cold) | 1.75 s | 1.75 s | 348.6 MiB | 9.1 MiB |
| `datasus` CLI → Parquet → DuckDB `GROUP BY` (cold) | 1.40 s | 1.40 s | 301.8 MiB | 9.1 MiB |
| Pre-converted Parquet → DuckDB `GROUP BY` (warm) | 25.6 ms | 25.6 ms | 112.2 MiB | 0 B |
| `PySUS` 2.12.2 (`DBC.load()` → pandas → DuckDB) | 16.73 s | 16.73 s | 1,356.8 MiB | 172.5 MiB |
| `pyreaddbc` → `dbfread` → pandas → DuckDB | 20.72 s | 20.72 s | 3,803.3 MiB | 159.4 MiB |
| `datasus-dbc` → `dbfread` → Arrow → DuckDB | 31.13 s | 31.13 s | 3,387.0 MiB | 159.4 MiB |

### Scaling across file sizes (medians)

| File | Workflow | First row | Total | Peak RSS | Disk written |
|---|---|---:|---:|---:|---:|
| `RDAC2601.dbc` (4,774 × 114, 337.2 KiB) | `read_dbc` stream / `GROUP BY` | 82.0 / 39.0 ms | 83.4 / 39.0 ms | 139.5 / 107.5 MiB | 0 B |
| `RDAC2601.dbc` | `datasus` → Parquet `GROUP BY` (cold / warm) | 92.4 / 33.9 ms | 92.4 / 33.9 ms | 117.1 / 125.1 MiB | 215.8 KiB / 0 B |
| `RDAC2601.dbc` | `pyreaddbc` → `dbfread` → pandas | 717.0 ms | 717.0 ms | 213.8 MiB | 3.2 MiB |
| `RDSC2501.dbc` (53,631 × 113, 4.0 MiB) | `read_dbc` stream / `GROUP BY` | 137.3 / 104.3 ms | 443.4 / 104.3 ms | 159.2 / 113.7 MiB | 0 B |
| `RDSC2501.dbc` | `datasus` → Parquet `GROUP BY` (cold / warm) | 577.2 / 33.5 ms | 577.2 / 33.5 ms | 125.7 / 115.3 MiB | 2.2 MiB / 0 B |
| `RDSC2501.dbc` | `pyreaddbc` → `dbfread` → pandas | 6.67 s | 6.67 s | 985.7 MiB | 35.9 MiB |

**Where `read_dbc` loses:**

- **Repeated queries on static files:** A `.dbc` file is a single sequential PKWare-DCL stream with no columnar layout or row-group index, so every query decompresses the stream from the start. On `RDSP2501.dbc`, `GROUP BY` on pre-converted Parquet takes **25.6 ms** vs **338.6 ms** for `read_dbc` (~13x faster). If you query the same files repeatedly, convert once with `COPY (SELECT * FROM read_dbc(...)) TO 'out.parquet'`.
- **Full materialisation peak RSS on large files:** `CREATE TABLE AS SELECT *` on `RDSP2501.dbc` peaked at 361.4 MiB vs 348.6 MiB for `datasus` → Parquet (~3.7% higher).
- **Tiny files:** On `RDAC2601.dbc` (337.2 KiB), DuckDB startup (~35 ms) dominates and `read_dbc` full materialisation is only ~1.3x faster than `datasus` CLI + Parquet (100.2 ms vs 128.2 ms).

## Limitations

- **`count(*)` decodes column 0:** DuckDB v2.0.1's C API passes a projection of the first column for `count(*)`, so `count(*)` still decodes column 0 (and fails if it contains an invalid value); it still scans every file to the end and verifies the CRC.
- **No mid-chunk interrupt checks:** DuckDB v2.0.1's C v2 `Context` exposes no interrupt-status getter, so query cancellation waits until the current chunk call returns to DuckDB (including across long runs of deleted records).
- **Custom multi-file layer:** DuckDB v2.0.1's built-in multi-file wrapper drops custom `NAMED_ONLY` parameters from C v2 extensions ([docs/research/upstream-multifile-kwargs.md](docs/research/upstream-multifile-kwargs.md)), so `read_dbc` implements globs, file lists, `union_by_name`, and `filename` itself. As a result, built-in multi-file features such as Hive partitioning, schema remapping, `file_row_number`, and custom `filename` column names are not supported.
- **Exact DuckDB version coupling:** The extension builds against DuckDB's unstable C API v2 (`USE_UNSTABLE_C_API=1` / `C_STRUCT_UNSTABLE`), so binaries load only in the exact `TARGET_DUCKDB_VERSION` (`v2.0.1`).
- **Remote reads (`httpfs`) untested:** Files are opened through DuckDB's `FileSystem` VFS (`VfsRead`), so `httpfs`/S3 paths should route through DuckDB, but only local files have been tested.
- **Plain `.dbf` handling is strict:** Uncompressed `.dbf` files (no CRC) are checked strictly for exact record count and reject trailing bytes beyond a single optional `0x1A` EOF marker.

## How it works

The Cargo workspace is split into four crates:

- `crates/format` (`read_dbc_format`): pure-Rust, zero-runtime-dependency streaming `.dbc`/`.dbf` reader (header parser, PKWare DCL decoder, IEEE CRC-32, code pages, exact value parsers, batch sink). See [crates/format/README.md](crates/format/README.md).
- `crates/core` (`read_dbc_core`): DuckDB C API v2 table function (`read_dbc`), `read_dbc_version()`, `FROM '*.dbc'` replacement scan, multi-file schema unification, DuckDB VFS adapter, and panic-to-error guard.
- `crates/native` (`read_dbc`) and `crates/wasm` (`read_dbc_wasm`): thin entrypoint wrappers registering `read_dbc_core` via `duckdb-neo`.

**Streaming DCL decoder.** `DclReader` is a pull-based `std::io::Read` translation of Mark Adler's `blast.c`. It decodes the fixed Huffman tables via shared lookup tables and uses a 32 KiB output ring buffer (serving as both output staging and the 4 KiB maximum match history window) plus a 32 KiB input buffer. Memory usage does not scale with file size, and the header's record count is never used to preallocate buffers.

**Bind and parallel execution.** At bind time, `read_dbc` reads only the `H`-byte DBF header of each matched file (no decompression) to resolve types, apply `types` overrides, and validate or unify schemas. During execution, each file is claimed as one batch (`claim_batch`) and decoded front to back by a single worker while multiple files run in parallel across DuckDB threads. Reporting the file index as the batch index lets DuckDB preserve file order and record order whenever `preserve_insertion_order` is enabled.

## Building and testing

Prerequisites: Rust (stable), Python 3 with `venv`, Make, and Git. Downloaded fixtures, parity checks, and benchmarks also use `curl` (with FTP support) and `uv`; fuzzing requires nightly Rust and `cargo-fuzz`.

```shell
# Build and run SQLLogicTests (test/sql/*.test on committed fixtures in test/fixtures/)
make configure
make debug           # build/debug/read_dbc.duckdb_extension
make test            # runs test/sql/*.test against the debug build
make release         # optimised build (LTO, stripped)
make test_release

# Pure-Rust format crate tests
cargo test -p read_dbc_format
cargo test -p read_dbc_format --features writer   # also checks test/fixtures/ are up to date

# Regenerate deterministic synthetic fixtures (see test/fixtures/README.md)
cargo run -p read_dbc_format --example make_fixtures --features writer -- test/fixtures
```

**Downloaded DATASUS fixtures and parity:**

```shell
scripts/fixtures/build-reference.sh                                    # fetches SIH files + builds dbfread/raw oracles
uv run --script scripts/parity/parity.py --build build/release         # SIH parity (7.6M values)
scripts/fixtures/fetch-families.sh                                     # fetches CNES, SIM, SINASC, SIA samples
uv run --script scripts/parity/family_parity.py --build build/release  # cross-family parity (1.66M values)
```

Set `READ_DBC_REQUIRE_FIXTURES=1` when running `cargo test -p read_dbc_format` to fail rather than skip if downloaded fixtures under `test/data/` are absent.

**Benchmarks and fuzzing:**

```shell
uv run --script scripts/bench/bench.py   # full benchmark suite (see scripts/bench/README.md)

# Fuzzing (see docs/fuzzing.md)
cd crates/format
cargo +nightly fuzz run dbc_reader fuzz/corpus/dbc_reader -- -max_total_time=900 -rss_limit_mb=512 -timeout=5
cargo +nightly fuzz run dcl fuzz/corpus/dcl -- -max_total_time=600 -rss_limit_mb=512 -timeout=5
```

## Credits and license

- **PKWare DCL decoder:** `crates/format/src/dcl.rs` is an altered Rust translation of Mark Adler's `blast.c` (zlib `contrib/blast`, zlib license; original notice retained in the source file).
- **Extension template:** Scaffolded from [duckdb/extension-template-rs](https://github.com/duckdb/extension-template-rs) at commit `92050f5`.
- **Test oracles and benchmark baselines:** `datasus-core`, `datasus-dbc`, `blast-dbf`, and `dbfread` (plus `PySUS`, `pyreaddbc`, and `datasus-cli` in the benchmark suite).
- **Data source:** Public health microdata fixtures from DATASUS (Brazilian Ministry of Health).

Licensed under the MIT License; see [LICENSE](LICENSE).

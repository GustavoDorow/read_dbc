# read_dbc_format

A streaming, dependency-free Rust reader for DATASUS `.dbc` files (a dBase III
header, a CRC-32, and a PKWare DCL "implode"-compressed record area), and for
plain `.dbf` files. It has no DuckDB code; the `read_dbc_core` crate wires it
into a table function.

## Design

- **Streaming, bounded memory.** Input is any `std::io::Read` (no `Seek`).
  The DCL decoder (`DclReader`, an adaptation of Mark Adler's `blast.c`) keeps
  a 32 KiB output ring and a 32 KiB input buffer. The table reader holds the
  header, one record, and a reusable text buffer. Memory does not depend on
  the file size, and the header's record count is never used to size an
  allocation.
- **Batches with projection.** `DbcReader::read_batch(n, &mut sink)` decodes up
  to `n` rows into a caller-provided `BatchSink`, parsing only the projected
  fields. Text arrives as a borrowed `&str`. Each call examines at most
  `max(n, 2048)` physical records, so a batch can be short or empty (runs of
  deleted records, or `n == 0`) before the end: loop until
  `reader.is_finished()`, not until a batch returns 0.
- **Schema from the header alone.** `Header::read` reads only the header
  bytes and `resolve_schema(&header, &options)` returns the columns, using the
  same rules as `DbcReader`.
- **Types from the header only** (table below). A field's type never makes
  opening fail: anything without a typed mapping is read as VARCHAR and
  `Column::fallback` says why.
- **Encodings.** Auto uses the LDID byte (table below); unknown LDIDs fall
  back to Latin-1. Latin-1, CP437, CP850, CP860, CP1252 and UTF-8 (strict)
  can also be set explicitly.
- **Strict integrity.** It checks the header layout, every blast error, the
  deletion flags, exactly `N` records followed by an optional `0x1A`, the DCL
  end code with no trailing bytes, and the CRC-32. Two deliberate choices:
  the unused padding bits after the DCL end code (in its last byte) are not
  checked, since they carry no data and encoders leave arbitrary values; and
  plain `.dbf` files, which have no CRC, are just as strict about trailing
  bytes (only one `0x1A` may follow the records). The checks at the end of
  the stream run *before the last record is handed out*. The call that
  consumes the final record is never `Ok` for a corrupt file, and
  `is_finished()` means the file was read to the end and verified.
- **Streaming caveat.** Batches before the last one are returned before the
  CRC is known. A consumer that stops early (a `LIMIT` query, a streaming
  pipeline) can receive rows from a corrupt file; a full scan always verifies.

## Type mapping

| dBase field | Column type | Values |
|---|---|---|
| `C` | VARCHAR | trailing spaces/NULs stripped, leading bytes kept; blank → `""` |
| `N`/`F`, decimals 0, width ≤ 18 | BIGINT | blank → NULL; anything but a plain decimal number is an error |
| `N`/`F`, width 19–38 or decimals > 0 (decimals < width) | DECIMAL(width, decimals) | exact (no floating point); blank → NULL; i64 up to width 18, i128 above |
| `D`, width 8 | DATE | `YYYYMMDD`; blank, `00000000` or invalid → NULL |
| `L` | BOOLEAN | `TtYy` → true, `FfNn` → false, `?`/blank → NULL; anything else is an error |
| **Fallback**: any other type (`M`, `B`, `G`, `I`, `T`, `@`, `Y`, VFP `0`, unknown bytes), `D` with width ≠ 8, `N`/`F` wider than 38, `N`/`F` with decimals ≥ width (e.g. `N(4,9)`) | VARCHAR | `C` rules, decoded with the file encoding; `Column::fallback = Some(FallbackReason::…)` (`UnsupportedType`, `DateWidth`, `NumericTooWide`, `DecimalsNotBelowWidth`) |

A zero-width field and header structure problems are still errors at open.
`fallback` is `None` when the caller chose VARCHAR (`all_varchar` or a type
override). Per-field overrides (`ReadOptions::types`): any field → VARCHAR;
`C` → DATE (strict `YYYYMMDD`; blank or `00000000` → NULL, any other invalid
date is an error); `C` → BIGINT (strict integer); any field → its natural
type.

Select fields by their unique, case-insensitive header name (for example,
`"COL".into()`) or their zero-based position (`TypeOverrideField::Index(1)`).
Use the position when multiple fields share a header name.

## Encodings (`Encoding::Auto`)

| LDID (header byte 29) | Encoding |
|---|---|
| 0x01, 0x09, 0x0B, 0x0D, 0x0F, 0x11, 0x15, 0x18, 0x19, 0x1B | CP437 |
| 0x02, 0x0A, 0x0E, 0x10, 0x12, 0x14, 0x16, 0x1A, 0x1D, 0x25, 0x37 | CP850 |
| 0x24 | CP860 (Portuguese MS-DOS) |
| 0x03, 0x57, 0x58, 0x59 | CP1252 |
| **anything else**, including 0x00 and LDIDs of code pages not implemented here | **Latin-1** (never fails, keeps every byte recoverable) |

The LDID table is dbfread 2.0.7's `codepages.py` restricted to the
implemented code pages. The code-page tables were generated from, and
checked entry by entry against, Python's codecs. Names accepted by
`Encoding::from_name` include `cp860`/`ibm860`, `cp850`/`ibm850`,
`latin1`/`iso-8859-1`, `cp1252`/`windows-1252` and `utf8`.

## Usage

```rust
use read_dbc_format::{ColumnBuffers, DbcReader, ReadOptions};

let file = std::fs::File::open("RDAC0801.dbc")?;
let mut reader = DbcReader::new(file, ReadOptions::default())?;
reader.set_projection(&[0, 3])?;                 // decode only two columns
let mut buffers = ColumnBuffers::new(2);         // or your own BatchSink
while !reader.is_finished() {                    // CRC verified at the end
    reader.read_batch(2048, &mut buffers)?;      // may return 0 mid-file
}
```

## Tests and tools

```bash
cargo test -p read_dbc_format            # unit + synthetic + fixture parity
cargo run --release -p read_dbc_format --example bench -- read test/data/RDSC2501.dbc
```

**Committed fixtures** (`test/fixtures/`, always tested, see its README):
small synthetic `.dbc`/`.dbf` files covering every type, the encodings,
deleted records, the EOF marker, empty tables, fallback columns, duplicate
names, a schema change, and six corrupt variants, plus the real
`RDAC0801.dbc`. `tests/committed_fixtures.rs` checks their documented
values and errors. They are generated by a test-only writer behind the
non-default `writer` feature (`read_dbc_format::writer`: a minimal DBF/DBC
writer with a greedy DCL encoder supporting both literal modes and all
dictionary sizes; not for production use):

```bash
cargo run -p read_dbc_format --example make_fixtures --features writer -- test/fixtures
cargo test -p read_dbc_format --features writer   # also fails if the committed files are stale
```

The normal build has no dependencies and does not include the writer.

**Downloaded fixtures**: tests compare against `test/data/*.dbc` and
`test/reference/*` and are skipped with a `SKIP:` message if those files are missing; set
`READ_DBC_REQUIRE_FIXTURES=1` (as CI should) to make a missing fixture fail
the test instead (create them with
`scripts/fixtures/build-reference.sh`). `datasus-core` is a dev-dependency,
used only as a parity oracle and a benchmark baseline. cargo-fuzz targets live
in `fuzz/` (excluded from the workspace): `dcl` (raw DCL streams) and
`dbc_reader` (whole files with random options, type overrides and raw-record
reads; one mode treats the input as a plain DBF and wraps it into a `.dbc`
with a correct CRC, so the value parsers are reached).

## Differences from dbfread 2.0.7

- `N`/`F`: dbfread strips `*` overflow padding, accepts `,` and exponents, and
  returns floats. This crate parses exact decimals and treats `*`, `,`,
  exponents, or more fractional digits than declared as an error.
- `N`/`F` made only of NUL bytes: dbfread raises; this crate returns NULL
  (like blank).
- `N`/`F` with tabs or other non-space ASCII whitespace around or inside the
  number: Python's `int()`/`float()` (and so dbfread) accept it; this crate
  treats it as an error. Only spaces are padding.
- Unsupported field types: dbfread raises (or needs a custom parser); this
  crate reads them as VARCHAR (see the fallback row above).
- LDID 0x00: dbfread decodes as ASCII (failing on any byte ≥ 0x80); this
  crate uses Latin-1.
- `D`: dbfread raises on an invalid non-blank date. This crate returns NULL
  (the dBase convention). That includes values with embedded spaces such as
  `"2024 1 1"`, which dbfread parses as a date.

## License

MIT. `src/dcl.rs` is an altered version of `blast.c`, which is under the zlib
license; that license notice is kept in the file.

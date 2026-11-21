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
- **Types from the header only.** `C` becomes VARCHAR (trailing spaces and
  NULs stripped, blank → `""`). `N`/`F` become BIGINT or an exact
  DECIMAL(width, scale), with blank → NULL. `D` becomes DATE, with blank or
  invalid → NULL. `L` becomes BOOLEAN. `all_varchar` and per-field overrides
  are also available (`C` → DATE/BIGINT, any field → VARCHAR).
- **Encodings.** Auto uses the LDID byte (CP437/CP850/CP1252, otherwise
  Latin-1). Latin-1, CP850, CP1252 and UTF-8 (strict) can also be set
  explicitly.
- **Strict integrity.** It checks the header layout, every blast error, the
  deletion flags, exactly `N` records followed by an optional `0x1A`, the DCL
  end code with no trailing bytes, and the CRC-32. The checks at the end of
  the stream run *before the last record is handed out*. The call that
  consumes the final record is never `Ok` for a corrupt file, and
  `is_finished()` means the file was read to the end and verified.
- **Streaming caveat.** Batches before the last one are returned before the
  CRC is known. A consumer that stops early (a `LIMIT` query, a streaming
  pipeline) can receive rows from a corrupt file; a full scan always verifies.

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

Fixture tests compare against `test/data/*.dbc` and `test/reference/*` and are
skipped with a `SKIP:` message if those files are missing; set
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
- `D`: dbfread raises on an invalid non-blank date. This crate returns NULL
  (the dBase convention). That includes values with embedded spaces such as
  `"2024 1 1"`, which dbfread parses as a date.

## License

MIT. `src/dcl.rs` is an altered version of `blast.c`, which is under the zlib
license; that license notice is kept in the file.

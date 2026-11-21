# read_dbc

A DuckDB extension for querying DATASUS `.dbc` files directly from SQL. A
`.dbc` file is a dBase (DBF) file whose records are compressed with PKWare DCL
"implode"; `read_dbc` decompresses and parses it on the fly, so there is no
separate convert-to-DBF or convert-to-CSV step before you can query the data.

**Status: under construction.** `read_dbc(...)` works (see Usage); it is not
yet published as a community extension.

## Building

You need Rust (stable), Python 3 with `venv`, Make and Git. Clone with
submodules:

```shell
git clone --recurse-submodules https://github.com/GustavoDorow/read_dbc
cd read_dbc
```

Then configure, build and run the SQL tests:

```shell
make configure   # sets up a Python venv with the DuckDB test runner
make debug       # builds build/debug/read_dbc.duckdb_extension
make test        # runs the SQLLogicTests in test/sql
```

Use `make release` / `make test_release` for an optimised build. The pure-Rust
format crate can be tested on its own with `cargo test -p read_dbc_format`.

## Loading the extension

The extension targets DuckDB v2.0.1 and is not signed, so start DuckDB with
`-unsigned`:

```shell
duckdb -unsigned
```

```sql
LOAD 'build/debug/read_dbc.duckdb_extension';
SELECT read_dbc_version();
SELECT count(*) FROM read_dbc('test/fixtures/RDAC0801.dbc');
```

## Usage

```sql
SELECT * FROM read_dbc('RDAC0801.dbc');
SELECT * FROM 'data/RD*.dbc';                  -- replacement scan (.dbc only)
SELECT * FROM read_dbc(['a.dbc', 'dir/*.dbc'], union_by_name := true, filename := true);
SELECT * FROM read_dbc('RDSC2501.dbc', types := {'DT_INTER': 'DATE'});
```

`read_dbc(files, encoding := 'auto', all_varchar := false, include_deleted := false,
verify_crc := true, types := NULL, union_by_name := false, filename := false)`

| Option | Meaning |
|---|---|
| `files` | A path or glob, or a list of them. Globs go through DuckDB's file system; matches are sorted, list order is kept; a pattern matching nothing is an error. `.dbf` files (by extension) are read as plain, uncompressed DBF. |
| `encoding` | `auto` (from the header's language-driver byte; unknown → Latin-1), `latin1`/`iso-8859-1`, `cp437`, `cp850`, `cp860`, `cp1252`/`windows-1252`, `utf8` (strict). DATASUS SIH files are CP850. |
| `all_varchar` | Read every field as text (trailing spaces stripped). |
| `include_deleted` | Also return deleted records, with a `_deleted BOOLEAN` column. |
| `verify_crc` | Check the CRC-32 stored in `.dbc` files (at the end of each file). |
| `types` | STRUCT or MAP of column → type: `VARCHAR`, `BIGINT`, `DATE`, `BOOLEAN`, `DECIMAL(w,s)`. Column names are case-insensitive. `C` → `DATE` reads `YYYYMMDD` (blank or `00000000` → NULL, anything else invalid is an error); `C` → `BIGINT` is a strict integer; any field → `VARCHAR`. |
| `union_by_name` | Combine files with different layouts (see below). |
| `filename` | Add a `filename VARCHAR` column (the path as expanded). |

Types come from the header: `C` → VARCHAR, `N` without decimals (width ≤ 18) →
BIGINT, other `N`/`F` → DECIMAL(width, decimals), `D` → DATE, `L` → BOOLEAN,
anything else → VARCHAR (see `crates/format/README.md`). Duplicate field names
are renamed `A`, `A_1`, `A_2`, ... like `read_csv`.

**Several files.** Without `union_by_name`, all files must have the same column
names (case-insensitive) and types in the same order. With it, columns are
matched by name (case-insensitive, first-seen order and spelling), missing
columns are NULL, and differing types are promoted exactly: BIGINT and
DECIMAL become the smallest DECIMAL holding both (BIGINT counts as 19 integer
digits; an error if more than 38 digits are needed, in which case use `types`
or `all_varchar`), any other mix becomes VARCHAR (values rendered like a cast
to VARCHAR). Header problems are reported at bind (only headers are read
then), data problems while scanning; every error names the file.

**Parallelism and order.** Each file is decoded front to back by one thread;
different files are read in parallel. Rows come out in file order, then record
order (unless `preserve_insertion_order` is off).

**Known limitation.** DuckDB v2.0.1's C API reports `count(*)` as a scan of the
first column, so `count(*)` decodes the first column (and fails if it holds an
invalid value); it still reads every file to the end and checks the CRC.

## Credits

Scaffolded from [duckdb/extension-template-rs](https://github.com/duckdb/extension-template-rs)
at commit `92050f5`. The reference copies under `docs/template-examples/` come
from that template.

## License

MIT, see [LICENSE](LICENSE).

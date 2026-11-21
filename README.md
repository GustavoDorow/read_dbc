# read_dbc

A DuckDB extension for querying DATASUS `.dbc` files directly from SQL. A
`.dbc` file is a dBase (DBF) file whose records are compressed with PKWare DCL
"implode"; `read_dbc` decompresses and parses it on the fly, so there is no
separate convert-to-DBF or convert-to-CSV step before you can query the data.

**Status: under construction.** The extension currently only registers a
placeholder function, `read_dbc_version()`.

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
```

## Credits

Scaffolded from [duckdb/extension-template-rs](https://github.com/duckdb/extension-template-rs)
at commit `92050f5`. The reference copies under `docs/template-examples/` come
from that template.

## License

MIT, see [LICENSE](LICENSE).

# read_dbc Benchmark Suite

Reproducible end-to-end benchmark comparing direct DuckDB queries via `read_dbc` against convert-first DATASUS `.dbc` workflows.

## Prerequisites

- Rust toolchain (`cargo`, `rustc`)
- `make`, `git`, `curl` (with FTP support), Python 3.11+
- [`uv`](https://docs.astral.sh/uv/) (runs the PEP 723 script with exact pinned dependencies)

## Build the release extension

```bash
git submodule update --init
make configure && make release
```

This builds `build/release/extension/read_dbc/read_dbc.duckdb_extension`.

## Run the benchmark

```bash
uv run --script scripts/bench/bench.py
```

On first run, `bench.py` automatically:
1. Fetches the standard SIH/RD fixtures (`RDAC2601.dbc`, `RDSC2501.dbc`) via `scripts/fixtures/fetch.sh` and downloads the large São Paulo fixture (`RDSP2501.dbc`) into `test/data/bench/`, verifying SHA-256 digests for all files.
2. Builds `scripts/fixtures/dbc2dbf` (`datasus-dbc 0.1.1`) in `--release --locked` mode.
3. Installs `datasus-cli 0.1.0` (`datasus` binary) into `build/bench-tools/` (gitignored).
4. Runs 5 interleaved repetitions per file × workflow, each in a fresh child OS process with temporary files placed on the repository's filesystem under `build/bench-tmp/` (avoiding RAM-backed `/tmp` `tmpfs`).
5. Writes raw per-run and aggregated results to `docs/bench-results/` (`results.json`, `results.csv`, `summary.csv`).

### Useful options

```bash
# Quick 1-rep smoke run on the small file
uv run --script scripts/bench/bench.py --reps 1 --files RDAC2601

# Run only read_dbc and datasus-cli workflows
uv run --script scripts/bench/bench.py --workflows A_stream A_materialize A_count A_agg E_stream E_materialize E_agg E_cached_agg
```

## Workflows

| ID | Description |
|---|---|
| `A_stream` | `read_dbc` direct: `SELECT * FROM read_dbc(f)` streamed in 2,048-row Arrow batches |
| `A_materialize` | `read_dbc` direct: `CREATE TABLE t AS SELECT * FROM read_dbc(f)` |
| `A_count` | `read_dbc` direct: `SELECT count(*) FROM read_dbc(f)` |
| `A_agg` | `read_dbc` direct: `SELECT MUNIC_RES, sum(VAL_TOT) FROM read_dbc(f) GROUP BY 1` |
| `B_datasus_dbc_dbfread` | `datasus-dbc` 0.1.1 `.dbc`→`.dbf`, `dbfread` 2.0.7 (`iso-8859-1`) into `list[dict]`, Arrow → DuckDB 2.0.1 table |
| `C1_pyreaddbc_pandas` | `pyreaddbc` 2.0.4 `.dbc`→`.dbf`, `dbfread` 2.0.7 → `pandas` 3.1.1 DataFrame → DuckDB 2.0.1 table |
| `C2_pysus_pandas` | `PySUS` 2.12.2 (`DBC.load()` → `pandas` DataFrame) → DuckDB 1.5.6 table (isolated worker `worker_pysus.py`, since `pysus==2.12.2` pins `duckdb>=1.4.4,<2.0.0`) |
| `D_datasus_dbc_read_dbf` | `datasus-dbc` `.dbc`→`.dbf` → DuckDB community `read_dbf` (checks availability; skipped if unavailable for DuckDB v2.0.1) |
| `E_stream` | `datasus` CLI 0.1.0 `.dbc`→Parquet → DuckDB 2.0.1 `read_parquet` streamed in 2,048-row Arrow batches |
| `E_materialize` | `datasus` CLI 0.1.0 `.dbc`→Parquet → DuckDB 2.0.1 `CREATE TABLE` via `read_parquet` |
| `E_agg` | `datasus` CLI 0.1.0 `.dbc`→Parquet → DuckDB 2.0.1 `read_parquet` `GROUP BY` aggregation |
| `E_cached_agg` | Pre-converted Parquet already on disk → DuckDB 2.0.1 `read_parquet` `GROUP BY` aggregation (warm/repeated-query baseline) |

## Measurement methodology

- **Isolation:** Every repetition runs in a fresh OS process with lazy imports (only the libraries needed by that workflow are imported).
- **Timers:** `time.perf_counter()` starts after Python imports and covers opening DuckDB (plus `LOAD read_dbc` for Group A), any conversion/parsing step, and returning the first row / completing the query.
- **Peak RSS:** `max(VmHWM, RUSAGE_CHILDREN.ru_maxrss)` on Linux (capturing both the Python process and any external CLI converter like `fixture-dbc2dbf` or `datasus`).
- **Disk bytes written:** Total bytes of intermediate files created on disk (`.dbf`, `.parquet`, plus any DuckDB temp spill files). For `C2_pysus_pandas`, includes both the temporary `.dbf` written by `pyreaddbc.dbc2dbf` (and unlinked by PySUS) and the `.parquet` file written by `DBC.to_parquet()`.
- **Page cache:** Input `.dbc` files are read once into the OS page cache before timing so repetition 1 does not pay an asymmetric cold-read penalty.

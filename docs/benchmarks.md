# read_dbc Benchmarks

End-to-end measurements comparing direct SQL queries with `read_dbc` against convert-first DATASUS `.dbc` workflows.

- **Reproduce:** `uv run --script scripts/bench/bench.py` (see [scripts/bench/README.md](../scripts/bench/README.md))
- **Raw data:** [results.json](bench-results/results.json), [results.csv](bench-results/results.csv), [summary.csv](bench-results/summary.csv)

## Environment

### Hardware

| Component | Specification |
|---|---|
| CPU | AMD Ryzen 5 5600 (6 physical cores / 12 threads, boost up to 4.47 GHz, 32 MiB L3) |
| RAM | 31.2 GiB (33,518,972,928 bytes) |
| Storage | NVMe SSD (`Fanxiang S790 1TB` / `XrayDisk 1TB SSD`), btrfs on `/dev/mapper/root` (`/home`) |
| OS / Kernel | Omarchy Linux (Arch-based), kernel `7.2.15-1-omarchy` (`x86_64`) |

Temporary files were placed on the repository's NVMe btrfs filesystem (`build/bench-tmp/`) rather than `/tmp` (`tmpfs`), so intermediate `.dbf` and `.parquet` writes hit real disk storage rather than RAM.

### Software

| Component | Version |
|---|---|
| `read_dbc` | `0.1.0` (release build, `lto = true`, DuckDB C API v2, commit `110f0d8`) |
| DuckDB | `2.0.1` (workflows A, B, C1, D, E); `1.5.6` (workflow C2 — pinned by `pysus==2.12.2`) |
| Rust / Cargo | `rustc 1.98.0 (88d9e12ae 2026-08-18)` |
| Python / `uv` | Python `3.13.15`, `uv 0.13.9` |
| `datasus-dbc` | `0.1.1` (via `scripts/fixtures/dbc2dbf`, release build) |
| `datasus-cli` | `0.1.0` (`datasus convert --keep-names`, powered by `datasus-core` / `datasus-arrow`) |
| `dbfread` | `2.0.7` |
| `pyreaddbc` | `2.0.4` |
| `PySUS` | `2.12.2` (with `pandas 2.3.3`, `duckdb 1.5.6` in isolated `worker_pysus.py`) |
| `pandas` / `pyarrow` / `psutil` | `3.1.1` / `23.0.1` / `7.2.2` |

## Input files

Monthly DATASUS SIH/RD (AIH reduzida) files from `ftp://ftp.datasus.gov.br/dissemin/publicos/SIHSUS/200801_/Dados/`:

| File | Tier | Period / State | Rows | Cols | `.dbc` size | Uncompressed `.dbf` | `.parquet` (Zstd) | SHA-256 (`.dbc`) |
|---|---|---|---:|---:|---:|---:|---:|---|
| `RDAC2601.dbc` | Small | Jan 2026, Acre | 4,774 | 114 | 337.2 KiB | 3.2 MiB | 215.8 KiB | `385882b4dfa4...` |
| `RDSC2501.dbc` | Medium | Jan 2025, Santa Catarina | 53,631 | 113 | 4.0 MiB | 35.9 MiB | 2.2 MiB | `00d5831a83be...` |
| `RDSP2501.dbc` | Large | Jan 2025, São Paulo | 238,144 | 113 | 17.9 MiB | 159.4 MiB | 9.1 MiB | `33f811186a6a...` |

## Methodology

- **5 interleaved repetitions** per file and workflow (rep 1 across all workflows, then rep 2, etc.), each in a **fresh child OS process** with lazy imports (only libraries required by that workflow are imported).
- **Warm page cache:** Each `.dbc` file is read once sequentially before timing starts so rep 1 does not pay an asymmetric cold-read penalty.
- **Timers:** `time.perf_counter()` starts after Python imports and includes opening DuckDB (plus `LOAD read_dbc` for Group A), any conversion/parsing, and delivering the first row (`First row`) or finishing the full query/materialisation (`Total`). For blocking operations (`CREATE TABLE`, `count(*)`, `GROUP BY`), `First row` equals `Total`.
- **Peak RSS:** `max(VmHWM, RUSAGE_CHILDREN.ru_maxrss)` on Linux, capturing both the Python process and any external converter binary (`fixture-dbc2dbf`, `datasus`).
- **Disk written:** Total bytes of intermediate files written to disk (`.dbf`, `.parquet`, plus any DuckDB temp spill). For `C2_pysus_pandas`, includes both the temporary `.dbf` written by `pyreaddbc.dbc2dbf` (and deleted by PySUS) and the resulting `.parquet`.
- **Correctness check:** After timing and RSS capture, every run verified exact row counts (4,774 / 53,631 / 238,144) or `MUNIC_RES` group counts (54 / 499 / 1,354). Zero mismatches occurred.

### Workflow definitions

- **`A_stream`:** `SELECT * FROM read_dbc(f)` streamed in 2,048-row Arrow batches (`First row` = first batch; `Total` = all rows drained and CRC-verified).
- **`A_materialize`:** `CREATE TABLE t AS SELECT * FROM read_dbc(f)`.
- **`A_count`:** `SELECT count(*) FROM read_dbc(f)` (scans full file, decodes column 0 due to DuckDB v2.0.1 C API, verifies CRC-32).
- **`A_agg`:** `SELECT MUNIC_RES, sum(VAL_TOT) FROM read_dbc(f) GROUP BY 1` (projection pushdown parses only 2 columns; exact `DECIMAL(14,2)` sum).
- **`B_datasus_dbc_dbfread`:** `fixture-dbc2dbf` (`datasus-dbc 0.1.1`) `.dbc`→`.dbf` → `dbfread` (`encoding='iso-8859-1'`) into `list[dict]` → `pyarrow.Table.from_pylist` → DuckDB table.
- **`C1_pyreaddbc_pandas`:** `pyreaddbc.dbc2dbf` `.dbc`→`.dbf` → `dbfread` → `pandas.DataFrame` → DuckDB table.
- **`C2_pysus_pandas`:** `PySUS 2.12.2` `DBC(path).load()` (`pyreaddbc` → `.dbf` → numpy `stream_dbf_fast` → `.parquet` → `pandas.DataFrame`, all columns as strings) → DuckDB `1.5.6` table.
- **`D_datasus_dbc_read_dbf`:** `datasus-dbc` `.dbc`→`.dbf` → DuckDB community extension `read_dbf` (`INSTALL read_dbf FROM community`). **Skipped:** HTTP 404 for DuckDB `v2.0.1` (`linux_amd64`).
- **`E_stream`:** `datasus convert --keep-names` `.dbc`→`.parquet` → DuckDB `read_parquet` streamed in 2,048-row Arrow batches.
- **`E_materialize`:** `datasus convert --keep-names` `.dbc`→`.parquet` → DuckDB `CREATE TABLE t AS SELECT * FROM read_parquet(...)`.
- **`E_agg`:** `datasus convert --keep-names` `.dbc`→`.parquet` → DuckDB `SELECT MUNIC_RES, sum(VAL_TOT) FROM read_parquet(...) GROUP BY 1` (cold, starting from `.dbc`).
- **`E_cached_agg`:** Same `GROUP BY` query on the pre-converted `.parquet` already on disk (warm / repeated-query baseline).

## Results

All timings and RSS figures report **median / min** across 5 repetitions.

### RDAC2601 — Small (4,774 rows × 114 cols, 337.2 KiB `.dbc`)

| Workflow | First row (med / min) | Total (med / min) | Peak RSS (med / min) | Disk written |
|---|---:|---:|---:|---:|
| `A_stream` (`read_dbc` stream) | 82.0 ms / 71.4 ms | 83.4 ms / 76.7 ms | 139.5 / 134.6 MiB | 0 B |
| `A_materialize` (`read_dbc` → table) | 100.2 ms / 94.2 ms | 100.2 ms / 94.2 ms | 114.9 / 107.8 MiB | 0 B |
| `A_count` (`read_dbc` `count(*)`) | 37.0 ms / 35.4 ms | 37.0 ms / 35.4 ms | 90.0 / 88.0 MiB | 0 B |
| `A_agg` (`read_dbc` `GROUP BY`) | 39.0 ms / 38.1 ms | 39.0 ms / 38.1 ms | 107.5 / 103.8 MiB | 0 B |
| `B_datasus_dbc_dbfread` | 1.12 s / 944.0 ms | 1.12 s / 944.0 ms | 280.4 / 274.6 MiB | 3.2 MiB |
| `C1_pyreaddbc_pandas` | 717.0 ms / 519.3 ms | 717.0 ms / 519.3 ms | 213.8 / 213.2 MiB | 3.2 MiB |
| `C2_pysus_pandas` | 740.2 ms / 631.8 ms | 740.2 ms / 631.8 ms | 361.6 / 350.1 MiB | 3.5 MiB |
| `D_datasus_dbc_read_dbf` | skipped (404 on v2.0.1) | — | — | — |
| `E_stream` (`datasus` → Parquet stream) | 98.5 ms / 88.3 ms | 99.0 ms / 88.7 ms | 141.6 / 135.7 MiB | 215.8 KiB |
| `E_materialize` (`datasus` → Parquet → table) | 128.2 ms / 125.8 ms | 128.2 ms / 125.8 ms | 129.7 / 122.1 MiB | 215.8 KiB |
| `E_agg` (`datasus` → Parquet → `GROUP BY`) | 92.4 ms / 90.3 ms | 92.4 ms / 90.3 ms | 117.1 / 109.1 MiB | 215.8 KiB |
| `E_cached_agg` (pre-converted Parquet `GROUP BY`) | 33.9 ms / 32.7 ms | 33.9 ms / 32.7 ms | 125.1 / 118.9 MiB | 0 B |

### RDSC2501 — Medium (53,631 rows × 113 cols, 4.0 MiB `.dbc`)

| Workflow | First row (med / min) | Total (med / min) | Peak RSS (med / min) | Disk written |
|---|---:|---:|---:|---:|
| `A_stream` (`read_dbc` stream) | 137.3 ms / 84.4 ms | 443.4 ms / 432.2 ms | 159.2 / 147.2 MiB | 0 B |
| `A_materialize` (`read_dbc` → table) | 654.6 ms / 633.8 ms | 654.6 ms / 633.8 ms | 240.6 / 232.4 MiB | 0 B |
| `A_count` (`read_dbc` `count(*)`) | 99.7 ms / 96.3 ms | 99.7 ms / 96.3 ms | 90.1 / 88.0 MiB | 0 B |
| `A_agg` (`read_dbc` `GROUP BY`) | 104.3 ms / 100.5 ms | 104.3 ms / 100.5 ms | 113.7 / 109.6 MiB | 0 B |
| `B_datasus_dbc_dbfread` | 9.05 s / 8.07 s | 9.05 s / 8.07 s | 1,012.8 / 1,007.9 MiB | 35.9 MiB |
| `C1_pyreaddbc_pandas` | 6.67 s / 5.81 s | 6.67 s / 5.81 s | 985.7 / 985.4 MiB | 35.9 MiB |
| `C2_pysus_pandas` | 7.04 s / 6.78 s | 7.04 s / 6.78 s | 683.6 / 673.8 MiB | 38.9 MiB |
| `D_datasus_dbc_read_dbf` | skipped (404 on v2.0.1) | — | — | — |
| `E_stream` (`datasus` → Parquet stream) | 560.5 ms / 479.2 ms | 627.1 ms / 530.1 ms | 195.6 / 187.8 MiB | 2.2 MiB |
| `E_materialize` (`datasus` → Parquet → table) | 878.6 ms / 740.9 ms | 878.6 ms / 740.9 ms | 246.4 / 240.3 MiB | 2.2 MiB |
| `E_agg` (`datasus` → Parquet → `GROUP BY`) | 577.2 ms / 570.5 ms | 577.2 ms / 570.5 ms | 125.7 / 125.6 MiB | 2.2 MiB |
| `E_cached_agg` (pre-converted Parquet `GROUP BY`) | 33.5 ms / 33.3 ms | 33.5 ms / 33.3 ms | 115.3 / 111.2 MiB | 0 B |

### RDSP2501 — Large (238,144 rows × 113 cols, 17.9 MiB `.dbc`)

| Workflow | First row (med / min) | Total (med / min) | Peak RSS (med / min) | Disk written |
|---|---:|---:|---:|---:|
| `A_stream` (`read_dbc` stream) | 98.7 ms / 89.7 ms | 1.03 s / 1.01 s | 168.1 / 158.6 MiB | 0 B |
| `A_materialize` (`read_dbc` → table) | 1.30 s / 1.14 s | 1.30 s / 1.14 s | 361.4 / 354.8 MiB | 0 B |
| `A_count` (`read_dbc` `count(*)`) | 279.3 ms / 226.5 ms | 279.3 ms / 226.5 ms | 86.1 / 84.2 MiB | 0 B |
| `A_agg` (`read_dbc` `GROUP BY`) | 338.6 ms / 242.4 ms | 338.6 ms / 242.4 ms | 114.1 / 106.1 MiB | 0 B |
| `B_datasus_dbc_dbfread` | 31.13 s / 28.53 s | 31.13 s / 28.53 s | 3,387.0 / 3,373.1 MiB | 159.4 MiB |
| `C1_pyreaddbc_pandas` | 20.72 s / 20.50 s | 20.72 s / 20.50 s | 3,803.3 / 3,802.9 MiB | 159.4 MiB |
| `C2_pysus_pandas` | 16.73 s / 16.57 s | 16.73 s / 16.57 s | 1,356.8 / 1,329.0 MiB | 172.5 MiB |
| `D_datasus_dbc_read_dbf` | skipped (404 on v2.0.1) | — | — | — |
| `E_stream` (`datasus` → Parquet stream) | 1.46 s / 1.42 s | 1.70 s / 1.65 s | 310.9 / 301.9 MiB | 9.1 MiB |
| `E_materialize` (`datasus` → Parquet → table) | 1.75 s / 1.73 s | 1.75 s / 1.73 s | 348.6 / 343.9 MiB | 9.1 MiB |
| `E_agg` (`datasus` → Parquet → `GROUP BY`) | 1.40 s / 1.39 s | 1.40 s / 1.39 s | 301.8 / 301.6 MiB | 9.1 MiB |
| `E_cached_agg` (pre-converted Parquet `GROUP BY`) | 25.6 ms / 25.0 ms | 25.6 ms / 25.0 ms | 112.2 / 109.8 MiB | 0 B |

## Interpretation

1. **Streaming first-row latency and bounded memory are `read_dbc`'s clearest advantages.** Across a 50× increase in row count (4.8K → 238K rows), `A_stream` delivers the first 2,048-row batch in ~82–137 ms (min ~71–90 ms), `A_count` stays at ~86–90 MiB peak RSS, and `A_agg` stays at ~108–114 MiB peak RSS with zero bytes written to disk. Every convert-first workflow must finish converting the entire file before the first row is available (`1.46 s` for `datasus` CLI, `16.7 s` for PySUS, `20.7–31.1 s` for `dbfread` pipelines on `RDSP2501`).
2. **Cold analytical queries (`A_agg`, `A_count`) beat even the fastest Rust Parquet converter.** On `RDSP2501`, `read_dbc` runs the two-column `GROUP BY` aggregation in **339 ms** (min **242 ms**) at **114 MiB** RSS, compared to **1.40 s** and **302 MiB** RSS for `datasus convert` + `read_parquet` (`E_agg`) — because `read_dbc` pushes projection down to 2 of 113 columns and never writes a Parquet file or buffers the whole decompressed DBF in memory.
3. **Python `dbfread` and `pandas` pipelines are 13–24× slower and use 4–10× more RAM on `RDSP2501`.**
   - In `B_datasus_dbc_dbfread` (31.1 s, 3.3 GiB RSS), `datasus-dbc 0.1.1` decompression alone takes **13.0 s** (versus `0.43 s` for C `pyreaddbc`), `dbfread` takes **14.3 s**, and Arrow + DuckDB loading takes **3.9 s**.
   - In `C1_pyreaddbc_pandas` (20.7 s, 3.7 GiB RSS), C `pyreaddbc` decompresses in **0.43 s**, but `dbfread` → `pandas.DataFrame` takes **19.8 s** and peaks at 3.7 GiB RAM.
   - `C2_pysus_pandas` (16.7 s, 1.3 GiB RSS) cuts Python overhead via its numpy byte-level `stream_dbf_fast`, but still writes both a 159.4 MiB temporary `.dbf` and a 13.1 MiB `.parquet` to disk, keeps all columns untyped as strings (`VARCHAR`), and currently pins `duckdb<2.0.0`.
4. **Where `read_dbc` loses:**
   - **Repeated queries on the same file:** Once converted to Parquet (`E_cached_agg`), DuckDB executes the `RDSP2501` aggregation in **25.6 ms** vs **338.6 ms** for `read_dbc` (~13× faster). A `.dbc` file is a single sequential PKWare-DCL stream with no columnar storage or row-group index, so `read_dbc` must decompress the entire stream on every scan. For multi-query workloads over static files, convert once to Parquet (via `COPY (SELECT * FROM read_dbc(...)) TO 'out.parquet'` or `datasus convert`).
   - **Peak RSS on full table materialisation (`RDSP2501`):** `A_materialize` peaked at **361.4 MiB** vs **348.6 MiB** for `E_materialize` (~3.7% higher), because both hold the full 238K × 113 DuckDB table in RAM and DuckDB loads Parquet's columnar/dictionary batches with slightly lower peak working memory.
   - **Small files:** On `RDAC2601` (337 KiB), DuckDB startup (~35 ms) dominates and `read_dbc` is only ~1.2–1.3× faster than `datasus` CLI + Parquet for full scans (`100.2 ms` vs `128.2 ms`).
5. **Notes and caveats:**
   - **Workflow D (`read_dbf`):** `INSTALL read_dbf FROM community` failed with HTTP 404 against DuckDB `v2.0.1` (`linux_amd64`), so Workflow D was skipped.
   - **Numeric precision:** `read_dbc` reads `VAL_TOT` as exact `DECIMAL(14,2)` (`435,555,819.75` on `RDSP2501`); `datasus-cli` Parquet writes `DOUBLE` (`435,555,819.74999255`), and `PySUS` reads strings (`CAST(... AS DOUBLE)` gives `435,555,819.7499986`).
   - **Rep 1 variance on `RDSP2501`:** Across both Group A and Group E, repetition 1 on `RDSP2501` ran ~1.6× slower than repetitions 2–5 (e.g., `E_materialize` was 2.96 s in rep 1 and 1.73–1.75 s in reps 2–5; `A_materialize` was 2.04 s / 1.78 s in reps 1–2 and 1.14–1.30 s in reps 3–5), reflecting transient host load / CPU boost behaviour. Medians and minimums both reflect the steady-state regime; all individual runs are preserved in [results.csv](bench-results/results.csv).

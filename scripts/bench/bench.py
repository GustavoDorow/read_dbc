#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11,<3.15"
# dependencies = [
#   "duckdb==2.0.1",
#   "dbfread==2.0.7",
#   "pyreaddbc==2.0.4",
#   "pandas==3.1.1",
#   "pyarrow==23.0.1",
#   "psutil==7.2.2",
# ]
# ///
"""Reproducible end-to-end benchmark for the read_dbc DuckDB extension.

Compares direct DuckDB querying via `read_dbc` against convert-first workflows
on small (RDAC2601), medium (RDSC2501), and large (RDSP2501) DATASUS SIH/RD
`.dbc` files. Each repetition runs in an isolated child OS process so peak RSS
(`VmHWM` / `ru_maxrss`), timing, and temporary disk writes are cleanly isolated.

Usage:
    uv run --script scripts/bench/bench.py
    uv run --script scripts/bench/bench.py --reps 5 --out-dir docs/bench-results
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import platform
import resource
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
FTP_BASE = "ftp://ftp.datasus.gov.br/dissemin/publicos/SIHSUS/200801_/Dados"
DEFAULT_EXT = ROOT / "build" / "release" / "extension" / "read_dbc" / "read_dbc.duckdb_extension"
DBC2DBF_MANIFEST = ROOT / "scripts" / "fixtures" / "dbc2dbf" / "Cargo.toml"
BENCH_TOOLS_ROOT = ROOT / "build" / "bench-tools"
DBC2DBF_TARGET = BENCH_TOOLS_ROOT / "dbc2dbf-target"
DBC2DBF_BIN = DBC2DBF_TARGET / "release" / "fixture-dbc2dbf"
DATASUS_CLI_VERSION = "0.1.0"
DATASUS_CLI_BIN = BENCH_TOOLS_ROOT / "bin" / "datasus"
BENCH_TMP_BASE = ROOT / "build" / "bench-tmp"
PYSUS_WORKER = Path(__file__).resolve().parent / "worker_pysus.py"


@dataclass(frozen=True)
class BenchFile:
    name: str
    size_tier: str
    rel_path: str
    sha256: str
    via_fetch_sh: bool

    @property
    def path(self) -> Path:
        return ROOT / self.rel_path


FILES: list[BenchFile] = [
    BenchFile(
        name="RDAC2601",
        size_tier="small",
        rel_path="test/data/RDAC2601.dbc",
        sha256="385882b4dfa475a4823f734afd553622928c5076996e6fec320613bbb0b6d3e0",
        via_fetch_sh=True,
    ),
    BenchFile(
        name="RDSC2501",
        size_tier="medium",
        rel_path="test/data/RDSC2501.dbc",
        sha256="00d5831a83bea9d748b9d42e0ffcec892db99db5638c388e355e5b9139ab4810",
        via_fetch_sh=True,
    ),
    BenchFile(
        name="RDSP2501",
        size_tier="large",
        rel_path="test/data/bench/RDSP2501.dbc",
        sha256="33f811186a6a4dfa377cea2b75e739a7b1316c516d8895473af4c3685a095553",
        via_fetch_sh=False,
    ),
]

WORKFLOWS: list[tuple[str, str]] = [
    ("A_stream", "read_dbc direct: SELECT * streamed (Arrow 2048-row batches)"),
    ("A_materialize", "read_dbc direct: CREATE TABLE AS SELECT *"),
    ("A_count", "read_dbc direct: SELECT count(*)"),
    ("A_agg", "read_dbc direct: SELECT MUNIC_RES, sum(VAL_TOT) GROUP BY 1"),
    ("B_datasus_dbc_dbfread", "datasus-dbc (.dbc->.dbf) -> dbfread list[dict] -> Arrow -> DuckDB table"),
    ("C1_pyreaddbc_pandas", "pyreaddbc (.dbc->.dbf) -> dbfread -> pandas DataFrame -> DuckDB table"),
    ("C2_pysus_pandas", "PySUS 2.12.2 (DBC.load -> pandas DataFrame) -> DuckDB 1.5.6 table"),
    ("D_datasus_dbc_read_dbf", "datasus-dbc (.dbc->.dbf) -> DuckDB community read_dbf"),
    ("E_stream", "datasus CLI (.dbc->Parquet) -> DuckDB read_parquet streamed"),
    ("E_materialize", "datasus CLI (.dbc->Parquet) -> DuckDB CREATE TABLE read_parquet"),
    ("E_agg", "datasus CLI (.dbc->Parquet) -> DuckDB read_parquet GROUP BY agg"),
    ("E_cached_agg", "Pre-converted Parquet (warm) -> DuckDB read_parquet GROUP BY agg"),
]

AGG_SQL_TEMPLATE = (
    "SELECT MUNIC_RES, sum(VAL_TOT) AS total_val FROM {source} GROUP BY 1"
)


# ---------------------------------------------------------------------------
# Process resource helpers (used inside isolated worker processes)
# ---------------------------------------------------------------------------

def read_vmhwm_bytes() -> int:
    status = Path("/proc/self/status")
    if status.exists():
        for line in status.read_text(encoding="utf-8").splitlines():
            if line.startswith("VmHWM:"):
                return int(line.split()[1]) * 1024
    import psutil

    return int(psutil.Process().memory_info().rss)


def read_child_maxrss_bytes() -> int:
    ru = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    return int(ru if sys.platform == "darwin" else ru * 1024)


def dir_file_bytes(path: Path) -> int:
    if not path.exists():
        return 0
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file())


def sql_quote(path: Path) -> str:
    return "'" + str(path).replace("'", "''") + "'"


# ---------------------------------------------------------------------------
# Worker implementation (runs in a fresh process with lazy imports only)
# ---------------------------------------------------------------------------

def run_worker(args: argparse.Namespace) -> int:
    wf = args.workflow
    dbc_path: Path = args.file
    ext_path: Path = args.ext
    work_dir: Path = args.work_dir
    out_json: Path = args.out_json
    cached_pq: Path | None = args.cached_parquet

    os.environ["TMPDIR"] = str(work_dir)
    duckdb_tmp = work_dir / "duckdb_tmp"
    duckdb_tmp.mkdir(parents=True, exist_ok=True)

    result: dict[str, Any]
    try:
        if wf == "A_stream":
            import duckdb
            import pyarrow  # noqa: F401 - pre-import outside timed region

            t0 = time.perf_counter()
            con = duckdb.connect(
                config={
                    "allow_unsigned_extensions": "true",
                    "temp_directory": str(duckdb_tmp),
                }
            )
            con.execute(f"LOAD {sql_quote(ext_path)}")
            rbr = con.sql(f"SELECT * FROM read_dbc({sql_quote(dbc_path)})").to_arrow_reader(
                batch_size=2048
            )
            first_batch = next(iter(rbr))
            t_first = time.perf_counter()
            total_rows = first_batch.num_rows
            for batch in rbr:
                total_rows += batch.num_rows
            t_total = time.perf_counter()
            self_rss = read_vmhwm_bytes()
            child_rss = read_child_maxrss_bytes()
            disk_bytes = dir_file_bytes(duckdb_tmp)
            con.close()
            result = {
                "status": "ok",
                "first_row_s": t_first - t0,
                "total_s": t_total - t0,
                "peak_rss_bytes": max(self_rss, child_rss),
                "disk_write_bytes": disk_bytes,
                "rows_verified": int(total_rows),
                "details": {
                    "first_batch_rows": int(first_batch.num_rows),
                    "self_vmhwm_bytes": self_rss,
                    "child_maxrss_bytes": child_rss,
                },
            }

        elif wf == "A_materialize":
            import duckdb

            t0 = time.perf_counter()
            con = duckdb.connect(
                config={
                    "allow_unsigned_extensions": "true",
                    "temp_directory": str(duckdb_tmp),
                }
            )
            con.execute(f"LOAD {sql_quote(ext_path)}")
            con.execute(f"CREATE TABLE t AS SELECT * FROM read_dbc({sql_quote(dbc_path)})")
            first_row = con.execute("SELECT * FROM t LIMIT 1").fetchone()
            t_total = time.perf_counter()
            self_rss = read_vmhwm_bytes()
            child_rss = read_child_maxrss_bytes()
            disk_bytes = dir_file_bytes(duckdb_tmp)
            row_count = int(con.execute("SELECT count(*) FROM t").fetchone()[0])
            con.close()
            result = {
                "status": "ok",
                "first_row_s": t_total - t0,
                "total_s": t_total - t0,
                "peak_rss_bytes": max(self_rss, child_rss),
                "disk_write_bytes": disk_bytes,
                "rows_verified": row_count,
                "first_row_non_null": first_row is not None,
                "details": {
                    "self_vmhwm_bytes": self_rss,
                    "child_maxrss_bytes": child_rss,
                },
            }

        elif wf == "A_count":
            import duckdb

            t0 = time.perf_counter()
            con = duckdb.connect(
                config={
                    "allow_unsigned_extensions": "true",
                    "temp_directory": str(duckdb_tmp),
                }
            )
            con.execute(f"LOAD {sql_quote(ext_path)}")
            row_count = int(
                con.execute(f"SELECT count(*) FROM read_dbc({sql_quote(dbc_path)})").fetchone()[0]
            )
            t_total = time.perf_counter()
            self_rss = read_vmhwm_bytes()
            child_rss = read_child_maxrss_bytes()
            disk_bytes = dir_file_bytes(duckdb_tmp)
            con.close()
            result = {
                "status": "ok",
                "first_row_s": t_total - t0,
                "total_s": t_total - t0,
                "peak_rss_bytes": max(self_rss, child_rss),
                "disk_write_bytes": disk_bytes,
                "rows_verified": row_count,
                "details": {
                    "self_vmhwm_bytes": self_rss,
                    "child_maxrss_bytes": child_rss,
                },
            }

        elif wf == "A_agg":
            import duckdb

            t0 = time.perf_counter()
            con = duckdb.connect(
                config={
                    "allow_unsigned_extensions": "true",
                    "temp_directory": str(duckdb_tmp),
                }
            )
            con.execute(f"LOAD {sql_quote(ext_path)}")
            rows = con.execute(
                AGG_SQL_TEMPLATE.format(source=f"read_dbc({sql_quote(dbc_path)})")
            ).fetchall()
            t_total = time.perf_counter()
            self_rss = read_vmhwm_bytes()
            child_rss = read_child_maxrss_bytes()
            disk_bytes = dir_file_bytes(duckdb_tmp)
            con.close()
            total_val = sum(r[1] for r in rows if r[1] is not None)
            result = {
                "status": "ok",
                "first_row_s": t_total - t0,
                "total_s": t_total - t0,
                "peak_rss_bytes": max(self_rss, child_rss),
                "disk_write_bytes": disk_bytes,
                "rows_verified": len(rows),
                "details": {
                    "agg_groups": len(rows),
                    "agg_val_tot_sum": str(total_val),
                    "self_vmhwm_bytes": self_rss,
                    "child_maxrss_bytes": child_rss,
                },
            }

        elif wf == "B_datasus_dbc_dbfread":
            import dbfread
            import duckdb
            import pyarrow as pa

            dbf_path = work_dir / f"{dbc_path.stem}.dbf"
            t0 = time.perf_counter()
            subprocess.run(
                [str(DBC2DBF_BIN), str(dbc_path), str(dbf_path)],
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
            t_conv = time.perf_counter()
            rows = list(dbfread.DBF(str(dbf_path), encoding="iso-8859-1"))
            t_parse = time.perf_counter()
            tbl = pa.Table.from_pylist(rows)
            con = duckdb.connect(config={"temp_directory": str(duckdb_tmp)})
            con.execute("CREATE TABLE t AS SELECT * FROM tbl")
            first_row = con.execute("SELECT * FROM t LIMIT 1").fetchone()
            t_total = time.perf_counter()

            self_rss = read_vmhwm_bytes()
            child_rss = read_child_maxrss_bytes()
            dbf_bytes = dbf_path.stat().st_size
            disk_bytes = dbf_bytes + dir_file_bytes(duckdb_tmp)
            row_count = int(con.execute("SELECT count(*) FROM t").fetchone()[0])
            con.close()
            result = {
                "status": "ok",
                "first_row_s": t_total - t0,
                "total_s": t_total - t0,
                "peak_rss_bytes": max(self_rss, child_rss),
                "disk_write_bytes": disk_bytes,
                "rows_verified": row_count,
                "first_row_non_null": first_row is not None,
                "details": {
                    "convert_s": t_conv - t0,
                    "dbfread_s": t_parse - t_conv,
                    "duckdb_load_s": t_total - t_parse,
                    "dbf_bytes": dbf_bytes,
                    "self_vmhwm_bytes": self_rss,
                    "child_maxrss_bytes": child_rss,
                },
            }

        elif wf == "C1_pyreaddbc_pandas":
            import dbfread
            import duckdb
            import pandas as pd
            from pyreaddbc import dbc2dbf

            dbf_path = work_dir / f"{dbc_path.stem}.dbf"
            t0 = time.perf_counter()
            dbc2dbf(str(dbc_path), str(dbf_path))
            t_conv = time.perf_counter()
            df = pd.DataFrame(dbfread.DBF(str(dbf_path), encoding="iso-8859-1"))
            t_df = time.perf_counter()
            con = duckdb.connect(config={"temp_directory": str(duckdb_tmp)})
            con.execute("CREATE TABLE t AS SELECT * FROM df")
            first_row = con.execute("SELECT * FROM t LIMIT 1").fetchone()
            t_total = time.perf_counter()

            self_rss = read_vmhwm_bytes()
            child_rss = read_child_maxrss_bytes()
            dbf_bytes = dbf_path.stat().st_size
            disk_bytes = dbf_bytes + dir_file_bytes(duckdb_tmp)
            row_count = int(con.execute("SELECT count(*) FROM t").fetchone()[0])
            con.close()
            result = {
                "status": "ok",
                "first_row_s": t_total - t0,
                "total_s": t_total - t0,
                "peak_rss_bytes": max(self_rss, child_rss),
                "disk_write_bytes": disk_bytes,
                "rows_verified": row_count,
                "first_row_non_null": first_row is not None,
                "details": {
                    "convert_s": t_conv - t0,
                    "dbfread_pandas_s": t_df - t_conv,
                    "duckdb_load_s": t_total - t_df,
                    "dbf_bytes": dbf_bytes,
                    "self_vmhwm_bytes": self_rss,
                    "child_maxrss_bytes": child_rss,
                },
            }

        elif wf == "D_datasus_dbc_read_dbf":
            import duckdb

            con = duckdb.connect(config={"temp_directory": str(duckdb_tmp)})
            try:
                con.execute("INSTALL read_dbf FROM community")
                con.execute("LOAD read_dbf")
            except Exception as exc:
                con.close()
                result = {
                    "status": "skipped",
                    "error": f"{type(exc).__name__}: {exc}",
                }
                out_json.write_text(json.dumps(result), encoding="utf-8")
                return 0

            dbf_path = work_dir / f"{dbc_path.stem}.dbf"
            t0 = time.perf_counter()
            subprocess.run(
                [str(DBC2DBF_BIN), str(dbc_path), str(dbf_path)],
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
            t_conv = time.perf_counter()
            con.execute(f"CREATE TABLE t AS SELECT * FROM read_dbf({sql_quote(dbf_path)})")
            first_row = con.execute("SELECT * FROM t LIMIT 1").fetchone()
            t_total = time.perf_counter()
            self_rss = read_vmhwm_bytes()
            child_rss = read_child_maxrss_bytes()
            dbf_bytes = dbf_path.stat().st_size
            disk_bytes = dbf_bytes + dir_file_bytes(duckdb_tmp)
            row_count = int(con.execute("SELECT count(*) FROM t").fetchone()[0])
            con.close()
            result = {
                "status": "ok",
                "first_row_s": t_total - t0,
                "total_s": t_total - t0,
                "peak_rss_bytes": max(self_rss, child_rss),
                "disk_write_bytes": disk_bytes,
                "rows_verified": row_count,
                "first_row_non_null": first_row is not None,
                "details": {
                    "convert_s": t_conv - t0,
                    "duckdb_load_s": t_total - t_conv,
                    "dbf_bytes": dbf_bytes,
                    "self_vmhwm_bytes": self_rss,
                    "child_maxrss_bytes": child_rss,
                },
            }

        elif wf == "E_stream":
            import duckdb
            import pyarrow  # noqa: F401 - pre-import outside timed region

            pq_path = work_dir / f"{dbc_path.stem}.parquet"
            t0 = time.perf_counter()
            subprocess.run(
                [
                    str(DATASUS_CLI_BIN),
                    "convert",
                    "--keep-names",
                    "-q",
                    str(dbc_path),
                    "-o",
                    str(pq_path),
                ],
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
            t_conv = time.perf_counter()
            con = duckdb.connect(config={"temp_directory": str(duckdb_tmp)})
            rbr = con.sql(f"SELECT * FROM read_parquet({sql_quote(pq_path)})").to_arrow_reader(
                batch_size=2048
            )
            first_batch = next(iter(rbr))
            t_first = time.perf_counter()
            total_rows = first_batch.num_rows
            for batch in rbr:
                total_rows += batch.num_rows
            t_total = time.perf_counter()
            self_rss = read_vmhwm_bytes()
            child_rss = read_child_maxrss_bytes()
            pq_bytes = pq_path.stat().st_size
            disk_bytes = pq_bytes + dir_file_bytes(duckdb_tmp)
            con.close()
            result = {
                "status": "ok",
                "first_row_s": t_first - t0,
                "total_s": t_total - t0,
                "peak_rss_bytes": max(self_rss, child_rss),
                "disk_write_bytes": disk_bytes,
                "rows_verified": int(total_rows),
                "details": {
                    "convert_s": t_conv - t0,
                    "first_batch_rows": int(first_batch.num_rows),
                    "parquet_bytes": pq_bytes,
                    "self_vmhwm_bytes": self_rss,
                    "child_maxrss_bytes": child_rss,
                },
            }

        elif wf == "E_materialize":
            import duckdb

            pq_path = work_dir / f"{dbc_path.stem}.parquet"
            t0 = time.perf_counter()
            subprocess.run(
                [
                    str(DATASUS_CLI_BIN),
                    "convert",
                    "--keep-names",
                    "-q",
                    str(dbc_path),
                    "-o",
                    str(pq_path),
                ],
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
            t_conv = time.perf_counter()
            con = duckdb.connect(config={"temp_directory": str(duckdb_tmp)})
            con.execute(f"CREATE TABLE t AS SELECT * FROM read_parquet({sql_quote(pq_path)})")
            first_row = con.execute("SELECT * FROM t LIMIT 1").fetchone()
            t_total = time.perf_counter()
            self_rss = read_vmhwm_bytes()
            child_rss = read_child_maxrss_bytes()
            pq_bytes = pq_path.stat().st_size
            disk_bytes = pq_bytes + dir_file_bytes(duckdb_tmp)
            row_count = int(con.execute("SELECT count(*) FROM t").fetchone()[0])
            con.close()
            result = {
                "status": "ok",
                "first_row_s": t_total - t0,
                "total_s": t_total - t0,
                "peak_rss_bytes": max(self_rss, child_rss),
                "disk_write_bytes": disk_bytes,
                "rows_verified": row_count,
                "first_row_non_null": first_row is not None,
                "details": {
                    "convert_s": t_conv - t0,
                    "duckdb_load_s": t_total - t_conv,
                    "parquet_bytes": pq_bytes,
                    "self_vmhwm_bytes": self_rss,
                    "child_maxrss_bytes": child_rss,
                },
            }

        elif wf == "E_agg":
            import duckdb

            pq_path = work_dir / f"{dbc_path.stem}.parquet"
            t0 = time.perf_counter()
            subprocess.run(
                [
                    str(DATASUS_CLI_BIN),
                    "convert",
                    "--keep-names",
                    "-q",
                    str(dbc_path),
                    "-o",
                    str(pq_path),
                ],
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
            t_conv = time.perf_counter()
            con = duckdb.connect(config={"temp_directory": str(duckdb_tmp)})
            rows = con.execute(
                AGG_SQL_TEMPLATE.format(source=f"read_parquet({sql_quote(pq_path)})")
            ).fetchall()
            t_total = time.perf_counter()
            self_rss = read_vmhwm_bytes()
            child_rss = read_child_maxrss_bytes()
            pq_bytes = pq_path.stat().st_size
            disk_bytes = pq_bytes + dir_file_bytes(duckdb_tmp)
            con.close()
            total_val = sum(r[1] for r in rows if r[1] is not None)
            result = {
                "status": "ok",
                "first_row_s": t_total - t0,
                "total_s": t_total - t0,
                "peak_rss_bytes": max(self_rss, child_rss),
                "disk_write_bytes": disk_bytes,
                "rows_verified": len(rows),
                "details": {
                    "convert_s": t_conv - t0,
                    "query_s": t_total - t_conv,
                    "parquet_bytes": pq_bytes,
                    "agg_groups": len(rows),
                    "agg_val_tot_sum": float(total_val),
                    "self_vmhwm_bytes": self_rss,
                    "child_maxrss_bytes": child_rss,
                },
            }

        elif wf == "E_cached_agg":
            import duckdb

            assert cached_pq is not None and cached_pq.exists()
            t0 = time.perf_counter()
            con = duckdb.connect(config={"temp_directory": str(duckdb_tmp)})
            rows = con.execute(
                AGG_SQL_TEMPLATE.format(source=f"read_parquet({sql_quote(cached_pq)})")
            ).fetchall()
            t_total = time.perf_counter()
            self_rss = read_vmhwm_bytes()
            child_rss = read_child_maxrss_bytes()
            disk_bytes = dir_file_bytes(duckdb_tmp)
            con.close()
            total_val = sum(r[1] for r in rows if r[1] is not None)
            result = {
                "status": "ok",
                "first_row_s": t_total - t0,
                "total_s": t_total - t0,
                "peak_rss_bytes": max(self_rss, child_rss),
                "disk_write_bytes": disk_bytes,
                "rows_verified": len(rows),
                "details": {
                    "agg_groups": len(rows),
                    "agg_val_tot_sum": float(total_val),
                    "cached_parquet_bytes": cached_pq.stat().st_size,
                    "self_vmhwm_bytes": self_rss,
                    "child_maxrss_bytes": child_rss,
                },
            }

        else:
            result = {"status": "error", "error": f"Unknown workflow: {wf}"}
    except Exception as exc:
        result = {"status": "error", "error": f"{type(exc).__name__}: {exc}"}

    out_json.write_text(json.dumps(result), encoding="utf-8")
    return 0 if result.get("status") in ("ok", "skipped") else 1


# ---------------------------------------------------------------------------
# Setup, fixture verification, and metadata collection
# ---------------------------------------------------------------------------

def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def ensure_fixtures(selected: list[BenchFile], skip_download: bool) -> None:
    if any(bf.via_fetch_sh and not bf.path.exists() for bf in selected):
        if skip_download:
            raise SystemExit("Missing SIH fixtures in test/data/; run scripts/fixtures/fetch.sh")
        print("Fetching standard SIH fixtures via scripts/fixtures/fetch.sh ...", flush=True)
        subprocess.run([str(ROOT / "scripts" / "fixtures" / "fetch.sh")], check=True, cwd=ROOT)

    for bf in selected:
        if not bf.path.exists():
            if skip_download:
                raise SystemExit(f"Missing fixture {bf.path}; re-run without --skip-download")
            bf.path.parent.mkdir(parents=True, exist_ok=True)
            url = f"{FTP_BASE}/{bf.path.name}"
            partial = bf.path.with_suffix(".dbc.part")
            print(f"Downloading {bf.path.name} from {url} ...", flush=True)
            try:
                subprocess.run(
                    [
                        "curl",
                        "--fail",
                        "--silent",
                        "--show-error",
                        "--connect-timeout",
                        "15",
                        "--max-time",
                        "600",
                        "--proto",
                        "=ftp",
                        "--output",
                        str(partial),
                        url,
                    ],
                    check=True,
                )
                got = sha256_file(partial)
                if got != bf.sha256:
                    raise SystemExit(
                        f"SHA-256 mismatch for downloaded {bf.path.name}: "
                        f"expected {bf.sha256}, got {got}"
                    )
                partial.replace(bf.path)
            finally:
                if partial.exists():
                    partial.unlink()
        got = sha256_file(bf.path)
        if got != bf.sha256:
            raise SystemExit(
                f"SHA-256 mismatch for existing {bf.path}: expected {bf.sha256}, got {got}"
            )
        print(f"Fixture OK: {bf.rel_path} (sha256 {got[:12]}...)", flush=True)


def ensure_tools(ext_path: Path, selected_workflows: set[str]) -> dict[str, Any]:
    if any(w.startswith("A_") for w in selected_workflows) and not ext_path.exists():
        raise SystemExit(
            f"Extension not found at {ext_path}.\n"
            "Build it first with: git submodule update --init && make configure && make release"
        )

    if any(w in ("B_datasus_dbc_dbfread", "D_datasus_dbc_read_dbf") for w in selected_workflows):
        if not DBC2DBF_BIN.exists():
            print("Building scripts/fixtures/dbc2dbf (release) ...", flush=True)
            subprocess.run(
                [
                    "cargo",
                    "build",
                    "--release",
                    "--manifest-path",
                    str(DBC2DBF_MANIFEST),
                    "--target-dir",
                    str(DBC2DBF_TARGET),
                    "--locked",
                ],
                check=True,
                cwd=ROOT,
            )

    if any(w.startswith("E_") for w in selected_workflows):
        if not DATASUS_CLI_BIN.exists():
            print(f"Installing datasus-cli {DATASUS_CLI_VERSION} into {BENCH_TOOLS_ROOT} ...", flush=True)
            BENCH_TOOLS_ROOT.mkdir(parents=True, exist_ok=True)
            subprocess.run(
                [
                    "cargo",
                    "install",
                    "--version",
                    DATASUS_CLI_VERSION,
                    "--locked",
                    "datasus-cli",
                    "--root",
                    str(BENCH_TOOLS_ROOT),
                ],
                check=True,
                cwd=ROOT,
            )

    pysus_env: dict[str, Any] = {}
    if "C2_pysus_pandas" in selected_workflows:
        print("Warming PySUS 2.12.2 uv environment ...", flush=True)
        proc = subprocess.run(
            ["uv", "run", "--script", str(PYSUS_WORKER), "--check-env"],
            check=True,
            capture_output=True,
            text=True,
            cwd=ROOT,
        )
        pysus_env = json.loads(proc.stdout.strip().splitlines()[-1])
    return pysus_env


def collect_system_info(ext_path: Path, pysus_env: dict[str, Any]) -> dict[str, Any]:
    import dbfread
    import duckdb
    import pandas
    import psutil
    import pyarrow
    import pyreaddbc

    def cmd_out(cmd: list[str]) -> str:
        try:
            return subprocess.check_output(cmd, text=True, stderr=subprocess.DEVNULL).strip()
        except Exception:
            return ""

    cpu_model = ""
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.exists():
        for line in cpuinfo.read_text(encoding="utf-8").splitlines():
            if line.startswith("model name"):
                cpu_model = line.split(":", 1)[1].strip()
                break

    os_pretty = platform.platform()
    os_release = Path("/etc/os-release")
    if os_release.exists():
        for line in os_release.read_text(encoding="utf-8").splitlines():
            if line.startswith("PRETTY_NAME="):
                os_pretty = line.split("=", 1)[1].strip().strip('"')
                break

    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    ext_version = ""
    if ext_path.exists():
        con.execute(f"LOAD {sql_quote(ext_path)}")
        ext_version = str(con.execute("SELECT read_dbc_version()").fetchone()[0])
    con.close()

    return {
        "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hardware": {
            "cpu_model": cpu_model or platform.processor(),
            "physical_cores": psutil.cpu_count(logical=False),
            "logical_cpus": psutil.cpu_count(logical=True),
            "ram_bytes": int(psutil.virtual_memory().total),
            "disk_lsblk": cmd_out(["lsblk", "-d", "-o", "NAME,MODEL,ROTA,SIZE,TRAN"]),
            "bench_fs": cmd_out(["df", "-Th", str(ROOT)]),
            "os": os_pretty,
            "kernel": platform.release(),
            "arch": platform.machine(),
        },
        "software": {
            "git_commit": cmd_out(["git", "rev-parse", "HEAD"]),
            "git_branch": cmd_out(["git", "rev-parse", "--abbrev-ref", "HEAD"]),
            "read_dbc_version": ext_version,
            "rustc": cmd_out(["rustc", "--version"]),
            "uv": cmd_out(["uv", "--version"]),
            "python": sys.version.split()[0],
            "duckdb": duckdb.__version__,
            "dbfread": getattr(dbfread, "__version__", "2.0.7"),
            "pyreaddbc": getattr(pyreaddbc, "version", "2.0.4"),
            "pandas": pandas.__version__,
            "pyarrow": pyarrow.__version__,
            "psutil": psutil.__version__,
            "datasus_dbc_crate": "0.1.1",
            "datasus_cli": cmd_out([str(DATASUS_CLI_BIN), "--version"]) if DATASUS_CLI_BIN.exists() else "",
            "pysus_worker": pysus_env,
        },
    }


def inspect_files(
    selected: list[BenchFile],
    ext_path: Path,
    cached_pq_dir: Path,
    need_cached_pq: bool,
) -> dict[str, dict[str, Any]]:
    import duckdb
    from pyreaddbc import dbc2dbf

    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD {sql_quote(ext_path)}")

    info: dict[str, dict[str, Any]] = {}
    cached_pq_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=BENCH_TMP_BASE, prefix="meta_") as td:
        for bf in selected:
            row_count = int(
                con.execute(f"SELECT count(*) FROM read_dbc({sql_quote(bf.path)})").fetchone()[0]
            )
            cols = con.execute(
                f"DESCRIBE SELECT * FROM read_dbc({sql_quote(bf.path)})"
            ).fetchall()
            agg = con.execute(
                f"SELECT count(*), sum(total_val) FROM "
                f"({AGG_SQL_TEMPLATE.format(source=f'read_dbc({sql_quote(bf.path)})')})"
            ).fetchone()
            tmp_dbf = Path(td) / f"{bf.name}.dbf"
            dbc2dbf(str(bf.path), str(tmp_dbf))
            dbf_bytes = tmp_dbf.stat().st_size
            tmp_dbf.unlink()

            pq_path = cached_pq_dir / f"{bf.name}.parquet"
            pq_bytes = 0
            if need_cached_pq and DATASUS_CLI_BIN.exists():
                subprocess.run(
                    [
                        str(DATASUS_CLI_BIN),
                        "convert",
                        "--keep-names",
                        "-q",
                        "--force",
                        str(bf.path),
                        "-o",
                        str(pq_path),
                    ],
                    check=True,
                )
                pq_bytes = pq_path.stat().st_size

            info[bf.name] = {
                "name": bf.name,
                "size_tier": bf.size_tier,
                "rel_path": bf.rel_path,
                "sha256": bf.sha256,
                "dbc_bytes": bf.path.stat().st_size,
                "dbf_bytes": dbf_bytes,
                "parquet_bytes": pq_bytes,
                "rows": row_count,
                "columns": len(cols),
                "agg_groups": int(agg[0]),
                "agg_val_tot_sum": str(agg[1]),
                "cached_parquet_path": str(pq_path) if pq_bytes else "",
            }
    con.close()
    return info


def warm_page_cache(path: Path) -> None:
    with path.open("rb") as f:
        while f.read(1 << 20):
            pass


def invoke_workflow(
    wf: str,
    bf: BenchFile,
    fmeta: dict[str, Any],
    ext_path: Path,
    rep: int,
) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(dir=BENCH_TMP_BASE, prefix=f"{bf.name}_{wf}_r{rep}_") as td:
        work_dir = Path(td)
        out_json = work_dir / "result.json"
        if wf == "C2_pysus_pandas":
            cmd = [
                "uv",
                "run",
                "--script",
                str(PYSUS_WORKER),
                "--file",
                str(bf.path),
                "--work-dir",
                str(work_dir),
                "--dbf-bytes",
                str(fmeta["dbf_bytes"]),
                "--out-json",
                str(out_json),
            ]
        else:
            cmd = [
                sys.executable,
                str(Path(__file__).resolve()),
                "--worker",
                "--workflow",
                wf,
                "--file",
                str(bf.path),
                "--ext",
                str(ext_path),
                "--work-dir",
                str(work_dir),
                "--out-json",
                str(out_json),
            ]
            if wf == "E_cached_agg" and fmeta.get("cached_parquet_path"):
                cmd += ["--cached-parquet", fmeta["cached_parquet_path"]]

        proc = subprocess.run(cmd, capture_output=True, text=True, cwd=ROOT)
        if out_json.exists():
            payload = json.loads(out_json.read_text(encoding="utf-8"))
        else:
            payload = {
                "status": "error",
                "error": f"Worker exited {proc.returncode} without JSON output: {proc.stderr.strip()}",
            }
        payload["file"] = bf.name
        payload["workflow"] = wf
        payload["rep"] = rep
        return payload


# ---------------------------------------------------------------------------
# Aggregation & reporting
# ---------------------------------------------------------------------------

def summarize(runs: list[dict[str, Any]], selected_files: list[BenchFile], selected_wfs: list[tuple[str, str]]) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for bf in selected_files:
        for wf, desc in selected_wfs:
            group = [r for r in runs if r["file"] == bf.name and r["workflow"] == wf]
            ok = [r for r in group if r.get("status") == "ok"]
            skipped = [r for r in group if r.get("status") == "skipped"]
            errors = [r for r in group if r.get("status") == "error"]
            if ok:
                first_vals = [r["first_row_s"] for r in ok]
                total_vals = [r["total_s"] for r in ok]
                rss_vals = [r["peak_rss_bytes"] for r in ok]
                disk_vals = [r["disk_write_bytes"] for r in ok]
                rows.append(
                    {
                        "file": bf.name,
                        "workflow": wf,
                        "description": desc,
                        "status": "ok",
                        "reps_ok": len(ok),
                        "reps_failed": len(errors),
                        "first_row_median_s": statistics.median(first_vals),
                        "first_row_min_s": min(first_vals),
                        "total_median_s": statistics.median(total_vals),
                        "total_min_s": min(total_vals),
                        "peak_rss_median_mib": statistics.median(rss_vals) / (1024 * 1024),
                        "peak_rss_min_mib": min(rss_vals) / (1024 * 1024),
                        "disk_write_median_bytes": int(statistics.median(disk_vals)),
                        "disk_write_min_bytes": int(min(disk_vals)),
                        "note": "",
                    }
                )
            elif skipped:
                rows.append(
                    {
                        "file": bf.name,
                        "workflow": wf,
                        "description": desc,
                        "status": "skipped",
                        "reps_ok": 0,
                        "reps_failed": 0,
                        "first_row_median_s": None,
                        "first_row_min_s": None,
                        "total_median_s": None,
                        "total_min_s": None,
                        "peak_rss_median_mib": None,
                        "peak_rss_min_mib": None,
                        "disk_write_median_bytes": None,
                        "disk_write_min_bytes": None,
                        "note": skipped[0].get("error", "skipped"),
                    }
                )
            else:
                rows.append(
                    {
                        "file": bf.name,
                        "workflow": wf,
                        "description": desc,
                        "status": "error",
                        "reps_ok": 0,
                        "reps_failed": len(errors),
                        "first_row_median_s": None,
                        "first_row_min_s": None,
                        "total_median_s": None,
                        "total_min_s": None,
                        "peak_rss_median_mib": None,
                        "peak_rss_min_mib": None,
                        "disk_write_median_bytes": None,
                        "disk_write_min_bytes": None,
                        "note": errors[0].get("error", "error") if errors else "not run",
                    }
                )
    return rows


def fmt_time(sec: float | None) -> str:
    if sec is None:
        return "—"
    if sec < 1.0:
        return f"{sec * 1000:.1f} ms"
    return f"{sec:.2f} s"


def fmt_bytes(b: int | None) -> str:
    if b is None:
        return "—"
    if b == 0:
        return "0 B"
    if b < 1024 * 1024:
        return f"{b / 1024:.1f} KiB"
    return f"{b / (1024 * 1024):.1f} MiB"


def write_outputs(
    out_dir: Path,
    sys_info: dict[str, Any],
    file_meta: dict[str, dict[str, Any]],
    runs: list[dict[str, Any]],
    summary_rows: list[dict[str, Any]],
    reps: int,
) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)

    # Strip local-only cached_parquet_path from committed JSON metadata.
    clean_files = {
        k: {fk: fv for fk, fv in v.items() if fk != "cached_parquet_path"}
        for k, v in file_meta.items()
    }

    payload = {
        "reps": reps,
        "system": sys_info,
        "files": clean_files,
        "summary": summary_rows,
        "runs": runs,
    }
    (out_dir / "results.json").write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")

    run_fields = [
        "file",
        "workflow",
        "rep",
        "status",
        "first_row_s",
        "total_s",
        "peak_rss_bytes",
        "disk_write_bytes",
        "rows_verified",
        "error",
    ]
    with (out_dir / "results.csv").open("w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=run_fields)
        w.writeheader()
        for r in runs:
            w.writerow({k: r.get(k, "") for k in run_fields})

    sum_fields = [
        "file",
        "workflow",
        "status",
        "reps_ok",
        "reps_failed",
        "first_row_median_s",
        "first_row_min_s",
        "total_median_s",
        "total_min_s",
        "peak_rss_median_mib",
        "peak_rss_min_mib",
        "disk_write_median_bytes",
        "disk_write_min_bytes",
        "note",
    ]
    with (out_dir / "summary.csv").open("w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=sum_fields)
        w.writeheader()
        for r in summary_rows:
            w.writerow({k: r.get(k, "") for k in sum_fields})


def print_summary(summary_rows: list[dict[str, Any]], selected_files: list[BenchFile]) -> None:
    for bf in selected_files:
        print(f"\n### {bf.name} ({bf.size_tier})\n")
        print(
            "| Workflow | First row (med / min) | Total (med / min) | Peak RSS (med / min) | Disk written |"
        )
        print("|---|---:|---:|---:|---:|")
        for r in summary_rows:
            if r["file"] != bf.name:
                continue
            if r["status"] != "ok":
                note = r["note"].splitlines()[0][:80]
                print(f"| `{r['workflow']}` | {r['status']} | {note} | — | — |")
                continue
            fr = f"{fmt_time(r['first_row_median_s'])} / {fmt_time(r['first_row_min_s'])}"
            tot = f"{fmt_time(r['total_median_s'])} / {fmt_time(r['total_min_s'])}"
            rss = f"{r['peak_rss_median_mib']:.1f} / {r['peak_rss_min_mib']:.1f} MiB"
            disk = fmt_bytes(r["disk_write_median_bytes"])
            print(f"| `{r['workflow']}` | {fr} | {tot} | {rss} | {disk} |")


def main() -> int:
    ap = argparse.ArgumentParser(description="Benchmark read_dbc vs convert-first DATASUS workflows.")
    ap.add_argument("--reps", type=int, default=5, help="Repetitions per workflow and file (default: 5)")
    ap.add_argument(
        "--files",
        nargs="+",
        choices=[f.name for f in FILES],
        default=[f.name for f in FILES],
        help="Subset of files to benchmark",
    )
    ap.add_argument(
        "--workflows",
        nargs="+",
        choices=[w[0] for w in WORKFLOWS],
        default=[w[0] for w in WORKFLOWS],
        help="Subset of workflows to run",
    )
    ap.add_argument("--ext", type=Path, default=DEFAULT_EXT, help="Path to read_dbc.duckdb_extension")
    ap.add_argument(
        "--out-dir",
        type=Path,
        default=ROOT / "docs" / "bench-results",
        help="Output directory for JSON/CSV results",
    )
    ap.add_argument("--skip-download", action="store_true", help="Fail instead of downloading missing fixtures")

    # Internal worker arguments
    ap.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    ap.add_argument("--workflow", type=str, help=argparse.SUPPRESS)
    ap.add_argument("--file", type=Path, help=argparse.SUPPRESS)
    ap.add_argument("--work-dir", type=Path, help=argparse.SUPPRESS)
    ap.add_argument("--out-json", type=Path, help=argparse.SUPPRESS)
    ap.add_argument("--cached-parquet", type=Path, help=argparse.SUPPRESS)
    args = ap.parse_args()

    if args.worker:
        return run_worker(args)

    selected_files = [f for f in FILES if f.name in args.files]
    selected_wfs = [w for w in WORKFLOWS if w[0] in args.workflows]
    selected_wf_names = {w[0] for w in selected_wfs}

    BENCH_TMP_BASE.mkdir(parents=True, exist_ok=True)
    ensure_fixtures(selected_files, skip_download=args.skip_download)
    pysus_env = ensure_tools(args.ext, selected_wf_names)
    sys_info = collect_system_info(args.ext, pysus_env)

    cached_pq_dir = BENCH_TMP_BASE / "cached_parquet"
    file_meta = inspect_files(
        selected_files,
        args.ext,
        cached_pq_dir,
        need_cached_pq="E_cached_agg" in selected_wf_names,
    )

    runs: list[dict[str, Any]] = []
    skipped_wfs: set[tuple[str, str]] = set()
    started = time.perf_counter()

    try:
        for bf in selected_files:
            fmeta = file_meta[bf.name]
            print(
                f"\n=== {bf.name} ({bf.size_tier}): {fmeta['rows']:,} rows x {fmeta['columns']} cols, "
                f"{fmt_bytes(fmeta['dbc_bytes'])} .dbc -> {fmt_bytes(fmeta['dbf_bytes'])} .dbf ===",
                flush=True,
            )
            warm_page_cache(bf.path)

            for rep in range(1, args.reps + 1):
                for wf, _ in selected_wfs:
                    if (bf.name, wf) in skipped_wfs:
                        continue
                    rec = invoke_workflow(wf, bf, fmeta, args.ext, rep)
                    runs.append(rec)
                    st = rec.get("status")
                    if st == "ok":
                        print(
                            f"  [{bf.name} r{rep}/{args.reps}] {wf:<23} "
                            f"first={fmt_time(rec['first_row_s']):>8}  "
                            f"total={fmt_time(rec['total_s']):>8}  "
                            f"rss={rec['peak_rss_bytes']/(1024*1024):6.1f} MiB  "
                            f"disk={fmt_bytes(rec['disk_write_bytes']):>9}  "
                            f"rows={rec.get('rows_verified')}",
                            flush=True,
                        )
                    elif st == "skipped":
                        skipped_wfs.add((bf.name, wf))
                        first_line = str(rec.get("error", "")).splitlines()[0]
                        print(
                            f"  [{bf.name} r{rep}/{args.reps}] {wf:<23} SKIPPED: {first_line}",
                            flush=True,
                        )
                    else:
                        print(
                            f"  [{bf.name} r{rep}/{args.reps}] {wf:<23} ERROR: {rec.get('error')}",
                            flush=True,
                        )
    finally:
        if cached_pq_dir.exists():
            shutil.rmtree(cached_pq_dir, ignore_errors=True)

    summary_rows = summarize(runs, selected_files, selected_wfs)
    write_outputs(args.out_dir, sys_info, file_meta, runs, summary_rows, args.reps)
    print_summary(summary_rows, selected_files)
    elapsed = time.perf_counter() - started
    print(f"\nWrote raw results to {args.out_dir} in {elapsed:.1f} s.", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

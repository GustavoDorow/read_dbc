#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11,<3.15"
# dependencies = [
#   "pysus==2.12.2",
#   "duckdb==1.5.6",
#   "psutil==7.2.2",
# ]
# ///
"""Isolated benchmark worker for PySUS==2.12.2 (requires duckdb<2.0.0)."""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import resource
import sys
import time
from pathlib import Path


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


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--check-env", action="store_true")
    ap.add_argument("--file", type=Path)
    ap.add_argument("--work-dir", type=Path)
    ap.add_argument("--dbf-bytes", type=int, default=0)
    ap.add_argument("--out-json", type=Path)
    args = ap.parse_args()

    # Redirect noisy library banners (PySUS prints a welcome message on import)
    # away from stdout.
    orig_stdout = sys.stdout
    sys.stdout = sys.stderr
    try:
        import duckdb
        import pandas
        import pysus
        from pysus.api.extensions import DBC

        if args.check_env:
            info = {
                "pysus": pysus.__version__ if hasattr(pysus, "__version__") else "2.12.2",
                "duckdb": duckdb.__version__,
                "pandas": pandas.__version__,
                "python": sys.version.split()[0],
            }
            orig_stdout.write(json.dumps(info) + "\n")
            return 0

        assert args.file and args.work_dir and args.out_json
        os.environ["TMPDIR"] = str(args.work_dir)
        # Keep PySUS cache inside work_dir so nothing touches $HOME.
        pysus.set_cache(str(args.work_dir / "pysus_cache"))
        pysus.disable_progress_bars()

        src_link = args.work_dir / args.file.name
        os.symlink(args.file.resolve(), src_link)
        pq_path = src_link.with_suffix(".parquet")

        t0 = time.perf_counter()
        df = asyncio.run(DBC(path=src_link).load())
        t_df = time.perf_counter()
        con = duckdb.connect()
        con.execute("CREATE TABLE t AS SELECT * FROM df")
        first_row = con.execute("SELECT * FROM t LIMIT 1").fetchone()
        t_total = time.perf_counter()

        self_rss = read_vmhwm_bytes()
        child_rss = read_child_maxrss_bytes()
        peak_rss = max(self_rss, child_rss)

        pq_bytes = pq_path.stat().st_size if pq_path.exists() else 0
        # PySUS DBC.to_parquet() calls pyreaddbc.dbc2dbf() to write the full
        # uncompressed .dbf file, streams it to .parquet, then unlinks the .dbf.
        disk_bytes = args.dbf_bytes + pq_bytes

        # Post-timing verification (does not affect recorded timing or RSS).
        row_count = int(con.execute("SELECT count(*) FROM t").fetchone()[0])
        agg_groups, agg_sum = con.execute(
            "SELECT count(*), sum(s) FROM "
            "(SELECT MUNIC_RES, sum(CAST(VAL_TOT AS DOUBLE)) AS s FROM t GROUP BY 1)"
        ).fetchone()
        con.close()

        result = {
            "status": "ok",
            "first_row_s": t_total - t0,
            "total_s": t_total - t0,
            "peak_rss_bytes": peak_rss,
            "disk_write_bytes": disk_bytes,
            "rows_verified": row_count,
            "first_row_non_null": first_row is not None,
            "details": {
                "load_df_s": t_df - t0,
                "duckdb_load_s": t_total - t_df,
                "self_vmhwm_bytes": self_rss,
                "child_maxrss_bytes": child_rss,
                "dbf_bytes": args.dbf_bytes,
                "parquet_bytes": pq_bytes,
                "duckdb_version": duckdb.__version__,
                "agg_groups": int(agg_groups),
                "agg_val_tot_sum": float(agg_sum),
            },
        }
    except Exception as exc:
        result = {
            "status": "error",
            "error": f"{type(exc).__name__}: {exc}",
        }
    finally:
        sys.stdout = orig_stdout

    if args.out_json:
        args.out_json.write_text(json.dumps(result), encoding="utf-8")
    else:
        print(json.dumps(result))
    return 0 if result.get("status") == "ok" else 1


if __name__ == "__main__":
    raise SystemExit(main())

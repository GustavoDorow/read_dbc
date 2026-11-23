#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["duckdb==2.0.1"]
# ///
"""Parity check: read_dbc against the dbfread references of the SIH fixtures.

For each of the five downloaded SIH files in test/data (create them with
scripts/fixtures/fetch.sh; missing files are skipped with a message), read
`read_dbc(path, encoding := 'latin1')` and compare it with
test/reference/<NAME>.dbfread.jsonl.gz (dbfread 2.0.7, Latin-1, type-tagged
values): column names and order, row count and every value.

  * str   -> VARCHAR, equal text
  * int   -> BIGINT, equal
  * float -> DECIMAL(w,s): the float's repr rounded to the column's scale
             must equal the exact DECIMAL (dbfread parses with float())
  * None  -> NULL

Also checks that encoding := 'auto' (CP850 from the LDID byte) differs from
Latin-1 only in RDSC2501's AUD_JUST values with non-ASCII bytes, that those
decode to Portuguese text, that the other files are identical under both,
and that a union_by_name read of all five files at once has the summed row
count. Prints a summary table, writes it to scripts/parity/RESULTS.md and
exits non-zero on any mismatch.

Usage: uv run --script scripts/parity/parity.py [--build build/release]
"""

from __future__ import annotations

import argparse
import gzip
import json
import sys
import time
from decimal import Decimal
from pathlib import Path

import duckdb

ROOT = Path(__file__).resolve().parents[2]
FILES = ["RDAC0801", "RDRR1501", "RDAP2401", "RDSC2501", "RDAC2601"]
MAX_REPORTED = 10
FAMILY_RESULTS_HEADING = "## DATASUS family parity"


def connect(build: Path) -> duckdb.DuckDBPyConnection:
    ext = build / "read_dbc.duckdb_extension"
    if not ext.exists():
        sys.exit(f"extension not found: {ext} (run `make release` or `make debug`)")
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{ext}'")
    return con


def load_reference(name: str) -> list[dict]:
    path = ROOT / "test" / "reference" / f"{name}.dbfread.jsonl.gz"
    with gzip.open(path, "rt", encoding="utf-8") as f:
        return [json.loads(line) for line in f]


def column_scales(con, path: str) -> dict[str, int | None]:
    scales = {}
    for name, ctype, *_ in con.execute(f"DESCRIBE SELECT * FROM read_dbc('{path}', encoding := 'latin1')").fetchall():
        if ctype.startswith("DECIMAL("):
            scales[name] = int(ctype[len("DECIMAL(") : -1].split(",")[1])
        else:
            scales[name] = None
    return scales


def equal(tag: str, payload, got, scale: int | None) -> bool:
    if tag == "NoneType":
        return got is None
    if got is None:
        return False
    if tag == "str":
        return isinstance(got, str) and got == payload
    if tag == "int":
        if isinstance(got, Decimal):
            return got == Decimal(payload)
        return isinstance(got, int) and not isinstance(got, bool) and got == int(payload)
    if tag == "float":
        if not isinstance(got, Decimal) or scale is None:
            return False
        expected = Decimal(payload).quantize(Decimal(1).scaleb(-scale))
        return got == expected
    if tag == "bool":
        return got is payload
    return False


def compare_file(con, name: str, problems: list[str]) -> dict:
    path = str(ROOT / "test" / "data" / f"{name}.dbc")
    ref = load_reference(name)
    rel = f"test/data/{name}.dbc"
    cur = con.execute(f"SELECT * FROM read_dbc('{path}', encoding := 'latin1')")
    cols = [d[0] for d in cur.description]
    rows = cur.fetchall()
    scales = column_scales(con, path)
    mismatches = 0
    ref_cols = list(ref[0].keys()) if ref else []
    if cols != ref_cols:
        mismatches += 1
        problems.append(f"{rel}: columns differ: {cols} vs reference {ref_cols}")
    if len(rows) != len(ref):
        mismatches += 1
        problems.append(f"{rel}: {len(rows)} rows but the reference has {len(ref)}")
    compared = 0
    for i, (row, expected) in enumerate(zip(rows, ref)):
        for j, col in enumerate(cols):
            if col not in expected:
                continue
            tag, payload = expected[col]
            compared += 1
            if not equal(tag, payload, row[j], scales.get(col)):
                mismatches += 1
                if mismatches <= MAX_REPORTED:
                    problems.append(f"{rel}: row {i}, {col}: got {row[j]!r}, reference {tag} {payload!r}")
    return {"file": rel, "rows": len(rows), "columns": len(cols), "compared": compared, "mismatches": mismatches}


def check_auto(con, name: str, problems: list[str]) -> str:
    """auto (CP850) vs latin1: only RDSC2501 AUD_JUST non-ASCII values may differ."""
    path = str(ROOT / "test" / "data" / f"{name}.dbc")
    auto = con.execute(f"SELECT * FROM read_dbc('{path}')").fetchall()
    latin = con.execute(f"SELECT * FROM read_dbc('{path}', encoding := 'latin1')").fetchall()
    cols = [d[0] for d in con.execute(f"SELECT * FROM read_dbc('{path}') LIMIT 0").description]
    diffs: dict[str, int] = {}
    portuguese_ok = True
    examples = []
    for a, l in zip(auto, latin):
        for j, (x, y) in enumerate(zip(a, l)):
            if x != y:
                diffs[cols[j]] = diffs.get(cols[j], 0) + 1
                # CP850 bytes decoded as Latin-1 give C1 controls / odd symbols;
                # re-encoding the Latin-1 text and decoding as CP850 must match auto.
                if y.encode("latin-1").decode("cp850") != x:
                    portuguese_ok = False
                if len(examples) < 3:
                    examples.append(x)
    if len(auto) != len(latin):
        problems.append(f"{name}: auto and latin1 row counts differ")
    if name == "RDSC2501":
        if set(diffs) != {"AUD_JUST"} or diffs["AUD_JUST"] < 55:
            problems.append(f"{name}: expected 55+ AUD_JUST differences between auto and latin1, got {diffs}")
        if not portuguese_ok:
            problems.append(f"{name}: auto AUD_JUST values are not the CP850 reading of the bytes")
        bad = [e for e in examples if any(0x80 <= ord(c) <= 0x9F for c in e)]
        if bad:
            problems.append(f"{name}: auto AUD_JUST still contains C1 control characters: {bad!r}")
        return f"{name}: {diffs.get('AUD_JUST', 0)} AUD_JUST values differ; e.g. {examples[:2]!r}"
    if diffs:
        problems.append(f"{name}: auto and latin1 differ in {diffs}")
    return f"{name}: identical under auto and latin1"


def main() -> int:
    ap = argparse.ArgumentParser()
    default_build = ROOT / "build" / "release"
    if not (default_build / "read_dbc.duckdb_extension").exists():
        default_build = ROOT / "build" / "debug"
    ap.add_argument("--build", type=Path, default=default_build)
    args = ap.parse_args()
    con = connect(args.build)

    present = [n for n in FILES if (ROOT / "test" / "data" / f"{n}.dbc").exists()]
    for n in FILES:
        if n not in present:
            print(f"SKIP: test/data/{n}.dbc is missing (run scripts/fixtures/fetch.sh)")
    if not present:
        print("SKIP: no SIH fixtures; nothing to compare")
        return 0

    problems: list[str] = []
    results = []
    started = time.perf_counter()
    for name in present:
        results.append(compare_file(con, name, problems))
    notes = [check_auto(con, name, problems) for name in present]

    paths = ", ".join(f"'{ROOT / 'test' / 'data' / (n + '.dbc')}'" for n in present)
    union_rows, union_cols, union_files = con.execute(
        f"SELECT count(*), (SELECT count(*) FROM (DESCRIBE SELECT * FROM read_dbc([{paths}], union_by_name := true))), "
        f"count(DISTINCT filename) FROM read_dbc([{paths}], union_by_name := true, encoding := 'latin1', filename := true)"
    ).fetchone()
    expected_rows = sum(r["rows"] for r in results)
    union_mismatch = int(union_rows != expected_rows or union_files != len(present))
    if union_mismatch:
        problems.append(f"union_by_name: {union_rows} rows from {union_files} files, expected {expected_rows} from {len(present)}")
    # Values of the union must equal the per-file reads (checked via EXCEPT ALL on a shared column subset).
    union_value_mismatch = con.execute(
        f"""SELECT count(*) FROM (
              SELECT N_AIH, VAL_TOT, DT_INTER, filename FROM read_dbc([{paths}], union_by_name := true, encoding := 'latin1', filename := true)
              EXCEPT ALL
              ({' UNION ALL '.join(f"SELECT N_AIH, VAL_TOT, DT_INTER, filename FROM read_dbc('{ROOT / 'test' / 'data' / (n + '.dbc')}', encoding := 'latin1', filename := true)" for n in present)})
            )"""
    ).fetchone()[0]
    if union_value_mismatch:
        problems.append(f"union_by_name: {union_value_mismatch} rows differ from the per-file reads")
    elapsed = time.perf_counter() - started

    lines = [
        "# read_dbc parity results",
        "",
        f"Extension: `{args.build.relative_to(ROOT) if args.build.is_relative_to(ROOT) else args.build}`, DuckDB {duckdb.__version__}. "
        "Reference: dbfread 2.0.7 (Latin-1), `test/reference/*.dbfread.jsonl.gz`. "
        "Generated by `scripts/parity/parity.py`.",
        "",
        "| File | Rows | Columns | Values compared | Mismatches |",
        "|---|---:|---:|---:|---:|",
    ]
    for r in results:
        lines.append(f"| `{r['file']}` | {r['rows']:,} | {r['columns']} | {r['compared']:,} | {r['mismatches']} |")
    lines.append(
        f"| union_by_name of all {len(present)} | {union_rows:,} | {union_cols} | rows = sum, values = per-file reads | "
        f"{union_mismatch + union_value_mismatch} |"
    )
    lines += ["", "Encoding `auto` (CP850 from the LDID byte) vs `latin1`:", ""]
    lines += [f"- {n}" for n in notes]
    lines += ["", f"Total mismatches: {sum(r['mismatches'] for r in results) + union_mismatch + union_value_mismatch}; "
              f"problems: {len(problems)}. Run time {elapsed:.1f} s."]
    if problems:
        lines += ["", "## Problems", ""] + [f"- {p}" for p in problems]
    text = "\n".join(lines) + "\n"
    print(text)
    results_path = ROOT / "scripts" / "parity" / "RESULTS.md"
    if results_path.exists():
        _, heading, family_results = results_path.read_text(encoding="utf-8").partition(
            FAMILY_RESULTS_HEADING
        )
        if heading:
            text += "\n" + heading + family_results
    results_path.write_text(text, encoding="utf-8")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

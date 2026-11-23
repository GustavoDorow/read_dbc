#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["dbfread==2.0.7", "duckdb==2.0.1"]
# ///
"""Compare every active DATASUS family value with dbfread on converted DBFs."""

import argparse
import struct
import subprocess
import sys
from decimal import Decimal
from pathlib import Path
from tempfile import TemporaryDirectory

import duckdb
from dbfread import DBF

from parity import FAMILY_RESULTS_HEADING, MAX_REPORTED, ROOT, connect

FIXTURES = ROOT / "scripts" / "fixtures"
FAMILY_DATA = ROOT / "test" / "data" / "families"
# The language-driver bytes in the pinned family files (format crate's Encoding::from_ldid).
FAMILY_AUTO_ENCODINGS = {0x00: "latin1", 0x03: "cp1252", 0x58: "cp1252"}


def build_converter():
    target = ROOT / "target" / "fixture-converter"
    subprocess.run(
        [
            "cargo", "build", "--locked", "--release",
            "--manifest-path", FIXTURES / "dbc2dbf" / "Cargo.toml",
            "--target-dir", target,
        ],
        check=True,
        stdout=sys.stderr,
    )
    return target / "release" / "fixture-dbc2dbf"


def normalize_dbf_terminator(dbf):
    """Translate DATASUS's NUL header terminator for dbfread in the scratch DBF."""
    with dbf.open("r+b") as out:
        out.seek(8)
        (header_len,) = struct.unpack("<H", out.read(2))
        out.seek(header_len - 1)
        if out.read(1) != b"\x00":
            return False
        out.seek(header_len - 1)
        out.write(b"\x0d")
        return True


def same_family_value(actual, expected):
    if isinstance(actual, Decimal) and type(expected) in (int, float):
        # dbfread returns Python float for fractional numerics. Compare its decimal
        # representation to DuckDB's exact DECIMAL without converting DuckDB to float.
        return actual == Decimal(str(expected))
    return type(actual) is type(expected) and actual == expected


def read_family_rows(con, dbc, encoding):
    query = con.execute("SELECT * FROM read_dbc(?, encoding := ?)", [str(dbc), encoding])
    return [description[0] for description in query.description], query.fetchall()


def compare_latin1(dbc, dbf, columns, rows, problems):
    reference = DBF(dbf, encoding="iso-8859-1")
    if columns != reference.field_names:
        problems.append(f"{dbc.name} column names or order differ from dbfread.")
    compared = mismatches = reference_rows = 0
    column_indices = [(i, name) for i, name in enumerate(columns) if name in reference.field_names]
    for reference_rows, expected in enumerate(reference, start=1):
        if reference_rows > len(rows):
            continue
        row = rows[reference_rows - 1]
        for index, name in column_indices:
            compared += 1
            if not same_family_value(row[index], expected[name]):
                mismatches += 1
                if mismatches <= MAX_REPORTED:
                    problems.append(
                        f"{dbc.name} row {reference_rows}, {name} differs from dbfread "
                        f"(DuckDB {row[index]!r}, dbfread {expected[name]!r})."
                    )
    if len(rows) != reference_rows:
        problems.append(
            f"{dbc.name} returned {len(rows):,} rows, dbfread returned {reference_rows:,}."
        )
    return compared, mismatches


def check_family_auto(con, dbc, columns, rows, problems):
    with dbc.open("rb") as source:
        ldid = source.read(30)[29]
    selected = FAMILY_AUTO_ENCODINGS[ldid]
    auto_columns, auto_rows = read_family_rows(con, dbc, "auto")
    explicit = (columns, rows) if selected == "latin1" else read_family_rows(con, dbc, selected)
    if (auto_columns, auto_rows) != explicit:
        problems.append(f"{dbc.name} auto output differs from explicit {selected}.")
    non_ascii = sum(
        isinstance(value, str) and not value.isascii() for row in auto_rows for value in row
    )
    verdict = f"`{selected}`, ASCII only"
    note = None
    if non_ascii:
        verdict = f"`{selected}`, {non_ascii:,} non-ASCII values"
        if "ALVARA" in auto_columns:
            index = auto_columns.index("ALVARA")
            examples = sorted({row[index] for row in auto_rows if not row[index].isascii()})
            rendered = ", ".join(f"`{value}`" for value in examples[:3])
            note = f"{dbc.name} `ALVARA` under auto includes {rendered}."
    return ldid, verdict, note


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", type=Path, default=ROOT / "build" / "debug")
    args = parser.parse_args()
    con = connect(args.build)
    manifest = (FIXTURES / "families" / "files.tsv").read_text(encoding="utf-8")
    fixtures = [
        (family, FAMILY_DATA / Path(ftp_path).name)
        for family, ftp_path in (line.split("\t") for line in manifest.splitlines())
    ]
    for _, dbc in fixtures:
        if not dbc.is_file():
            parser.exit(1, f"Missing {dbc}. Run scripts/fixtures/fetch-families.sh.\n")
    converter = build_converter()
    totals = mismatches = 0
    problems = []
    notes = []
    lines = [
        FAMILY_RESULTS_HEADING,
        "",
        f"DuckDB {duckdb.__version__} with `encoding := 'latin1'` and dbfread 2.0.7 with ISO-8859-1.",
        "The reference DBFs were produced by the existing datasus-dbc converter.",
        "",
        "| File | Family | Rows | Cols | Values compared | Mismatches | LDID (byte 29) | Auto encoding |",
        "|---|---|---:|---:|---:|---:|---|---|",
    ]
    with TemporaryDirectory(prefix="read-dbc-families-") as temp:
        for family, dbc in fixtures:
            dbf = Path(temp) / dbc.with_suffix(".dbf").name
            subprocess.run([converter, dbc, dbf], check=True, stdout=sys.stderr)
            if normalize_dbf_terminator(dbf):
                notes.append(
                    f"{dbc.name} has a NUL field-descriptor terminator. The scratch DBF uses "
                    "`0x0D` at that byte for dbfread. The header fields and record bytes are unchanged."
                )
            columns, rows = read_family_rows(con, dbc, "latin1")
            compared, differences = compare_latin1(dbc, dbf, columns, rows, problems)
            ldid, verdict, note = check_family_auto(con, dbc, columns, rows, problems)
            lines.append(
                f"| `{dbc.name}` | {family} | {len(rows):,} | {len(columns)} | {compared:,} | "
                f"{differences:,} | `0x{ldid:02X}` | {verdict} |"
            )
            totals += compared
            mismatches += differences
            if note:
                notes.append(note)
    lines += ["", f"Total: {totals:,} values compared, {mismatches:,} value mismatches.", ""]
    lines += [f"- {note}" for note in notes]
    if problems:
        lines += ["", "### Problems", ""] + [f"- {problem}" for problem in problems]
    print("\n".join(lines))
    return int(bool(mismatches or problems))


if __name__ == "__main__":
    sys.exit(main())

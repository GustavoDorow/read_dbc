# /// script
# requires-python = ">=3.11"
# dependencies = ["dbfread==2.0.7"]
# ///
"""Write typed dbfread and fixed-width byte oracles for the DATASUS SIH fixtures."""

import argparse
import gzip
import hashlib
import json
import os
import re
import struct
import sys
import zlib
from collections import Counter
from contextlib import contextmanager
from datetime import date, datetime
from decimal import Decimal
from io import TextIOWrapper
from pathlib import Path

if not sys.flags.isolated:
    os.execv(sys.executable, [sys.executable, "-I", __file__, *sys.argv[1:]])

from dbfread import DBF

LATIN1 = "iso-8859-1"
DATE_LIKE_MIN_SHARE = 0.5
# dBase language-driver IDs (header byte 29) that are relevant here.
CODEPAGES = {0x00: None, 0x01: "cp437", 0x02: "cp850", 0x03: "cp1252", 0x57: "cp1252"}
LEADING_ZERO = re.compile(rb"[+-]?0[0-9]")
NUMERIC_LITERAL = re.compile(rb"[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[Ee][+-]?[0-9]+)?")
INTEGER_LITERAL = re.compile(rb"[+-]?[0-9]+")


class DigestingStream:
    """Text sink that hashes the uncompressed UTF-8 JSON Lines alongside writing them."""

    def __init__(self, stream):
        self.stream = stream
        self.sha256 = hashlib.sha256()

    def write(self, text):
        self.sha256.update(text.encode("utf-8"))
        self.stream.write(text)


@contextmanager
def open_gzip_jsonl(path):
    """Omit the filename and timestamp from gzip headers for reproducible output."""
    with path.open("wb") as output:
        with gzip.GzipFile(filename="", fileobj=output, mode="wb", mtime=0, compresslevel=6) as compressed:
            with TextIOWrapper(compressed, encoding="utf-8", newline="\n") as stream:
                yield DigestingStream(stream)


def write_jsonl_record(output, record):
    output.write(json.dumps(record, ensure_ascii=True, separators=(",", ":"), allow_nan=False) + "\n")


def tag_dbfread_value(value):
    """Serialize Python values without relying on JSON number precision."""
    python_type = type(value)
    if value is None or python_type in (str, bool):
        payload = value
    elif python_type in (int, float):
        payload = repr(value)
    elif python_type is Decimal:
        payload = str(value)
    elif python_type in (date, datetime):
        payload = value.isoformat()
    elif python_type is bytes:
        payload = value.hex()
    else:
        raise TypeError(f"Unsupported dbfread value type: {python_type.__name__}")
    return [python_type.__name__, payload]


def write_dbfread_oracle(dbf_path, output_path):
    """Use dbfread defaults and retain the descriptor order in each record dict."""
    records = [dict(record) for record in DBF(str(dbf_path), encoding=LATIN1)]
    value_types = {}
    with open_gzip_jsonl(output_path) as output:
        for record in records:
            tagged = {field: tag_dbfread_value(value) for field, value in record.items()}
            for field, (type_name, _) in tagged.items():
                value_types.setdefault(field, Counter())[type_name] += 1
            write_jsonl_record(output, tagged)
    return len(records), value_types, output.sha256.hexdigest()


def verify_dbc_crc(dbc_path, dbf_path):
    """Check the DBC's stored CRC-32 over the raw header plus the decompressed record area."""
    dbc = dbc_path.read_bytes()
    dbf = dbf_path.read_bytes()
    header_len = struct.unpack_from("<H", dbc, 8)[0]
    stored = struct.unpack_from("<I", dbc, header_len)[0]
    computed = zlib.crc32(dbc[:header_len] + dbf[header_len:])
    return {
        "header_matches_dbf": dbc[:header_len] == dbf[:header_len],
        "stored_hex": f"{stored:08x}",
        "computed_hex": f"{computed:08x}",
        "pass": stored == computed,
        "covered_bytes": len(dbf),
    }


def read_dbf_header(dbf):
    """Read the dBase III header and field descriptors using their on-disk offsets."""
    header = dbf.read(32)
    if len(header) != 32 or header[0] != 0x03:
        raise ValueError("Expected a complete dBase III DBF header (version 0x03)")
    n_records, header_len, record_len = struct.unpack_from("<IHH", header, 4)
    if header_len < 33 or (header_len - 33) % 32:
        raise ValueError(f"Invalid DBF descriptor area length: {header_len}")
    descriptors = dbf.read(header_len - 32)
    if len(descriptors) != header_len - 32 or descriptors[-1] != 0x0D:
        raise ValueError("Expected a complete DBF descriptor area ending in 0x0D")
    fields = []
    names = {"_deleted"}
    for offset in range(0, len(descriptors) - 1, 32):
        descriptor = descriptors[offset:offset + 32]
        name = descriptor[:11].split(b"\0", 1)[0].decode(LATIN1)
        if not name or name in names or descriptor[16] == 0:
            raise ValueError(f"Invalid or duplicate DBF field descriptor: {descriptor.hex()}")
        names.add(name)
        fields.append({
            "name": name,
            "type": chr(descriptor[11]),
            "length": descriptor[16],
            "decimals": descriptor[17],
        })
    if 1 + sum(field["length"] for field in fields) != record_len:
        raise ValueError("DBF field widths and deletion flag do not match the record length")
    return {
        "version_byte": header[0],
        "n_records": n_records,
        "header_len": header_len,
        "record_len": record_len,
        "codepage_byte": header[29],
        "codepage_name": CODEPAGES.get(header[29], "unknown"),
    }, fields


def record_profile_examples(values, predicate):
    """Count matching physical values and return up to three distinct raw examples."""
    count = 0
    examples = []
    for raw, trimmed, occurrences in values:
        if predicate(raw, trimmed):
            count += occurrences
            if len(examples) < 3:
                examples.append(raw.decode(LATIN1))
    return {"count": count, "examples": examples}


def is_calendar_date(value):
    if len(value) != 8 or not value.isascii() or not value.isdigit():
        return False
    try:
        date.fromisoformat(value.decode("ascii"))
    except ValueError:
        return False
    return True


def profile_dbf_field(field, frequencies):
    """Count source-byte patterns with each distinct value weighted by its frequency."""
    values = [(raw, raw.strip(), count) for raw, count in frequencies.items()]
    lengths = [len(trimmed) for _, trimmed, _ in values]
    non_ascii = record_profile_examples(values, lambda raw, _: not raw.isascii())
    non_ascii["examples_as_cp850"] = [example.encode(LATIN1).decode("cp850") for example in non_ascii["examples"]]
    stats = {
        "all_blank_count": sum(count for _, trimmed, count in values if not trimmed),
        "non_ascii": non_ascii,
        "nul_bytes": record_profile_examples(values, lambda raw, _: b"\0" in raw),
        "min_trimmed_length": min(lengths, default=None),
        "max_trimmed_length": max(lengths, default=None),
    }
    if field["type"] in {"N", "C"}:
        stats["leading_zeros"] = record_profile_examples(values, lambda _, value: LEADING_ZERO.match(value))
    if field["type"] == "N":
        rules = {
            "decimal_point": lambda _, value: b"." in value,
            "exponent": lambda _, value: b"e" in value.lower() and NUMERIC_LITERAL.fullmatch(value),
            "sign": lambda _, value: b"+" in value or b"-" in value,
            "non_digit_junk": lambda _, value: value and not NUMERIC_LITERAL.fullmatch(value),
        }
        if field["decimals"] == 0:
            rules["outside_int64"] = lambda _, value: (
                INTEGER_LITERAL.fullmatch(value) and not -(2**63) <= int(value) < 2**63
            )
        stats["numeric"] = {
            name: record_profile_examples(values, predicate) for name, predicate in rules.items()
        }
    if field["type"] == "C" and field["length"] == 8:
        valid = sum(count for _, trimmed, count in values if is_calendar_date(trimmed))
        nonblank = sum(count for _, trimmed, count in values if trimmed)
        stats["character_date"] = {
            "looks_like_date": bool(nonblank) and valid / nonblank >= DATE_LIKE_MIN_SHARE,
            "valid_count": valid,
            "invalid": record_profile_examples(values, lambda _, value: value and not is_calendar_date(value)),
        }
    return {**field, **stats}


def write_raw_oracle(dbf_path, output_path):
    """Write every physical record with the exact field bytes and deletion byte."""
    with dbf_path.open("rb") as dbf, open_gzip_jsonl(output_path) as output:
        header, fields = read_dbf_header(dbf)
        frequencies = [Counter() for field in fields]
        deletion_flags = Counter()
        for index in range(header["n_records"]):
            raw_record = dbf.read(header["record_len"])
            flag = raw_record[:1]
            if len(raw_record) != header["record_len"] or flag not in (b" ", b"*"):
                raise ValueError(f"{dbf_path.name}: short record or invalid deletion flag at row {index + 1}")
            deletion_flags[flag] += 1
            record = {"_deleted": flag.decode(LATIN1)}
            offset = 1
            for field, counts in zip(fields, frequencies, strict=True):
                end = offset + field["length"]
                field_bytes = raw_record[offset:end]
                counts[field_bytes] += 1
                record[field["name"]] = field_bytes.decode(LATIN1)
                offset = end
            write_jsonl_record(output, record)
        trailer = dbf.read()
        raw_sha256 = output.sha256.hexdigest()

    return {
        **header,
        "deleted_records": deletion_flags[b"*"],
        "trailing_byte_count": len(trailer),
        "trailing_bytes_hex": trailer.hex(),
        "has_eof_marker": trailer.startswith(b"\x1a"),
        "raw_jsonl_sha256": raw_sha256,
        "fields": [profile_dbf_field(field, counts) for field, counts in zip(fields, frequencies, strict=True)],
    }


def compare_dbf_layouts(profiles):
    """Identify additions, removals, and descriptor changes between successive periods."""
    layouts = [
        {f["name"]: [f["type"], f["length"], f["decimals"]] for f in profile["fields"]}
        for profile in profiles
    ]
    for index, profile in enumerate(profiles):
        if index == 0:
            profile["layout_changes"] = None
            continue
        prior, current = layouts[index - 1:index + 1]
        profile["layout_changes"] = {
            "compared_with": profiles[index - 1]["name"],
            "added": [name for name in current if name not in prior],
            "removed": [name for name in prior if name not in current],
            "changed": {
                name: {"before": prior[name], "after": descriptor}
                for name, descriptor in current.items() if name in prior and descriptor != prior[name]
            },
        }


def write_profile_markdown(path, profiles):
    """Summarize DBF headers and profile counts in fixture period order."""
    lines = [
        "# DATASUS SIH fixture profiles",
        "",
        "Counts include every physical record. Per-file JSON profiles include up to three distinct raw examples per metric.",
        "",
        "| File | DBC bytes | DBF bytes | Version | Records | Columns | Header bytes | Record bytes | Codepage byte | Deleted | Trailer | DBC CRC-32 |",
        "| --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | ---: | --- | --- |",
    ]
    for p in profiles:
        trailer = p["trailing_bytes_hex"] or "none"
        lines.append(
            f"| [{p['name']}]({p['name']}.profile.json) | {p['dbc_bytes']:,} | {p['dbf_bytes']:,} | "
            f"`0x{p['version_byte']:02x}` | {p['n_records']:,} | {len(p['fields'])} | {p['header_len']} | "
            f"{p['record_len']} | `0x{p['codepage_byte']:02x}` ({p['codepage_name']}) | {p['deleted_records']} | {trailer} | "
            f"{'pass' if p['dbc_crc32']['pass'] else 'FAIL'} `{p['dbc_crc32']['stored_hex']}` |"
        )

    non_ascii = [
        (p["name"], f["name"], f["non_ascii"]) for p in profiles for f in p["fields"] if f["non_ascii"]["count"]
    ]
    lines += ["", "## Non-ASCII values", ""]
    if not non_ascii:
        lines.append("None.")
    for name, field, stats in non_ascii:
        lines.append(f"- **{name}** `{field}`: {stats['count']} values. Examples (Latin-1 decoding → CP850 decoding):")
        for latin1, cp850 in zip(stats["examples"], stats["examples_as_cp850"]):
            lines.append(f"  - `{latin1.rstrip()}` → `{cp850.rstrip()}`")

    lines += ["", "## Layout changes", "", "Each file is compared with the preceding sampled period."]
    for p in profiles:
        changes = p["layout_changes"]
        if changes is None:
            continue
        labels = []
        for key in ("added", "removed"):
            if changes[key]:
                labels.append(f"{key} " + ", ".join(f"`{name}`" for name in changes[key]))
        for name, change in changes["changed"].items():
            before, after = ("/".join(map(str, change[k])) for k in ("before", "after"))
            labels.append(f"`{name}` changed from `{before}` to `{after}` (type/length/decimals)")
        lines.append(f"\n- **{p['name']}** " + (". ".join(labels) or "same layout") + ".")

    descriptors = [{f["name"]: f"{f['type']}/{f['length']}/{f['decimals']}" for f in p["fields"]} for p in profiles]
    all_names = list(dict.fromkeys(name for layout in descriptors for name in layout))
    varying = [name for name in all_names if len({layout.get(name) for layout in descriptors}) > 1]
    lines += [
        "", "### Fields whose descriptor differs across files", "",
        "Type/width/decimals per file; `—` means the field is absent.", "",
        "| Field | " + " | ".join(p["name"] for p in profiles) + " |",
        "| --- |" + " --- |" * len(profiles),
    ]
    for name in varying:
        lines.append(f"| `{name}` | " + " | ".join(layout.get(name, "—") for layout in descriptors) + " |")

    for p in profiles:
        lines += [
            "", f"## {p['name']}", "",
            "| Field | Type | Width | Decimals | Blank | Non-ASCII | Leading zero | Trimmed length | N point / exponent / sign / junk / overflow | C(8) date-like? valid / invalid | dbfread types |",
            "| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- | --- | --- |",
        ]
        for f in p["fields"]:
            leading = f["leading_zeros"]["count"] if "leading_zeros" in f else "—"
            date_stats = f.get("character_date")
            dates = (
                f"{'yes' if date_stats['looks_like_date'] else 'no'} {date_stats['valid_count']} / "
                f"{date_stats['invalid']['count']}" if date_stats else "—"
            )
            types = ", ".join(f"{name} {count}" for name, count in sorted(f["dbfread_types"].items()))
            numeric = f.get("numeric")
            markers = " / ".join(str(metric["count"]) for metric in numeric.values()) if numeric else "—"
            if numeric and "outside_int64" not in numeric:
                markers += " / —"
            lines.append(
                f"| `{f['name']}` | {f['type']} | {f['length']} | {f['decimals']} | {f['all_blank_count']} | "
                f"{f['non_ascii']['count']} | {leading} | {f['min_trimmed_length']}–{f['max_trimmed_length']} | "
                f"{markers} | {dates} | {types} |"
            )
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("dbfs", type=Path, nargs="+")
    args = parser.parse_args()

    profiles = []
    for dbf_path in sorted(args.dbfs, key=lambda path: (path.stem[4:], path.stem)):
        name = dbf_path.stem
        profile = write_raw_oracle(dbf_path, args.output_dir / f"{name}.raw.jsonl.gz")
        active_records, value_types, dbfread_sha256 = write_dbfread_oracle(
            dbf_path, args.output_dir / f"{name}.dbfread.jsonl.gz"
        )
        if active_records != profile["n_records"] - profile["deleted_records"]:
            raise ValueError(f"{name}: dbfread row count differs from the active raw record count")
        if active_records and list(value_types) != [field["name"] for field in profile["fields"]]:
            raise ValueError(f"{name}: dbfread field order differs from the DBF descriptors")
        for field in profile["fields"]:
            field["dbfread_types"] = dict(sorted(value_types.get(field["name"], {}).items()))
        dbc_path = args.data_dir / f"{name}.dbc"
        crc = verify_dbc_crc(dbc_path, dbf_path)
        if not crc["header_matches_dbf"]:
            raise ValueError(f"{name}: DBF header differs from the DBC header")
        profiles.append({
            "name": name,
            "dbc_bytes": dbc_path.stat().st_size,
            "dbf_bytes": dbf_path.stat().st_size,
            "dbc_crc32": crc,
            "dbfread_records": active_records,
            "dbfread_jsonl_sha256": dbfread_sha256,
            **profile,
        })
        print(
            f"{name}: wrote {active_records:,} typed and {profile['n_records']:,} raw rows; "
            f"DBC CRC-32 {'pass' if crc['pass'] else 'FAIL'} ({crc['stored_hex']})",
            flush=True,
        )

    compare_dbf_layouts(profiles)
    for profile in profiles:
        path = args.output_dir / f"{profile['name']}.profile.json"
        path.write_text(json.dumps(profile, indent=2) + "\n", encoding="utf-8")
    write_profile_markdown(args.output_dir / "PROFILE.md", profiles)


if __name__ == "__main__":
    main()

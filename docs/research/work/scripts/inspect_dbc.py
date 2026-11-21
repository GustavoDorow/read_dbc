"""Measure DATASUS DBC layout, stream boundaries, DBF fields, and CRC hypotheses."""

import argparse
from collections import Counter, defaultdict
import ctypes as c
import hashlib
import json
from pathlib import Path
import struct
import zlib


CRC_VARIANTS = {
    # polynomial in the implementation's bit order, reflected, check value
    "IEEE": (0xEDB88320, True, 0xCBF43926),
    "CRC32C": (0x82F63B78, True, 0xE3069283),
    "BZIP2": (0x04C11DB7, False, 0xFC891918),
}
MASK = 0xFFFFFFFF
MAX_OUTPUT = 64 * 1024 * 1024


def load_probe(path):
    """Load the bounded blast wrapper and check CRC-32 reference vectors."""
    probe = c.CDLL(str(path.resolve()))
    probe.probe_dcl.argtypes = [
        c.c_char_p, c.c_size_t, c.POINTER(c.c_char), c.c_size_t,
        c.POINTER(c.c_size_t), c.POINTER(c.c_size_t),
    ]
    probe.probe_dcl.restype = c.c_int
    probe.probe_crc_register.argtypes = [
        c.c_char_p, c.c_size_t, c.c_uint32, c.c_uint32, c.c_int,
    ]
    probe.probe_crc_register.restype = c.c_uint32
    for polynomial, reflected, check in CRC_VARIANTS.values():
        value = probe.probe_crc_register(b"123456789", 9, polynomial, MASK, reflected)
        assert value ^ MASK == check
    return probe


def decode_dcl(probe, payload, output_capacity):
    """Return blast status, all produced bytes, and unused whole input bytes."""
    output = c.create_string_buffer(output_capacity)
    size, unused = c.c_size_t(), c.c_size_t()
    status = probe.probe_dcl(
        payload, len(payload), output, output_capacity, c.byref(size), c.byref(unused)
    )
    return status, output[:size.value], unused.value


def check_stream_controls(probe):
    """Check EOS, truncated input, and reporting of bytes after EOS."""
    stream = bytes.fromhex("00 04 82 24 25 8f 80 7f")
    decoded = b"AIAIAIAIAIAIA"
    assert decode_dcl(probe, stream, 32) == (0, decoded, 0)
    assert decode_dcl(probe, stream + b"tail", 32) == (0, decoded, 4)
    assert decode_dcl(probe, stream[:-1], 32)[0] == 2


def check_crc_and_lengths(probe, stored, ranges, extra_lengths):
    """Compare each CRC register and candidate length in both byte orders."""
    matches = []
    ieee_values = {}
    for name, data in ranges.items():
        ieee_values[name] = f"{zlib.crc32(data):08x}"
        for variant, (polynomial, reflected, _) in CRC_VARIANTS.items():
            for initial in (0, MASK):
                register = probe.probe_crc_register(data, len(data), polynomial, initial, reflected)
                for final_xor in (0, MASK):
                    value = register ^ final_xor
                    if (variant, initial, final_xor) == ("IEEE", MASK, MASK):
                        assert f"{value:08x}" == ieee_values[name]
                    for order in ("little", "big"):
                        if value.to_bytes(4, order) == stored:
                            matches.append(
                                f"{variant} init={initial:08x} xorout={final_xor:08x} {order} {name}"
                            )
    lengths = {name: len(data) for name, data in ranges.items()} | extra_lengths
    length_matches = [
        f"{name} {order}" for name, value in lengths.items() if 0 <= value <= MASK
        for order in ("little", "big") if value.to_bytes(4, order) == stored
    ]
    return matches, length_matches, ieee_values


def inspect_records(header, output, record_length, record_count):
    """Count descriptor patterns, record flags, and field-edge padding bytes."""
    descriptors = [header[pos:pos + 32] for pos in range(32, len(header) - 1, 32)]
    fields = [(chr(field[11]), field[16], field[17]) for field in descriptors]
    field_types = Counter(kind for kind, _, _ in fields)
    decimals = defaultdict(set)
    for kind, _, places in fields:
        decimals[kind].add(places)
    declared_bytes = record_count * record_length
    complete_records = min(len(output) // record_length, record_count)
    complete_bytes = complete_records * record_length
    flags = Counter(output[:complete_bytes:record_length])
    offset = 1
    padding = defaultdict(Counter)
    for kind, width, _ in fields:
        if width > 0 and offset + width <= record_length:
            first = Counter(output[offset:complete_bytes:record_length])
            last = Counter(output[offset + width - 1:complete_bytes:record_length])
            for side, counts in (("first", first), ("last", last)):
                for byte, label in ((0, "NUL"), (32, "space")):
                    padding[kind][f"{side}_{label}"] += counts[byte]
        offset += width
    remainder = output[declared_bytes:]
    return {
        "field_count": len(fields),
        "field_types": field_types,
        "decimals_by_type": {kind: sorted(values) for kind, values in decimals.items()},
        "nonzero_field_address_count": sum(field[12:16] != b"\0" * 4 for field in descriptors),
        "nonzero_descriptor_tail_count": sum(field[18:32] != b"\0" * 14 for field in descriptors),
        "nul_in_field_name_count": sum(0 in field[:11] for field in descriptors),
        "sum_width_plus_flag": offset,
        "declared_record_bytes": declared_bytes,
        "complete_declared_records": complete_records,
        "output_minus_declared_bytes": len(output) - declared_bytes,
        "eof_marker": remainder == b"\x1a",
        "extra_prefix_hex": remainder[:32].hex(),
        "record_flags": {f"{byte:02x}": count for byte, count in flags.items()},
        "record_area_nul_count": output[:complete_bytes].count(0),
        "field_edge_counts": dict(padding),
    }


def analyze_dbc(probe, file_path):
    """Measure a DBC file without normalizing its stored header or output."""
    path = Path(file_path)
    data = path.read_bytes()
    record_count, header_length, record_length = struct.unpack_from("<IHH", data, 4)
    if header_length < 33 or (header_length - 33) % 32 or record_length < 1:
        raise ValueError(f"DBC probe: unexpected DBF dimensions in {path}")
    header = data[:header_length]
    stored = data[header_length:header_length + 4]
    payload = data[header_length + 4:]
    if len(payload) < 2:
        raise ValueError(f"DBC probe: missing payload in {path}")
    status, output, unused = decode_dcl(probe, payload, MAX_OUTPUT)
    no_skip_status, no_skip_output, _ = decode_dcl(probe, data[header_length:], MAX_OUTPUT)
    if status:
        raise ValueError(f"DBC probe: blast at H+4 returned {status} in {path}")

    # Try both positions after removing the field to make a no-skip control.
    stripped_container = header + payload
    stripped_no_skip = decode_dcl(probe, stripped_container[header_length:], MAX_OUTPUT)
    assert stripped_no_skip == (status, output, unused)
    stripped_skip_status = decode_dcl(
        probe, stripped_container[header_length + 4:], MAX_OUTPUT
    )[0]

    stream = payload[:len(payload) - unused]
    records = output[:record_count * record_length]
    normalized_header = header[:-1] + b"\r"
    output_without_final_1a = output.removesuffix(b"\x1a")
    ranges = {
        "fixed_header": header[:32],
        "descriptors": header[32:-1],
        "header": header,
        "header_without_final_byte": header[:-1],
        "header_normalized_to_0D": normalized_header,
        "compressed_to_EOF": payload,
        "compressed_consumed": stream,
        "compressed_consumed_without_DCL_header": stream[2:],
        "trailer": payload[len(stream):],
        "decoded_all": output,
        "decoded_declared_records": records,
        "decoded_without_final_1A": output_without_final_1a,
        "decoded_with_forced_final_1A": output_without_final_1a + b"\x1a",
        "raw_header_plus_decoded_all": header + output,
        "raw_header_plus_declared_records": header + records,
        "raw_header_plus_decoded_without_final_1A": header + output_without_final_1a,
        "raw_header_plus_decoded_with_forced_final_1A": header + output_without_final_1a + b"\x1a",
        "normalized_header_plus_decoded_all": normalized_header + output,
        "normalized_header_plus_declared_records": normalized_header + records,
        "omitted_header_final_byte_plus_decoded_all": header[:-1] + output,
        "raw_header_plus_compressed_to_EOF": header + payload,
        "header_plus_zero_field_plus_compressed_to_EOF": header + bytes(4) + payload,
        "stored_field_plus_compressed_to_EOF": stored + payload,
        "whole_DBC_including_stored_field": data,
    }
    matches, length_matches, ieee_values = check_crc_and_lengths(
        probe, stored, ranges,
        {"declared_record_length": record_length, "declared_record_count": record_count},
    )
    return {
        "file": path.name, "size": len(data), "sha256": hashlib.sha256(data).hexdigest(),
        "version": f"{header[0]:02x}", "language_driver": f"{header[29]:02x}",
        "header_length": header_length, "header_final_byte": f"{header[-1]:02x}",
        "record_length": record_length, "record_count": record_count,
        "literal_mode": stream[0], "dictionary_code": stream[1],
        "dictionary_bytes": 1 << (stream[1] + 6),
        "stored_raw_hex": stored.hex(),
        "compressed_consumed": len(stream), "unused_compressed_bytes": unused,
        "decoded_length": len(output), "no_skip_blast_status": no_skip_status,
        "no_skip_output_length": len(no_skip_output),
        "stripped_skip_blast_status": stripped_skip_status,
        "crc_matches": matches, "length_matches": length_matches,
        "ieee_crc32": ieee_values,
    } | inspect_records(header, output, record_length, record_count)


def main():
    """Emit per-file measurements and all matching hypotheses as JSON."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe", type=Path, required=True)
    parser.add_argument("dbc", nargs="+", type=Path)
    args = parser.parse_args()
    probe = load_probe(args.probe)
    check_stream_controls(probe)
    print(json.dumps([analyze_dbc(probe, path) for path in args.dbc], indent=2))


if __name__ == "__main__":
    main()

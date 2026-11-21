"""Compare the 256 entries at the _crc32 table address in TAB415's IMPBORL.DLL."""

import argparse
from pathlib import Path
import re
import struct
import subprocess


def verify_crc_table(dll_path):
    """Verify the compiled CRC lookup table equals reflected IEEE CRC-32."""
    dump = subprocess.check_output(
        ["objdump", "-s", "--start-address=0x4200c4", "--stop-address=0x4204c4",
         str(dll_path.resolve())],
        text=True,
    )
    rows = re.findall(r"^\s*([0-9a-f]{6}) ((?:[0-9a-f]{8} ){3}[0-9a-f]{8}) ", dump, re.M)
    assert [int(address, 16) for address, _ in rows] == list(range(0x4200C4, 0x4204C4, 16))
    data = bytes.fromhex(" ".join(hex_columns for _, hex_columns in rows))
    observed = struct.unpack("<256I", data)
    expected = []
    for index in range(256):
        register = index
        for _ in range(8):
            register = (register >> 1) ^ (0xEDB88320 if register & 1 else 0)
        expected.append(register)
    assert observed == tuple(expected)
    print("IMPBORL.DLL: all 256 CRC table entries match reflected IEEE polynomial 0xEDB88320.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dll", type=Path)
    verify_crc_table(parser.parse_args().dll)

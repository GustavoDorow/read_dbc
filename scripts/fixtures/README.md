# DATASUS SIH fixture pipeline

These five monthly AIH reduzida files come from the public [DATASUS SIHSUS FTP directory](ftp://ftp.datasus.gov.br/dissemin/publicos/SIHSUS/200801_/Dados/). The compressed inputs total 5,214,105 bytes.

| File | Period and state | Why it was selected |
| --- | --- | --- |
| `RDAC0801.dbc` | January 2008, Acre | Early, small fixture with 86 fields and a DBF EOF marker. |
| `RDRR1501.dbc` | January 2015, Roraima | Small fixture showing the intermediate 113-field layout, including the expanded secondary diagnosis fields. |
| `RDAP2401.dbc` | January 2024, Amapá | Small recent fixture for comparing the 113-field layout and values across periods and states. |
| `RDSC2501.dbc` | January 2025, Santa Catarina | Larger fixture with over 50,000 records to exercise the same layout at higher volume. |
| `RDAC2601.dbc` | January 2026, Acre | Recent 114-field layout with `FONTE_ORC`, paired with the oldest Acre fixture. |

## Reproduce

Run from the repository root with Bash, GNU coreutils, curl with FTP support, Rust/Cargo, a C compiler (`cc`), and uv with Python 3.11 or newer.

```bash
scripts/fixtures/fetch.sh
time scripts/fixtures/build-reference.sh
```

`build-reference.sh` also runs `fetch.sh`, so the timed command alone runs the entire pipeline. For a clean run, remove the generated directories first.

```bash
rm -rf -- test/data test/reference
time scripts/fixtures/build-reference.sh
```

To prove reproducibility, record checksums before the clean run and compare them afterward. Gzip headers use `mtime=0` and no filename, so the `.jsonl.gz` files are byte-stable for a given zlib.

```bash
(cd test && sha256sum data/* reference/*) > /tmp/fixtures.before
rm -rf -- test/data test/reference
time scripts/fixtures/build-reference.sh
(cd test && sha256sum --check --strict /tmp/fixtures.before)
```

`fetch.sh` uses `SHA256SUMS` as the fixed input list and checks every file, including existing downloads. It fails on a mismatch and leaves an existing file in place for inspection. Each download is verified before publication. Cargo uses the fixture-specific lockfile. The C build uses [blast-dbf](https://github.com/eaglebh/blast-dbf/tree/f4b567e1248ae97f97f7c528e935c102da6bcd83) at a fixed revision and checks source hashes before compilation. The pipeline compares both DBF outputs byte for byte before publishing the verified DBF and references.

## Reference formats

- `<NAME>.dbfread.jsonl.gz` contains one active record per line, in file and field order. The reader uses [dbfread 2.0.7](https://dbfread.readthedocs.io/en/latest/) with `encoding="iso-8859-1"` and all other settings at their defaults. Each value is `[python_type, payload]`. The payload is a string for `int` and `float` via `repr`, a string for `Decimal`, ISO text for `date` and `datetime`, hex for `bytes`, a JSON boolean for `bool`, JSON null for `NoneType`, or the original text for `str`. For example, `["str","001"]`, `["int","1"]`, `["float","1.0"]`, `["str",""]`, and `["NoneType",null]` are distinct.
- `<NAME>.raw.jsonl.gz` contains one physical record per line. `_deleted` is the exact first record byte decoded as Latin-1 (`" "` for active, `"*"` for deleted). The remaining keys follow DBF descriptor order. Their string values include every field byte, including padding and leading zeros. Encode each decoded JSON string as Latin-1 to recover the original bytes.
- `<NAME>.profile.json` records header facts, field metrics, up to three distinct raw examples in first-appearance order, and layout changes relative to the preceding sampled period. [PROFILE.md](../../test/reference/PROFILE.md) tabulates the profiles.

Each profile also records:

- `dbc_crc32`: the CRC-32 stored as a little-endian u32 at offset `header_len` of the `.dbc`, recomputed with IEEE/zlib CRC-32 over the raw DBC header bytes followed by the whole decompressed record area (everything after the header in the `.dbf`, including any trailing `0x1A`). `header_matches_dbf` confirms the DBF header is the DBC header verbatim; the build fails if it is not.
- `codepage_name`: the language-driver byte (offset 29) mapped to a Python codec. Every fixture has `0x02` (CP850). The only non-ASCII bytes in this set are in RDSC2501 `AUD_JUST`, and they decode correctly as CP850 (`0x87` → `ç`, `0xA0` → `á`), not as Latin-1. The Latin-1 oracles keep the exact bytes either way, and `non_ascii.examples_as_cp850` shows the CP850 reading.
- `dbfread_types`: the count of each Python type dbfread produced per field. dbfread returns `int` for any numeric text that parses as an integer and `float` otherwise, and it skips deleted records.
- `raw_jsonl_sha256` and `dbfread_jsonl_sha256`: SHA-256 digests of the uncompressed oracle streams. They support content comparisons that do not depend on the zlib build used for gzip.

Both oracles are UTF-8 JSON Lines with non-ASCII characters represented by JSON escapes. Gzip headers use a fixed timestamp and omit the filename. The Python script runs through PEP 723 metadata with the pinned dependency and restarts the interpreter with `-I`.

Profile statistics include deleted records. NUL bytes inside a field are counted separately as `nul_bytes`. Trimming removes ASCII whitespace from both ends, and the blank count includes values with zero bytes remaining after trimming. A leading zero is a zero followed by another digit at the start of a trimmed value, with an optional sign. For numeric fields, junk is any nonblank value outside signed decimal or scientific notation. The int64 check covers signed integer literals in zero-decimal numeric fields. Character-date checks run on every eight-byte `C` field. `looks_like_date` is true when at least half of the non-blank values are valid `YYYYMMDD` calendar dates. In these fixtures that is `NASC`, `DT_INTER`, and `DT_SAIDA`. `GESTOR_DT` is entirely blank, and the postal code `CEP` is not date-like. Empty values count as blank and other non-calendar values count as invalid. There are no `D` fields; all dates are stored as `C(8)`.

## CNES, SIM, SINASC, and SIA fixtures

[files.tsv](families/files.tsv) lists five small public DATASUS files from
Acre, Roraima, and Amapá. It records the family and exact FTP path for each
sample. [SHA256SUMS](families/SHA256SUMS) pins their contents. The fetch script
checks existing files and verifies new downloads before placing them in
`test/data/families/`.

```bash
scripts/fixtures/fetch-families.sh
make debug
uv run --script scripts/parity/family_parity.py --build build/debug
```

The parity script builds the locked datasus-dbc converter and converts the
samples in a temporary directory. It checks the schema, active row count, and
each Latin-1 value against dbfread 2.0.7. It also checks the result of auto
against the explicit encoding selected by the language-driver byte and reports
examples from the CNES establishment license field when it contains non-ASCII
text. The report goes to stdout and the script exits nonzero on differences.
See [RESULTS.md](../parity/RESULTS.md#datasus-family-parity) for the recorded run.

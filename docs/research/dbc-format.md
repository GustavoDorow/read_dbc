# DATASUS DBC binary format

**The four bytes are a little-endian IEEE CRC-32 of the original DBF byte stream.** Compute it over the unmodified stored header followed by every byte produced by the PKWARE DCL stream, including the DBF `0x1A` EOF byte when present. This matched 11 DATASUS FTP downloads (10 distinct contents from five families, covering 1994–2024) and both upstream `blast-dbf` fixtures. Static inspection of the official TabWin 4.15 converter independently confirmed the algorithm and byte range.

## Byte layout — VERIFIED (FTP samples and converter)

Offsets are zero-based and ranges are half-open. `H = u16le(file[8:10])`, `R = u16le(file[10:12])`, and `N = u32le(file[4:8])`. The stored DBF header occupies exactly `H` bytes starting at byte zero.

| Offset / range | Size | Meaning |
|---|---:|---|
| `0` | 1 | DBF version byte, `0x03` in the sampled files |
| `1:4` | 3 | DBF update date bytes |
| `4:8` | 4 | DBF record count `N`, unsigned LE |
| `8:10` | 2 | DBF header length `H`, unsigned LE |
| `10:12` | 2 | DBF record length `R`, unsigned LE, including the deletion flag |
| `12:29` | 17 | Other fixed DBF header bytes |
| `29` | 1 | Language-driver ID byte in the xBase convention |
| `30:32` | 2 | Remaining fixed DBF header bytes |
| `32:H-1` | `H-33` | Array of 32-byte field descriptors in the sampled DBF headers |
| `H-1` | 1 | Header terminator slot, observed as `0x0D` and `0x00` |
| `H:H+4` | 4 | Little-endian CRC-32 of the header and full decoded stream |
| `H+4` | 1 | DCL literal mode byte |
| `H+5` | 1 | DCL dictionary code byte |
| `H+6` onward | Variable | DCL token bits through the end code and the rest of its last byte |
| After the final DCL byte | 0 observed | Whole bytes beyond the end of the stream |

Reconstruction for the checksum is `file[0:H] || blast(file[H+4:])`. The first ten bytes are part of the DBF header. The separate format note in `read.dbc` gives an offset of `H+14` using its stated definition of `H`. Its C implementation reads exactly `H` bytes from offset zero and starts DCL at `H+4`, consistent with the tested files. See [OGC's reproduction of the dBASE III PLUS and IV header structures](https://docs.ogc.org/bp/16-070r4.html), [read.dbc format note, lines 12–22](https://github.com/danicat/read.dbc/blob/52e0b15e79b8917e7814b0f8c20c14ae25b9f87e/DBC_FORMAT.md#L12-L22), and [read.dbc decoder, lines 110–173](https://github.com/danicat/read.dbc/blob/52e0b15e79b8917e7814b0f8c20c14ae25b9f87e/src/dbc2dbf.c#L110-L173).

## 1. Checksum — VERIFIED (experiments and official converter)

- **Algorithm.** CRC-32/ISO-HDLC (IEEE, as returned by zlib's `crc32`). Polynomial `0x04C11DB7`, reflected polynomial `0xEDB88320`, reflected input and output, internal initial register `0xFFFFFFFF`, final XOR `0xFFFFFFFF`, and check value `0xCBF43926` for ASCII `123456789`. Serialize the result as a little-endian `u32`. The zlib API can be initialized with `crc32(0, Z_NULL, 0)` and updated first with the raw header, then with decoded chunks. See [zlib v1.3.1 CRC API](https://github.com/madler/zlib/blob/v1.3.1/zlib.h#L1712-L1740) and [PKWARE DCL v1.02 release notes, CRC conditioning](https://sources.ru/pascal/archives/pklib102.htm).
- **Coverage.** Feed exactly the stored bytes `[0,H)`, including `H-1`, then the full decompressed payload through DCL EOS. For `RDAC2401.dbc`, bytes `fe 87 8e 78` encode `0x788e87fe` and the decoded stream has no DBF EOF byte. For `STAC2401.dbc`, `53 6c 73 07` encodes `0x07736c53`. Its stored header ends with `0x00` and its decoded stream ends with `0x1A`. Replacing the header's `0x00` with `0x0D` produces `0x20328f51` over that reconstructed DBF, and dropping the decoded EOF marker gives a different CRC as well. These distinctions matter if the output DBF header is repaired or the record stream is handed off without the marker. See [per-file CRC results](work/dbc-probe-results.json).
- **Hypotheses tested.** For each candidate byte range, tried reflected IEEE, reflected CRC-32C/Castagnoli (polynomial `0x1EDC6F41`, reflected `0x82F63B78`), and MSB-first CRC-32/BZIP2 (polynomial `0x04C11DB7`). Each was tested with both `0` and `0xFFFFFFFF` initial registers, both final XOR values, and both output byte orders. The C implementation was checked against the three standard `123456789` vectors and against zlib on all IEEE ranges. The ranges covered the fixed header, descriptors, stored header with/without its last byte and with normalized `0x0D`, compressed bytes through physical EOF or DCL EOS, DCL bytes excluding the first two, trailing bytes, raw decoded output, declared record bytes, decoded output with `0x1A` stripped or forced, each listed reconstruction in the probe, raw header plus compressed payload, the CRC field plus payload, and the whole DBC with the CRC field stored or zeroed. The size of each range, record length and record count were compared in both byte orders. No length, alternative polynomial/conditioning/order, or compressed-only, records-only, or header-only CRC matched on any tested file. Additional matching range labels in the JSON represent identical inputs for that file, such as `header || declared_records` when there is no EOF byte. The raw header plus actual decoded output is the reconstruction that matched across the entire sample. See [the hypothesis matrix in the experiment script](work/scripts/inspect_dbc.py) and [full results](work/dbc-probe-results.json).

### Official compressor and decompressor

The DATASUS TabWin manual documents that compression creates a CRC and that **Testar CRC** checks DBC integrity (printed p. 108). Its glossary attributes DBC compression to DATASUS's CompDBF. The official `TAB415.zip` includes the command-line `dbf2dbc.exe` and its `IMPBORL.DLL`. The following addresses are from the x86 binaries in that archive, whose SHA-256 is `a7371b19292f368212575d9bb59e261c95b3135c46a1294bc370f04ba21192e6`. See [official TabWin manual](ftp://ftp.datasus.gov.br/tabwin/tabwin/TabWin.pdf), [official archive](ftp://ftp.datasus.gov.br/tabwin/tabwin/TAB415.zip), and [retained disassembly and hashes](work/tabwin-crc-disassembly.txt).

| Address | Observed behavior |
|---|---|
| DLL `0x410000–0x410039` | Exported `_crc32` loads the passed running register and updates it with `(crc >> 8) ^ table[(crc ^ byte) & 0xff]`. All 256 entries at `0x4200c4` matched the reflected IEEE table. |
| EXE `0x408e5f–0x408ee7` | Initializes the register to all ones, reads `H` bytes from the start of the DBF, updates the register with those bytes, and writes header plus a reserved four-byte slot. |
| EXE `0x408d02–0x408d14`, `0x408eec–0x408efc`, `0x408f6a–0x408f90` | The input callback passed to `_implode` CRCs each chunk it supplies. The call sets binary/uncoded mode and a dictionary of `0x1000` bytes. |
| EXE `0x408f95–0x408fc8` | Complements the register (`neg` then `dec`), seeks to `H`, and writes the four-byte x86 value. |
| EXE `0x4091fb–0x409291`, `0x408da3–0x408db2`, `0x409302–0x409328`, `0x4093ae–0x4093b6` | The decompressor initializes and updates the CRC with the stored header, reads the four-byte expected value, updates the CRC in the `_explode` output callback, finalizes, and compares with the stored value. |

This is a static check of the shipped binaries. The experiment did not execute the Windows tools or establish which converter version produced each FTP file. The DLL lookup table can be rechecked with [verify_tabwin_crc.py](work/scripts/verify_tabwin_crc.py).

## 2. Files with the stream at `H` — VERIFIED for controls, UNVERIFIED for DATASUS releases

- **VERIFIED (FTP and controls).** All 11 original downloads decoded from `H+4` to EOS. Starting at `H` gave Adler status `-1` (invalid literal flag) for every original download and both upstream fixtures. Removing the four bytes created a valid synthetic no-checksum container for each file. Starting those controls at `H` recovered identical output and stream boundaries, while starting at `H+4` failed with `-1` or `-2`. See [offset tests and their status codes](work/dbc-probe-results.json) and [Adler's API return codes](https://github.com/madler/zlib/blob/v1.3.1/contrib/blast/blast.h#L68-L79).
- **VERIFIED (source).** `datasus-core` 0.1.0 tries `H+4` first and retries `H` after a decompression error other than a hard output-limit error. It does not compare the stored CRC. It inherits the two offsets and the rationale in the comment from historical `datasusr` 0.1.1. Its test `tiny_dbc_with_and_without_crc` builds both variants in memory from a tiny literal-only DCL stream. The four-byte test variant uses zero padding. The separate whole-file DCL path is also covered by a synthetic test. See [published datasus-core container.rs, lines 126–145 and 172–221](https://github.com/StrategicProjects/datasus-rs/blob/6f93742f66a9ce9c5cd4cfd917e3799367d9d072/crates/datasus-core/src/container.rs#L126-L221), [published tests/read.rs, lines 262–301](https://github.com/StrategicProjects/datasus-rs/blob/6f93742f66a9ce9c5cd4cfd917e3799367d9d072/crates/datasus-core/tests/read.rs#L262-L301), and [CRAN datasusr 0.1.1 archive, src/dbc_reader.c lines 306–332](https://cran.r-project.org/src/contrib/Archive/datasusr/datasusr_0.1.1.tar.gz).
- **UNVERIFIED (provenance).** The fallback has a demonstrated use on the synthetic variant. No published DATASUS file requiring it was identified in the samples or cited by those implementations. A 2016 report of a CRC error for `DNRRR95` was resolved by changing the download to binary mode. Today's binary FTP download under that historical filename matches the checksum and is byte-identical to `DNRRR1995.dbc` from the newer directory. See [original report](https://www.mail-archive.com/r-br@listas.c3sl.ufpr.br/msg16464.html), [resolution](https://www.mail-archive.com/r-br@listas.c3sl.ufpr.br/msg16470.html), and [sample hashes](work/dbc-probe-results.json).

## 3. Stream end and record length — VERIFIED in the corpus

All files reached DCL EOS with zero unused whole compressed bytes. The probe supplies all bytes after `H+4` as initial input to zlib v1.3.1's `blast` and captures `left` on return. Adler's sample stream with four extra bytes returned exactly four unused bytes, and truncating it returned the input-exhausted error. The count is in whole bytes. The last consumed byte can also contain unused high bits after EOS. See [blast.h, input and unused-byte contract](https://github.com/madler/zlib/blob/v1.3.1/contrib/blast/blast.h#L45-L79), [blast.c, bit handling and return](https://github.com/madler/zlib/blob/v1.3.1/contrib/blast/blast.c#L61-L94), and [unused-byte values](work/dbc-probe-results.json).

Every download decoded to exactly `N × R` bytes or `N × R + 1` bytes. The additional byte was `0x1A` in each of the five files where present, and six downloads (five unique) had no additional byte. The two upstream fixtures cover both cases and reconstructed byte-for-byte to the DBFs published beside them. DCL EOS is a compression token independent of the optional DBF EOF data byte. Whether any other producer emits whole trailing compressed bytes, extra decoded data, or inconsistent DBF row counts remains **UNVERIFIED**. See [per-file decoded lengths](work/dbc-probe-results.json), [upstream fixture pairs](https://github.com/eaglebh/blast-dbf/tree/f4b567e1248ae97f97f7c528e935c102da6bcd83), and [Adler's EOS branch](https://github.com/madler/zlib/blob/v1.3.1/contrib/blast/blast.c#L323-L341).

## 4. PKWARE DCL — VERIFIED by source and sampled stream headers

All sampled streams start with `00 06` (raw literals, 4,096-byte dictionary). The official compressor inspected above also requests that combination. Adler accepts literal mode `0` for raw 8-bit literals and `1` for literals decoded with a fixed Huffman table. Codes `4`, `5`, and `6` give windows of 1,024, 2,048, and 4,096 bytes. DATASUS publication using coded literals or a smaller dictionary remains **UNVERIFIED**. See [blast.c, lines 244–280 and 291–335](https://github.com/madler/zlib/blob/v1.3.1/contrib/blast/blast.c#L244-L335), [sample mode and dictionary values](work/dbc-probe-results.json), and [official converter disassembly](work/tabwin-crc-disassembly.txt).

- Stream bits are consumed low bit first within each byte. The token tag is `0` for a literal, `1` for a length code and optional distance. Fixed Huffman codes are decoded in Adler's reversed/inverted representation. See [blast.c, lines 61–94 and 109–150](https://github.com/madler/zlib/blob/v1.3.1/contrib/blast/blast.c#L61-L150).
- EOS is length symbol 15, with base 264 and 8 extra bits all set, giving **519**. In consumption order the token is the tag bit `1`, seven zero code bits, then eight `1` bits. It has no distance. Actual copies have lengths 2–518 and may overlap, so copy forward as bytes become available. See [blast.c tables and decode loop](https://github.com/madler/zlib/blob/v1.3.1/contrib/blast/blast.c#L306-L376) and [datasus-core synthetic encoding of EOS, lines 262–278](https://github.com/StrategicProjects/datasus-rs/blob/6f93742f66a9ce9c5cd4cfd917e3799367d9d072/crates/datasus-core/tests/read.rs#L262-L278).
- Distance is `(distance_symbol << k) + extra + 1`, with 64 distance symbols and `k = 2` for length 2, otherwise the dictionary code. This gives the exact maximum distance of 256 for length 2 and `64 × 2^dict` for longer matches. Reject a distance beyond the number of bytes previously produced. A ring retaining **at most 4,096 bytes of decoded history** is sufficient even for overlapping copies. Adler implements the ring and output staging within one 4,096-byte buffer. See [blast.c state, lines 36–58](https://github.com/madler/zlib/blob/v1.3.1/contrib/blast/blast.c#L36-L58) and [distance/overlap handling, lines 306–376](https://github.com/madler/zlib/blob/v1.3.1/contrib/blast/blast.c#L306-L376).

## 5. DBF observations — VERIFIED in the corpus

- All FTP files have version `0x03`, the dBASE III-style version without memo, and `H = 32 + 32 × field_count + 1`. Descriptors start at byte 32. Within each descriptor, bytes 0–10 are the name area, byte 11 is the ASCII type, bytes 12–15 are the field address slot, byte 16 is width, and byte 17 is decimal count. Bytes 12–15 and 18–31 were zero in every sampled descriptor, and names all contained a NUL in the name area. Compute record offsets from the widths. Starting at byte 1 for the first field, their sum plus the flag byte equaled `R` in each file. See [OGC's dBASE III PLUS and IV descriptor tables](https://docs.ogc.org/bp/16-070r4.html) and [field measurements](work/dbc-probe-results.json).
- The final header byte was `0x0D` except for `STAC2401.dbc`, where it was `0x00`. The raw byte at offset 29 was `0x00`, `0x02`, `0x03`, or `0x35` in the FTP corpus, and additionally `0x57` in the upstream `sids.dbc` fixture. The original dBASE III PLUS documentation treats bytes 28–31 as reserved. Later xBase conventions identify byte 29 as the language driver. This probe checked byte values and did not validate a character encoding for each file. See [OGC header tables](https://docs.ogc.org/bp/16-070r4.html) and [measured header values](work/dbc-probe-results.json).
- Official downloads had `C` (character) and `N` (numeric) fields. The decimal count was 0 for `C` and 0 or 2 for `N`. The upstream `storm.dbc` fixture also exercised a `D` field, and the two fixtures added numeric decimal counts 3, 6, 13, and 14. Occurrence of `D`, `L`, `F`, or `M` in official DATASUS releases remains **UNVERIFIED** by this sample. A field containing date-like characters may have type `C`. See [types and decimal counts](work/dbc-probe-results.json) and [dBASE type definitions](https://docs.ogc.org/bp/16-070r4.html).
- Each declared record started with `0x20` (active). No `0x2A` deleted flags occurred, although DBF defines that flag and compatible readers handle it. Record bodies had no NUL bytes. Character fields showed trailing spaces and numeric fields showed leading spaces in the FTP sample. Header NULs and space padding in the record area are distinct. All declared record counts agreed with the full decoded record area after allowing for the optional EOF byte. See [record flag and padding counts](work/dbc-probe-results.json), [OGC dBASE records section](https://docs.ogc.org/bp/16-070r4.html), and [datasus-core container.rs, lines 100–116](https://github.com/StrategicProjects/datasus-rs/blob/6f93742f66a9ce9c5cd4cfd917e3799367d9d072/crates/datasus-core/src/container.rs#L100-L116).

## Files tested

Fetched in binary mode on 2026-11-21 from the DATASUS FTP under `/dissemin/publicos/`. Exact paths are recorded in [download_dbc_samples.sh](work/scripts/download_dbc_samples.sh). The `EOF` column refers to a decoded DBF `0x1A`, and `CRC` gives the stored value after reading it as little-endian, with every entry matching `CRC32(raw header || full decoded output)`.

| File (family) | Bytes | SHA-256 | H | R | N | Fields (types × count) | LDID | Mode / dictionary | EOF | CRC match |
|---|---:|---|---:|---:|---:|---|---|---|---|---|
| RDAC0801.dbc (SIH) | 114153 | `fde119624f5ba8400fdd25f086be6180bb25440c141dbf9ae634b7f417d0f28d` | 2785 | 500 | 2087 | 86 (C×55, N×31) | `02` | 0 / 6 (4 KiB) | Yes | `9b94505a` |
| RDAC2401.dbc (SIH) | 313213 | `41b7ad58932cd56d6c60455cbf67e7995f5fd2e64375d0cc440631a191638429` | 3649 | 702 | 4315 | 113 (C×77, N×36) | `02` | 0 / 6 (4 KiB) | No | `788e87fe` |
| RDRR2401.dbc (SIH) | 275725 | `37741f8b16adcf0f19ff837f9abbf9c46af4b6c128f5599867fe0fa7940eccb5` | 3649 | 702 | 3714 | 113 (C×77, N×36) | `02` | 0 / 6 (4 KiB) | No | `97e2d8db` |
| STAC2401.dbc (CNES) | 62096 | `b411030223ac367e16a024b0e0233c6d8a80f877a7b493d746cd3a46c82b42ab` | 6689 | 499 | 1374 | 208 (C×153, N×55) | `00` | 0 / 6 (4 KiB) | Yes | `07736c53` |
| DOAC2022.dbc (SIM) | 330408 | `04a015b690068baeb42e7e83165e3b9de43f2881bebdf46f0eea5ad0b9984a51` | 2817 | 490 | 4159 | 87 (C×87) | `35` | 0 / 6 (4 KiB) | Yes | `df3f9ff0` |
| DNAC2022.dbc (SINASC) | 681675 | `199c1acfe6311d08a421b6df1c30d1dd47f12147393e5a385187020441f8e77d` | 1985 | 224 | 14483 | 61 (C×61) | `00` | 0 / 6 (4 KiB) | Yes | `4c9de919` |
| DNAC1996.DBC (SINASC) | 247527 | `9da9227caa11802bc7c46454fd93ed002624c0a932261f713feb44ca9203100f` | 705 | 162 | 14236 | 21 (C×21) | `00` | 0 / 6 (4 KiB) | No | `94647e19` |
| DNRAC1994.dbc (SINASC) | 277876 | `6aa5d52bdc3e12603cfe230afd164c62bde8089b50532271b2465a52a12ce65d` | 993 | 239 | 12127 | 30 (C×30) | `00` | 0 / 6 (4 KiB) | No | `5d5b4bc3` |
| DNRRR1995.dbc (SINASC) | 156322 | `bb06e2c576acbc036fd392e0c29005b4e7cb4e99b5b44ad0972586d57e5b4a99` | 993 | 239 | 7020 | 30 (C×30) | `00` | 0 / 6 (4 KiB) | No | `a661fc3d` |
| DNRRR95.DBC (SINASC alias) | 156322 | `bb06e2c576acbc036fd392e0c29005b4e7cb4e99b5b44ad0972586d57e5b4a99` | 993 | 239 | 7020 | 30 (C×30) | `00` | 0 / 6 (4 KiB) | No | `a661fc3d` |
| PAAC2401.dbc (SIA) | 2767043 | `b1540eba00a1fa2c1dd1f6e2b26e502838f07f235428187f0cc55e181c36fbc7` | 1953 | 397 | 105836 | 60 (C×50, N×10) | `03` | 0 / 6 (4 KiB) | Yes | `33934882` |
| sids.dbc (upstream fixture) | 5354 | `02dde5d3654a5efd095f87788aa3a67841a5adc5f155ed68e3b3941351e69935` | 481 | 168 | 100 | 14 (C×2, N×12) | `57` | 0 / 6 (4 KiB) | Yes | `02fc0674` |
| storm.dbc (upstream fixture) | 1443 | `ca7a1d5b36b7b426e752ae80267c993575d1a127207e2a206dc1416abf8ff9a9` | 225 | 71 | 100 | 6 (C×3, D×1, N×2) | `00` | 0 / 6 (4 KiB) | No | `6a52ab41` |

## Reader behavior — VERIFIED by source inspection

| Source | Behavior |
|---|---|
| [Pablo Fonseca's original eaglebh/blast-dbf, blast-dbf.c lines 48–87](https://github.com/eaglebh/blast-dbf/blob/f4b567e1248ae97f97f7c528e935c102da6bcd83/blast-dbf.c#L48-L87) | Copies `H` bytes from byte zero and seeks to `H+4` before calling `blast`. The original repo is credited by the R and Python derivatives. |
| [R read.dbc decoder, src/dbc2dbf.c lines 110–173](https://github.com/danicat/read.dbc/blob/52e0b15e79b8917e7814b0f8c20c14ae25b9f87e/src/dbc2dbf.c#L110-L173) | Copies the raw header and skips four bytes without verification. |
| [R read.dbc encoder, src/dbf2dbc.c lines 49–93](https://github.com/danicat/read.dbc/blob/52e0b15e79b8917e7814b0f8c20c14ae25b9f87e/src/dbf2dbc.c#L49-L93) | Writes four zeros before compressing, so third-party files can have the standard stream offset with a placeholder checksum. |
| [Rust datasus-dbc 0.1.1, src/decompress.rs lines 26–56](https://github.com/mymatsubara/datasus-dbc/blob/76a2d4917fd9070864d89ac03a9dd81596d01796/src/decompress.rs#L26-L56) | Reads a four-byte `crc32` buffer and chains the header with `ExplodeReader`. The checksum buffer is unused after reading. |
| [pyreaddbc 2.0.4 source distribution](https://pypi.org/project/pyreaddbc/2.0.4/#files) and [PySUS extensions.py lines 747–753](https://github.com/AlertaDengue/PySUS/blob/30479ce504c15c0c9acf8189c509417e1f693801/pysus/api/extensions.py#L747-L753) | PySUS calls `pyreaddbc.dbc2dbf`. The Python package's `pyreaddbc/c-src/dbc2dbf.c` reads `H` (lines 87–109), writes a header with `H-1` set to `0x0D` (lines 112–139), and skips four bytes before decoding (lines 141–150). The change would alter the header bytes fed to a DBC CRC verifier. |
| [Rust datasus-core 0.1.0, src/container.rs lines 126–145 and 172–221](https://github.com/StrategicProjects/datasus-rs/blob/6f93742f66a9ce9c5cd4cfd917e3799367d9d072/crates/datasus-core/src/container.rs#L126-L221), [src/blast.rs lines 180–249](https://github.com/StrategicProjects/datasus-rs/blob/6f93742f66a9ce9c5cd4cfd917e3799367d9d072/crates/datasus-core/src/blast.rs#L180-L249) | Uses a Rust port of `blast`. Tries both DCL offsets after a recognized DBF header and can also try a whole-file DCL stream when no header is recognized. The record-area path can accept output stopped at the header-derived limit, so its success alone does not establish EOS, exact output size, or CRC. |

## Implications for a streaming decoder

- Parse and bounds-check the raw header, start the standard stream at `H+4`, and hash the raw header before any DBF normalization. Update CRC with every decoded byte, including deletion flags and any decoded EOF marker. Compare at DCL EOS. A reader that stops early can report checksum verification only if it finishes decoding the stream.
- Keep at most 4 KiB of DCL history plus parsing buffers. Keep separate accounting for complete records, the optional decoded `0x1A`, DCL EOS, and unused whole input bytes. At full validation, check the output against `N × R` and the optional EOF byte. Apply an independent configured bound to decompression of untrusted input.
- A strict DATASUS checksum check is supported by the evidence. A reader intended to accept third-party DBCs can expose a permissive mode for placeholder fields or experimental no-field containers, with checksum status visible to callers. Four zero bytes alone are ambiguous because zero is also a valid CRC value. Treat the `H` retry as a separate layout decision and check its decoded record structure and EOS before accepting it.

## Reproduce

The original FTP samples are in [work/data](work/data/). The Python experiments use isolation, explicit file arguments and bounded output. [The C probe](work/scripts/dbc_blast_probe.c) uses unmodified zlib v1.3.1 `blast.c` and `blast.h` from [contrib/blast](https://github.com/madler/zlib/tree/v1.3.1/contrib/blast). Run from `docs/research/work` with the retained source files.

```bash
cc -Wall -Wextra -Werror -O2 -shared -fPIC -I sources \
  scripts/dbc_blast_probe.c sources/blast.c -o sources/dbc_blast_probe.so
python3 -I scripts/inspect_dbc.py --probe sources/dbc_blast_probe.so \
  data/* sources/sids.dbc sources/storm.dbc > dbc-probe-results.json
python3 -I scripts/verify_tabwin_crc.py sources/IMPBORL.DLL
```

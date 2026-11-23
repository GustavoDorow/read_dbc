# Fuzzing `read_dbc_format`

Two cargo-fuzz targets live in `crates/format/fuzz/` (excluded from the workspace):

| Target | Input | What it exercises |
|---|---|---|
| `dcl` | arbitrary bytes | the streaming PKWare DCL decoder; output is streamed into a fixed buffer and discarded (capped at 16 MiB per input) |
| `dbc_reader` | arbitrary bytes plus fuzzer-chosen options | header parsing, schema resolution, type overrides, batch decoding, `next_raw_record`, CRC and end-of-stream checks; includes a mode that wraps the input as a `.dbf` into a `.dbc` with a correct CRC so the value parsers are reached |

## Running

```sh
cargo install cargo-fuzz --locked        # needs a nightly toolchain
cd crates/format
mkdir -p fuzz/corpus/dbc_reader fuzz/corpus/dcl
cp ../../test/fixtures/*.dbc ../../test/fixtures/*.dbf fuzz/corpus/dbc_reader/
cargo +nightly fuzz run dbc_reader fuzz/corpus/dbc_reader -- -max_total_time=900 -rss_limit_mb=512 -timeout=5
cargo +nightly fuzz run dcl fuzz/corpus/dcl -- -max_total_time=600 -rss_limit_mb=512 -timeout=5
```

## Results (2026-11-23, AMD Ryzen 5 5600, rustc 1.102.0-nightly, AddressSanitizer)

| Target | Time | Executions | Exec/s | Coverage (edges / features) | Crashes | Timeouts | OOMs |
|---|---:|---:|---:|---:|---:|---:|---:|
| `dbc_reader` | ~17 min + 2 min | 180,000+ in the final 2-minute run | ~1,500 | 1,232 / 4,399 | 0 | 0 | 0 |
| `dcl` | ~17 min + 10 min | 456,767 in the final 10-minute run | ~760 | 182 / 604 | 0 | 0 | 0 (after harness fix) |

Seed corpus: the committed fixtures in `test/fixtures/`. The corpora grew to 6,853 (`dbc_reader`) and 1,849 (`dcl`) inputs; they are not committed.

### Finding: harness out-of-memory, not a library bug

The first `dcl` harness called `dcl::decompress_all`, which collects the whole output into a `Vec`. A 108 KB input that legitimately expands to hundreds of megabytes exceeded the 512 MB RSS limit. The library's streaming path keeps a fixed ring buffer and is unaffected; the extension never collects output. The harness now streams into a fixed buffer like the extension, and the same input runs in 21 ms. `decompress_all` remains a test convenience and is documented as unbounded.

During development, a differential harness compared the decoder with Mark Adler's reference `blast.c` on about 143,000 generated streams with no mismatches, and about 3.2 million CRC-preserving mutations of real files produced no panics; these harnesses are not part of the repository.

//! Throughput / memory comparison with datasus-core.
//!
//! ```text
//! cargo run --release -p read_dbc_format --example bench -- <mode> <file.dbc> [iterations]
//! ```
//!
//! Modes:
//! * `dcl`        – this crate: DCL decompression only (streamed from the file)
//! * `read`       – this crate: full read, all columns, CRC verified
//! * `read-none`  – this crate: full read with an empty projection (COUNT(*))
//! * `core-blast` – datasus-core: slice-based blast of the whole file in memory
//! * `core-read`  – datasus-core: `open` + `collect()` of all columns
//!
//! Prints the best time over the iterations, MB/s of decompressed data and,
//! on Linux, the process's peak RSS (`VmHWM` from `/proc/self/status`; the
//! same figure `/usr/bin/time -v` reports as "Maximum resident set size").

use std::fs::File;
use std::io::Read;
use std::time::Instant;

use read_dbc_format::{BatchSink, DbcReader, DclReader, ReadOptions, Value};

struct Consume(u64);
impl BatchSink for Consume {
    fn value(&mut self, _c: usize, _r: usize, v: Value<'_>) {
        self.0 = self.0.wrapping_add(match v {
            Value::Null => 1,
            Value::Text(s) => s.len() as u64,
            Value::BigInt(x) | Value::Decimal64(x) => x as u64,
            Value::Decimal128(x) => x as u64,
            Value::Date(x) => x as u64,
            Value::Boolean(b) => b as u64,
        });
    }
}

fn run(mode: &str, path: &str) -> (u64, u64) {
    match mode {
        "dcl" => {
            let mut f = File::open(path).unwrap();
            let mut fixed = [0u8; 12];
            f.read_exact(&mut fixed).unwrap();
            let h = u16::from_le_bytes([fixed[8], fixed[9]]) as usize;
            let mut skip = vec![0u8; h + 4 - 12];
            f.read_exact(&mut skip).unwrap();
            let mut d = DclReader::new(f);
            let mut buf = vec![0u8; 1 << 16];
            let mut total = 0u64;
            loop {
                let n = d.read_decoded(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                total += n as u64;
            }
            d.check_end().unwrap();
            (total, total)
        }
        "read" | "read-none" => {
            let f = File::open(path).unwrap();
            let mut r = DbcReader::new(f, ReadOptions::default()).unwrap();
            if mode == "read-none" {
                r.set_projection(&[]).unwrap();
            }
            let mut sink = Consume(0);
            while !r.is_finished() {
                r.read_batch(2048, &mut sink).unwrap();
            }
            let bytes = r.header().data_len();
            (bytes, sink.0)
        }
        "core-blast" => {
            let bytes = std::fs::read(path).unwrap();
            let h = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
            let mut out = Vec::new();
            datasus_core::blast::blast(&bytes[h + 4..], &mut out, usize::MAX).unwrap();
            (out.len() as u64, out.len() as u64)
        }
        "core-read" => {
            let t = datasus_core::open(path, &datasus_core::ReadOptions::default()).unwrap();
            let cols = t.collect();
            let bytes = t.data().records().len() as u64;
            (bytes, cols.len() as u64)
        }
        _ => panic!("unknown mode {mode}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (mode, path) = (&args[1], &args[2]);
    let iters: usize = args.get(3).map_or(5, |s| s.parse().unwrap());
    let mut best = f64::INFINITY;
    let mut bytes = 0;
    for _ in 0..iters {
        let t = Instant::now();
        let (b, check) = run(mode, path);
        let el = t.elapsed().as_secs_f64();
        std::hint::black_box(check);
        bytes = b;
        best = best.min(el);
    }
    let hwm = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .map(|l| l.split_whitespace().nth(1).unwrap_or("?").to_string())
        })
        .unwrap_or_else(|| "?".into());
    println!(
        "{mode}: {bytes} decompressed bytes, best of {iters}: {:.1} ms, {:.0} MB/s, peak RSS {hwm} KiB",
        best * 1e3,
        bytes as f64 / best / 1e6
    );
}

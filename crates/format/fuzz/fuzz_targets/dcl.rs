//! Arbitrary bytes through the streaming DCL decoder.
//!
//! Output is streamed into a fixed buffer and discarded, as the extension
//! does: a valid DCL stream may legitimately expand a small input into
//! hundreds of megabytes, so collecting it (as `decompress_all` does) would
//! only measure the harness. Output is capped to keep each run fast.
#![no_main]

use libfuzzer_sys::fuzz_target;
use read_dbc_format::dcl::DclReader;

const MAX_OUTPUT: usize = 16 << 20;

fuzz_target!(|data: &[u8]| {
    let mut reader = DclReader::new(data);
    let mut buf = [0u8; 8192];
    let mut total = 0usize;
    while total < MAX_OUTPUT {
        match reader.read_decoded(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => total += n,
        }
    }
});

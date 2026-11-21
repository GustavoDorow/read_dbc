//! Arbitrary bytes through the DCL decoder.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = read_dbc_format::dcl::decompress_all(data);
});

//! With the `writer` feature: the committed files in `test/fixtures/` must
//! be exactly what `tests/support/fixture_gen.rs` generates (so the
//! generator, README and SHA256SUMS never drift from the files).
#![cfg(feature = "writer")]

#[path = "support/fixture_gen.rs"]
mod fixture_gen;
#[path = "support/sha256.rs"]
mod sha256;

use std::path::PathBuf;

use read_dbc_format::dcl::decompress_all;

#[test]
fn committed_fixtures_match_the_generator() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test/fixtures");
    let real = std::fs::read(dir.join(fixture_gen::REAL_FILE)).unwrap();
    let files = fixture_gen::generate(Some(&real));
    // Deterministic.
    let again = fixture_gen::generate(Some(&real));
    assert!(files == again);
    for (name, bytes) in &files {
        let committed = std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(
            &committed == bytes,
            "test/fixtures/{name} is stale; rerun: cargo run -p read_dbc_format \
             --example make_fixtures --features writer -- test/fixtures"
        );
    }
}

#[test]
fn generated_streams_use_the_declared_dcl_parameters() {
    for fx in fixture_gen::fixtures() {
        let Some(o) = fx.dcl else { continue };
        let h = u16::from_le_bytes([fx.bytes[8], fx.bytes[9]]) as usize;
        let stream = &fx.bytes[h + 4..];
        assert_eq!(
            stream[..2],
            [o.coded_literals as u8, o.dict_bits as u8],
            "{}",
            fx.name
        );
        decompress_all(stream).unwrap_or_else(|e| panic!("{}: {e}", fx.name));
    }
    // types.dbc decompresses to plain.dbf's record area.
    let fx = fixture_gen::fixtures();
    let get = |n: &str| &fx.iter().find(|f| f.name == n).unwrap().bytes;
    let (dbc, dbf) = (get("types.dbc"), get("plain.dbf"));
    let h = u16::from_le_bytes([dbf[8], dbf[9]]) as usize;
    assert_eq!(dbc[..h], dbf[..h]);
    assert_eq!(decompress_all(&dbc[h + 4..]).unwrap(), dbf[h..]);
}

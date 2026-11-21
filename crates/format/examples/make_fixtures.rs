//! Write the synthetic test fixtures (plus `README.md` and `SHA256SUMS`)
//! into a directory, deterministically:
//!
//! ```text
//! cargo run -p read_dbc_format --example make_fixtures --features writer -- test/fixtures
//! ```
//!
//! The real DATASUS file `RDAC0801.dbc` is not generated: copy it into the
//! directory first (from `test/data/`); if present, its hash is checked and
//! it is listed in `SHA256SUMS` and the README.

#[path = "../tests/support/fixture_gen.rs"]
mod fixture_gen;
#[path = "../tests/support/sha256.rs"]
mod sha256;

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir: PathBuf = std::env::args_os()
        .nth(1)
        .ok_or("usage: make_fixtures <output-dir>")?
        .into();
    std::fs::create_dir_all(&dir)?;
    let real_path = dir.join(fixture_gen::REAL_FILE);
    let real = match std::fs::read(&real_path) {
        Ok(b) => {
            let h = sha256::sha256_hex(&b);
            if h != fixture_gen::REAL_FILE_SHA256 {
                return Err(format!(
                    "{} has SHA-256 {h}, expected {}",
                    real_path.display(),
                    fixture_gen::REAL_FILE_SHA256
                )
                .into());
            }
            Some(b)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "note: {} is missing (copy test/data/{} there); it will not be listed",
                real_path.display(),
                fixture_gen::REAL_FILE
            );
            None
        }
        Err(e) => return Err(e.into()),
    };
    for (name, bytes) in fixture_gen::generate(real.as_deref()) {
        std::fs::write(dir.join(&name), &bytes)?;
        println!("{:>6}  {name}", bytes.len());
    }
    Ok(())
}

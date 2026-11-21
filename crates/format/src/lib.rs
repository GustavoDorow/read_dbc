//! `read_dbc_format`: pure-Rust readers for DATASUS `.dbc` files.
//!
//! A `.dbc` file is a dBase (DBF) file whose record area is compressed with
//! PKWare DCL "implode". This crate will hold the streaming DCL decoder and
//! the DBF parser. It deliberately has no DuckDB dependency, so it can be
//! unit-tested and fuzzed on its own.

/// Version of this crate, as declared in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert!(!VERSION.is_empty());
    }
}

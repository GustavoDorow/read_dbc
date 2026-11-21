//! `read_dbc`: a DuckDB extension that reads DATASUS `.dbc` files.
//!
//! The format work (DCL decompression, DBF parsing) lives in
//! [`read_dbc_format`]; this crate registers DuckDB functions on top of it.

use duckdb_neo::{Result, extension::Extension};

pub mod scan;
pub mod version;

/// The category every function of this extension is documented under in `duckdb_functions()`.
pub const CATEGORY: &str = "read_dbc";

/// Register everything the extension provides.
pub fn register(extension: &Extension) -> Result<()> {
    version::register(extension)?;
    scan::register(extension)?;
    Ok(())
}

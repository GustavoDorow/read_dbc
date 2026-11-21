use duckdb_neo::{Result, duckdb_entrypoint_c_api_v2, extension::Extension};

#[duckdb_entrypoint_c_api_v2(extension_name = "read_dbc", api_version = "2.0.1")]
pub fn entrypoint(extension: &Extension) -> Result<()> {
    read_dbc_core::register(extension)
}

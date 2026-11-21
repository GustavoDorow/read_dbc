//! Placeholder scalar function: `read_dbc_version()` returns the extension version.

use duckdb_neo::{
    DuckDBType, Result,
    connection::Context,
    data_chunk::VectorCollection,
    extension::Extension,
    scalar::{ScalarCallbacks, ScalarFunctionBuilder},
    signature::{FunctionDocumentationBuilder, SignatureBuilder},
    vector::{Unknown, Vector},
};

use crate::CATEGORY;

/// The crate version, as declared in the workspace `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

struct VersionScalar;

impl ScalarCallbacks for VersionScalar {
    type BindData = ();
    type InitData = ();

    fn exec(
        &self,
        _context: &Context,
        _bind_data: Option<&Self::BindData>,
        _init_data: Option<&mut Self::InitData>,
        vectors: &mut VectorCollection,
        output: Vector<'_, Unknown>,
    ) -> Result<()> {
        let mut output = output.cast::<String>()?;
        let rows = vectors.row_count()?;
        output.set_size(rows)?;
        let mut writer = output.writer()?;
        for row in 0..rows {
            writer.set(row, VERSION)?;
        }
        Ok(())
    }
}

pub fn register(extension: &Extension) -> Result<()> {
    ScalarFunctionBuilder::new(
        "read_dbc_version",
        SignatureBuilder::new()
            .return_type(String::logical_type(extension)?)
            .documentation(
                FunctionDocumentationBuilder::new()
                    .description("Return the version of the read_dbc extension.")
                    .add_example("Show the extension version", "SELECT read_dbc_version()")
                    .add_category(CATEGORY),
            ),
        VersionScalar,
    )
    .register(extension)
}

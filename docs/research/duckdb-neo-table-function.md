# Implementing `read_dbc` with duckdb-neo 0.20001.0

## Scope and conclusion

The single-file reader can stream through DuckDB's VFS, project columns, write all five requested SQL types, and report cardinality and progress. The multi-file wrapper provides globs, file lists, schema reconciliation, and filename and Hive columns. **The named-option surface remains blocked on the pinned DuckDB implementation.** DuckDB copies only a typed kwargs schema onto the wrapper, while the Rust C v2 signature builder exposes named-only parameters and an untyped kwargs declaration. The standalone function supports the requested named arguments, including STRUCT and MAP for `types`. The wrapper can carry equivalent options in explicit file structs, with a different SQL calling convention. See §§1 and 5 for the source trace.

**Source notation.** [Neo] refers to the supplied unpacked `duckdb-neo-0.20001.0` tree. [Core] refers to paths inside `libduckdb-sys-1.20001.0/duckdb.tar.gz` under its Cargo registry source directory, relative to the archive's `duckdb/` root. Neo pins that sys crate with `capi-v2`, and the sys release pin identifies DuckDB v2.0.1, commit `8d78114e90a53823803660bf821f6073ac87da8c`. [Repo] refers to this repository. “Verified” means read in the pinned source, including test code where noted. No sketches or tests were compiled or run. [Neo] `Cargo.toml:12–16,190–193`, `src/lib.rs:95–103`. [Sys] `.duckdb-release:28–30`.

## 1. Registration, bind and lifecycle

**Verified in source**

- `TableFunctionBuilder::new(name, signature, implementation).register(extension)` installs the function. Registration accepts a `Connection` or `Extension`. The table function has no signature return type. Bind must append at least one result column with `BindFunctionHandle::add_result_column(&str, &LogicalType)`. The current core entrypoint registers only the version scalar, so add the reader registration there. [Neo] `src/table_function/mod.rs:45–58,648–664,688–699,789–816`, `src/handles.rs:132–144`, `src/signature.rs:251–257`. [Core] `src/main/capi/v2/capi_v2_func_table.cpp:407–446,837–862`. [Repo] `Cargo.toml:10–13`, `crates/core/src/lib.rs:13–17`, `crates/core/src/version.rs:43–57`, `crates/native/src/lib.rs:1–6`.
- `bind(&self, &Context, BindArguments<'_>, BindFunctionHandle<'_>) -> Result<BindData>` computes the schema and returns owned data. `init_global_state(&self, &Context, &BindData, InitColumnData<'_>) -> Result<Option<GlobalState>>` creates shared execution state. In that same trampoline `max_threads(&self, &Context, &BindData, Option<&GlobalState>, InitColumnData<'_>) -> Result<Option<NonZeroUsize>>` runs immediately after global init. `None` keeps the default of one. `init_local_state(&self, &Context, &BindData, Option<&GlobalState>, InitColumnData<'_>) -> Result<Option<LocalState>>` initializes a worker. `exec` gets shared bind/global references, optional exclusive local state, a mutable output chunk and the projection mapping. Only bind and exec need custom implementations for a minimal function. [Neo] `src/table_function/mod.rs:82–98,129–155,267–329,819–893`. [Core] `src/main/capi/v2/capi_v2_func_table.cpp:149–154,469–496,502–539`.
- Cardinality is a bind-time setter, `bind.set_cardinality(count, is_exact)?`, with `count: usize`. It supplies an optimizer estimate and, when exact, a maximum. A different actual count is not a runtime error. Progress is `progress(&self, &Context, &BindData, Option<&GlobalState>) -> Result<Option<f64>>` and requires `.with_progress()`. Return a fraction in `0.0..=1.0`. A registered callback returning `None` leaves the progress at zero. The engine clamps and converts to percent. It can call progress concurrently with exec. [Neo] `src/table_function/mod.rs:60–68,331–345,676–686,729–737,848–859`. [Core] `src/main/capi/v2/capi_v2_func_table.cpp:648–662,694–718`. [Repo] `docs/template-examples/sequence.rs:113–165,278–296`.
- Use `.param(FunctionParameter::new::<T>(name))` for a required argument and `.kind(FunctionParameterKind::Named)` for a named-only slot. `with_default` and `default_value` allow omission. The default kind is `Standard`, which also accepts a name. The binder fills named defaults and orders fixed positional, positional variadic, named-only, then named variadic arguments. `arg(0)?` selects the first slot. `arg_by_name(name)?` is case-insensitive, skips positional-only slots, and returns `None` when no slot matches. Standalone table-function test code exercises both `=` and `:=` named syntax. [Neo] `src/signature.rs:42–55,110–150,238–257,283–294`, `src/bind_arguments.rs:14–18,49–99,234–247,397–416`. [Core] `src/function/function.cpp:290–315`, `src/main/capi/v2/capi_v2_func_table.cpp:407–420`.
- `BindArgument::value()?` returns `Option<Value>`. `None` covers unresolved/non-foldable values, while a constant SQL NULL is a null `Value`. `resolved_value::<T>()?` returns `Option<Option<T>>` with the same distinction. Table calls normally deliver already folded and cast constants, including defaults. Other folding errors are propagated. [Neo] `src/bind_arguments.rs:130–172`. [Core] `src/function/function_binder.cpp:498–522,598–608,620–657`, `src/main/capi/v2/capi_v2_func_table.cpp:407–420`.
- A named-only `types` declared as `duckdb_neo::types::Any` preserves the supplied SQL type. For a MAP-only contract, declare `Map<String, String>` with an empty `Map::<String, String>::default()` or a typed null default. It implements `DuckDBType`, `ToValue` and `FromValue`, and its Rust payload is `Vec<(String, Option<String>)>`. For arbitrary STRUCT field names, inspect the actual `Value::logical_type()?.get_params()?` and pair each field name with `Value::get_child(i)?`. The C logical-type implementation exposes STRUCT field names and child types in order. `Value::get::<String>()?` distinguishes SQL NULL and uses the VARCHAR getter. [Neo] `src/types/primitive.rs:19–29`, `src/types/map.rs:18–27,41–100`, `src/types/mod.rs:101–114,134–146`, `src/types/string.rs:32–36`, `src/logical_type.rs:138–185`, `src/value.rs:283–293,312–325,388–412`. [Core] `src/main/capi/v2/capi_v2_logical_type.cpp:155–164,192–197`, `src/function/function_binder.cpp:460–479,498–502,604–608`.

**Proposed bind.** The following defaults illustrate mechanics. Choose the default encoding and CRC policy when implementing the format contract. `None::<String>` supplies a typed SQL NULL as the `ANY` default. Use a typed MAP parameter when supporting MAP alone. For both forms, match the runtime type and require non-null VARCHAR type names, unique column keys, and supported destination types. In a multi-file scan, track overrides separately from the per-file schema so an override for a column absent in one file can still apply in another.

```rust
use duckdb_neo::{
    DuckDBType, Error, FromValue, Result,
    bind_arguments::BindArguments,
    connection::Context,
    logical_type::{LogicalType, LogicalTypeID},
    signature::{FunctionParameter as Param, FunctionParameterKind as Kind, SignatureBuilder},
    types::{Any, DateValue, DecimalSignature, Map},
    value::Value,
};

fn file_signature() -> SignatureBuilder {
    SignatureBuilder::new()
        .param(Param::new::<String>("path").kind(Kind::PositionalOnly))
        .param(Param::with_default("encoding", None::<String>).kind(Kind::Named))
        .param(Param::with_default("all_varchar", false).kind(Kind::Named))
        .param(Param::with_default("include_deleted", false).kind(Kind::Named))
        .param(Param::with_default("verify_crc", false).kind(Kind::Named))
        .param(Param::new::<Any>("types").kind(Kind::Named).default_value(None::<String>))
}

fn named<T: FromValue>(args: &BindArguments<'_>, name: &str) -> Result<Option<T>> {
    let Some(arg) = args.arg_by_name(name)? else {
        return Ok(None);
    };
    arg.resolved_value::<T>()?
        .ok_or_else(|| Error::invalid_input(format!("{name} must be constant")))
}

fn type_overrides(value: &Value) -> Result<Vec<(String, Option<String>)>> {
    if value.is_null()? {
        return Ok(Vec::new());
    }
    let ty = value.logical_type()?;
    match ty.id()? {
        LogicalTypeID::Map => {
            // Validate MAP(VARCHAR, VARCHAR) against ty before this conversion.
            let map = value.get::<Map<String, String>>()?
                .ok_or_else(|| Error::invalid_input("types must be a non-NULL map"))?;
            Ok(map.0)
        }
        LogicalTypeID::Struct => ty.get_params()?.into_iter().enumerate()
            .map(|(i, (name, _))| Ok((name, value.get_child(i)?.get::<String>()?)))
            .collect(),
        _ => Err(Error::invalid_input("types must be a STRUCT or MAP")),
    }
}

// In bind:
// let path = args.arg(0)?.resolved_value::<String>()?.flatten()
//     .ok_or_else(|| Error::invalid_input("read_dbc: expected a non-NULL file path"))?;
// let encoding = named::<String>(&args, "encoding")?;
// let include_deleted = named::<bool>(&args, "include_deleted")?.unwrap_or(false);
// let overrides = match args.arg_by_name("types")? {
//     Some(arg) => type_overrides(&arg.value()?
//         .ok_or_else(|| Error::invalid_input("types must be constant"))?)?,
//     None => Vec::new(),
// };
```

Read and validate the header in bind, then retain the owned path, decoding options, DBF layout, per-file schema and header record count. For each field append its chosen type. The primitives use `DuckDBType::logical_type`. The runtime decimal constructor is `DecimalSignature { width, scale }.logical_type(context)`. Its `check()` validates `1 <= width <= 38` and `scale <= width`. To parse override strings use `LogicalType::from_text(context, text)?`, inspect `id()`, whitelist the supported IDs and extract runtime decimal parameters with `DecimalSignature::from_logical_type`. Store the resulting decoding plan as owned Rust values. [Neo] `src/types/mod.rs:60–85`, `src/types/string.rs:39–55`, `src/types/primitive.rs:73–100`, `src/types/temporal.rs:22–32,62–68`, `src/types/decimal.rs:307–312,362–395`, `src/logical_type.rs:108–117,138–140`.

```rust
// These calls illustrate the schema constructors used in BindFunctionHandle::add_result_column.
let decimal = DecimalSignature { width: 24, scale: 2 };
decimal.check()?;
bind.add_result_column("amount", &decimal.logical_type(context)?)?;
bind.add_result_column("name", &String::logical_type(context)?)?;
bind.add_result_column("id", &i64::logical_type(context)?)?;
bind.add_result_column("day", &DateValue::logical_type(context)?)?;
bind.add_result_column("active", &bool::logical_type(context)?)?;
bind.set_cardinality(header_record_count, include_deleted)?;
```

**Inference for DBC.** The header count is a count of physical records. Use it as an exact cardinality only if every record is emitted and the decoder enforces the header count. With `include_deleted=false` it is an estimate, since the flags are in the record stream. For progress, increment an atomic count of physical records consumed, including deleted ones. Keep final completion separate from the row count until the decoder finishes end-of-stream and any CRC check. A buffered input byte count can run ahead of decompression. The repo's format observations distinguish the record count, deleted flags, and optional decoded EOF byte. [Repo] `docs/research/dbc-format.md:27–32,69–74`.

## 2. Projection and filters

**Verified in source.** Enable `.with_projection_pushdown(true)`. During init, `InitColumnData::count()` and `get_column_index(projected_index)` map scan positions to bind-declared indices. During exec use `ExecColumnInfo::count()` and `get_column_index(output_index)`. They describe the actual chunk order, including when the wrapper asks for reordered local columns. Without pushdown the C v2 shim exposes every declared column. For a row-only scan the shim falls back to the first declared column and supplies a scratch vector when the engine's output chunk has zero columns. The reader must still set the row count through that vector. [Neo] `src/table_function/mod.rs:100–126,164–200,648–674`, `src/multi_file/tests.rs:113–137,225–235,280–296`. [Core] `src/main/capi/v2/capi_v2_func_table.cpp:449–465,605–644`, `src/common/multi_file/table_function_multi_file.cpp:243–247`.

```rust
// In impl TableFunctionCallbacks for DbcReader. The row and column helpers are reader code.
fn exec(
    &self,
    context: &Context,
    bind_data: &DbcBind,
    local: Option<&mut DbcLocal>,
    global: Option<&FileGlobal>,
    output: &mut DataChunk<'_>,
    column_info: ExecColumnInfo<'_>,
) -> Result<()> {
    let local = local.ok_or_else(|| Error::internal("read_dbc: missing local state"))?;
    let global = global.ok_or_else(|| Error::internal("read_dbc: missing global state"))?;
    // Open on first call after claim, count physical records, and validate before EOF.
    let rows = local.next_visible_rows(context, bind_data, global, output.capacity()?)?;
    for output_index in 0..column_info.count()? {
        let bind_index = column_info.get_column_index(output_index)?;
        let column = &bind_data.columns[bind_index];
        write_projected_column(output, output_index, column, &rows)?;
    }
    Ok(())
}
```

**Verified in source.** `TableFilterPushdownCallbacks::pushdown_filter(&self, &Context, &BindData, PushdownData<'_>) -> Result<()>` is wired by `.with_filter_pushdown()`. The callback reads `filter_count()`, `filter(i)`, and the pushdown-time column mapping. `filter(i)` borrows a bound expression for the callback. The expression API can inspect its kind, children, constants and reference indices. `accept_pushdown(i)` removes DuckDB's responsibility for that predicate. Save any accepted predicate as owned data using interior mutability. The C v2 hook is a complex-filter callback, which the multi-file adapter does not forward to the inner function. [Neo] `src/table_function/mod.rs:202–265,347–356,578–591,895–911`, `src/expression.rs:80–153`. [Core] `src/main/capi/v2/capi_v2_func_table.cpp:867–872`, `src/common/multi_file/table_function_multi_file.cpp:889–908`. [Repo] `docs/template-examples/sequence.rs:234–275,293–296`.

**Proposed first pass.** Skip conversion and string transcoding for unprojected fields. The sequential decoder still consumes complete records and checks flags and CRC as required. Leave value predicates to DuckDB. A predicate on a DBF value cannot skip compressed bytes in the general case. The wrapper can handle its generated filename and Hive constants using its own pushdown. [Core] `src/include/duckdb/common/multi_file/multi_file_function.hpp:103–116,750–755`, `src/common/multi_file/multi_file_reader.cpp:773–798`.

## 3. Output vectors and errors

**Verified in source.** The exec trampoline borrows writable `DataChunk<'_>`. Use `output.capacity()?` as the upper bound. The engine's default `STANDARD_VECTOR_SIZE` is 2048 and can be overridden at build time. Borrow each vector with `get_vector_at_mut::<T>(output_index)?`, call `set_size(n)?`, then acquire `writer()?`. Typed narrowing checks the logical type and an out-of-range vector index panics. `set` writes a valid row, `set_null` clears validity and `set_option` dispatches between the two. The shim takes vector 0's size and propagates the count through the chunk. Set each vector's size when using safe typed writers, which check their own bounds. EOF is `Ok(())` with an empty chunk, or explicitly vector 0 sized to zero. With a claim callback this means end of the current batch. [Neo] `src/table_function/mod.rs:129–155,829–838,1018–1027`, `src/data_chunk.rs:427–440,466–493`, `src/vector/mod.rs:582–591,665–672`, `src/vector/writer.rs:298–327`. [Core] `src/include/duckdb/common/vector_size.hpp:15–24`, `src/main/capi/v2/capi_v2_func_table.cpp:576–599,615–644`.

| SQL type | Vector type `T` | Example non-NULL write |
| --- | --- | --- |
| BIGINT | `i64` | `writer.set(row, &number)?` |
| BOOLEAN | `bool` | `writer.set(row, &flag)?` |
| DATE | `DateValue` | `writer.set(row, &DateValue(days_since_1970))?` with signed `i32` days |
| DECIMAL width 1–4 | `ShortDecimal` | `writer.set(row, &ShortDecimal(scaled_i16))?` |
| DECIMAL width 5–9 | `WordDecimal` | `writer.set(row, &WordDecimal(scaled_i32))?` |
| DECIMAL width 10–18 | `LongDecimal` | `writer.set(row, &LongDecimal(scaled_i64))?` |
| DECIMAL width 19–38 | `HugeDecimal` | `writer.set(row, &HugeDecimal(scaled_i128))?` |
| VARCHAR | `String` | `writer.set(row, utf8_str)?` where `utf8_str: &str` |

[Neo] `src/types/mod.rs:60–74`, `src/types/primitive.rs:137–149,151–181`, `src/types/temporal.rs:22–68`, `src/types/decimal.rs:45–117`, `src/types/string.rs:298–310`.

**Verified in source.** DECIMAL uses four width classes. Even if the parser uses `i64` for all widths through 18, widths through 9 must be narrowed before writing. The runtime wrappers validate the vector's width class and copy the physical payload. Validate scaled digits separately, for example with `decimal_signature.with_payload(scaled_i128).check()?`, followed by checked integer narrowing. The packed decimal markers account for vector alignment. Do not borrow the packed `i128` field or reinterpret a DuckDB DECIMAL vector as an `i128` slice. [Neo] `src/types/decimal.rs:45–117,325–349,397–400`.

```rust
use duckdb_neo::types::HugeDecimal;

let mut vector = output.get_vector_at_mut::<HugeDecimal>(output_index)?;
vector.set_size(rows.len())?;
let mut writer = vector.writer()?;
for (row, scaled) in scaled_values.enumerate() {
    let checked = scaled
        .map(|value| {
            decimal_signature.with_payload(value).check()?;
            Ok::<_, Error>(HugeDecimal(value))
        })
        .transpose()?;
    writer.set_option(row, checked.as_ref())?;
}
```

For a width of 10–18, select `get_vector_at_mut::<LongDecimal>(output_index)?` and write `LongDecimal(i64::try_from(value).map_err(|_| Error::invalid_input("DECIMAL payload overflow"))?)` after the same precision check. The 1–4 and 5–9 branches use checked `i16` and `i32` conversion respectively. [Neo] `src/types/decimal.rs:94–117,340–349`.

**Verified in source.** For UTF-8 bytes, borrow with `std::str::from_utf8(bytes)` and pass the resulting `&str` to `VectorWriter<String>::set` or `set_option`. The Rust API needs no intermediate `String`. DuckDB copies bytes into an inline slot or a vector arena. `VectorWriter<String>::set_parts` can concatenate UTF-8 pieces directly there. For legacy encoded text, transcode into reusable scratch and then write valid UTF-8. The public arbitrary byte writer targets `BlobValue`. [Neo] `src/types/string.rs:298–325`, `src/vector/writer.rs:320–327,570–585,642–662`, `src/bytes.rs:88–136`.

**Verified error path.** Return `Err(Error::invalid_input(format!("read_dbc: {path}: record {record_no}: {detail}")))` for bad options, malformed values, truncation or CRC mismatch. Propagate lower-level failures with `?`, adding file and record context when useful. The Rust trampoline writes the error code and message onto the C error handle. The engine throws it before accepting the chunk. [Neo] `src/error.rs:132–137`, `src/builder_helpers.rs:103–113,168–209`, `src/table_function/mod.rs:145–155,800–806`. [Core] `src/main/capi/v2/capi_v2_func_table.cpp:625–644`.

**Proposed EOF policy.** A stretch of deleted records must not produce an empty chunk in the middle of a file. Continue until at least one visible row is ready or the file genuinely ends. Check the decoded end and CRC before returning final EOF. An early SQL stop such as `LIMIT` can release a reader before it sees EOF. If `verify_crc=true` must guarantee a full-file check regardless of query consumption, perform an explicit validation pass before emitting rows. Otherwise document CRC verification on full consumption. A destructor cannot communicate a normal `Result` to the query. [Neo] `src/table_function/mod.rs:829–838,1018–1027`, `src/builder_helpers.rs:115–132`. [Core] `src/common/multi_file/table_function_multi_file.cpp:834–839`. [Repo] `docs/research/dbc-format.md:27–32`.

## 4. DuckDB file access

**Verified in source.** `FileSystem::new(context)?` borrows the callback's filesystem. `FileBuilder::new(&fs, path)?.read().open()?` returns an owned `File`. `file.size()?` returns `u64`. `read(len)?` allocates a buffer, while `read_into(&mut [u8])?` fills caller-provided storage and returns the number of bytes read. Sequential reads advance the cursor. A short read can occur and zero bytes is EOF for a nonempty buffer. `tell()` returns the cursor and `seek(usize)` takes an absolute byte position. `read_exact_at(offset, buf)` is positional and leaves the cursor unchanged. `close()` closes the underlying file, and handle destruction is automatic on drop. [Neo] `src/file.rs:72–92,150–171,180–184,242–255,280–352,370–386`, `src/links.rs:119–128`. [Core] `src/include/duckdb_v2.h:3161–3186`. [Repo] `docs/template-examples/copy.rs:213–249,297–340`.

**Verified ownership and routing.** `File: Send` and is explicitly documented as non-`Sync`. It can live in `LocalState` as-is, or in `GlobalState` behind `Mutex<File>` or `Mutex<Decoder>`. Context-opened files can survive the callback within scan state and must be dropped before the connection ends. The context itself and its borrowed filesystem are callback scoped. Connection- or instance-opened files keep their owner alive. `.parallel_access()` does not make the Rust file shareable. [Neo] `src/file.rs:72–92,214–219,251–255,280–309`, `src/connection.rs:347–356`, `src/links.rs:15–38`, `src/table_function/mod.rs:821–827`.

The context filesystem routes open through DuckDB's VFS and supplies the session opener for settings and secrets. The VFS selects a registered subsystem for the path and can try autoloading when a subsystem is absent. DuckDB maps `http://`, `https://`, and `s3://` to `httpfs`. A loaded and configured `httpfs` therefore handles those paths through this Rust API. This is a source-derived routing conclusion. HTTP range behavior, credentials, glob support of a given protocol and remote request patterns were not exercised. [Core] `src/main/capi/v2/capi_v2_file_system.cpp:103–110,172–209,285–293`, `src/common/virtual_file_system.cpp:220–240,464–499`, `src/include/duckdb/main/extension_entries.hpp:1667–1669`.

**Proposed adapter.** Use a reusable input buffer, for example a `BufReader` over this adapter if the decoder frequently requests small reads. For initial implementation open in bind to parse only the header, then reopen the claimed file for execution. A metadata change between bind and exec should be an error if it invalidates the decoding layout.

```rust
use std::io::{self, Read};

use duckdb_neo::file::{File, FileBuilder, FileSystem};

struct VfsRead(File);

impl Read for VfsRead {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.0.read_into(bytes).map_err(io::Error::other)
    }
}

fn open_input(context: &Context, path: &str) -> Result<VfsRead> {
    let fs = FileSystem::new(context)?;
    let file = FileBuilder::new(&fs, path)?.read().open()?;
    Ok(VfsRead(file))
}
```

## 5. Multi-file registration, schema and option forwarding

**Verified registration and input.** First register a separately named single-file table function. Register the wrapper with `MultiFileFunctionBuilder::new("read_dbc", "read_dbc_file").reader_type("DBC").file_extension("dbc").register(extension)?`. The extension registration looks up the inner name in the catalog and copies/selects an overload. It requires exactly one required parameter and that parameter must accept position. The wrapping implementation also requires exactly one positional slot of type VARCHAR or ANY. Give all named-only options defaults if the function will be wrapped. A required named-only argument counts toward the required-parameter limit, even though it is not positional. Neo's exposed builder has no visibility switch, and the C v2 registration shown creates a regular catalog function. Treat the inner name as SQL-callable. An internal-looking name is only a naming convention. [Neo] `src/multi_file/mod.rs:12–51,53–92`, `src/table_function/mod.rs:564–685,809–816`, `src/handles.rs:132–144,247–259`, `src/multi_file/tests.rs:307–310,344–362`. [Core] `src/main/capi/v2/capi_v2_func_table.cpp:926–973,1642–1654,1858–1889`, `src/include/duckdb/function/function.hpp:400–426`, `src/common/multi_file/table_function_multi_file.cpp:867–887`.

A VARCHAR inner parameter receives only the resolved path. An ANY inner parameter receives either a VARCHAR or a STRUCT with `filename VARCHAR` and the other fields as open options. Decode the struct with the same child access as in §1. Forward recognized VFS hints with `builder = builder.set_value(name, &value)?` before `open()`. The wrapper catalog has VARCHAR, LIST(ANY), and VARIANT overloads. A list may contain paths or file structs, and the VARIANT overload unwraps a file value. A struct with open information is treated as an explicit file, bypassing glob expansion for that entry. `.file_extension("dbc")` configures the fallback glob for directories and takes no leading dot. [Neo] `src/multi_file/mod.rs:14–18,47–51`, `src/file.rs:229–248`. [Core] `src/common/multi_file/table_function_multi_file.cpp:75–112,918–922`, `src/common/open_file_info.cpp:10–75`, `src/common/multi_file/multi_file_reader.cpp:202–256,1002–1020`, `src/common/multi_file/multi_file_list.cpp:408–428`, `src/main/capi/v2/capi_v2_func_table.cpp:936–940,1843–1855`.

**Verified built-ins and schema.** DuckDB adds `union_by_name`, `filename`, `hive_partitioning`, and related options to its own typed kwargs schema. It parses built-in options before reader options. `filename=true` adds a visible constant VARCHAR column, and a string supplies a custom column name, useful for avoiding collisions. The engine can also expose a virtual filename when no visible one was added. Hive columns are derived from path partitions, with matching partition keys required across files. The inner bind and exec only provide physical DBF columns. [Core] `src/include/duckdb/common/multi_file/multi_file_function.hpp:103–116,271–286`, `src/common/multi_file/multi_file_reader.cpp:122–143,506–533,678–712,763–798`.

Without `union_by_name` or an explicit `schema`, the wrapper uses the first file's schema and maps later files by name. It can reorder fields, but a missing projected root column can error. With `union_by_name=true` DuckDB expands the file list, reads schemas for all files through bind tasks, appends names in first-seen order, and reconciles duplicate names with `LogicalType::DefaultForceMaxLogicalType`. Matching is case-insensitive. A missing column becomes typed NULL. The wrapper applies its type mapping and casts to the combined schema after reading the projected file-native types. The generic C v2 builder does not install a custom schema-combination callback or the native cast-map mode. Keep bind cheap and ready to run again when a file is opened for execution. The stock `schema` option cannot be combined with `union_by_name=true` and controls wrapper mapping, distinct from DBC field decoding overrides. [Core] `src/common/multi_file/multi_file_reader.cpp:627–638,811–852,1039–1078,1292–1311`, `src/common/multi_file/union_by_name.cpp:19–29,49–78`, `src/common/multi_file/multi_file_function.cpp:49–56`, `src/common/multi_file/multi_file_column_mapper.cpp:165–212,928–953,987–1008`, `src/common/multi_file/table_function_multi_file.cpp:551–582,637–655,714–724`, `src/include/duckdb/common/multi_file/table_function_multi_file.hpp:187–210`, `src/main/capi/v2/capi_v2_func_table.cpp:936–949`.

**Verified named-option discrepancy.**

1. The Rust wrapper docs and C header say the inner named options are forwarded. The actual wrapper constructs its own signature, then merges only `single_file_function.GetSignature().GetTypedKwargs()`. It does not copy the inner `KEYWORD_ONLY` entries. [Neo] `src/multi_file/mod.rs:12–18`. [Core] `src/include/duckdb_v2.h:14700–14717`, `src/common/multi_file/table_function_multi_file.cpp:881–889`.
2. Rust `FunctionParameterKind::Named` creates a `KEYWORD_ONLY` parameter, and `NamedVariadic` creates a plain `VAR_KEYWORD` parameter. `duckdb_v2_function_signature_add_parameter` adds an ordinary parameter. The C helper preserves any typed kwargs already present, but does not create a schema from these declarations. Rust's public signature builder exposes no `AddTypedKwargs` equivalent. [Neo] `src/signature.rs:42–55,232–299`. [Core] `src/main/capi/v2/capi_v2_func_signature.cpp:6–42`, `src/include/duckdb/main/capi/capi_function_signature.hpp:19–25,62–78`, `src/include/duckdb/function/function.hpp:341–386`.
3. When binding the outer call, DuckDB rejects a name missing from the wrapper's typed schema. The wrapper has a later reader-option parser that knows how to look up inner keyword-only slots, and refills inner named defaults at per-file bind. The SQL binder prevents a custom option from reaching that parser through the wrapper signature. **Source-derived outcome:** `read_dbc('x.dbc', encoding='...')`, and the other four reader options, are rejected on this path. A direct call to `read_dbc_file` can accept all five. Turning `types` into MAP, or adding plain `NamedVariadic`, leaves the wrapper's typed schema unchanged. [Core] `src/function/function_binder.cpp:550–608,645–657`, `src/include/duckdb/common/multi_file/multi_file_function.hpp:271–286`, `src/common/multi_file/table_function_multi_file.cpp:82–97,444–481,881–889`.

**Proposed integration choices.** Keep the direct reader and wrapper as two entry points if the SQL surface can be split. The native wrapper can also support an explicit file-list workaround using an ANY inner parameter. Parse a reserved `dbc_options` STRUCT from each file's metadata separately from the VFS hints. This is a proposed convention, with input transport verified above. Ordinary globs work with defaults. To combine globbing and per-file options through this workaround, the caller would need to expand the glob before constructing the file structs. To deliver the exact single SQL interface with custom named options and the native wrapper, fix DuckDB to expose those declarations in the outer typed schema or use a newer pin after verifying the forwarding behavior there. An extension-owned multi-file function could retain the SQL interface but would have to implement schema and file orchestration itself. `FileSystem::glob` is available for that work. [Neo] `src/file.rs:134–147`. [Core] `src/common/multi_file/multi_file_list.cpp:408–428`, `src/common/multi_file/table_function_multi_file.cpp:881–889`.

```rust
// In crates/core registration, after version::register(extension)?.
// Switch the first Param to Any if supporting explicit per-file options.
TableFunctionBuilder::new("read_dbc_file", file_signature(), DbcReader)
    .with_projection_pushdown(true)
    .with_progress()
    .with_claim_batch() // one whole file is one claim, see §7
    .register(extension)?;
MultiFileFunctionBuilder::new("read_dbc", "read_dbc_file")
    .reader_type("DBC")
    .file_extension("dbc")
    .register(extension)?;
```

```sql
-- Source-supported call shapes, once the sketch is implemented.
SELECT * FROM read_dbc_file('x.dbc', include_deleted=true, types={'ID': 'BIGINT'});
SELECT * FROM read_dbc_file('x.dbc', types=MAP {'AMOUNT': 'DECIMAL(24,2)'});
SELECT * FROM read_dbc(['data/*.dbc', 'other.dbc'], union_by_name=true, filename=true);
-- Proposed convention, requires the ANY file argument and dbc_options parsing.
SELECT * FROM read_dbc(
    [{'filename': 'x.dbc', 'dbc_options': {'encoding': 'windows-1252'}}],
    union_by_name=true, filename='source_path'
);
```

**Verified optional callback.** `TableGetBindInfoCallbacks::bind_info(&self, &Context, &BindData, BindInfoApi<'_>) -> Result<()>` is installed by `.with_get_bind_info()`. It can be called more than once. `api.set_column_identifier(index, &[], &identifier_value)?` sets a top-level INTEGER field ID or VARCHAR name, and a nonempty child path addresses nested fields. `api.set_option(key, &value)?` exposes reader metadata. Without the callback, normal name mapping still works. DBC can omit this hook unless it has meaningful persistent field IDs or metadata to expose. A local column ordinal is not a stable cross-file ID. A metadata option named `row_count` does not replace `set_cardinality`. [Neo] `src/table_function/mod.rs:593–604,975–1015`. [Core] `src/common/multi_file/table_function_multi_file.cpp:114–161`, `src/common/multi_file/multi_file_reader.cpp:627–638`.

**Verified test source.** `multi_file/tests.rs` registers a VARCHAR `read_kv_file` with projection, bind info, and claim callbacks, then wraps it as `read_kv`. Its bind uses VFS reads but loads the whole fixture into memory. The fixture claims chunks of 100 rows with an atomic counter, writes projected BIGINT columns, and emits an empty chunk to end each batch. It tests a single file, a glob with a `schema=map {...}` mapping renamed and reordered fields by integer field ID, projected/aggregate scans, a directory suffix fallback and a direct inner call. A separate test checks default name mapping for reordered columns and a no-match glob. These tests do not exercise forwarding custom named reader options or the `union_by_name=true` switch. [Neo] `src/multi_file/tests.rs:27–85,113–172,225–341,344–362`.

**Verified reporting limits.** The wrapper reads each bound file's cardinality estimate. When every union reader has one, the outer estimate can sum them, without automatically making that sum a guaranteed upper bound. Where it uses the format fallback it multiplies the first file's estimate by file count, or estimates from file sizes. Wrapped progress uses the inner progress callback, with an outer average by file and accounting for skipped and completed files. Report per-file completion only after final validation. [Core] `src/common/multi_file/table_function_multi_file.cpp:155–161,175–185,403–408,602–608,747–765`, `src/include/duckdb/common/multi_file/multi_file_function.hpp:1287–1385`.

## 6. Replacement scan

**Verified API.** `ReplacementScanCallbacks::scan` sees unresolved table references. Return `Ok(())` without setting a reference to decline. Set `ReplacementType::Table(qualified_name)`, then append the path with `handle.add_parameter(path.value(context)?)`. `add_parameter_with_name` also exists and still follows the target signature. The repository template does the suffix check and registration through `ReplacementScanBuilder::new(...).register(extension)`. The C layer creates a normal table-function reference with the given arguments. **Inference.** The same callback can target the registered multi-file `read_dbc`. A VARCHAR path resolves to its path overload and uses the wrapper's supported arguments and the inner reader's defaults. [Neo] `src/replacement_scan/mod.rs:38–82,196–242`, `src/handles.rs:261–274`, `src/qualified_name.rs:44–49,86–103`, `src/types/string.rs:45–59`. [Core] `src/main/capi/v2/capi_v2_replacement_scan.cpp:66–84`, `src/common/multi_file/multi_file_reader.cpp:1002–1020`. [Repo] `docs/template-examples/replacement_scan.rs:1–11,72–94,189–206`.

```rust
impl ReplacementScanCallbacks for DbcReplacement {
    fn scan(&self, context: &Context, name: &QualifiedName, handle: ReplacementHandle<'_>)
        -> Result<()>
    {
        let view = name.get_view()?;
        if view.catalog.is_none()
            && view.schema.is_none()
            && view.table.to_ascii_lowercase().ends_with(".dbc")
        {
            handle.set_reference(ReplacementType::Table("read_dbc".try_into()?))?;
            handle.add_parameter(view.table.value(context)?)?;
        }
        Ok(())
    }
}
// After both table functions have registered:
// ReplacementScanBuilder::new(DbcReplacement).register(extension)?;
```

## 7. One sequential decoder per file

**Verified scheduling.** For a single open file, the wrapper can use the inner `MaxThreads` to cap the pipeline. For multiple files it leaves that cap unset and the outer scan starts from DuckDB's scheduler thread count. In this path `TryInitializeScan` uses its own compare-and-exchange single-assignment guard only for an inner function with *no native init_local function*. The C v2 table function always has a native init-local shim, even when Rust returns `None`. Therefore `max_threads() = Some(1)` alone is insufficient to enforce a single worker per file under a multi-file scan. Without a claim hook, the initialization path may assign one file to multiple workers before an exec marks it exhausted. [Core] `src/common/multi_file/table_function_multi_file.cpp:270–294,314–346,372–392,535–549`, `src/main/capi/v2/capi_v2_func_table.cpp:503–539,858–862`, `src/include/duckdb/common/multi_file/multi_file_function.hpp:703–748,862–899`.

**Verified claim contract.** Implement `TableClaimBatchCallbacks::claim_batch(&self, &Context, &BindData, Option<&mut LocalState>, Option<&GlobalState>) -> Result<bool>` and register `.with_claim_batch()`. `true` gives this worker a batch. Exec can produce multiple chunks for it, then an empty chunk signals batch completion. `false` means there is no next batch to assign. In the wrapper, the C v2 adapter claims batches externally and the outer scheduler advances to another file when a claim fails. Jobs retain a reader reference and scan after releasing the claim lock. Direct SQL calls use the C v2 shim's own claim/exec loop. [Neo] `src/table_function/mod.rs:369–382,606–616,1018–1037`. [Core] `src/main/capi/v2/capi_v2_func_table.cpp:568–599,936–948`, `src/common/multi_file/table_function_multi_file.cpp:314–346,372–392`, `src/include/duckdb/common/multi_file/multi_file_function.hpp:703–748,962–983`.

**Proposed state split.**

- `BindData` is immutable per-file schema, path, options and header metadata.
- `GlobalState` is per-file scan state, shared across its workers and progress. Use an `AtomicBool` to claim that file once, an atomic physical-row counter, and an atomic completion flag. Keep the registered callback object free of per-scan mutable decoder state.
- `LocalState` is per worker and current file. It starts with no decoder, gets marked by the successful claim, and opens the stream lazily in the first exec for that claim. This avoids file I/O while the outer claim lock is held. As soon as the current file has been assigned, a losing worker returns false for that file and can move to the next file. Different files can exec concurrently, bounded by available workers, with each file's decompressor driven by one worker. A worker can read several files over its lifetime. The wrapper owns separate scan-wide scheduling state and keeps the file reader and its state alive while its job is still active. [Core] `src/common/multi_file/table_function_multi_file.cpp:19–50,55–60,286–294,314–346,372–379`, `src/include/duckdb/common/multi_file/multi_file_function.hpp:703–748`.

```rust
use std::{num::NonZeroUsize, sync::atomic::{AtomicBool, AtomicU64, Ordering}};

#[derive(Default)]
struct FileGlobal {
    assigned: AtomicBool,
    records_consumed: AtomicU64,
    complete: AtomicBool,
}

impl TableClaimBatchCallbacks for DbcReader {
    fn claim_batch(
        &self,
        _context: &Context,
        _bind: &DbcBind,
        local: Option<&mut DbcLocal>,
        global: Option<&FileGlobal>,
    ) -> Result<bool> {
        let local = local.ok_or_else(|| Error::internal("read_dbc: missing local state"))?;
        let global = global.ok_or_else(|| Error::internal("read_dbc: missing global state"))?;
        let claimed = !global.assigned.swap(true, Ordering::Relaxed);
        local.claimed = claimed;
        Ok(claimed)
    }
}

// In impl TableFunctionCallbacks for DbcReader:
// type BindData = DbcBind;
// type LocalState = DbcLocal;
// type GlobalState = FileGlobal;
fn init_global_state(
    &self,
    _context: &Context,
    _bind: &DbcBind,
    _columns: InitColumnData<'_>,
) -> Result<Option<FileGlobal>> {
    Ok(Some(FileGlobal::default()))
}

fn init_local_state(
    &self,
    _context: &Context,
    _bind: &DbcBind,
    _global: Option<&FileGlobal>,
    _columns: InitColumnData<'_>,
) -> Result<Option<DbcLocal>> {
    Ok(Some(DbcLocal::default())) // unclaimed, decoder unopened
}

fn max_threads(
    &self,
    _context: &Context,
    _bind: &DbcBind,
    _global: Option<&FileGlobal>,
    _columns: InitColumnData<'_>,
) -> Result<Option<NonZeroUsize>> {
    Ok(NonZeroUsize::new(1))
}

fn progress(
    &self,
    _context: &Context,
    bind: &DbcBind,
    global: Option<&FileGlobal>,
) -> Result<Option<f64>> {
    Ok(global.map(|global| {
        if global.complete.load(Ordering::Relaxed) {
            return 1.0;
        }
        let total = u64::from(bind.header_record_count); // DBF count as u32
        let scanned = global.records_consumed.load(Ordering::Relaxed).min(total);
        scanned as f64 / (total as f64 + 1.0) // reserve completion for validation
    }))
}
```

## 8. FFI, cancellation and compatibility

- **Verified state bounds.** The implementation is `Send + Sync + 'static`. `BindData` and `GlobalState` are `Any + Send + Sync`, and `LocalState` is `Any + Send`. DuckDB owns opaque boxed state until its destroy callback runs. Keep callback handles, expression borrows, chunks and vector writers in their callback scope. Retain copied metadata, the owned decoder and file, and owned indices instead. [Neo] `src/table_function/mod.rs:47–48,93–97,140–152,181–198,240–251,284–325,821–827`, `src/builder_helpers.rs:115–132`, `src/connection.rs:347–356`, `src/data_chunk.rs:315–328`.
- **Verified panic handling.** Callback trampolines catch Rust unwinding and turn a panic into an INTERNAL DuckDB error, which invalidates the instance. Opaque state destruction is an extern callback without that catch. Prefer ordinary `Result` errors on data, I/O, conversion and CRC failures. Keep destructors non-panicking and put fallible completion checks in exec. [Neo] `src/builder_helpers.rs:115–132,158–212`, `src/error.rs:132–155`. [Core] `src/main/capi/v2/capi_v2_func_table.cpp:627–644`.
- **Verified cancellation surface.** `Connection::interrupt_query()` and `InterruptHandle::interrupt()` request cancellation from the caller. The callback `Context` and C v2 table exec surface have no public interrupt-status getter in this pin. DuckDB's pipeline checks interrupts between work units and at operator start. A synchronous Rust exec can delay cancellation while it is decompressing or blocked in VFS, so keep chunk and I/O work bounded where practical. Skipping a long run of deleted rows may still extend a single call because an empty chunk signals completion. [Neo] `src/connection.rs:96–109,260–271,347–376`, `src/table_function/mod.rs:829–838`. [Core] `src/include/duckdb_v2.h:343–352,2003–2019,13712–13749`, `src/parallel/pipeline_executor.cpp:330–336,902–905`.
- **Verified compatibility.** Table functions, VFS reads, multi-file registration and replacement scans used here are marked unstable v2.0.0 in the v2.0.1 headers. The API version in the entrypoint does not promise ABI support across releases. Neo documents a loadable `build-extension` and exact-build pinning, with `C_STRUCT_UNSTABLE` packaging. Keep the version and commit pinned and recheck the source contracts on upgrade. [Neo] `extensions.md:3–26,45–54`. [Core] `src/include/duckdb_v2.h:103–118,2986–3018,3161–3186,10504–10523,14376–14396,14802–14822`. [Repo] `Cargo.toml:10–13`, `crates/native/src/lib.rs:1–6`.

//! The `read_dbc` table function and the `FROM 'file.dbc'` replacement scan.
//!
//! DuckDB v2.0.1's multi-file wrapper drops custom named parameters (see
//! `docs/research/upstream-multifile-kwargs.md`), so file lists, globs,
//! `union_by_name` and `filename` are implemented here.
//!
//! Execution: every file is one "batch". Workers claim file indexes from a
//! global counter (`claim_batch`), so each file is decoded front to back by a
//! single worker while different files run in parallel. The batch index
//! reported to DuckDB is the file index, which lets DuckDB restore file order
//! (then record order) when insertion order must be preserved.

mod schema;
mod sink;

use std::{
    io::{self, Read},
    num::NonZeroUsize,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use duckdb_neo::{
    DuckDBType, Result, ToValue,
    bind_arguments::BindArguments,
    connection::Context,
    data_chunk::DataChunk,
    error::Error,
    extension::Extension,
    file::{File, FileBuilder, FileSystem},
    logical_type::{LogicalType, LogicalTypeID},
    qualified_name::QualifiedName,
    replacement_scan::{
        ReplacementHandle, ReplacementScanBuilder, ReplacementScanCallbacks, ReplacementType,
    },
    signature::{
        FunctionDocumentationBuilder, FunctionParameter as Param, FunctionParameterKind as Kind,
        SignatureBuilder,
    },
    table_function::{
        BindFunctionHandle, ExecColumnInfo, InitColumnData, PartitionData, PartitioningData,
        TableClaimBatchCallbacks, TableFunctionBuilder, TableFunctionCallbacks, TablePartitionInfo,
        TablePartitioningCallbacks,
    },
    types::{Any, DateValue, DecimalSignature, Map},
    value::Value,
};
use read_dbc_format::{ColumnType, DbcReader, Encoding, Header, ReadOptions, resolve_schema};

use self::schema::{Conv, FileSchema, OutColumn};
use self::sink::{Buf, Sink, Slot, write_buf, write_nulls};
use crate::CATEGORY;

pub const FUNCTION_NAME: &str = "read_dbc";
const DELETED_COLUMN: &str = "_deleted";
const FILENAME_COLUMN: &str = "filename";
const ENCODING_NAMES: &str = "'auto', 'latin1' (or 'iso-8859-1'), 'cp437', 'cp850', 'cp860', 'cp1252' (or 'windows-1252'), 'utf8'";

/// Run a callback body, turning a Rust panic into an ordinary error so it
/// cannot invalidate the database (duckdb-neo would report it as INTERNAL).
fn guard<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(panic) => {
            let msg = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("unknown panic");
            Err(Error::invalid_input(format!(
                "read_dbc: internal error (panic): {msg}"
            )))
        }
    }
}

fn err(msg: impl Into<String>) -> Error {
    Error::invalid_input(format!("read_dbc: {}", msg.into()))
}

fn file_err(path: &str, e: impl std::fmt::Display) -> Error {
    err(format!("\"{path}\": {e}"))
}

/// `std::io::Read` over a DuckDB VFS file (local, or httpfs when loaded).
struct VfsRead(File);

impl Read for VfsRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.0.read_into(buf).map_err(io::Error::other)
    }
}

fn open(context: &Context, path: &str) -> Result<VfsRead> {
    let fs = FileSystem::new(context)?;
    let file = FileBuilder::new(&fs, path)?
        .read()
        .open()
        .map_err(|e| file_err(path, format!("cannot open file: {e}")))?;
    Ok(VfsRead(file))
}

fn is_dbf(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".dbf")
}

/// Everything known about one input file after bind.
#[derive(Debug)]
struct FileBind {
    path: String,
    dbf: bool,
    record_count: u64,
    /// Reader options for this file (per-file type overrides included).
    options: ReadOptions,
    /// The column types the reader must produce (checked again at open).
    types: Vec<ColumnType>,
    /// Per output data column: the file column and conversion, if present.
    plan: Vec<Option<(usize, Conv)>>,
}

#[derive(Debug)]
struct ScanBind {
    files: Vec<FileBind>,
    columns: Vec<OutColumn>,
    /// Number of data columns (before `_deleted` and `filename`).
    data_columns: usize,
    deleted_column: Option<usize>,
    filename_column: Option<usize>,
    total_records: u64,
}

struct ScanGlobal {
    next_file: AtomicUsize,
    records_done: AtomicU64,
    /// Declared column behind each output vector.
    out_columns: Vec<usize>,
}

/// The file a worker is decoding.
struct OpenFile {
    index: usize,
    reader: Option<DbcReader<VfsRead>>,
    sink: Sink,
    /// For each output vector: `Some(buf)` if the file provides it.
    buf_of_output: Vec<Option<usize>>,
    records_seen: u64,
}

#[derive(Default)]
struct ScanLocal {
    claimed: Option<usize>,
    current: Option<OpenFile>,
    last_batch: usize,
}

fn named<T: duckdb_neo::FromValue>(args: &BindArguments<'_>, name: &str) -> Result<Option<T>> {
    let Some(arg) = args.arg_by_name(name)? else {
        return Ok(None);
    };
    match arg.resolved_value::<T>()? {
        Some(v) => Ok(v),
        None => Err(err(format!("{name} must be a constant"))),
    }
}

fn parse_files(value: &Value) -> Result<Vec<String>> {
    let ty = value.logical_type()?;
    let bad = || {
        err(
            "the first argument must be a file path or glob (VARCHAR) or a list of them (VARCHAR[])",
        )
    };
    if value.is_null()? {
        return Err(bad());
    }
    match ty.id()? {
        LogicalTypeID::Varchar => Ok(vec![value.get::<String>()?.ok_or_else(bad)?]),
        LogicalTypeID::List => {
            let mut out = Vec::new();
            for child in value.children()? {
                let s =
                    child.get::<String>().ok().flatten().ok_or_else(|| {
                        err("the file list must contain only non-NULL VARCHAR paths")
                    })?;
                out.push(s);
            }
            if out.is_empty() {
                return Err(err("the file list is empty"));
            }
            Ok(out)
        }
        _ => Err(bad()),
    }
}

fn parse_types(value: &Value) -> Result<Vec<(String, ColumnType)>> {
    if value.is_null()? {
        return Ok(Vec::new());
    }
    let ty = value.logical_type()?;
    let entries: Vec<(String, Option<String>)> = match ty.id()? {
        LogicalTypeID::Map => value
            .get::<Map<String, String>>()
            .map_err(|_| {
                err("types must be a STRUCT or MAP of column name to type name (VARCHAR)")
            })?
            .map(|m| m.0)
            .unwrap_or_default(),
        LogicalTypeID::Struct => {
            let mut out = Vec::new();
            for (i, (name, _)) in ty.get_params()?.into_iter().enumerate() {
                let v = value.get_child(i)?.get::<String>().map_err(|_| {
                    err(format!(
                        "types: the type for column \"{name}\" must be a VARCHAR type name"
                    ))
                })?;
                out.push((name, v));
            }
            out
        }
        _ => {
            return Err(err(
                "types must be a STRUCT or MAP of column name to type name, e.g. types := {'DT_INTER': 'DATE'}",
            ));
        }
    };
    let mut out: Vec<(String, ColumnType)> = Vec::new();
    for (name, t) in entries {
        let t = t.ok_or_else(|| err(format!("types: the type for column \"{name}\" is NULL")))?;
        let parsed = schema::parse_type_name(&t).ok_or_else(|| {
            err(format!(
                "types: unknown type \"{t}\" for column \"{name}\"; supported: {}",
                schema::TYPE_NAMES
            ))
        })?;
        if out.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name)) {
            return Err(err(format!("types: column \"{name}\" is listed twice")));
        }
        out.push((name, parsed));
    }
    Ok(out)
}

/// Expand each pattern through DuckDB's file system; a pattern matching
/// nothing is an error. Each pattern's matches are sorted; pattern order is kept.
fn expand(context: &Context, patterns: &[String]) -> Result<Vec<String>> {
    let fs = FileSystem::new(context)?;
    let mut out = Vec::new();
    for p in patterns {
        let mut matches = fs
            .glob(p)
            .map_err(|e| err(format!("cannot list files for \"{p}\": {e}")))?;
        if matches.is_empty() {
            return Err(err(format!(
                "no files found that match the pattern \"{p}\""
            )));
        }
        matches.sort();
        out.extend(matches);
    }
    Ok(out)
}

fn logical_type(context: &Context, t: ColumnType) -> Result<LogicalType> {
    match t {
        ColumnType::Varchar => String::logical_type(context),
        ColumnType::BigInt => i64::logical_type(context),
        ColumnType::Date => DateValue::logical_type(context),
        ColumnType::Boolean => bool::logical_type(context),
        ColumnType::Decimal { width, scale } => {
            DecimalSignature { width, scale }.logical_type(context)
        }
    }
}

fn bind_scan(
    context: &Context,
    args: &BindArguments<'_>,
    handle: &BindFunctionHandle<'_>,
) -> Result<ScanBind> {
    let files_value = args
        .arg(0)?
        .value()?
        .ok_or_else(|| err("the file argument must be a constant"))?;
    let patterns = parse_files(&files_value)?;

    let encoding_name = named::<String>(args, "encoding")?.unwrap_or_else(|| "auto".into());
    let encoding = Encoding::from_name(&encoding_name).ok_or_else(|| {
        err(format!(
            "unknown encoding \"{encoding_name}\"; valid encodings: {ENCODING_NAMES}"
        ))
    })?;
    let all_varchar = named::<bool>(args, "all_varchar")?.unwrap_or(false);
    let include_deleted = named::<bool>(args, "include_deleted")?.unwrap_or(false);
    let verify_crc = named::<bool>(args, "verify_crc")?.unwrap_or(true);
    let union_by_name = named::<bool>(args, "union_by_name")?.unwrap_or(false);
    let filename = named::<bool>(args, "filename")?.unwrap_or(false);
    let overrides = match args.arg_by_name("types")? {
        Some(a) => match a.value()? {
            Some(v) => parse_types(&v)?,
            None => return Err(err("types must be a constant")),
        },
        None => Vec::new(),
    };

    let paths = expand(context, &patterns)?;

    // Headers only: no record data is read or decompressed at bind.
    let mut schemas = Vec::with_capacity(paths.len());
    let mut files = Vec::with_capacity(paths.len());
    let mut override_used = vec![false; overrides.len()];
    for path in &paths {
        let mut input = open(context, path)?;
        let (header, _) = Header::read(&mut input, encoding).map_err(|e| file_err(path, e))?;
        let raw_names: Vec<String> = header.fields.iter().map(|f| f.name.clone()).collect();
        let names = schema::dedupe_names(&raw_names);
        let mut file_types = Vec::new();
        for (k, (key, t)) in overrides.iter().enumerate() {
            if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(key)) {
                override_used[k] = true;
                let original = &raw_names[i];
                if raw_names
                    .iter()
                    .filter(|n| n.eq_ignore_ascii_case(original))
                    .count()
                    > 1
                {
                    return Err(file_err(
                        path,
                        format!(
                            "types: cannot override \"{key}\": the file has several fields named \"{original}\""
                        ),
                    ));
                }
                file_types.push((original.clone(), *t));
            }
        }
        let options = ReadOptions {
            encoding,
            all_varchar,
            include_deleted,
            verify_crc,
            types: file_types,
        };
        let columns = resolve_schema(&header, &options).map_err(|e| file_err(path, e))?;
        let types: Vec<ColumnType> = columns.iter().map(|c| c.column_type).collect();
        for reserved in [
            include_deleted.then_some(DELETED_COLUMN),
            filename.then_some(FILENAME_COLUMN),
        ]
        .into_iter()
        .flatten()
        {
            if names.iter().any(|n| n.eq_ignore_ascii_case(reserved)) {
                return Err(file_err(
                    path,
                    format!(
                        "the file already has a column named \"{reserved}\", which read_dbc would add"
                    ),
                ));
            }
        }
        schemas.push(FileSchema {
            names,
            types: types.clone(),
        });
        files.push(FileBind {
            path: path.clone(),
            dbf: is_dbf(path),
            record_count: header.record_count as u64,
            options,
            types,
            plan: Vec::new(),
        });
    }
    if let Some(k) = override_used.iter().position(|u| !u) {
        return Err(err(format!(
            "types: column \"{}\" was not found in {}",
            overrides[k].0,
            if paths.len() == 1 {
                format!("\"{}\"", paths[0])
            } else {
                "any of the files".into()
            }
        )));
    }

    let mut columns = schema::combine(&schemas, &paths, union_by_name).map_err(err)?;
    for (file, s) in files.iter_mut().zip(&schemas) {
        file.plan = schema::plan_file(&columns, s);
    }
    let data_columns = columns.len();
    let mut deleted_column = None;
    let mut filename_column = None;
    if include_deleted {
        deleted_column = Some(columns.len());
        columns.push(OutColumn {
            name: DELETED_COLUMN.into(),
            column_type: ColumnType::Boolean,
        });
    }
    if filename {
        filename_column = Some(columns.len());
        columns.push(OutColumn {
            name: FILENAME_COLUMN.into(),
            column_type: ColumnType::Varchar,
        });
    }

    for c in &columns {
        handle.add_result_column(&c.name, &logical_type(context, c.column_type)?)?;
    }
    let total_records: u64 = files.iter().map(|f| f.record_count).sum();
    handle.set_cardinality(total_records as usize, include_deleted)?;

    Ok(ScanBind {
        files,
        columns,
        data_columns,
        deleted_column,
        filename_column,
        total_records,
    })
}

impl ScanBind {
    /// Open file `index` and prepare its projection for the output vectors.
    fn open_file(
        &self,
        context: &Context,
        index: usize,
        out_columns: &[usize],
    ) -> Result<OpenFile> {
        let f = &self.files[index];
        let input = open(context, &f.path)?;
        let mut reader = if f.dbf {
            DbcReader::new_dbf(input, f.options.clone())
        } else {
            DbcReader::new(input, f.options.clone())
        }
        .map_err(|e| file_err(&f.path, e))?;
        let now: Vec<ColumnType> = reader.columns().iter().map(|c| c.column_type).collect();
        if now != f.types {
            return Err(file_err(
                &f.path,
                "the file's columns changed after the query was planned",
            ));
        }
        let mut projection = Vec::new();
        let mut sink = Sink::default();
        let mut buf_of_output = vec![None; out_columns.len()];
        for (pos, &col) in out_columns.iter().enumerate() {
            if col >= self.data_columns {
                continue;
            }
            if let Some((field, conv)) = f.plan[col] {
                projection.push(field);
                sink.slots.push(Slot {
                    buf: sink.bufs.len(),
                    conv,
                });
                buf_of_output[pos] = Some(sink.bufs.len());
                sink.bufs.push(Buf::new(self.columns[col].column_type));
            }
        }
        reader
            .set_projection(&projection)
            .map_err(|e| file_err(&f.path, e))?;
        Ok(OpenFile {
            index,
            reader: Some(reader),
            sink,
            buf_of_output,
            records_seen: 0,
        })
    }

    /// Fill `output` from the worker's current file. Leaves it empty when
    /// the file is finished (which ends the batch).
    fn fill(
        &self,
        context: &Context,
        global: &ScanGlobal,
        local: &mut ScanLocal,
        output: &mut DataChunk<'_>,
    ) -> Result<()> {
        let Some(index) = local.claimed else {
            return Ok(());
        };
        if local.current.as_ref().is_none_or(|c| c.index != index) {
            local.current = Some(self.open_file(context, index, &global.out_columns)?);
        }
        let file = local.current.as_mut().expect("opened above");
        let path = &self.files[index].path;
        let capacity = output.capacity()?;
        loop {
            let Some(reader) = file.reader.as_mut() else {
                // Finished: an empty chunk ends this file's batch.
                local.current = None;
                local.claimed = None;
                return Ok(());
            };
            if reader.is_finished() {
                file.reader = None;
                continue;
            }
            file.sink.clear();
            let result = reader.read_batch(capacity, &mut file.sink);
            let seen = reader.progress().records_read;
            global
                .records_done
                .fetch_add(seen - file.records_seen, Ordering::Relaxed);
            file.records_seen = seen;
            let rows = result.map_err(|e| file_err(path, e))?;
            if let Some(e) = file.sink.error.take() {
                return Err(file_err(path, e));
            }
            if rows == 0 {
                // Possibly a run of deleted records; keep going until rows or the end.
                continue;
            }
            self.write(output, file, &global.out_columns, index, rows)?;
            return Ok(());
        }
    }

    fn write(
        &self,
        output: &mut DataChunk<'_>,
        file: &OpenFile,
        out_columns: &[usize],
        index: usize,
        rows: usize,
    ) -> Result<()> {
        for (pos, &col) in out_columns.iter().enumerate() {
            let t = self.columns[col].column_type;
            if Some(col) == self.deleted_column {
                let mut v = output.get_vector_at_mut::<bool>(pos)?;
                v.set_size(rows)?;
                let mut w = v.writer()?;
                for i in 0..rows {
                    w.set(i, &file.sink.deleted.get(i).copied().unwrap_or(false))?;
                }
            } else if Some(col) == self.filename_column {
                let mut v = output.get_vector_at_mut::<String>(pos)?;
                v.set_size(rows)?;
                let mut w = v.writer()?;
                let path = self.files[index].path.as_str();
                for i in 0..rows {
                    w.set(i, path)?;
                }
            } else if let Some(b) = file.buf_of_output[pos] {
                write_buf(output, pos, t, &file.sink.bufs[b], rows)?;
            } else {
                write_nulls(output, pos, t, rows)?;
            }
        }
        Ok(())
    }
}

struct ReadDbc;

impl TableFunctionCallbacks for ReadDbc {
    type BindData = ScanBind;
    type GlobalState = ScanGlobal;
    type LocalState = ScanLocal;

    fn bind(
        &self,
        context: &Context,
        arguments: BindArguments<'_>,
        handle: BindFunctionHandle<'_>,
    ) -> Result<Self::BindData> {
        guard(|| bind_scan(context, &arguments, &handle))
    }

    fn init_global_state(
        &self,
        _context: &Context,
        _bind: &Self::BindData,
        columns: InitColumnData<'_>,
    ) -> Result<Option<Self::GlobalState>> {
        guard(|| {
            let mut out_columns = Vec::new();
            for i in 0..columns.count()? {
                out_columns.push(columns.get_column_index(i)?);
            }
            Ok(Some(ScanGlobal {
                next_file: AtomicUsize::new(0),
                records_done: AtomicU64::new(0),
                out_columns,
            }))
        })
    }

    fn init_local_state(
        &self,
        _context: &Context,
        _bind: &Self::BindData,
        _global: Option<&Self::GlobalState>,
        _columns: InitColumnData<'_>,
    ) -> Result<Option<Self::LocalState>> {
        Ok(Some(ScanLocal::default()))
    }

    fn max_threads(
        &self,
        _context: &Context,
        bind: &Self::BindData,
        _global: Option<&Self::GlobalState>,
        _columns: InitColumnData<'_>,
    ) -> Result<Option<NonZeroUsize>> {
        // One worker per file; DuckDB caps this at its thread count.
        Ok(NonZeroUsize::new(bind.files.len().max(1)))
    }

    fn progress(
        &self,
        _context: &Context,
        bind: &Self::BindData,
        global: Option<&Self::GlobalState>,
    ) -> Result<Option<f64>> {
        let Some(global) = global else {
            return Ok(None);
        };
        if bind.total_records == 0 {
            return Ok(Some(0.0));
        }
        let done = global
            .records_done
            .load(Ordering::Relaxed)
            .min(bind.total_records);
        Ok(Some(done as f64 / bind.total_records as f64))
    }

    fn exec(
        &self,
        context: &Context,
        bind: &Self::BindData,
        local: Option<&mut Self::LocalState>,
        global: Option<&Self::GlobalState>,
        output: &mut DataChunk<'_>,
        _columns: ExecColumnInfo<'_>,
    ) -> Result<()> {
        guard(|| {
            let local = local.ok_or_else(|| Error::internal("read_dbc: missing local state"))?;
            let global = global.ok_or_else(|| Error::internal("read_dbc: missing global state"))?;
            let result = bind.fill(context, global, local, output);
            if result.is_err() {
                // Never hand out more rows from a failed file.
                local.current = None;
                local.claimed = None;
            }
            result
        })
    }
}

impl TableClaimBatchCallbacks for ReadDbc {
    fn claim_batch(
        &self,
        _context: &Context,
        bind: &Self::BindData,
        local: Option<&mut Self::LocalState>,
        global: Option<&Self::GlobalState>,
    ) -> Result<bool> {
        guard(|| {
            let local = local.ok_or_else(|| Error::internal("read_dbc: missing local state"))?;
            let global = global.ok_or_else(|| Error::internal("read_dbc: missing global state"))?;
            let i = global.next_file.fetch_add(1, Ordering::SeqCst);
            if i >= bind.files.len() {
                local.claimed = None;
                return Ok(false);
            }
            local.claimed = Some(i);
            Ok(true)
        })
    }
}

impl TablePartitioningCallbacks for ReadDbc {
    fn partition_data(
        &self,
        _context: &Context,
        _bind: &Self::BindData,
        local: Option<&mut Self::LocalState>,
        _global: Option<&Self::GlobalState>,
        _partition_data: PartitionData<'_>,
    ) -> Result<usize> {
        // The batch index is the file index: files are claimed in increasing
        // order, so it never decreases on a thread.
        let Some(local) = local else {
            return Ok(0);
        };
        if let Some(i) = local.claimed {
            local.last_batch = i;
        }
        Ok(local.last_batch)
    }

    fn partitioning(
        &self,
        _context: &Context,
        _bind: &Self::BindData,
        _data: PartitioningData<'_>,
    ) -> Result<TablePartitionInfo> {
        Ok(TablePartitionInfo::NotPartitioned)
    }
}

/// `FROM 'x.dbc'` and `FROM 'dir/*.dbc'` (extension case-insensitive) become
/// `read_dbc(...)`. `.dbf` is left to other extensions.
struct DbcReplacement;

impl ReplacementScanCallbacks for DbcReplacement {
    fn scan(
        &self,
        context: &Context,
        name: &QualifiedName,
        handle: ReplacementHandle<'_>,
    ) -> Result<()> {
        guard(|| {
            let view = name.get_view()?;
            if view.catalog.is_some()
                || view.schema.is_some()
                || !view.table.to_ascii_lowercase().ends_with(".dbc")
            {
                return Ok(());
            }
            handle.set_reference(ReplacementType::Table(FUNCTION_NAME.try_into()?))?;
            handle.add_parameter(view.table.value(context)?)
        })
    }
}

fn signature() -> SignatureBuilder {
    SignatureBuilder::new()
        .param(Param::new::<Any>("files").kind(Kind::PositionalOnly))
        .param(Param::with_default("encoding", "auto".to_string()).kind(Kind::Named))
        .param(Param::with_default("all_varchar", false).kind(Kind::Named))
        .param(Param::with_default("include_deleted", false).kind(Kind::Named))
        .param(Param::with_default("verify_crc", true).kind(Kind::Named))
        .param(
            Param::new::<Any>("types")
                .kind(Kind::Named)
                .default_value(None::<String>),
        )
        .param(Param::with_default("union_by_name", false).kind(Kind::Named))
        .param(Param::with_default("filename", false).kind(Kind::Named))
        .documentation(
            FunctionDocumentationBuilder::new()
                .description(
                    "Read DATASUS .dbc files (and plain .dbf files) as a table. Accepts a path, a glob, or a list of them.",
                )
                .add_example("One file", "SELECT * FROM read_dbc('RDAC0801.dbc')")
                .add_example(
                    "Several layouts",
                    "SELECT * FROM read_dbc('data/RD*.dbc', union_by_name := true, filename := true)",
                )
                .add_example(
                    "Options",
                    "SELECT * FROM read_dbc('RDSC2501.dbc', encoding := 'cp850', types := {'DT_INTER': 'DATE'})",
                )
                .add_category(CATEGORY),
        )
}

pub fn register(extension: &Extension) -> Result<()> {
    TableFunctionBuilder::new(FUNCTION_NAME, signature(), ReadDbc)
        .with_projection_pushdown(true)
        .with_progress()
        .with_claim_batch()
        .with_partitioning()
        .register(extension)?;
    ReplacementScanBuilder::new(DbcReplacement).register(extension)
}

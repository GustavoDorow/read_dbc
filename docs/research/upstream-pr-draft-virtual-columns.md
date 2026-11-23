# Adapt non-projection-pushdown readers in `TableFunctionMultiFileWrapper`

Local review draft. DuckDB expects contributors to describe the change in their own words if they submit it. [AI policy](https://github.com/duckdb/duckdb/blob/f6f2e607114d9a9a45fb84da0477c2d306875524/AI_POLICY.md)

## Problem

Wrapping a single-file table function that does not enable `projection_pushdown` (the default for C API v2 table functions) via `TableFunctionMultiFileWrapper::CreateFunction` breaks whenever a query projects a proper subset or reordered subset of columns — for example `SELECT s, filename FROM fs('dir/*.data', filename := true)` or `SELECT i FROM fs('dir/*.data')`.

- `MultiFileFunction` unconditionally assumes `projection_pushdown = true`: `MultiFileColumnMapper` builds `reader_data.expressions` matching `input.column_indexes`, and `MultiFileReader::FinalizeChunk` writes `executor.expressions[i]` into `output_chunk.data[i]`.
- `TableFunctionMultiFileWrapper::CreateFunction` overwrote `result.projection_pushdown = single_file_function.projection_pushdown`. When `false`, `PhysicalPlanGenerator::CreatePlan(LogicalGet &op)` gives `PhysicalTableScan` an `output_chunk` sized to all returned columns while still passing the projected `column_ids` to the scan. `FinalizeChunk` then writes projected expressions into the leading slots of the full-width `output_chunk`, triggering an assertion failure in debug builds (`src/execution/expression_executor.cpp:168`) and likely writing wrong/uninitialized column slots in builds without assertions.
- Separately, queries requesting multi-file virtual columns such as `SELECT count(*) FROM fs(...)` or `SELECT filename FROM fs(...)` (without `filename := true`) failed at planning with `Not implemented Error: Virtual columns require projection pushdown` (`src/execution/physical_plan/plan_get.cpp:221`), a symptom of the same root cause.

## Minimal reproduction

Given a directory containing two empty files `a.data` and `b.data`, register a C API v2 single-file table function `f(path VARCHAR)` returning `(s VARCHAR, i BIGINT)` without calling `duckdb_v2_table_function_set_projection_pushdown(fn, true, ...)`, and wrap it as multi-file function `fs`:

```c
#define DUCKDB_V2_API_ALLOW_UNSTABLE 1
#include "duckdb_v2.h"
#include <assert.h>

void register_readers(duckdb_v2_connection_handle conn,
                      duckdb_v2_table_function_bind_callback_fn bind,
                      duckdb_v2_table_function_init_global_callback_fn init_global,
                      duckdb_v2_table_function_exec_callback_fn exec) {
    duckdb_v2_factory_handle factory = NULL;
    duckdb_v2_table_function_handle reader = NULL;
    duckdb_v2_function_signature_handle signature = NULL;
    duckdb_v2_logical_type_handle varchar_type = NULL;
    duckdb_v2_multi_file_function_handle wrapper = NULL;

    assert(duckdb_v2_connection_get_factory(conn, &factory, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_factory_create_type_from_id(factory, DUCKDB_V2_LOGICAL_TYPE_ID_VARCHAR,
           NULL, NULL, NULL, 0, &varchar_type, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_table_function_create(factory, &reader, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_table_function_set_name(reader, "f", 1, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_table_function_get_signature(reader, &signature, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_function_signature_add_parameter(signature, "path", 4, varchar_type, NULL,
           DUCKDB_V2_FUNCTION_PARAMETER_KIND_STANDARD, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_table_function_set_bind_callback(reader, bind, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_table_function_set_init_global_callback(reader, init_global, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_table_function_set_exec_callback(reader, exec, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_connection_register_table_function(conn, reader, NULL) == DUCKDB_V2_ERROR_NONE);
    duckdb_v2_table_function_destroy(&reader);
    duckdb_v2_logical_type_destroy(&varchar_type);

    assert(duckdb_v2_multi_file_function_create(factory, &wrapper, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_multi_file_function_set_name(wrapper, "fs", 2, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_multi_file_function_set_single_file_function(wrapper, "f", 1, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_connection_register_multi_file_function(conn, wrapper, NULL) == DUCKDB_V2_ERROR_NONE);
    duckdb_v2_multi_file_function_destroy(&wrapper);
}
```

```sql
SELECT s, filename FROM fs('dir/*.data', filename := true);
SELECT i FROM fs('dir/*.data');
SELECT count(*) FROM fs('dir/*.data');
```

Before this fix, the first two queries hit the assertion in `ExpressionExecutor::ExecuteExpression` (`src/execution/expression_executor.cpp:168`), and `SELECT count(*)` fails with `Not implemented Error: Virtual columns require projection pushdown`. With this fix, all three succeed.

## Fix

- **Keep `result.projection_pushdown = true` on the multi-file wrapper** (`TableFunctionMultiFileWrapper::CreateFunction`). `MultiFileFunction` always projects its output in `MultiFileColumnMapper` + `FinalizeChunk`, so the physical scan contract must match `column_indexes` / `projection_ids`. Filter pushdown, filter pruning, `supports_pushdown_type`, and `late_materialization` remain gated on `single_file_function.projection_pushdown`.
- **Adapt non-projection-pushdown readers inside `TableFunctionFileReader`**:
  - On bind, build `all_column_indexes` (`[0 .. types.size() - 1]`) when `!function.projection_pushdown`, and pass it to `init_global` / `init_local` via `GetInitInput`.
  - In `TryInitializeScan`, when `!function.projection_pushdown` and the thread switches to a new reader (or `full_chunk.GetTypes() != types`), reinitialize `lstate.full_chunk` with the reader's bound `types`. Because `lstate.reader` still holds a `shared_ptr` to the previous reader until after the switch check, pointer-reuse (ABA) across files with different schemas cannot occur.
  - In `Scan`, execute the wrapped function into `lstate.full_chunk`, reference each requested `column_indexes[i]` vector from `lstate.full_chunk` into `chunk.data[i]`, and propagate row count via `chunk.SetChildCardinality(lstate.full_chunk.size())` (which also handles empty `column_indexes` such as `SELECT count(*)`).
- **Preserve the single-file virtual column rule**: when `!function.projection_pushdown`, `TableFunctionMultiFileWrapper::GetVirtualColumns` does not expose reader-supplied virtual columns (e.g. `file_row_number`), and `TableFunctionFileReader::AddVirtualColumn` throws `NotImplementedException("Virtual columns require projection pushdown")`. Multi-file virtual columns synthesized by the wrapper (`filename`, `file_index`, `count(*)`) continue to work.

## Why an adapter instead of an error

Two simpler alternatives were considered:

1. Rejecting non-projection-pushdown functions at wrapper registration / `CreateFunction` time.
2. Throwing a planning-time error when a non-projection-pushdown multi-file scan projects a strict subset or reordered column list.

Both alternatives make the default C API v2 experience hostile: `projection_pushdown` is `false` by default in C API v2, and simple format extensions (which decode whole records/rows anyway and rely on `duckdb_v2_connection_register_multi_file_function` to add globs, `filename`, and `union_by_name`) would either fail to wrap at all or fail on basic queries like `SELECT col, filename FROM read_fmt('*.ext', filename := true)` and `SELECT count(*) FROM read_fmt('*.ext')`.

Adapting inside `TableFunctionFileReader` keeps the change local to `table_function_multi_file.{hpp,cpp}`, adds zero overhead to readers that do support projection pushdown (`parquet`, `csv`, `json`, etc.), and requires no ABI changes.

## Tests

Four regression test cases in `test/api/capi/v2/test_capi_v2_table_function.cpp` cover:

- Single-column and subset projections (`SELECT i`, `SELECT s, filename`), reordered projection (`SELECT i, s`), `count(*)`, `filename := true`, and virtual `filename`.
- Multi-chunk (>2048 rows per file, 5,000 rows × 2 files = 10,000 rows) run with `SET threads = 4` and `init_local`, verifying `init_local` receives all bound column indices even on `SELECT count(*)`.
- `union_by_name := true` across 3 files with differing column counts, types, order, and missing columns on a single thread (forcing `lstate.full_chunk` reinitialization across files with different schemas).
- Blocking reader-supplied virtual columns (`file_row_number` and `file_row_number := true`) on both C API v2 readers and a direct C++ `TableFunction` with `projection_pushdown = false` and `get_virtual_columns` set, plus a direct `TableFunctionFileReader::AddVirtualColumn` guard check.

A standalone C++ C API v2 reproduction exercising `ps_std`, `ps_pos`, and `ps_any` wrappers (10 queries each, including `filename := true`, `union_by_name := true`, `count(*)`, `VARIANT`, `string_agg`, and `CREATE TABLE AS`) passed 30/30 queries for both `projection_pushdown = false` (at 3 rows and 2048 rows per file) and `projection_pushdown = true`.

Local Ninja debug build with sanitizers disabled, measured on branch `fix/multi-file-virtual-columns-no-projection` at commit `c860d72740154ce39b1c186c5f2ba110017db904`.

| Test selection | Test cases passed |
| --- | ---: |
| New C API v2 wrapper regressions (`V2 table: multi-file wrapper*`) | 4 |
| `unittest "[capi_v2]"` | 624 |
| `unittest "[capi]"` | 207 |
| CSV multi-file, glob and schema-union selection (16 matched; 1 skipped: `require tpch`) | 15 |
| Parquet multi-file and schema-union selection | 23 |
| Shared multi-file and JSON single-file wrapper selection | 5 |

Changed-file formatting passed with DuckDB's clang-format 11.0.1 tooling (`scripts/format.py --check --skip-typos origin/main`) and `git diff --check` passed.

## Risks and trade-offs

- **Extra per-thread `DataChunk` allocation in no-pushdown mode:** Each worker thread scanning a non-projection-pushdown wrapped function allocates one full-width `DataChunk` (`lstate.full_chunk`) matching the current file's bound schema, reused across chunks of that file and reinitialized only when switching to a reader with different types. Readers with `projection_pushdown = true` do not allocate or touch `full_chunk`.
- **Full-width decoding on subset queries:** A wrapped reader without `projection_pushdown` still decodes all columns of each file into `full_chunk`, and `TableFunctionFileReader::Scan` zero-copy references only the requested columns into `chunk`. This matches single-file DuckDB behavior (`PhysicalTableScan` + `PhysicalProjection`), and readers that want column skipping can opt into `projection_pushdown = true`.
- **Reader-supplied virtual columns:** Wrapped readers without `projection_pushdown` still cannot provide reader-level virtual columns such as `file_row_number` (consistent with single-file scans and C API v2 registration rules); only multi-file framework virtual columns (`filename`, `file_index`, empty-projection `count(*)`) are enabled by the wrapper.
- **No ABI changes:** All changes are internal to `TableFunctionFileReader` and `TableFunctionMultiFileWrapper` on top of `main` (`f6f2e607114d9a9a45fb84da0477c2d306875524`).

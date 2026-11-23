# Multi-file wrapper (`TableFunctionMultiFileWrapper`) triggers assertion / column mismatch when wrapping a function without `projection_pushdown`

Local issue draft (not filed on GitHub).

## Version / Commit

- Repository: `duckdb/duckdb`
- Branch / Commit: `main` at `f6f2e607114d9a9a45fb84da0477c2d306875524` (`v2.1.0-dev1272`)
- Build: Debug (Ninja, Clang 23.1.3, assertions enabled)

## Summary

When a single-file table function without `projection_pushdown` (the default for C API v2 table functions) is wrapped as a multi-file function via `TableFunctionMultiFileWrapper::CreateFunction` (e.g. `duckdb_v2_connection_register_multi_file_function`), any query that projects a proper subset or reordered subset of columns — including `SELECT s, filename FROM fs('dir/*.data', filename := true)` or even `SELECT i FROM fs('dir/*.data')` — crashes in debug builds with an assertion failure inside `MultiFileReader::FinalizeChunk` -> `ExpressionExecutor::ExecuteExpression`:

```text
INTERNAL Error: Assertion triggered in file "src/execution/expression_executor.cpp" on line 168: result.GetType().id() == expressions[expr_idx]->GetReturnType().id()
```

In builds without assertions (not directly observed; inferred from reading `plan_get.cpp` and `multi_file_reader.cpp`), `FinalizeChunk` appears to write projected expressions into the leading slots of the full-width `output_chunk` and leave trailing slots untouched, so the `PhysicalProjection` above `PhysicalTableScan` would likely read wrong or uninitialized column slots.

With `projection_pushdown` enabled on the single-file function (`duckdb_v2_table_function_set_projection_pushdown(fn, true, ...)`), all queries succeed. Today's clean `Not implemented Error: Virtual columns require projection pushdown` (`src/execution/physical_plan/plan_get.cpp:221`) on `SELECT count(*) FROM fs(...)` or `SELECT filename FROM fs(...)` (without `filename := true`) is a symptom of the same root cause: `TableFunctionMultiFileWrapper::CreateFunction` sets `result.projection_pushdown = false`, pushing the scan into `plan_get.cpp`'s non-pushdown path even though `MultiFileFunction` itself always projects columns and evaluates multi-file virtual columns. Passing `filename := true` adds `filename` as a regular returned column and bypasses that check, exposing the `output_chunk` layout mismatch directly.

## Minimal Reproduction

Given a directory containing two empty files `a.data` and `b.data`, compile and link against `libduckdb.so` (debug build) and `src/include/duckdb_v2.h`:

```cpp
#define DUCKDB_V2_API_ALLOW_UNSTABLE 1
#include "duckdb_v2.h"
#include <atomic>
#include <cstdio>
#include <cstring>
#include <string>

struct Global {
	std::atomic<bool> done {false};
};
static void DelGlobal(void *p) {
	delete static_cast<Global *>(p);
}

static void BindCb(duckdb_v2_function_bind_info_handle, duckdb_v2_table_function_bind_info_handle result,
                   duckdb_v2_context_handle ctx, duckdb_v2_error_info_handle *err) {
	duckdb_v2_factory_handle f = nullptr;
	duckdb_v2_context_get_factory(ctx, &f, nullptr);
	duckdb_v2_logical_type_handle vc = nullptr, bi = nullptr;
	duckdb_v2_factory_create_type_from_id(f, DUCKDB_V2_LOGICAL_TYPE_ID_VARCHAR, nullptr, nullptr, nullptr, 0, &vc, err);
	duckdb_v2_factory_create_type_from_id(f, DUCKDB_V2_LOGICAL_TYPE_ID_BIGINT, nullptr, nullptr, nullptr, 0, &bi, err);
	duckdb_v2_table_function_bind_add_result_column(result, "s", 1, vc, err);
	duckdb_v2_table_function_bind_add_result_column(result, "i", 1, bi, err);
	duckdb_v2_logical_type_destroy(&vc);
	duckdb_v2_logical_type_destroy(&bi);
}

static void InitGlobal(duckdb_v2_table_function_init_global_info_handle info, duckdb_v2_context_handle,
                       duckdb_v2_error_info_handle *err) {
	duckdb_v2_opaque g = {new Global, DelGlobal, nullptr};
	duckdb_v2_table_function_init_global_set_global_state(info, &g, err);
}

static void ExecCb(duckdb_v2_table_function_exec_info_handle info, duckdb_v2_context_handle ctx,
                   duckdb_v2_error_info_handle *err) {
	void *gp = nullptr;
	duckdb_v2_data_chunk_handle chunk = nullptr;
	duckdb_v2_table_function_exec_get_global_state(info, &gp, err);
	duckdb_v2_table_function_exec_get_output_chunk(info, &chunk, err);
	auto &g = *static_cast<Global *>(gp);
	idx_t ncols = 0;
	duckdb_v2_data_chunk_get_vector_count(chunk, &ncols, nullptr);
	if (g.done.exchange(true)) {
		duckdb_v2_vector_handle v0 = nullptr;
		duckdb_v2_data_chunk_get_vector(chunk, 0, &v0, err);
		duckdb_v2_vector_set_size(v0, 0, err);
		return;
	}
	duckdb_v2_factory_handle f = nullptr;
	duckdb_v2_context_get_factory(ctx, &f, nullptr);
	duckdb_v2_value_handle sv = nullptr, iv = nullptr;
	duckdb_v2_value_create_varchar(f, "hello", 5, &sv, nullptr);
	duckdb_v2_value_create_int(f, 42, &iv, nullptr);
	for (idx_t c = 0; c < ncols; c++) {
		duckdb_v2_vector_handle v = nullptr;
		duckdb_v2_data_chunk_get_vector(chunk, c, &v, err);
		duckdb_v2_vector_set_size(v, 1, err);
		duckdb_v2_logical_type_handle t = nullptr;
		duckdb_v2_vector_get_logical_type(v, &t, nullptr);
		DUCKDB_V2_LOGICAL_TYPE_ID id;
		duckdb_v2_logical_type_get_id(t, &id, nullptr);
		duckdb_v2_logical_type_destroy(&t);
		duckdb_v2_vector_set_value(v, 0, id == DUCKDB_V2_LOGICAL_TYPE_ID_VARCHAR ? sv : iv, err);
	}
	duckdb_v2_value_destroy(&sv);
	duckdb_v2_value_destroy(&iv);
}
```

Register `f(path VARCHAR)` without calling `duckdb_v2_table_function_set_projection_pushdown(fn, true, ...)`, wrap it as multi-file function `fs`, and run:

```sql
SELECT s, filename FROM fs('dir/*.data', filename := true);
```

## Expected vs Actual

- **Expected:** Either the query returns the projected `(s, filename)` rows (as it does when `projection_pushdown` is enabled), or if non-projection-pushdown readers are unsupported by the multi-file wrapper, registration or planning reports a clear error.
- **Actual** (observed on unpatched `main` `f6f2e607`):
  - `SELECT s, i FROM fs('dir/*.data')` -> OK (`rows=2`)
  - `SELECT s, i, filename FROM fs('dir/*.data', filename := true)` -> OK (`rows=2`)
  - `SELECT s, filename FROM fs('dir/*.data', filename := true)` -> **INTERNAL Error (debug assertion)**
  - `SELECT i FROM fs('dir/*.data')` -> **INTERNAL Error (debug assertion)**
  - `SELECT count(*) FROM fs('dir/*.data')` -> `Not implemented Error: Virtual columns require projection pushdown`
  - `SELECT filename FROM fs('dir/*.data')` -> `Not implemented Error: Virtual columns require projection pushdown`

### Assertion Text and Stack Trace

```text
INTERNAL Error: Assertion triggered in file "src/execution/expression_executor.cpp" on line 168: result.GetType().id() == expressions[expr_idx]->GetReturnType().id()

Stack Trace:
...
duckdb::DuckDBAssertInternal(bool, char const*, char const*, int)
duckdb::ExpressionExecutor::ExecuteExpression(unsigned long, duckdb::Vector&)
duckdb::MultiFileReader::FinalizeChunk(duckdb::ClientContext&, duckdb::MultiFileBindData const&, duckdb::BaseFileReader&, duckdb::MultiFileReaderData const&, duckdb::DataChunk&, duckdb::DataChunk&, duckdb::ExpressionExecutor&, duckdb::optional_ptr<duckdb::MultiFileReaderGlobalState, true>)
duckdb::MultiFileFunction<duckdb::TableFunctionMultiFileWrapper>::DecodeCurrentJob(...)
duckdb::MultiFileFunction<duckdb::TableFunctionMultiFileWrapper>::MultiFileScan(...)
duckdb::PhysicalTableScan::GetDataInternal(...)
```

## Root Cause and Likely Location

1. `MultiFileFunction` (`src/include/duckdb/common/multi_file/multi_file_function.hpp:111`) sets `projection_pushdown = true` and unconditionally assumes the scan's output chunk matches `input.column_indexes` (or `input.projection_ids`):
   - `MultiFileInitGlobal` passes `input.column_indexes` to `InitializeReader` -> `MultiFileColumnMapper::CreateColumnMappingByMapper`, producing `reader_data.expressions` of length `input.column_indexes.size()`.
   - `MultiFileReader::FinalizeChunk` (`src/common/multi_file/multi_file_reader.cpp:939-941`) writes `executor.expressions[i]` directly into `output_chunk.data[i]`.
2. `TableFunctionMultiFileWrapper::CreateFunction` (`src/common/multi_file/table_function_multi_file.cpp:945`) overwrites `result.projection_pushdown = single_file_function.projection_pushdown;`.
3. When `single_file_function.projection_pushdown` is `false`:
   - `PhysicalPlanGenerator::CreatePlan(LogicalGet &op)` (`src/execution/physical_plan/plan_get.cpp:191-237`) builds `PhysicalTableScan` with `types = op.returned_types` (all bound columns, e.g. `[VARCHAR, BIGINT, VARCHAR]` for `s, i, filename`) while still passing the projected `column_ids` (e.g. `[0, 2]` for `s, filename`), and places a `PhysicalProjection` above the scan.
   - `MultiFileScan` receives an `output_chunk` with `op.returned_types` (`[VARCHAR, BIGINT, VARCHAR]`), but `FinalizeChunk` executes `executor.expressions` (`[VARCHAR (s), VARCHAR (filename)]`) into slots `0` and `1` — attempting to write `VARCHAR` (`filename`) into slot `1` (`BIGINT`), triggering the assertion at `src/execution/expression_executor.cpp:168`.
   - Independently, `MultiFileFunction::InitializeDecodeChunk` (`src/include/duckdb/common/multi_file/multi_file_function.hpp:619-694`) initializes `lstate.scan_chunk` with only the projected `reader.column_indexes`, whereas a wrapped single-file function without `projection_pushdown` expects a chunk containing all of its declared columns in declared order.

## Possible Fixes

1. **Adapt non-projection-pushdown readers inside `TableFunctionMultiFileWrapper` / `TableFunctionFileReader`:**
   - Keep `result.projection_pushdown = true` in `TableFunctionMultiFileWrapper::CreateFunction` (instead of copying `single_file_function.projection_pushdown`), because `MultiFileFunction` always performs column mapping and projection in `MultiFileColumnMapper` + `FinalizeChunk`. Keep filter pushdown / pruning / late materialization gated on `single_file_function.projection_pushdown`.
   - In `TableFunctionFileReader`: when `!function.projection_pushdown`, pass all bound column indices `[0 .. types.size() - 1]` to `init_global` / `init_local`, scan the wrapped single-file function into a full-width chunk of `types`, and project the requested `column_indexes` (referencing each column vector from the full-width chunk into `chunk`, and propagating cardinality even when `column_indexes` is empty, e.g. `SELECT count(*)`).
   - Do not expose reader-supplied virtual columns (e.g. `file_row_number` from `function.get_virtual_columns`) when `!function.projection_pushdown`, preserving the single-file rule that reader virtual columns require projection pushdown. Multi-file virtual columns (`count(*)`, virtual `filename`, `file_index`) work naturally because the wrapper itself handles projection.
2. **Minimal planning-time error:** If adapting non-projection-pushdown readers is undesired, throw a clear error at planning time whenever a non-projection-pushdown multi-file reader is scanned with a strict subset or reordered projection (so `output_chunk` cannot silently disagree with `FinalizeChunk`).
3. **Registration-time rejection:** Alternatively, if wrapping a table function without `projection_pushdown` is intentionally unsupported, reject it explicitly in `TableFunctionMultiFileWrapper::CreateFunction` / multi-file registration rather than crashing in `FinalizeChunk`.

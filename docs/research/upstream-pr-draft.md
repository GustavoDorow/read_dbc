# Forward named-only parameters through multi-file table functions

Local review draft. DuckDB expects contributors to describe the change in their own words if they submit it. [AI policy](https://github.com/duckdb/duckdb/blob/f6f2e607114d9a9a45fb84da0477c2d306875524/AI_POLICY.md)

## Problem

A single-file reader registered through the C API v2 can declare `encoding VARCHAR DEFAULT 'utf-8'` as a named-only parameter. The same argument fails on the multi-file wrapper with `Binder Error: Invalid named parameter "encoding"`. The wrapper builds its SQL signature from the reader's typed kwargs and omits the fixed keyword-only slots that the C API creates. The error occurs before the reader's per-file bind callback runs. Registration also counts required keyword-only slots against the requirement for a single file argument.

## Minimal reproduction

Run the following with an open v2 connection, an existing file named `sample.data`, and a bind and exec callback for a table function that returns one column. The callbacks can expose the bound `encoding` value or return an empty result. The SQL binder fails before either callback can run.

```c
#define DUCKDB_V2_API_ALLOW_UNSTABLE 1
#include "duckdb_v2.h"
#include <assert.h>

void register_readers(duckdb_v2_connection_handle conn,
                      duckdb_v2_table_function_bind_callback_fn bind,
                      duckdb_v2_table_function_exec_callback_fn exec) {
    duckdb_v2_factory_handle factory = NULL;
    duckdb_v2_table_function_handle reader = NULL;
    duckdb_v2_function_signature_handle signature = NULL;
    duckdb_v2_logical_type_handle varchar_type = NULL;
    duckdb_v2_value_handle utf8 = NULL;
    duckdb_v2_multi_file_function_handle wrapper = NULL;

    assert(duckdb_v2_connection_get_factory(conn, &factory, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_factory_create_type_from_id(factory, DUCKDB_V2_LOGICAL_TYPE_ID_VARCHAR,
           NULL, NULL, NULL, 0, &varchar_type, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_value_create_varchar(factory, "utf-8", 5, &utf8, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_table_function_create(factory, &reader, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_table_function_set_name(reader, "read_one", 8, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_table_function_get_signature(reader, &signature, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_function_signature_add_parameter(signature, "path", 4, varchar_type, NULL,
           DUCKDB_V2_FUNCTION_PARAMETER_KIND_POSITIONAL_ONLY, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_function_signature_add_parameter(signature, "encoding", 8, varchar_type, utf8,
           DUCKDB_V2_FUNCTION_PARAMETER_KIND_NAMED_ONLY, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_table_function_set_bind_callback(reader, bind, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_table_function_set_exec_callback(reader, exec, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_connection_register_table_function(conn, reader, NULL) == DUCKDB_V2_ERROR_NONE);
    duckdb_v2_table_function_destroy(&reader);
    duckdb_v2_value_destroy(&utf8);
    duckdb_v2_logical_type_destroy(&varchar_type);

    assert(duckdb_v2_multi_file_function_create(factory, &wrapper, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_multi_file_function_set_name(wrapper, "read_many", 9, NULL) == DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_multi_file_function_set_single_file_function(wrapper, "read_one", 8, NULL) ==
           DUCKDB_V2_ERROR_NONE);
    assert(duckdb_v2_connection_register_multi_file_function(conn, wrapper, NULL) == DUCKDB_V2_ERROR_NONE);
    duckdb_v2_multi_file_function_destroy(&wrapper);
}
```

```sql
SELECT * FROM read_many('sample.data', encoding := 'latin-1');
```

With the fix, each file's bind callback receives `latin-1`. If the caller omits the argument, the callback receives `utf-8`.

## Fix

- Copy fixed keyword-only slots before the wrapper signature's variadic options slot. The `VARCHAR`, `ANY[]`, and `VARIANT` overloads inherit the types, defaults, and requiredness.
- Route fixed keyword-only values to the single-file reader and record them in each file's bound call so C API callbacks can look them up. Shared typed options still follow the existing generic parser path.
- Accept readers that require keyword-only arguments when the file is their one required positional argument. If an overload can already be called with just the file, continue to select it ahead of overloads that require a keyword.
- Report case-insensitive collisions with declared multi-file options and the wrapper's `path`, `paths`, and `options` slots at registration, with the offending name in the error. Collisions between a forwarded named-only parameter and one of the wrapped reader's own typed kwargs options report a distinct error identifying the wrapped function rather than calling it a reserved parameter. The reserved-name check covers declared multi-file options; undeclared `ParseOption` aliases (e.g. `hive_type_autocast`) cannot be passed by callers through the wrapper anyway, so a forwarded parameter with such a name is harmless (the test relies on that).

## Tests

The C API v2 regression records the arguments seen by each file's bind callback (verifying consistency if a file binds more than once) and emits one row per file using per-scan global state. It covers single paths, lists, globs, `VARIANT`, defaults, explicit NULL, BOOL values, STRUCT and MAP values passed to `ANY`, casing, catalog discovery, `filename` with `union_by_name`, required options, reserved names, reader option collisions, and invalid calls.

The standalone C reproduction compiled against the v2 header and ran against the patched library. Its bind callback checked that it received `latin-1`.

Local Ninja debug build with sanitizers disabled, measured on branch `fix/multi-file-forward-named-params` at commit `57a0c9a29653a2c9afaca730ee35b193d3b539e2`.

| Test selection | Test cases passed |
| --- | ---: |
| New C API v2 wrapper regressions (`V2 table: multi-file wrapper*`) | 3 |
| `unittest "[capi]"` | 207 |
| `unittest "[capi_v2]"` | 623 |
| CSV multi-file, glob and schema-union selection (16 matched; 1 skipped: `require tpch`) | 15 |
| Parquet multi-file and schema-union selection | 23 |
| Shared multi-file and JSON single-file wrapper selection | 5 |

Changed-file formatting passed with DuckDB's clang-format 11.0.1 tooling (`scripts/format.py --check --skip-typos origin/main`) and `git diff --check` passed.

## Compatibility and backport

Existing C API and extension ABI layouts are preserved. There is one behavioral trade-off: single-file readers declaring a named-only parameter with the same name as a built-in multi-file option (such as `filename` or `union_by_name`) previously had that parameter silently dropped when wrapped, whereas registration now fails with an `InvalidInputException`. Failing at registration makes the collision visible instead of silently ignoring a reader parameter; if maintainers prefer backwards compatibility for existing wrappers that happen to declare such a parameter, an alternative is to let the multi-file option win and emit a warning instead of failing registration. The multi-file reader keeps its prior overload preference when a path-only call is possible.

The base is `main` at `f6f2e607114d9a9a45fb84da0477c2d306875524`. DuckDB's contributor guide directs patch branches toward `main`. Consider a `v2.0-cyanoptera` backport after review because v2.0.1 also drops these parameters and the fix needs no ABI addition. Adapt the C API test to the release branch's signature API and verify against that branch.

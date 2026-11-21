//! Pure schema logic for `read_dbc`: duplicate-name renaming, type override
//! parsing, cross-file schema checks, `union_by_name` type promotion, and
//! per-file column plans. No DuckDB types here, so it is unit-testable.

use read_dbc_format::ColumnType;

/// DuckDB's maximum DECIMAL precision.
pub const MAX_DECIMAL_WIDTH: u8 = 38;

/// Integer digits a BIGINT needs as a DECIMAL (`i64::MAX` has 19 digits).
const BIGINT_DIGITS: u8 = 19;

/// Rename later duplicates of a name (ASCII case-insensitive) to `NAME_1`,
/// `NAME_2`, ..., skipping suffixes that collide with any other name, like
/// DuckDB's `read_csv`.
pub fn dedupe_names(names: &[String]) -> Vec<String> {
    let lower: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    let mut seen: Vec<String> = Vec::with_capacity(names.len());
    let mut out = Vec::with_capacity(names.len());
    for (name, key) in names.iter().zip(&lower) {
        if !seen.contains(key) {
            seen.push(key.clone());
            out.push(name.clone());
            continue;
        }
        // Skip suffixes already emitted or used by an original name.
        let mut k = 1usize;
        loop {
            let candidate = format!("{name}_{k}");
            let cl = candidate.to_ascii_lowercase();
            if !seen.contains(&cl) && !lower.contains(&cl) {
                seen.push(cl);
                out.push(candidate);
                break;
            }
            k += 1;
        }
    }
    out
}

/// The type names accepted in `types := {...}`.
pub const TYPE_NAMES: &str = "VARCHAR, BIGINT, DATE, BOOLEAN, DECIMAL(width,scale)";

/// Parse a type name for a `types` override (case-insensitive):
/// `VARCHAR`/`TEXT`/`STRING`, `BIGINT`/`INT64`/`LONG`, `DATE`,
/// `BOOLEAN`/`BOOL`, `DECIMAL(w,s)`/`NUMERIC(w,s)`.
pub fn parse_type_name(name: &str) -> Option<ColumnType> {
    if let Some(t) = ColumnType::from_name(name) {
        return Some(t);
    }
    let compact: String = name
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    let inner = compact
        .strip_prefix("DECIMAL(")
        .or_else(|| compact.strip_prefix("NUMERIC("))?
        .strip_suffix(')')?;
    let (w, s) = inner.split_once(',')?;
    let width: u8 = w.parse().ok()?;
    let scale: u8 = s.parse().ok()?;
    if width == 0 || width > MAX_DECIMAL_WIDTH || scale >= width {
        return None;
    }
    Some(ColumnType::Decimal { width, scale })
}

/// Integer and fractional digits of a numeric type, if numeric.
fn digits(t: ColumnType) -> Option<(u8, u8)> {
    match t {
        ColumnType::BigInt => Some((BIGINT_DIGITS, 0)),
        ColumnType::Decimal { width, scale } => Some((width - scale, scale)),
        _ => None,
    }
}

/// The `union_by_name` type of a column seen as `a` in one file and `b` in
/// another. `Err` carries a reason when no exact type exists.
///
/// * equal types stay;
/// * BIGINT/DECIMAL combinations become the smallest DECIMAL holding both
///   exactly (BIGINT counts as 19 integer digits), if that fits in 38 digits;
/// * anything else (VARCHAR with anything, DATE with BOOLEAN, ...) becomes
///   VARCHAR.
pub fn promote(a: ColumnType, b: ColumnType) -> Result<ColumnType, String> {
    if a == b {
        return Ok(a);
    }
    match (digits(a), digits(b)) {
        (Some((ia, sa)), Some((ib, sb))) => {
            let int = ia.max(ib);
            let scale = sa.max(sb);
            let width = int as u16 + scale as u16;
            if width > MAX_DECIMAL_WIDTH as u16 {
                return Err(format!(
                    "{a} and {b} have no common exact type: it would need DECIMAL({width},{scale}), \
                     wider than DuckDB's maximum of 38 digits"
                ));
            }
            Ok(ColumnType::Decimal {
                width: width as u8,
                scale,
            })
        }
        _ => Ok(ColumnType::Varchar),
    }
}

/// How a file's value becomes a value of the output column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conv {
    /// Same type.
    Identity,
    /// BIGINT or DECIMAL → DECIMAL: multiply the scaled integer by `factor`
    /// (a power of ten).
    Rescale { factor: i128 },
    /// Any type → VARCHAR, rendered like DuckDB's cast to VARCHAR. `scale`
    /// is the source decimal scale (0 otherwise).
    ToText { scale: u8 },
}

/// The conversion from `from` (a file's column type) to `to` (the output
/// type), which [`promote`] guarantees exists.
pub fn conversion(from: ColumnType, to: ColumnType) -> Conv {
    if from == to {
        return Conv::Identity;
    }
    match to {
        ColumnType::Varchar => Conv::ToText {
            scale: match from {
                ColumnType::Decimal { scale, .. } => scale,
                _ => 0,
            },
        },
        ColumnType::Decimal {
            scale: to_scale, ..
        } => {
            let from_scale = match from {
                ColumnType::Decimal { scale, .. } => scale,
                _ => 0,
            };
            Conv::Rescale {
                factor: 10i128.pow((to_scale - from_scale) as u32),
            }
        }
        // `promote` only widens to DECIMAL or VARCHAR.
        _ => Conv::Identity,
    }
}

/// One file's columns as the bind phase sees them.
#[derive(Debug, Clone)]
pub struct FileSchema {
    /// Output names (after duplicate renaming), one per field.
    pub names: Vec<String>,
    /// Column types, one per field.
    pub types: Vec<ColumnType>,
}

/// An output column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutColumn {
    pub name: String,
    pub column_type: ColumnType,
}

/// Combine the schemas of all files. `paths` is used for messages only.
pub fn combine(
    files: &[FileSchema],
    paths: &[String],
    union_by_name: bool,
) -> Result<Vec<OutColumn>, String> {
    let Some(first) = files.first() else {
        return Ok(Vec::new());
    };
    let mut out: Vec<OutColumn> = first
        .names
        .iter()
        .zip(&first.types)
        .map(|(n, t)| OutColumn {
            name: n.clone(),
            column_type: *t,
        })
        .collect();
    if !union_by_name {
        for (f, file) in files.iter().enumerate().skip(1) {
            check_same(&out, file, &paths[0], &paths[f])?;
        }
        return Ok(out);
    }
    for (f, file) in files.iter().enumerate().skip(1) {
        for (name, t) in file.names.iter().zip(&file.types) {
            match out.iter_mut().find(|c| c.name.eq_ignore_ascii_case(name)) {
                Some(col) => {
                    col.column_type = promote(col.column_type, *t).map_err(|reason| {
                        format!(
                            "column \"{}\" cannot be unified across files ({} in an earlier file, {t} in \"{}\"): {reason}; \
                             use types := {{...}} or all_varchar := true",
                            col.name, col.column_type, paths[f]
                        )
                    })?;
                }
                None => out.push(OutColumn {
                    name: name.clone(),
                    column_type: *t,
                }),
            }
        }
    }
    Ok(out)
}

fn check_same(
    out: &[OutColumn],
    file: &FileSchema,
    first_path: &str,
    path: &str,
) -> Result<(), String> {
    let hint = "use union_by_name := true to combine files with different schemas";
    for (i, col) in out.iter().enumerate() {
        match (file.names.get(i), file.types.get(i)) {
            (None, _) | (_, None) => {
                return Err(format!(
                    "schema of \"{path}\" differs from \"{first_path}\": it has no column {} (\"{}\"); {hint}",
                    i + 1,
                    col.name
                ));
            }
            (Some(name), Some(t)) => {
                if !name.eq_ignore_ascii_case(&col.name) {
                    return Err(format!(
                        "schema of \"{path}\" differs from \"{first_path}\": column {} is \"{name}\" but \"{}\" in the first file; {hint}",
                        i + 1,
                        col.name
                    ));
                }
                if *t != col.column_type {
                    return Err(format!(
                        "schema of \"{path}\" differs from \"{first_path}\": column \"{name}\" is {t} but {} in the first file; {hint}",
                        col.column_type
                    ));
                }
            }
        }
    }
    if file.names.len() > out.len() {
        return Err(format!(
            "schema of \"{path}\" differs from \"{first_path}\": it has an extra column \"{}\"; {hint}",
            file.names[out.len()]
        ));
    }
    Ok(())
}

/// For each output column, the index of the matching column of `file`
/// (case-insensitive) and the conversion, or `None` when the file lacks it.
pub fn plan_file(out: &[OutColumn], file: &FileSchema) -> Vec<Option<(usize, Conv)>> {
    out.iter()
        .map(|col| {
            file.names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(&col.name))
                .map(|j| (j, conversion(file.types[j], col.column_type)))
        })
        .collect()
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian
/// (Howard Hinnant's `civil_from_days`).
pub fn civil_from_days(days: i32) -> (i64, u32, u32) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Render a scaled decimal like DuckDB (`-0.50`, `12.30`, `7`).
pub fn format_decimal(out: &mut String, value: i128, scale: u8) {
    use std::fmt::Write;
    if scale == 0 {
        let _ = write!(out, "{value}");
        return;
    }
    let neg = value < 0;
    let abs = value.unsigned_abs();
    let pow = 10u128.pow(scale as u32);
    let (int, frac) = (abs / pow, abs % pow);
    if neg {
        out.push('-');
    }
    let _ = write!(out, "{int}.{frac:0width$}", width = scale as usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn dedupe() {
        assert_eq!(dedupe_names(&s(&["A", "A", "a"])), s(&["A", "A_1", "a_2"]));
        assert_eq!(
            dedupe_names(&s(&["A", "A", "A_1"])),
            s(&["A", "A_2", "A_1"])
        );
        assert_eq!(dedupe_names(&s(&["X", "Y"])), s(&["X", "Y"]));
    }

    #[test]
    fn promotion() {
        use ColumnType::*;
        let d = |w, s| Decimal { width: w, scale: s };
        assert_eq!(promote(BigInt, BigInt), Ok(BigInt));
        assert_eq!(promote(BigInt, d(7, 2)), Ok(d(21, 2)));
        assert_eq!(promote(d(7, 2), d(12, 4)), Ok(d(12, 4)));
        assert_eq!(promote(d(10, 0), d(5, 4)), Ok(d(14, 4)));
        assert!(promote(d(38, 0), d(38, 2)).is_err());
        assert!(promote(BigInt, d(38, 20)).is_err());
        assert_eq!(promote(Date, Boolean), Ok(Varchar));
        assert_eq!(promote(Varchar, d(3, 1)), Ok(Varchar));
        assert_eq!(conversion(d(7, 2), d(21, 4)), Conv::Rescale { factor: 100 });
        assert_eq!(conversion(BigInt, d(21, 2)), Conv::Rescale { factor: 100 });
        assert_eq!(conversion(d(7, 2), Varchar), Conv::ToText { scale: 2 });
    }

    #[test]
    fn types() {
        assert_eq!(parse_type_name("date"), Some(ColumnType::Date));
        assert_eq!(
            parse_type_name("decimal( 7 , 2 )"),
            Some(ColumnType::Decimal { width: 7, scale: 2 })
        );
        assert_eq!(parse_type_name("DECIMAL(39,2)"), None);
        assert_eq!(parse_type_name("INTEGER"), None);
    }

    #[test]
    fn dates_and_decimals() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_737), (2024, 1, 15));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        let mut t = String::new();
        format_decimal(&mut t, -50, 2);
        assert_eq!(t, "-0.50");
        t.clear();
        format_decimal(&mut t, 123_456, 2);
        assert_eq!(t, "1234.56");
        t.clear();
        format_decimal(&mut t, 7, 0);
        assert_eq!(t, "7");
    }
}

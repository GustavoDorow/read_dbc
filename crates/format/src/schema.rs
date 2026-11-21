//! Output schema: how each field maps to a column type, decided from the
//! header alone (no data scan).

use std::fmt;

use crate::error::{Error, Result};
use crate::header::{Field, Header};
use crate::options::ReadOptions;

/// Logical type of an output column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnType {
    /// Text ([`Value::Text`](crate::Value::Text)).
    Varchar,
    /// 64-bit integer ([`Value::BigInt`](crate::Value::BigInt)).
    BigInt,
    /// Exact decimal. Values are integers scaled by `10^scale`, delivered as
    /// [`Value::Decimal64`](crate::Value::Decimal64) when `width <= 18`,
    /// otherwise [`Value::Decimal128`](crate::Value::Decimal128).
    Decimal {
        /// Total digits (precision), 1..=38.
        width: u8,
        /// Digits after the decimal point.
        scale: u8,
    },
    /// Date as days since 1970-01-01 ([`Value::Date`](crate::Value::Date)).
    Date,
    /// Boolean ([`Value::Boolean`](crate::Value::Boolean)).
    Boolean,
}

impl ColumnType {
    /// For decimals: true if values are delivered as `i128`.
    pub fn is_wide_decimal(self) -> bool {
        matches!(self, ColumnType::Decimal { width, .. } if width > 18)
    }

    /// Parse a SQL-ish type name: `VARCHAR`/`TEXT`/`STRING`,
    /// `BIGINT`/`INT64`, `DATE`, `BOOLEAN`/`BOOL` (case-insensitive).
    pub fn from_name(name: &str) -> Option<ColumnType> {
        Some(match name.trim().to_ascii_uppercase().as_str() {
            "VARCHAR" | "TEXT" | "STRING" => ColumnType::Varchar,
            "BIGINT" | "INT64" | "LONG" => ColumnType::BigInt,
            "DATE" => ColumnType::Date,
            "BOOLEAN" | "BOOL" => ColumnType::Boolean,
            _ => return None,
        })
    }
}

impl fmt::Display for ColumnType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ColumnType::Varchar => f.write_str("VARCHAR"),
            ColumnType::BigInt => f.write_str("BIGINT"),
            ColumnType::Decimal { width, scale } => write!(f, "DECIMAL({width},{scale})"),
            ColumnType::Date => f.write_str("DATE"),
            ColumnType::Boolean => f.write_str("BOOLEAN"),
        }
    }
}

/// How a field's bytes are turned into a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decoder {
    /// Strip trailing spaces/NULs, decode text. Blank → "".
    Text,
    /// Numeric with no decimals → i64. Blank → NULL.
    Int64,
    /// Numeric → scaled integer. Blank → NULL.
    Decimal { scale: u8, width: u8 },
    /// `D` field. Blank/zero/invalid → NULL.
    DateField,
    /// `L` field.
    Logical,
    /// `C` field read as `YYYYMMDD` (strict).
    TextDate,
    /// `C` field read as an integer (strict).
    TextInt64,
}

/// One output column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// The field name.
    pub name: String,
    /// Index into [`Header::fields`].
    pub field_index: usize,
    /// The output type.
    pub column_type: ColumnType,
    pub(crate) decoder: Decoder,
}

fn natural(field: &Field) -> std::result::Result<(ColumnType, Decoder), String> {
    let len = field.length;
    let dec = field.decimals;
    match field.field_type {
        b'C' => Ok((ColumnType::Varchar, Decoder::Text)),
        b'N' | b'F' => {
            if len > 38 {
                return Err(format!(
                    "numeric width {len} exceeds 38 digits (use all_varchar)"
                ));
            }
            if dec as usize > len {
                return Err(format!("decimal count {dec} exceeds the width {len}"));
            }
            if dec == 0 && len <= 18 {
                Ok((ColumnType::BigInt, Decoder::Int64))
            } else {
                let width = len as u8;
                Ok((
                    ColumnType::Decimal { width, scale: dec },
                    Decoder::Decimal { scale: dec, width },
                ))
            }
        }
        b'D' => {
            if len != 8 {
                return Err(format!("date field has width {len} (expected 8)"));
            }
            Ok((ColumnType::Date, Decoder::DateField))
        }
        b'L' => Ok((ColumnType::Boolean, Decoder::Logical)),
        _ => Err("unsupported field type (use all_varchar to read it as text)".into()),
    }
}

fn override_decoder(field: &Field, to: ColumnType) -> Option<Decoder> {
    match (field.field_type, to) {
        (_, ColumnType::Varchar) => Some(Decoder::Text),
        (b'C', ColumnType::Date) => Some(Decoder::TextDate),
        (b'C', ColumnType::BigInt) => Some(Decoder::TextInt64),
        _ => match natural(field) {
            Ok((t, d)) if t == to => Some(d),
            _ => None,
        },
    }
}

/// Resolve the output columns for `header` under `options`: one column
/// per field, in field order, with the type chosen from the header alone
/// (see the [crate documentation](crate#type-mapping)).
///
/// This is exactly what [`DbcReader`](crate::DbcReader) uses, so a planner
/// can call [`Header::read`] and then this function to declare the columns
/// without reading (or decompressing) any record data. Fails with
/// [`Error::UnsupportedField`] or [`Error::InvalidOption`] on the same
/// inputs that make [`DbcReader::new`](crate::DbcReader::new) fail.
pub fn resolve_schema(header: &Header, options: &ReadOptions) -> Result<Vec<Column>> {
    let mut overrides: Vec<Option<ColumnType>> = vec![None; header.fields.len()];
    for (name, to) in &options.types {
        let matches: Vec<usize> = header
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.name.eq_ignore_ascii_case(name))
            .map(|(i, _)| i)
            .collect();
        let i = match matches.as_slice() {
            [] => {
                return Err(Error::InvalidOption(format!(
                    "type override for unknown field {name:?}"
                )));
            }
            [i] => *i,
            _ => {
                return Err(Error::InvalidOption(format!(
                    "type override for {name:?} is ambiguous: several fields have that name"
                )));
            }
        };
        if overrides[i].is_some() {
            return Err(Error::InvalidOption(format!(
                "field {name:?} has more than one type override"
            )));
        }
        overrides[i] = Some(*to);
    }

    header
        .fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let (column_type, decoder) = if let Some(to) = overrides[i] {
                let d = override_decoder(field, to).ok_or_else(|| {
                    Error::InvalidOption(format!(
                        "cannot read field {:?} (type '{}', width {}, {} decimals) as {to}",
                        field.name,
                        field.type_char(),
                        field.length,
                        field.decimals
                    ))
                })?;
                (to, d)
            } else if options.all_varchar {
                (ColumnType::Varchar, Decoder::Text)
            } else {
                natural(field).map_err(|reason| Error::UnsupportedField {
                    field: field.name.clone(),
                    field_type: field.type_char(),
                    reason,
                })?
            };
            Ok(Column {
                name: field.name.clone(),
                field_index: i,
                column_type,
                decoder,
            })
        })
        .collect()
}

//! Column buffers filled by `DbcReader::read_batch` and written into DuckDB
//! vectors afterwards.

use duckdb_neo::{
    Result,
    data_chunk::DataChunk,
    error::Error,
    types::{DateValue, HugeDecimal, LongDecimal, ShortDecimal, WordDecimal},
};
use read_dbc_format::{BatchSink, ColumnType, Value};

use super::schema::{Conv, civil_from_days, format_decimal};

/// One column of decoded values, in the output column's representation.
#[derive(Debug)]
pub enum Buf {
    /// UTF-8 text concatenated in `data`; row `i` is `data[ends[i-1]..ends[i]]`.
    Text {
        data: String,
        ends: Vec<usize>,
        valid: Vec<bool>,
    },
    BigInt(Vec<Option<i64>>),
    /// Scaled decimal integers (all widths), narrowed when written.
    Decimal(Vec<Option<i128>>),
    Date(Vec<Option<i32>>),
    Boolean(Vec<Option<bool>>),
}

impl Buf {
    pub fn new(t: ColumnType) -> Buf {
        match t {
            ColumnType::Varchar => Buf::Text {
                data: String::new(),
                ends: Vec::new(),
                valid: Vec::new(),
            },
            ColumnType::BigInt => Buf::BigInt(Vec::new()),
            ColumnType::Decimal { .. } => Buf::Decimal(Vec::new()),
            ColumnType::Date => Buf::Date(Vec::new()),
            ColumnType::Boolean => Buf::Boolean(Vec::new()),
        }
    }

    fn clear(&mut self) {
        match self {
            Buf::Text { data, ends, valid } => {
                data.clear();
                ends.clear();
                valid.clear();
            }
            Buf::BigInt(v) => v.clear(),
            Buf::Decimal(v) => v.clear(),
            Buf::Date(v) => v.clear(),
            Buf::Boolean(v) => v.clear(),
        }
    }

    fn push_null(&mut self) {
        match self {
            Buf::Text { data, ends, valid } => {
                ends.push(data.len());
                valid.push(false);
            }
            Buf::BigInt(v) => v.push(None),
            Buf::Decimal(v) => v.push(None),
            Buf::Date(v) => v.push(None),
            Buf::Boolean(v) => v.push(None),
        }
    }

    fn push_text_with(&mut self, f: impl FnOnce(&mut String)) -> bool {
        match self {
            Buf::Text { data, ends, valid } => {
                f(data);
                ends.push(data.len());
                valid.push(true);
                true
            }
            _ => false,
        }
    }
}

/// Where a reader projection slot goes.
#[derive(Debug, Clone, Copy)]
pub struct Slot {
    /// Index into [`Sink::bufs`].
    pub buf: usize,
    pub conv: Conv,
}

/// A [`BatchSink`] for one file's projection.
#[derive(Debug, Default)]
pub struct Sink {
    pub slots: Vec<Slot>,
    pub bufs: Vec<Buf>,
    pub deleted: Vec<bool>,
    /// First conversion failure in the current batch (the trait cannot fail).
    pub error: Option<String>,
}

impl Sink {
    pub fn clear(&mut self) {
        for b in &mut self.bufs {
            b.clear();
        }
        self.deleted.clear();
        self.error = None;
    }

    fn mismatch(&mut self, what: &str) {
        if self.error.is_none() {
            self.error = Some(format!("internal error: unexpected {what} value"));
        }
    }
}

impl BatchSink for Sink {
    fn value(&mut self, column: usize, _row: usize, value: Value<'_>) {
        let Slot { buf, conv } = self.slots[column];
        let b = &mut self.bufs[buf];
        let ok = match (conv, value) {
            (_, Value::Null) => {
                b.push_null();
                true
            }
            (Conv::Identity, Value::Text(s)) => b.push_text_with(|d| d.push_str(s)),
            (Conv::Identity, Value::BigInt(x)) => match b {
                Buf::BigInt(v) => {
                    v.push(Some(x));
                    true
                }
                _ => false,
            },
            (Conv::Identity, Value::Decimal64(x)) => match b {
                Buf::Decimal(v) => {
                    v.push(Some(x as i128));
                    true
                }
                _ => false,
            },
            (Conv::Identity, Value::Decimal128(x)) => match b {
                Buf::Decimal(v) => {
                    v.push(Some(x));
                    true
                }
                _ => false,
            },
            (Conv::Identity, Value::Date(x)) => match b {
                Buf::Date(v) => {
                    v.push(Some(x));
                    true
                }
                _ => false,
            },
            (Conv::Identity, Value::Boolean(x)) => match b {
                Buf::Boolean(v) => {
                    v.push(Some(x));
                    true
                }
                _ => false,
            },
            (Conv::Rescale { factor }, Value::BigInt(x) | Value::Decimal64(x)) => {
                rescale(b, x as i128, factor)
            }
            (Conv::Rescale { factor }, Value::Decimal128(x)) => rescale(b, x, factor),
            (Conv::ToText { scale }, v) => b.push_text_with(|d| render(d, v, scale)),
            _ => false,
        };
        if !ok {
            self.mismatch("column");
        }
    }

    fn deleted(&mut self, _row: usize, deleted: bool) {
        self.deleted.push(deleted);
    }
}

fn rescale(b: &mut Buf, x: i128, factor: i128) -> bool {
    match b {
        Buf::Decimal(v) => {
            // `promote` sized the output so this cannot overflow; stay exact anyway.
            v.push(x.checked_mul(factor));
            x.checked_mul(factor).is_some()
        }
        _ => false,
    }
}

/// Render a value like DuckDB's cast to VARCHAR.
fn render(d: &mut String, v: Value<'_>, scale: u8) {
    use std::fmt::Write;
    match v {
        Value::Null => {}
        Value::Text(s) => d.push_str(s),
        Value::BigInt(x) => {
            let _ = write!(d, "{x}");
        }
        Value::Decimal64(x) => format_decimal(d, x as i128, scale),
        Value::Decimal128(x) => format_decimal(d, x, scale),
        Value::Date(days) => {
            let (y, m, day) = civil_from_days(days);
            let _ = write!(d, "{y:04}-{m:02}-{day:02}");
        }
        Value::Boolean(b) => d.push_str(if b { "true" } else { "false" }),
    }
}

/// Write `rows` values of `buf` into output vector `pos` of type `t`.
pub fn write_buf(
    output: &mut DataChunk<'_>,
    pos: usize,
    t: ColumnType,
    buf: &Buf,
    rows: usize,
) -> Result<()> {
    match (t, buf) {
        (ColumnType::Varchar, Buf::Text { data, ends, valid }) => {
            let mut v = output.get_vector_at_mut::<String>(pos)?;
            v.set_size(rows)?;
            let mut w = v.writer()?;
            let mut start = 0;
            for i in 0..rows {
                let end = ends[i];
                if valid[i] {
                    w.set(i, &data[start..end])?;
                } else {
                    w.set_null(i)?;
                }
                start = end;
            }
        }
        (ColumnType::BigInt, Buf::BigInt(vals)) => {
            let mut v = output.get_vector_at_mut::<i64>(pos)?;
            v.set_size(rows)?;
            let mut w = v.writer()?;
            for (i, x) in vals.iter().take(rows).enumerate() {
                w.set_option(i, x.as_ref())?;
            }
        }
        (ColumnType::Decimal { width, .. }, Buf::Decimal(vals)) => {
            write_decimal(output, pos, width, &vals[..rows])?
        }
        (ColumnType::Date, Buf::Date(vals)) => {
            let mut v = output.get_vector_at_mut::<DateValue>(pos)?;
            v.set_size(rows)?;
            let mut w = v.writer()?;
            for (i, x) in vals.iter().take(rows).enumerate() {
                w.set_option(i, x.map(DateValue).as_ref())?;
            }
        }
        (ColumnType::Boolean, Buf::Boolean(vals)) => {
            let mut v = output.get_vector_at_mut::<bool>(pos)?;
            v.set_size(rows)?;
            let mut w = v.writer()?;
            for (i, x) in vals.iter().take(rows).enumerate() {
                w.set_option(i, x.as_ref())?;
            }
        }
        _ => {
            return Err(Error::internal(format!(
                "read_dbc: buffer does not match column type {t}"
            )));
        }
    }
    Ok(())
}

fn narrow<T: TryFrom<i128>>(x: i128) -> Result<T> {
    T::try_from(x).map_err(|_| {
        Error::invalid_input(format!("read_dbc: DECIMAL value {x} overflows its column"))
    })
}

fn write_decimal(
    output: &mut DataChunk<'_>,
    pos: usize,
    width: u8,
    vals: &[Option<i128>],
) -> Result<()> {
    macro_rules! go {
        ($ty:ident, $prim:ty) => {{
            let mut v = output.get_vector_at_mut::<$ty>(pos)?;
            v.set_size(vals.len())?;
            let mut w = v.writer()?;
            for (i, x) in vals.iter().enumerate() {
                match x {
                    Some(x) => w.set(i, &$ty(narrow::<$prim>(*x)?))?,
                    None => w.set_null(i)?,
                }
            }
        }};
    }
    match width {
        1..=4 => go!(ShortDecimal, i16),
        5..=9 => go!(WordDecimal, i32),
        10..=18 => go!(LongDecimal, i64),
        _ => go!(HugeDecimal, i128),
    }
    Ok(())
}

/// Write `rows` NULLs of type `t` into output vector `pos`.
pub fn write_nulls(
    output: &mut DataChunk<'_>,
    pos: usize,
    t: ColumnType,
    rows: usize,
) -> Result<()> {
    macro_rules! go {
        ($ty:ty) => {{
            let mut v = output.get_vector_at_mut::<$ty>(pos)?;
            v.set_size(rows)?;
            let mut w = v.writer()?;
            for i in 0..rows {
                w.set_null(i)?;
            }
        }};
    }
    match t {
        ColumnType::Varchar => go!(String),
        ColumnType::BigInt => go!(i64),
        ColumnType::Date => go!(DateValue),
        ColumnType::Boolean => go!(bool),
        ColumnType::Decimal { width, .. } => match width {
            1..=4 => go!(ShortDecimal),
            5..=9 => go!(WordDecimal),
            10..=18 => go!(LongDecimal),
            _ => go!(HugeDecimal),
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(b: &Buf) -> Vec<Option<String>> {
        match b {
            Buf::Text { data, ends, valid } => {
                let mut start = 0;
                ends.iter()
                    .zip(valid)
                    .map(|(&end, &ok)| {
                        let s = data[start..end].to_string();
                        start = end;
                        ok.then_some(s)
                    })
                    .collect()
            }
            _ => panic!("not text"),
        }
    }

    #[test]
    fn to_text_renders_like_duckdb_casts() {
        let mut sink = Sink {
            slots: vec![
                Slot {
                    buf: 0,
                    conv: Conv::ToText { scale: 2 },
                },
                Slot {
                    buf: 1,
                    conv: Conv::ToText { scale: 0 },
                },
            ],
            bufs: vec![Buf::new(ColumnType::Varchar), Buf::new(ColumnType::Varchar)],
            ..Default::default()
        };
        sink.value(0, 0, Value::Decimal64(-50));
        sink.value(1, 0, Value::Date(19_737));
        sink.value(0, 1, Value::Decimal128(123_456));
        sink.value(1, 1, Value::Boolean(true));
        sink.value(0, 2, Value::Null);
        sink.value(1, 2, Value::BigInt(-42));
        sink.value(0, 3, Value::Text("  x"));
        sink.value(1, 3, Value::Boolean(false));
        assert!(sink.error.is_none());
        assert_eq!(
            text(&sink.bufs[0]),
            vec![
                Some("-0.50".into()),
                Some("1234.56".into()),
                None,
                Some("  x".into())
            ]
        );
        assert_eq!(
            text(&sink.bufs[1]),
            vec![
                Some("2024-01-15".into()),
                Some("true".into()),
                Some("-42".into()),
                Some("false".into())
            ]
        );
    }

    #[test]
    fn rescale_is_exact() {
        let mut sink = Sink {
            slots: vec![Slot {
                buf: 0,
                conv: Conv::Rescale { factor: 100 },
            }],
            bufs: vec![Buf::new(ColumnType::Decimal {
                width: 21,
                scale: 2,
            })],
            ..Default::default()
        };
        sink.value(0, 0, Value::BigInt(-5));
        sink.value(0, 1, Value::Decimal64(i64::MAX));
        sink.value(0, 2, Value::Null);
        assert!(sink.error.is_none());
        match &sink.bufs[0] {
            Buf::Decimal(v) => assert_eq!(v, &vec![Some(-500), Some(i64::MAX as i128 * 100), None]),
            _ => panic!(),
        }
        sink.clear();
        sink.value(0, 0, Value::Decimal128(i128::MAX));
        assert!(sink.error.is_some());
    }

    #[test]
    fn type_mismatch_is_reported() {
        let mut sink = Sink {
            slots: vec![Slot {
                buf: 0,
                conv: Conv::Identity,
            }],
            bufs: vec![Buf::new(ColumnType::BigInt)],
            ..Default::default()
        };
        sink.value(0, 0, Value::Text("x"));
        assert!(sink.error.is_some());
    }
}

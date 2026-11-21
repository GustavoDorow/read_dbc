//! Parsers for the raw bytes of individual dBase fields.
//!
//! All functions are pure and work on the field's bytes exactly as stored
//! (fixed width, padded). Errors are short static descriptions; the reader
//! adds the record/field/offset context.

/// Strip trailing spaces and NULs, the rule dbfread 2.0.7 applies to `C`
/// fields (`data.rstrip(b'\0 ')`). Leading bytes are untouched.
#[inline]
pub fn trim_text(raw: &[u8]) -> &[u8] {
    let mut end = raw.len();
    while end > 0 && matches!(raw[end - 1], b' ' | 0) {
        end -= 1;
    }
    &raw[..end]
}

#[inline]
fn is_blank(raw: &[u8]) -> bool {
    raw.iter().all(|&b| b == b' ' || b == 0)
}

#[inline]
fn trim_spaces(raw: &[u8]) -> &[u8] {
    let start = raw.iter().position(|&b| b != b' ').unwrap_or(raw.len());
    let end = raw
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(start, |p| p + 1);
    &raw[start..end]
}

const POW10: [u128; 39] = {
    let mut t = [1u128; 39];
    let mut i = 1;
    while i < 39 {
        t[i] = t[i - 1] * 10;
        i += 1;
    }
    t
};

/// Parse a numeric (`N`/`F`) field exactly into an integer scaled by
/// `10^scale`, never going through floating point.
///
/// * Blank (only spaces and NULs, or empty) → `Ok(None)`.
/// * Accepted: optional leading/trailing spaces, an optional `+`/`-` sign,
///   digits, and an optional `.` followed by at most `scale` digits (fewer
///   are zero-padded). At least one digit is required.
/// * Anything else (embedded spaces, `*` overflow markers, `,`, exponents,
///   too many fractional digits) is an error, as is an absolute value of
///   `10^max_digits` or more (`max_digits` is capped at 38).
pub fn parse_scaled(raw: &[u8], scale: u32, max_digits: u32) -> Result<Option<i128>, &'static str> {
    if is_blank(raw) {
        return Ok(None);
    }
    let s = trim_spaces(raw);
    let (neg, digits) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut acc: u128 = 0;
    let mut ndigits = 0u32;
    let mut frac: Option<u32> = None;
    for &b in digits {
        match b {
            b'0'..=b'9' => {
                if let Some(f) = frac.as_mut() {
                    if *f == scale {
                        return Err("more fractional digits than the field's decimal count");
                    }
                    *f += 1;
                }
                ndigits += 1;
                acc = acc
                    .checked_mul(10)
                    .and_then(|a| a.checked_add((b - b'0') as u128))
                    .ok_or("number too large")?;
            }
            b'.' if frac.is_none() => frac = Some(0),
            _ => return Err("not a valid decimal number"),
        }
    }
    if ndigits == 0 {
        return Err("not a valid decimal number");
    }
    let pad = scale - frac.unwrap_or(0);
    let value = acc
        .checked_mul(POW10[pad as usize])
        .ok_or("number too large")?;
    if value >= POW10[max_digits.min(38) as usize] {
        return Err("number does not fit the field's declared width");
    }
    let v = value as i128; // value < 10^38 < i128::MAX
    Ok(Some(if neg { -v } else { v }))
}

/// Parse an integer (`N`/`F` with no decimals, or a `C` field overridden to
/// BIGINT) into an `i64`. Same syntax as [`parse_scaled`] with scale 0
/// (a bare trailing `.` is accepted).
pub fn parse_i64(raw: &[u8]) -> Result<Option<i64>, &'static str> {
    match parse_scaled(raw, 0, 19)? {
        None => Ok(None),
        Some(v) => i64::try_from(v)
            .map(Some)
            .map_err(|_| "integer out of range for BIGINT"),
    }
}

/// Days from 1970-01-01 to the proleptic Gregorian date `y-m-d`.
/// From Howard Hinnant's `days_from_civil`.
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i32 - 719_468
}

fn is_leap(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

/// Parse exactly eight ASCII digits `YYYYMMDD` into days since the epoch.
/// Returns `None` if the bytes are not a valid calendar date (year 0001-9999).
pub fn parse_yyyymmdd(raw: &[u8]) -> Option<i32> {
    if raw.len() != 8 || !raw.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let num = |r: &[u8]| r.iter().fold(0u32, |a, &b| a * 10 + (b - b'0') as u32);
    let (y, m, d) = (num(&raw[..4]), num(&raw[4..6]), num(&raw[6..]));
    if y == 0 || !(1..=12).contains(&m) || d == 0 {
        return None;
    }
    let dim = match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if d > dim {
        return None;
    }
    Some(days_from_civil(y as i32, m, d))
}

/// A `D` field: `YYYYMMDD`. Blank or all-zero (only spaces, zeros and NULs)
/// → `None`; an invalid date also → `None` (the dBase convention; dbfread
/// raises instead).
pub fn parse_date_field(raw: &[u8]) -> Option<i32> {
    parse_yyyymmdd(raw)
}

/// A `C` field read as a date (type override): trailing spaces/NULs are
/// stripped; empty or exactly `00000000` (the usual "no date" filler in
/// DATASUS files) → `Ok(None)`; otherwise it must be a valid `YYYYMMDD`
/// (anything else, e.g. `20230230` or `0000000`, is an error).
pub fn parse_text_date(raw: &[u8]) -> Result<Option<i32>, &'static str> {
    let t = trim_text(raw);
    if t.is_empty() || t == b"00000000" {
        return Ok(None);
    }
    parse_yyyymmdd(t)
        .map(Some)
        .ok_or("not a valid YYYYMMDD date")
}

/// An `L` field: `T/t/Y/y` → true, `F/f/N/n` → false, `?` or blank → `None`
/// (dbfread 2.0.7's rules). Surrounding spaces are ignored.
pub fn parse_logical(raw: &[u8]) -> Result<Option<bool>, &'static str> {
    match trim_spaces(raw) {
        [] => Ok(None),
        [b'T' | b't' | b'Y' | b'y'] => Ok(Some(true)),
        [b'F' | b'f' | b'N' | b'n'] => Ok(Some(false)),
        [b'?'] => Ok(None),
        _ => Err("not a valid logical value (T/F/Y/N/?)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_trim() {
        assert_eq!(trim_text(b"  AB  \0 \0"), b"  AB");
        assert_eq!(trim_text(b"007"), b"007");
        assert_eq!(trim_text(b"   "), b"");
        assert_eq!(trim_text(b"\0\0"), b"");
        assert_eq!(trim_text(b"A\0B "), b"A\0B");
    }

    #[test]
    fn scaled() {
        let p = |s: &[u8], sc| parse_scaled(s, sc, 38);
        assert_eq!(p(b"        91.86", 2), Ok(Some(9186)));
        assert_eq!(p(b"         0.00", 2), Ok(Some(0)));
        assert_eq!(p(b"    -1.5", 2), Ok(Some(-150)));
        assert_eq!(p(b"+1.", 2), Ok(Some(100)));
        assert_eq!(p(b".5 ", 2), Ok(Some(50)));
        assert_eq!(p(b"12", 2), Ok(Some(1200)));
        assert_eq!(p(b"   ", 2), Ok(None));
        assert_eq!(p(b"", 2), Ok(None));
        assert_eq!(p(b"\0\0", 2), Ok(None));
        assert!(p(b"1.234", 2).is_err());
        assert!(p(b"1e5", 2).is_err());
        assert!(p(b"1,5", 2).is_err());
        assert!(p(b"*****", 2).is_err());
        assert!(p(b"- 1", 2).is_err());
        assert!(p(b"1 2", 2).is_err());
        assert!(p(b"-", 2).is_err());
        assert!(p(b".", 2).is_err());
        assert!(p(b"1..2", 2).is_err());
        assert!(p(b"--1", 2).is_err());
        assert_eq!(parse_scaled(b"99999", 0, 5), Ok(Some(99999)));
        assert!(parse_scaled(b"100000", 0, 5).is_err());
        assert!(parse_scaled(b"9.99", 2, 2).is_err());
        assert_eq!(parse_scaled(b"0.99", 2, 2), Ok(Some(99)));
        // 38 digits fit; 39 do not.
        let big = [b'9'; 38];
        assert_eq!(parse_scaled(&big, 0, 38), Ok(Some(POW10[38] as i128 - 1)));
        assert!(parse_scaled(&[b'9'; 39], 0, 38).is_err());
        assert!(parse_scaled(&[b'9'; 60], 0, 38).is_err());
    }

    #[test]
    fn ints() {
        assert_eq!(parse_i64(b"  0"), Ok(Some(0)));
        assert_eq!(parse_i64(b"-42 "), Ok(Some(-42)));
        assert_eq!(parse_i64(b"0042"), Ok(Some(42)));
        assert_eq!(parse_i64(b"9223372036854775807"), Ok(Some(i64::MAX)));
        assert_eq!(parse_i64(b"-9223372036854775808"), Ok(Some(i64::MIN)));
        assert!(parse_i64(b"9223372036854775808").is_err());
        assert!(parse_i64(b"1.5").is_err());
        assert_eq!(parse_i64(b"7."), Ok(Some(7)));
        assert_eq!(parse_i64(b"   "), Ok(None));
        assert!(parse_i64(b"12a").is_err());
    }

    #[test]
    fn dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(
            parse_yyyymmdd(b"19250818"),
            Some(days_from_civil(1925, 8, 18))
        );
        assert_eq!(parse_yyyymmdd(b"20240229"), Some(19_782));
        assert_eq!(parse_yyyymmdd(b"20230229"), None);
        assert_eq!(parse_yyyymmdd(b"19000229"), None);
        assert_eq!(
            parse_yyyymmdd(b"20000229"),
            Some(days_from_civil(2000, 2, 29))
        );
        assert_eq!(parse_yyyymmdd(b"20241301"), None);
        assert_eq!(parse_yyyymmdd(b"20240431"), None);
        assert_eq!(parse_yyyymmdd(b"00000101"), None);
        assert_eq!(parse_yyyymmdd(b"2024010"), None);
        assert_eq!(parse_yyyymmdd(b"2024-1-1"), None);
        assert_eq!(parse_date_field(b"        "), None);
        assert_eq!(parse_date_field(b"00000000"), None);
        assert_eq!(parse_date_field(b"garbage!"), None);
        assert_eq!(parse_text_date(b"20240101"), Ok(Some(19_723)));
        assert_eq!(parse_text_date(b"        "), Ok(None));
        assert_eq!(
            parse_text_date(b"2024010 "),
            Err("not a valid YYYYMMDD date")
        );
        assert_eq!(parse_text_date(b"00000000"), Ok(None));
        assert_eq!(parse_text_date(b"00000000  \0"), Ok(None));
        assert!(parse_text_date(b" 00000000").is_err());
        assert!(parse_text_date(b"0000000").is_err());
        assert!(parse_text_date(b"20230230").is_err());
        assert!(parse_text_date(b"00000101").is_err());
        assert!(parse_text_date(b" 2024010").is_err());
    }

    #[test]
    fn logical() {
        for b in [b"T", b"t", b"Y", b"y"] {
            assert_eq!(parse_logical(b), Ok(Some(true)));
        }
        for b in [b"F", b"f", b"N", b"n"] {
            assert_eq!(parse_logical(b), Ok(Some(false)));
        }
        assert_eq!(parse_logical(b"?"), Ok(None));
        assert_eq!(parse_logical(b" "), Ok(None));
        assert!(parse_logical(b"X").is_err());
        assert!(parse_logical(b"1").is_err());
    }
}

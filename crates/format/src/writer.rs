//! **Test tooling**: a minimal dBase III / DATASUS `.dbc` *writer*, behind
//! the non-default `writer` cargo feature.
//!
//! It exists to generate the synthetic fixtures in `test/fixtures/` (see
//! `examples/make_fixtures.rs`) and similar test inputs. It is not a
//! production writer: it builds whole files in memory, validates little
//! (it can deliberately produce broken files), and its DCL "implode"
//! encoder is a simple greedy matcher, not a good compressor. The streams
//! it produces are real PKWare DCL streams, in any literal mode and
//! dictionary size, and decode with any `blast`-compatible decoder.
//!
//! ```
//! use read_dbc_format::writer::{DclOptions, FieldDef, Table};
//!
//! let mut t = Table::new(vec![FieldDef::new("UF", b'C', 2, 0), FieldDef::new("N", b'N', 3, 0)]);
//! t.push(false, &[b"AC", b"7"]);
//! let dbc = t.to_dbc(&DclOptions::default());
//! # let mut r = read_dbc_format::DbcReader::new(&dbc[..], Default::default()).unwrap();
//! # assert_eq!(r.finish().unwrap().records, 1);
//! ```

use crate::crc32::crc32;
use crate::dcl::testing::{Token, encode, max_dist};

/// One field descriptor to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDef {
    /// Name bytes (at most 11; written NUL-padded). Not validated, so
    /// duplicate or odd names can be produced on purpose.
    pub name: Vec<u8>,
    /// Type byte (`b'C'`, `b'N'`, ...).
    pub field_type: u8,
    /// Width in bytes.
    pub length: u8,
    /// Decimal count.
    pub decimals: u8,
}

impl FieldDef {
    /// A field with an ASCII name.
    pub fn new(name: &str, field_type: u8, length: u8, decimals: u8) -> FieldDef {
        assert!(
            name.len() <= 11,
            "field name {name:?} is longer than 11 bytes"
        );
        FieldDef {
            name: name.as_bytes().to_vec(),
            field_type,
            length,
            decimals,
        }
    }
}

/// A table to serialize. All fields are public so tests can build
/// inconsistent headers; [`Table::new`] gives DATASUS-like defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// Version byte (default 0x03).
    pub version: u8,
    /// Last-update date bytes (years since 1900, month, day).
    pub last_update: [u8; 3],
    /// Language-driver ID at offset 29 (default 0x00).
    pub ldid: u8,
    /// Header terminator byte at `H - 1` (default 0x0D).
    pub terminator: u8,
    /// Field descriptors.
    pub fields: Vec<FieldDef>,
    /// Full records, each starting with its deletion flag.
    pub records: Vec<Vec<u8>>,
    /// Append the 0x1A end-of-file marker after the records (default true).
    pub eof_marker: bool,
    /// Record count written to the header; `None` = `records.len()`.
    pub record_count: Option<u32>,
    /// Record length written to the header; `None` = computed from the fields.
    pub record_len: Option<u16>,
}

impl Table {
    /// A table with these fields, no records, version 0x03, LDID 0x00,
    /// terminator 0x0D, a fixed update date and an end-of-file marker.
    pub fn new(fields: Vec<FieldDef>) -> Table {
        Table {
            version: 0x03,
            last_update: [124, 1, 2],
            ldid: 0,
            terminator: 0x0D,
            fields,
            records: Vec::new(),
            eof_marker: true,
            record_count: None,
            record_len: None,
        }
    }

    /// Record length implied by the fields (flag byte included).
    pub fn computed_record_len(&self) -> usize {
        1 + self.fields.iter().map(|f| f.length as usize).sum::<usize>()
    }

    /// Append a record from one value per field, padded to the field width
    /// with spaces: right-aligned for `N`/`F`, left-aligned otherwise.
    /// Panics if a value is wider than its field or the count is wrong.
    pub fn push(&mut self, deleted: bool, values: &[&[u8]]) {
        assert_eq!(values.len(), self.fields.len(), "one value per field");
        let mut rec = vec![if deleted { b'*' } else { b' ' }];
        for (f, v) in self.fields.iter().zip(values) {
            let w = f.length as usize;
            assert!(
                v.len() <= w,
                "value {:?} is wider than field {:?} ({w})",
                String::from_utf8_lossy(v),
                String::from_utf8_lossy(&f.name)
            );
            let pad = w - v.len();
            if matches!(f.field_type, b'N' | b'F') {
                rec.extend(std::iter::repeat_n(b' ', pad));
                rec.extend_from_slice(v);
            } else {
                rec.extend_from_slice(v);
                rec.extend(std::iter::repeat_n(b' ', pad));
            }
        }
        self.records.push(rec);
    }

    /// Append a record verbatim (flag byte included, not checked).
    pub fn push_raw(&mut self, record: Vec<u8>) {
        self.records.push(record);
    }

    /// The header bytes (`H = 32 + 32 * fields + 1`).
    pub fn header_bytes(&self) -> Vec<u8> {
        let h = 32 + 32 * self.fields.len() + 1;
        let r = self.record_len.unwrap_or(self.computed_record_len() as u16);
        let n = self.record_count.unwrap_or(self.records.len() as u32);
        let mut out = vec![0u8; 32];
        out[0] = self.version;
        out[1..4].copy_from_slice(&self.last_update);
        out[4..8].copy_from_slice(&n.to_le_bytes());
        out[8..10].copy_from_slice(&(h as u16).to_le_bytes());
        out[10..12].copy_from_slice(&r.to_le_bytes());
        out[29] = self.ldid;
        for f in &self.fields {
            let mut d = [0u8; 32];
            let n = f.name.len().min(11);
            d[..n].copy_from_slice(&f.name[..n]);
            d[11] = f.field_type;
            d[16] = f.length;
            d[17] = f.decimals;
            out.extend_from_slice(&d);
        }
        out.push(self.terminator);
        out
    }

    /// The plain `.dbf` file: header, records, optional 0x1A.
    pub fn to_dbf(&self) -> Vec<u8> {
        let mut out = self.header_bytes();
        for r in &self.records {
            out.extend_from_slice(r);
        }
        if self.eof_marker {
            out.push(0x1A);
        }
        out
    }

    /// The `.dbc` file: header, CRC-32, DCL stream of the record area.
    pub fn to_dbc(&self, opts: &DclOptions) -> Vec<u8> {
        dbf_to_dbc(&self.to_dbf(), opts)
    }
}

/// Parameters of the DCL stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DclOptions {
    /// Literal mode: `false` = raw 8-bit literals (what DATASUS uses),
    /// `true` = literals coded with the fixed Huffman table.
    pub coded_literals: bool,
    /// Dictionary code: 4, 5 or 6 (1, 2 or 4 KiB window).
    pub dict_bits: u32,
    /// Emit back-references (greedy matching); `false` = literals only.
    pub matching: bool,
}

impl Default for DclOptions {
    /// DATASUS's parameters: raw literals, 4 KiB dictionary, with matching.
    fn default() -> Self {
        DclOptions {
            coded_literals: false,
            dict_bits: 6,
            matching: true,
        }
    }
}

/// Wrap a plain DBF image into a `.dbc`: the first `H` bytes verbatim, the
/// CRC-32 of the whole DBF image (little-endian), then [`implode`] of the
/// rest. Panics if the image is shorter than its header length.
pub fn dbf_to_dbc(dbf: &[u8], opts: &DclOptions) -> Vec<u8> {
    let h = u16::from_le_bytes([dbf[8], dbf[9]]) as usize;
    let mut out = dbf[..h].to_vec();
    out.extend_from_slice(&crc32(dbf).to_le_bytes());
    out.extend_from_slice(&implode(&dbf[h..], opts));
    out
}

/// Compress `data` into a complete DCL stream (mode bytes, tokens, end code).
pub fn implode(data: &[u8], opts: &DclOptions) -> Vec<u8> {
    assert!(
        (4..=6).contains(&opts.dict_bits),
        "dictionary code must be 4, 5 or 6"
    );
    let tokens = if opts.matching {
        greedy_tokens(data, opts.dict_bits)
    } else {
        data.iter().map(|&b| Token::Literal(b)).collect()
    };
    encode(&tokens, opts.coded_literals, opts.dict_bits)
}

/// Greedy longest-match tokenizer with hash chains on 3-byte prefixes
/// (plus a short direct scan for length-2 matches, whose distance is
/// limited to 256).
fn greedy_tokens(data: &[u8], dict: u32) -> Vec<Token> {
    const HASH: usize = 1 << 12;
    const CHAIN: usize = 128;
    const NONE: usize = usize::MAX;
    let window = max_dist(3, dict) as usize;
    let mut head = vec![NONE; HASH];
    let mut prev = vec![NONE; data.len()];
    let hash = |i: usize| -> usize {
        let v = (data[i] as usize) << 16 | (data[i + 1] as usize) << 8 | data[i + 2] as usize;
        (v.wrapping_mul(2_654_435_761) >> 7) & (HASH - 1)
    };
    let insert = |i: usize, head: &mut Vec<usize>, prev: &mut Vec<usize>| {
        if i + 3 <= data.len() {
            let k = hash(i);
            prev[i] = head[k];
            head[k] = i;
        }
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let max_len = (data.len() - i).min(518);
        let mut best = (0usize, 0usize); // (len, dist)
        if max_len >= 3 {
            let mut cand = head[hash(i)];
            let mut steps = 0;
            while cand != NONE && i - cand <= window && steps < CHAIN {
                let l = (0..max_len)
                    .take_while(|&k| data[cand + k] == data[i + k])
                    .count();
                if l > best.0 {
                    best = (l, i - cand);
                    if l == max_len {
                        break;
                    }
                }
                cand = prev[cand];
                steps += 1;
            }
        }
        if best.0 < 3 && max_len >= 2 {
            let lo = i.saturating_sub(max_dist(2, dict) as usize);
            if let Some(c) = (lo..i).rev().find(|&c| data[c..c + 2] == data[i..i + 2]) {
                best = (2, i - c);
            }
        }
        if best.0 >= 2 {
            out.push(Token::Match {
                len: best.0 as u16,
                dist: best.1 as u16,
            });
            for k in i..i + best.0 {
                insert(k, &mut head, &mut prev);
            }
            i += best.0;
        } else {
            out.push(Token::Literal(data[i]));
            insert(i, &mut head, &mut prev);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dcl::decompress_all;

    #[test]
    fn implode_round_trips_all_modes() {
        let mut data = Vec::new();
        let mut x = 12345u32;
        for i in 0..20_000u32 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            // Repetitive text with noise, long runs and far repeats.
            data.push(match i % 97 {
                0..=40 => b"ANALISE CIRURGICA "[(i % 18) as usize],
                41..=60 => b' ',
                _ => (x >> 24) as u8,
            });
        }
        data.extend_from_within(..5000);
        for coded in [false, true] {
            for dict in 4..=6 {
                for matching in [false, true] {
                    let o = DclOptions {
                        coded_literals: coded,
                        dict_bits: dict,
                        matching,
                    };
                    let c = implode(&data, &o);
                    assert_eq!(c[..2], [coded as u8, dict as u8]);
                    assert_eq!(decompress_all(&c).unwrap(), data, "{o:?}");
                    if matching {
                        assert!(c.len() < data.len() * 3 / 4, "{o:?}: {}", c.len());
                    }
                }
            }
        }
        for small in [&b""[..], b"a", b"ab", b"aa", b"aaa", b"abab"] {
            let c = implode(small, &DclOptions::default());
            assert_eq!(decompress_all(&c).unwrap(), small);
        }
    }

    #[test]
    fn table_round_trips_through_reader() {
        let mut t = Table::new(vec![
            FieldDef::new("A", b'C', 3, 0),
            FieldDef::new("B", b'N', 5, 2),
        ]);
        t.push(false, &[b"x", b"1.5"]);
        t.push(true, &[b"yz", b""]);
        assert_eq!(t.records[0], b" x    1.5");
        assert_eq!(t.records[1], b"*yz      ");
        let dbc = t.to_dbc(&DclOptions::default());
        let mut r = crate::DbcReader::new(&dbc[..], Default::default()).unwrap();
        let s = r.finish().unwrap();
        assert_eq!((s.records, s.deleted, s.eof_marker), (2, 1, true));
        let dbf = t.to_dbf();
        let mut r = crate::DbcReader::new_dbf(&dbf[..], Default::default()).unwrap();
        assert_eq!(r.finish().unwrap().records, 2);
    }
}

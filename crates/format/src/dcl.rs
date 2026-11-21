//! Streaming decoder for PKWare Data Compression Library ("DCL implode")
//! streams, the format DATASUS uses for the record area of `.dbc` files.
//!
//! This module is a Rust adaptation of `blast.c` 1.3 by Mark Adler (zlib
//! `contrib/blast`), distributed under the zlib license reproduced below.
//!
//! > Copyright (C) 2003, 2012, 2013 Mark Adler
//! >
//! > This software is provided 'as-is', without any express or implied
//! > warranty.  In no event will the author be held liable for any damages
//! > arising from the use of this software.
//! >
//! > Permission is granted to anyone to use this software for any purpose,
//! > including commercial applications, and to alter it and redistribute it
//! > freely, subject to the following restrictions:
//! >
//! > 1. The origin of this software must not be misrepresented; you must not
//! >    claim that you wrote the original software. If you use this software
//! >    in a product, an acknowledgment in the product documentation would be
//! >    appreciated but is not required.
//! > 2. Altered source versions must be plainly marked as such, and must not be
//! >    misrepresented as being the original software.
//! > 3. This notice may not be removed or altered from any source distribution.
//! >
//! > Mark Adler    madler@alumni.caltech.edu
//!
//! **This is an altered version** of the original: it was translated to Rust
//! and restructured as a pull-based [`std::io::Read`] adapter. Input is read
//! through a buffered 64-bit bit reader, Huffman codes are decoded with lookup
//! tables derived from blast's canonical tables (instead of bit-by-bit), and
//! decoded bytes are staged in a fixed ring buffer that doubles as the
//! 4 KiB history window. The format semantics, the fixed code tables and the
//! error codes follow `blast.c`. The datasus-core crate (MIT) contains another
//! port of blast.c that was read for reference; no code was copied from it.
//!
//! # Memory
//!
//! A [`DclReader`] owns a 32 KiB output ring (history + staging) and a
//! 32 KiB input buffer, regardless of the stream size. The Huffman lookup
//! tables (about 17 KiB) are built once per process and shared.

use std::fmt;
use std::io::{self, Read};
use std::sync::OnceLock;

/// Size of the output ring. Must be a power of two and at least the 4 KiB
/// maximum match distance plus the 518-byte maximum match length.
const RING: usize = 1 << 15;
const RING_MASK: usize = RING - 1;
/// Longest copy a single token can produce.
const MAX_MATCH: usize = 518;
/// Default size of the compressed input buffer.
const INPUT_BUF: usize = 1 << 15;

/// Decoding errors, mirroring the return codes of `blast()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlastError {
    /// The input ended before the end-of-stream code (`blast()` code 2).
    OutOfInput,
    /// The first byte (literal mode) is neither 0 nor 1 (code -1).
    BadLiteralFlag,
    /// The second byte (dictionary size) is not 4, 5 or 6 (code -2).
    BadDictSize,
    /// A match refers to data before the start of the output (code -3).
    DistanceTooFar,
    /// An invalid Huffman code (code -9). The fixed DCL code tables are
    /// complete, so this cannot occur; it is kept for parity with blast.c.
    BadCode,
}

impl BlastError {
    /// The numeric return code `blast.c` uses for this error.
    pub fn code(self) -> i32 {
        match self {
            BlastError::OutOfInput => 2,
            BlastError::BadLiteralFlag => -1,
            BlastError::BadDictSize => -2,
            BlastError::DistanceTooFar => -3,
            BlastError::BadCode => -9,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            BlastError::OutOfInput => "compressed stream ended before the end code",
            BlastError::BadLiteralFlag => "invalid literal-mode byte (must be 0 or 1)",
            BlastError::BadDictSize => "invalid dictionary-size byte (must be 4, 5 or 6)",
            BlastError::DistanceTooFar => "match distance reaches before the start of the output",
            BlastError::BadCode => "invalid Huffman code",
        }
    }
}

impl fmt::Display for BlastError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (blast code {})", self.describe(), self.code())
    }
}

/// An error from [`DclReader`].
#[derive(Debug)]
#[allow(missing_docs)] // variant fields are described by the variant docs
pub enum DclError {
    /// The compressed stream is invalid. `offset` is the number of
    /// compressed bytes consumed (at least partially) when it was detected,
    /// relative to the start of the DCL stream.
    Blast { error: BlastError, offset: u64 },
    /// Whole bytes follow the end-of-stream code. `offset` is the position
    /// of the first such byte relative to the start of the DCL stream.
    TrailingData { offset: u64 },
    /// The underlying reader failed.
    Io(io::Error),
}

impl fmt::Display for DclError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DclError::Blast { error, offset } => {
                write!(f, "invalid DCL stream at compressed byte {offset}: {error}")
            }
            DclError::TrailingData { offset } => {
                write!(
                    f,
                    "unexpected data after the DCL end code at compressed byte {offset}"
                )
            }
            DclError::Io(e) => write!(f, "I/O error while reading compressed data: {e}"),
        }
    }
}

impl std::error::Error for DclError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DclError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<DclError> for io::Error {
    fn from(e: DclError) -> io::Error {
        match e {
            DclError::Io(e) => e,
            other => io::Error::new(io::ErrorKind::InvalidData, other),
        }
    }
}

// ---------------------------------------------------------------------------
// Huffman tables (from blast.c)
// ---------------------------------------------------------------------------

const MAXBITS: usize = 13;

/// Bit lengths of literal codes, in blast's compact (repeat, length) form.
const LITLEN: [u8; 98] = [
    11, 124, 8, 7, 28, 7, 188, 13, 76, 4, 10, 8, 12, 10, 12, 10, 8, 23, 8, 9, 7, 6, 7, 8, 7, 6, 55,
    8, 23, 24, 12, 11, 7, 9, 11, 12, 6, 7, 22, 5, 7, 24, 6, 11, 9, 6, 7, 22, 7, 11, 38, 7, 9, 8,
    25, 11, 8, 11, 9, 12, 8, 12, 5, 38, 5, 38, 5, 11, 7, 5, 6, 21, 6, 10, 53, 8, 7, 24, 10, 27, 44,
    253, 253, 253, 252, 252, 252, 13, 12, 45, 12, 45, 12, 61, 12, 45, 44, 173,
];
/// Bit lengths of length codes 0..15.
const LENLEN: [u8; 6] = [2, 35, 36, 53, 38, 23];
/// Bit lengths of distance codes 0..63.
const DISTLEN: [u8; 7] = [2, 20, 53, 230, 247, 151, 248];
/// Base values for length codes.
const BASE: [u32; 16] = [3, 2, 4, 5, 6, 7, 8, 9, 10, 12, 16, 24, 40, 72, 136, 264];
/// Extra bits for length codes.
const EXTRA: [u32; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];
/// Length value that encodes the end of the stream.
const END_LEN: u32 = 519;

/// Canonical Huffman description, as built by blast's `construct()`.
struct Canonical {
    count: [u16; MAXBITS + 1],
    symbol: Vec<u16>,
}

fn construct(rep: &[u8]) -> Canonical {
    let mut length = Vec::with_capacity(256);
    for &r in rep {
        for _ in 0..=(r >> 4) {
            length.push(r & 15);
        }
    }
    let mut count = [0u16; MAXBITS + 1];
    for &len in &length {
        count[len as usize] += 1;
    }
    let mut offs = [0u16; MAXBITS + 1];
    for len in 1..MAXBITS {
        offs[len + 1] = offs[len] + count[len];
    }
    let mut symbol = vec![0u16; length.len()];
    for (sym, &len) in length.iter().enumerate() {
        if len != 0 {
            symbol[offs[len as usize] as usize] = sym as u16;
            offs[len as usize] += 1;
        }
    }
    Canonical { count, symbol }
}

/// blast.c's `decode()`, run on a bit pattern (least significant bit is
/// the first stream bit). Returns `(symbol, code length)`, or `None` if no
/// code completes within `nbits` bits.
fn slow_decode(h: &Canonical, pattern: u32, nbits: u32) -> Option<(u16, u32)> {
    let mut code: i32 = 0;
    let mut first: i32 = 0;
    let mut index: i32 = 0;
    for len in 1..=nbits.min(MAXBITS as u32) {
        code |= (((pattern >> (len - 1)) & 1) ^ 1) as i32; // codes are inverted
        let count = h.count[len as usize] as i32;
        if code < first + count {
            return Some((h.symbol[(index + code - first) as usize], len));
        }
        index += count;
        first += count;
        first <<= 1;
        code <<= 1;
    }
    None
}

/// Lookup table indexed by the next `bits` stream bits. Each entry is
/// `(code length << 8) | symbol`; a code length of 0 marks an invalid code.
struct Lut {
    bits: u32,
    mask: u64,
    table: Vec<u16>,
}

impl Lut {
    fn new(rep: &[u8]) -> Lut {
        let h = construct(rep);
        let bits = (1..=MAXBITS).rev().find(|&l| h.count[l] != 0).unwrap_or(1) as u32;
        let table = (0..1u32 << bits)
            .map(|p| match slow_decode(&h, p, bits) {
                Some((sym, len)) => ((len as u16) << 8) | sym,
                None => 0,
            })
            .collect();
        Lut {
            bits,
            mask: (1u64 << bits) - 1,
            table,
        }
    }
}

struct Tables {
    lit: Lut,
    len: Lut,
    dist: Lut,
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| Tables {
        lit: Lut::new(&LITLEN),
        len: Lut::new(&LENLEN),
        dist: Lut::new(&DISTLEN),
    })
}

// ---------------------------------------------------------------------------
// Bit input
// ---------------------------------------------------------------------------

/// One `read` call on `inner`, retried on [`io::ErrorKind::Interrupted`].
/// A reader that claims to have read more than `buf.len()` bytes (a broken
/// [`Read`] implementation) is reported as [`io::ErrorKind::InvalidData`]
/// instead of causing an out-of-bounds slice later.
pub(crate) fn read_some<R: Read + ?Sized>(inner: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match inner.read(buf) {
            Ok(n) if n > buf.len() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "the underlying reader returned {n} bytes for a {}-byte buffer",
                        buf.len()
                    ),
                ));
            }
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

struct BitSource<R> {
    inner: R,
    buf: Box<[u8]>,
    pos: usize,
    len: usize,
    eof: bool,
    /// Total bytes obtained from `inner`.
    total_in: u64,
    /// Pending bits, least significant first. Bits at and above `bitcnt`
    /// are always zero.
    bitbuf: u64,
    bitcnt: u32,
}

impl<R: Read> BitSource<R> {
    /// Bytes of the stream consumed so far, counting a partially used byte.
    fn consumed(&self) -> u64 {
        self.total_in - (self.len - self.pos) as u64 - (self.bitcnt / 8) as u64
    }

    /// Top up the bit buffer to more than 56 bits, or as far as the input
    /// allows.
    fn refill(&mut self) -> io::Result<()> {
        while self.bitcnt <= 56 {
            let left = self.len - self.pos;
            if left >= 8 {
                let w = u64::from_le_bytes(self.buf[self.pos..self.pos + 8].try_into().unwrap());
                let n = (64 - self.bitcnt) >> 3;
                self.bitbuf |= w << self.bitcnt;
                self.bitcnt += n * 8;
                self.pos += n as usize;
                if self.bitcnt < 64 {
                    self.bitbuf &= (1u64 << self.bitcnt) - 1;
                }
            } else if left > 0 {
                self.bitbuf |= (self.buf[self.pos] as u64) << self.bitcnt;
                self.pos += 1;
                self.bitcnt += 8;
            } else if self.eof {
                return Ok(());
            } else {
                let n = read_some(&mut self.inner, &mut self.buf)?;
                self.pos = 0;
                self.len = n;
                self.total_in += n as u64;
                if n == 0 {
                    self.eof = true;
                }
            }
        }
        Ok(())
    }

    #[inline(always)]
    fn need(&mut self, n: u32) -> Result<(), Fail> {
        if self.bitcnt < n {
            self.refill().map_err(Fail::Io)?;
            if self.bitcnt < n {
                return Err(Fail::Blast(BlastError::OutOfInput));
            }
        }
        Ok(())
    }

    /// Take `n` (0..=32) bits; the caller ensured they are available.
    #[inline(always)]
    fn take(&mut self, n: u32) -> u32 {
        let v = (self.bitbuf & ((1u64 << n) - 1)) as u32;
        self.bitbuf >>= n;
        self.bitcnt -= n;
        v
    }

    #[inline(always)]
    fn decode(&mut self, lut: &Lut) -> Result<u32, Fail> {
        if self.bitcnt < lut.bits {
            self.refill().map_err(Fail::Io)?;
        }
        let e = lut.table[(self.bitbuf & lut.mask) as usize];
        let len = (e >> 8) as u32;
        if len == 0 || len > self.bitcnt {
            return Err(Fail::Blast(if self.bitcnt >= lut.bits {
                BlastError::BadCode
            } else {
                BlastError::OutOfInput
            }));
        }
        self.bitbuf >>= len;
        self.bitcnt -= len;
        Ok((e & 0xFF) as u32)
    }
}

enum Fail {
    Blast(BlastError),
    Io(io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// The two header bytes have not been read yet.
    Start,
    /// Decoding tokens.
    Body { coded: bool, dict: u32 },
    /// The end code was decoded.
    End,
    /// A blast error occurred (sticky).
    Failed(BlastError, u64),
    /// An I/O error occurred (sticky). The original error is kept in
    /// [`DclReader::io_error`] until it has been returned once.
    IoFailed,
}

/// A pull-based PKWare DCL decoder: wraps a reader of compressed bytes and
/// implements [`Read`] for the decompressed bytes.
///
/// Reading returns `Ok(0)` only after the end-of-stream code has been
/// decoded; if the input ends first, the read fails with
/// [`BlastError::OutOfInput`]. Bytes decoded before an error are delivered
/// first, and the error is then returned by every later read. An I/O error
/// from the inner reader is returned as-is (same kind and message) exactly
/// once; later reads return a generic error saying a previous read failed.
///
/// After the end of the stream, call [`DclReader::check_end`] to make sure
/// no whole bytes follow the end code.
pub struct DclReader<R> {
    src: BitSource<R>,
    ring: Box<[u8]>,
    /// Total bytes decoded into the ring.
    wpos: u64,
    /// Total bytes handed to the caller.
    rpos: u64,
    state: State,
    /// The inner reader's error while [`State::IoFailed`], until returned.
    io_error: Option<io::Error>,
}

impl<R: Read> DclReader<R> {
    /// Wrap `inner`, which must be positioned at the first byte of the DCL
    /// stream (the literal-mode byte).
    pub fn new(inner: R) -> Self {
        Self::with_input_buffer(inner, INPUT_BUF)
    }

    /// Like [`DclReader::new`] with a custom input buffer size (at least 1).
    pub fn with_input_buffer(inner: R, size: usize) -> Self {
        DclReader {
            src: BitSource {
                inner,
                buf: vec![0u8; size.max(1)].into_boxed_slice(),
                pos: 0,
                len: 0,
                eof: false,
                total_in: 0,
                bitbuf: 0,
                bitcnt: 0,
            },
            ring: vec![0u8; RING].into_boxed_slice(),
            wpos: 0,
            rpos: 0,
            state: State::Start,
            io_error: None,
        }
    }

    /// Compressed bytes consumed so far (a partially used byte counts).
    pub fn compressed_consumed(&self) -> u64 {
        self.src.consumed()
    }

    /// Compressed bytes read from the inner reader so far (including
    /// read-ahead not yet decoded).
    pub fn compressed_read(&self) -> u64 {
        self.src.total_in
    }

    /// Decompressed bytes delivered to the caller so far.
    pub fn decompressed_delivered(&self) -> u64 {
        self.rpos
    }

    /// True once the end code was decoded and every byte delivered.
    pub fn is_finished(&self) -> bool {
        self.state == State::End && self.rpos == self.wpos
    }

    /// The literal mode (`false` = uncoded, `true` = Huffman-coded) and
    /// dictionary size in bytes, once the stream header has been read.
    pub fn parameters(&self) -> Option<(bool, usize)> {
        match self.state {
            State::Body { coded, dict } => Some((coded, 64usize << dict)),
            _ => None,
        }
    }

    /// Give back the inner reader. Buffered read-ahead is lost.
    pub fn into_inner(self) -> R {
        self.src.inner
    }

    /// Verify that nothing but the unused high bits of the last byte
    /// follows the end code. Must be called after a read returned `Ok(0)`;
    /// before that it returns an error describing the unfinished stream.
    ///
    /// The values of those unused padding bits are intentionally not
    /// checked: they carry no data and encoders leave arbitrary values.
    pub fn check_end(&mut self) -> Result<(), DclError> {
        if !self.is_finished() {
            return Err(self.sticky_error().unwrap_or(DclError::Blast {
                error: BlastError::OutOfInput,
                offset: self.src.consumed(),
            }));
        }
        let offset = self.src.consumed();
        if self.src.bitcnt >= 8 || self.src.pos < self.src.len {
            return Err(DclError::TrailingData { offset });
        }
        if !self.src.eof {
            let mut probe = [0u8; 1];
            let n = read_some(&mut self.src.inner, &mut probe).map_err(DclError::Io)?;
            if n > 0 {
                self.src.total_in += n as u64;
                return Err(DclError::TrailingData { offset });
            }
            self.src.eof = true;
        }
        Ok(())
    }

    /// The sticky error. For an I/O failure the original error is handed
    /// out the first time, a generic one afterwards.
    fn sticky_error(&mut self) -> Option<DclError> {
        match self.state {
            State::Failed(error, offset) => Some(DclError::Blast { error, offset }),
            State::IoFailed => Some(DclError::Io(self.io_error.take().unwrap_or_else(|| {
                io::Error::other("a previous read of the compressed input failed")
            }))),
            _ => None,
        }
    }

    /// Read decompressed bytes into `out`. Same contract as [`Read::read`],
    /// with a typed error.
    pub fn read_decoded(&mut self, out: &mut [u8]) -> Result<usize, DclError> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            let avail = (self.wpos - self.rpos) as usize;
            if avail > 0 {
                let n = avail.min(out.len());
                let start = self.rpos as usize & RING_MASK;
                let first = n.min(RING - start);
                out[..first].copy_from_slice(&self.ring[start..start + first]);
                if first < n {
                    out[first..n].copy_from_slice(&self.ring[..n - first]);
                }
                self.rpos += n as u64;
                return Ok(n);
            }
            match self.state {
                State::End => return Ok(0),
                State::Failed(..) | State::IoFailed => return Err(self.sticky_error().unwrap()),
                State::Start | State::Body { .. } => {
                    if let Err(f) = self.fill() {
                        match f {
                            Fail::Blast(error) => {
                                self.state = State::Failed(error, self.src.consumed());
                            }
                            Fail::Io(e) => {
                                self.state = State::IoFailed;
                                if self.wpos == self.rpos {
                                    return Err(DclError::Io(e));
                                }
                                // Deliver the pending bytes first; the
                                // error is returned once they are drained.
                                self.io_error = Some(e);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Decode tokens until the ring is nearly full or the stream ends.
    fn fill(&mut self) -> Result<(), Fail> {
        let (coded, dict) = match self.state {
            State::Body { coded, dict } => (coded, dict),
            State::Start => {
                self.src.need(8)?;
                let lit = self.src.take(8);
                if lit > 1 {
                    return Err(Fail::Blast(BlastError::BadLiteralFlag));
                }
                self.src.need(8)?;
                let dict = self.src.take(8);
                if !(4..=6).contains(&dict) {
                    return Err(Fail::Blast(BlastError::BadDictSize));
                }
                self.state = State::Body {
                    coded: lit == 1,
                    dict,
                };
                (lit == 1, dict)
            }
            _ => return Ok(()),
        };
        let t = tables();
        let ring = &mut self.ring[..];
        let src = &mut self.src;
        let mut wpos = self.wpos;
        let limit = self.rpos + (RING - MAX_MATCH) as u64;
        let result = loop {
            if wpos > limit {
                break Ok(());
            }
            if src.bitcnt < 32
                && let Err(e) = src.refill()
            {
                break Err(Fail::Io(e));
            }
            if let Err(e) = src.need(1) {
                break Err(e);
            }
            if src.take(1) == 0 {
                // Literal.
                let sym = if coded {
                    match src.decode(&t.lit) {
                        Ok(s) => s,
                        Err(e) => break Err(e),
                    }
                } else {
                    if let Err(e) = src.need(8) {
                        break Err(e);
                    }
                    src.take(8)
                };
                ring[wpos as usize & RING_MASK] = sym as u8;
                wpos += 1;
                continue;
            }
            // Match (or end code).
            let sym = match src.decode(&t.len) {
                Ok(s) => s as usize,
                Err(e) => break Err(e),
            };
            if let Err(e) = src.need(EXTRA[sym]) {
                break Err(e);
            }
            let len = BASE[sym] + src.take(EXTRA[sym]);
            if len == END_LEN {
                self.state = State::End;
                break Ok(());
            }
            let shift = if len == 2 { 2 } else { dict };
            let dsym = match src.decode(&t.dist) {
                Ok(s) => s,
                Err(e) => break Err(e),
            };
            if let Err(e) = src.need(shift) {
                break Err(e);
            }
            let dist = ((dsym << shift) + src.take(shift) + 1) as usize;
            if dist as u64 > wpos {
                break Err(Fail::Blast(BlastError::DistanceTooFar));
            }
            let len = len as usize;
            let from = (wpos as usize).wrapping_sub(dist) & RING_MASK;
            let to = wpos as usize & RING_MASK;
            if dist >= len && from + len <= RING && to + len <= RING {
                ring.copy_within(from..from + len, to);
            } else {
                for i in 0..len {
                    ring[(to + i) & RING_MASK] = ring[(from + i) & RING_MASK];
                }
            }
            wpos += len as u64;
        };
        self.wpos = wpos;
        result
    }
}

impl<R: Read> Read for DclReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_decoded(buf).map_err(io::Error::from)
    }
}

impl<R> fmt::Debug for DclReader<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DclReader")
            .field("state", &self.state)
            .field("decoded", &self.wpos)
            .field("delivered", &self.rpos)
            .finish_non_exhaustive()
    }
}

/// Decompress a whole in-memory DCL stream (convenience for tests and
/// small inputs). Trailing bytes after the end code are an error.
pub fn decompress_all(input: &[u8]) -> Result<Vec<u8>, DclError> {
    let mut r = DclReader::new(input);
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = r.read_decoded(&mut buf)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    r.check_end()?;
    Ok(out)
}

/// A minimal DCL *encoder* for tests and fixtures (not optimized, not a
/// compressor: the caller chooses the tokens). Hidden from the docs.
#[doc(hidden)]
pub mod testing {
    use super::{BASE, EXTRA, Lut, tables};

    /// One token of a DCL stream.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Token {
        /// A literal byte.
        Literal(u8),
        /// Copy `len` (2..=518) bytes from `dist` bytes back.
        Match { len: u16, dist: u16 },
    }

    struct Bits {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }

    impl Bits {
        fn put(&mut self, v: u32, n: u32) {
            self.acc |= (v as u64) << self.n;
            self.n += n;
            while self.n >= 8 {
                self.out.push(self.acc as u8);
                self.acc >>= 8;
                self.n -= 8;
            }
        }
    }

    fn code(lut: &Lut, sym: u32) -> (u32, u32) {
        for (p, &e) in lut.table.iter().enumerate() {
            if (e & 0xFF) as u32 == sym && e >> 8 != 0 {
                let len = (e >> 8) as u32;
                return ((p as u32) & ((1 << len) - 1), len);
            }
        }
        panic!("symbol {sym} has no code");
    }

    /// Maximum match distance for a match of `len` with dictionary code `dict`.
    pub fn max_dist(len: u16, dict: u32) -> u16 {
        let shift = if len == 2 { 2 } else { dict };
        (64u32 << shift) as u16
    }

    /// Encode `tokens` with literal mode `coded` and dictionary code `dict`
    /// (4, 5 or 6), followed by the end code.
    pub fn encode(tokens: &[Token], coded: bool, dict: u32) -> Vec<u8> {
        let t = tables();
        let mut b = Bits {
            out: vec![coded as u8, dict as u8],
            acc: 0,
            n: 0,
        };
        for &tok in tokens {
            match tok {
                Token::Literal(x) => {
                    b.put(0, 1);
                    if coded {
                        let (c, n) = code(&t.lit, x as u32);
                        b.put(c, n);
                    } else {
                        b.put(x as u32, 8);
                    }
                }
                Token::Match { len, dist } => {
                    let len = len as u32;
                    assert!((2..=518).contains(&len));
                    let sym = (0..16)
                        .rev()
                        .find(|&s| BASE[s] <= len && len < BASE[s] + (1 << EXTRA[s]))
                        .unwrap();
                    b.put(1, 1);
                    let (c, n) = code(&t.len, sym as u32);
                    b.put(c, n);
                    b.put(len - BASE[sym], EXTRA[sym]);
                    let shift = if len == 2 { 2 } else { dict };
                    let d = dist as u32 - 1;
                    let (c, n) = code(&t.dist, d >> shift);
                    b.put(c, n);
                    b.put(d & ((1 << shift) - 1), shift);
                }
            }
        }
        // End code: length symbol 15 with all 8 extra bits set (519).
        b.put(1, 1);
        let (c, n) = code(&t.len, 15);
        b.put(c, n);
        b.put(0xFF, 8);
        if b.n > 0 {
            b.out.push(b.acc as u8);
        }
        b.out
    }

    /// Encode `data` as literals only (the DATASUS layout: uncoded, 4 KiB).
    pub fn encode_literals(data: &[u8]) -> Vec<u8> {
        let tokens: Vec<Token> = data.iter().map(|&x| Token::Literal(x)).collect();
        encode(&tokens, false, 6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADLER: [u8; 8] = [0x00, 0x04, 0x82, 0x24, 0x25, 0x8f, 0x80, 0x7f];

    fn blast_err(input: &[u8]) -> BlastError {
        match decompress_all(input) {
            Err(DclError::Blast { error, .. }) => error,
            other => panic!("expected blast error, got {other:?}"),
        }
    }

    #[test]
    fn adler_example() {
        assert_eq!(decompress_all(&ADLER).unwrap(), b"AIAIAIAIAIAIA");
    }

    #[test]
    fn tables_are_complete_so_bad_code_is_unreachable() {
        let t = tables();
        for (name, lut) in [("lit", &t.lit), ("len", &t.len), ("dist", &t.dist)] {
            assert!(
                lut.table.iter().all(|&e| e >> 8 != 0),
                "{name} table has gaps"
            );
        }
        assert_eq!((t.lit.bits, t.len.bits, t.dist.bits), (13, 7, 8));
        assert_eq!(BlastError::BadCode.code(), -9);
    }

    #[test]
    fn error_paths() {
        assert_eq!(blast_err(&[]), BlastError::OutOfInput);
        assert_eq!(blast_err(&[0]), BlastError::OutOfInput);
        assert_eq!(blast_err(&[2, 4]), BlastError::BadLiteralFlag);
        assert_eq!(blast_err(&[0, 3]), BlastError::BadDictSize);
        assert_eq!(blast_err(&[0, 7]), BlastError::BadDictSize);
        // Every proper prefix of the example runs out of input.
        for cut in 0..ADLER.len() {
            assert_eq!(
                blast_err(&ADLER[..cut]),
                BlastError::OutOfInput,
                "cut {cut}"
            );
        }
        // Byte 2 bit 0 is the first token's tag; setting it makes the
        // stream start with a match before any output exists.
        let mut bad = ADLER;
        bad[2] |= 1;
        assert_eq!(blast_err(&bad), BlastError::DistanceTooFar);
        // Trailing bytes after the end code.
        let mut trailing = ADLER.to_vec();
        trailing.push(0);
        assert!(matches!(
            decompress_all(&trailing),
            Err(DclError::TrailingData { offset: 8 })
        ));
        assert_eq!(BlastError::OutOfInput.code(), 2);
        assert_eq!(BlastError::BadLiteralFlag.code(), -1);
        assert_eq!(BlastError::BadDictSize.code(), -2);
        assert_eq!(BlastError::DistanceTooFar.code(), -3);
    }

    #[test]
    fn errors_are_sticky() {
        let mut r = DclReader::new(&[2u8, 4][..]);
        let mut b = [0u8; 4];
        assert!(r.read_decoded(&mut b).is_err());
        assert!(r.read_decoded(&mut b).is_err());
        assert!(r.check_end().is_err());
    }

    /// Reader that returns one byte per call.
    struct OneByte<'a>(&'a [u8]);
    impl Read for OneByte<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() || buf.is_empty() {
                return Ok(0);
            }
            buf[0] = self.0[0];
            self.0 = &self.0[1..];
            Ok(1)
        }
    }

    #[test]
    fn tiny_reads() {
        let mut r = DclReader::with_input_buffer(OneByte(&ADLER), 1);
        let mut out = Vec::new();
        let mut b = [0u8; 1];
        while r.read_decoded(&mut b).unwrap() == 1 {
            out.push(b[0]);
        }
        r.check_end().unwrap();
        assert_eq!(out, b"AIAIAIAIAIAIA");
        assert_eq!(r.compressed_consumed(), 8);
    }

    /// Tiny deterministic PRNG (xorshift64*).
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn random_stream(rng: &mut Rng, coded: bool, dict: u32, ntok: usize) -> (Vec<u8>, Vec<u8>) {
        use testing::{Token, encode, max_dist};
        let mut out = Vec::new();
        let mut toks = Vec::new();
        for _ in 0..ntok {
            if out.is_empty() || rng.below(3) == 0 {
                let x = if rng.below(2) == 0 {
                    b'A' + rng.below(4) as u8
                } else {
                    rng.next() as u8
                };
                toks.push(Token::Literal(x));
                out.push(x);
            } else {
                let len = match rng.below(4) {
                    0 => 2,
                    1 => 3 + rng.below(10) as u16,
                    2 => 2 + rng.below(517) as u16,
                    _ => 518,
                };
                let maxd = (max_dist(len, dict) as usize).min(out.len()) as u64;
                let dist = 1 + rng.below(maxd) as u16;
                for _ in 0..len {
                    let b = out[out.len() - dist as usize];
                    out.push(b);
                }
                toks.push(Token::Match { len, dist });
            }
        }
        (encode(&toks, coded, dict), out)
    }

    #[test]
    fn random_round_trips_all_modes() {
        let mut rng = Rng(0x1234_5678_9ABC_DEF1);
        for coded in [false, true] {
            for dict in 4..=6 {
                for ntok in [0, 1, 5, 100, 5000, 40_000] {
                    let (comp, expected) = random_stream(&mut rng, coded, dict, ntok);
                    assert_eq!(
                        decompress_all(&comp).unwrap(),
                        expected,
                        "coded={coded} dict={dict} n={ntok}"
                    );
                }
            }
        }
    }

    #[test]
    fn random_round_trips_tiny_io() {
        let mut rng = Rng(42);
        for (coded, dict) in [(false, 6), (true, 4), (true, 5)] {
            let (comp, expected) = random_stream(&mut rng, coded, dict, 3000);
            let mut r = DclReader::with_input_buffer(OneByte(&comp), 1);
            let mut out = Vec::new();
            let mut b = [0u8; 1];
            while r.read_decoded(&mut b).unwrap() == 1 {
                out.push(b[0]);
            }
            r.check_end().unwrap();
            assert_eq!(out, expected);
            assert_eq!(r.compressed_consumed(), comp.len() as u64);
            // Odd-sized reads through std::io::Read.
            let mut r = DclReader::new(&comp[..]);
            let mut out = Vec::new();
            let mut b = [0u8; 7];
            loop {
                let n = Read::read(&mut r, &mut b).unwrap();
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&b[..n]);
            }
            assert_eq!(out, expected);
        }
    }

    #[test]
    fn truncations_and_corruptions_never_succeed_silently() {
        let mut rng = Rng(7);
        let (comp, expected) = random_stream(&mut rng, false, 6, 2000);
        for cut in (0..comp.len()).step_by(97).chain([comp.len() - 1]) {
            assert!(decompress_all(&comp[..cut]).is_err(), "cut {cut}");
        }
        // Flipped bits must never panic. (They can legitimately decode to
        // the same bytes, e.g. a different distance into a run; catching
        // that is the CRC's job, tested at the container level.)
        let mut changed = 0;
        for i in (2..comp.len()).step_by(53) {
            let mut c = comp.clone();
            c[i] ^= 0x10;
            match decompress_all(&c) {
                Ok(out) if out == expected => {}
                _ => changed += 1,
            }
        }
        assert!(changed > 0);
    }

    /// Hands out `data` and then fails with a distinctive error.
    struct FailAfter<'a>(&'a [u8]);
    impl Read for FailAfter<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() {
                return Err(io::Error::new(io::ErrorKind::ConnectionReset, "boom-7f3a"));
            }
            let n = buf.len().min(self.0.len());
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn io_error_with_pending_bytes_is_returned_once_after_them() {
        let data: Vec<u8> = (0..30_000u32).map(|i| (i % 251) as u8).collect();
        let comp = testing::encode_literals(&data);
        let mut r = DclReader::new(FailAfter(&comp[..20_000]));
        let mut out = Vec::new();
        let mut b = [0u8; 4096];
        let err = loop {
            match r.read_decoded(&mut b) {
                Ok(0) => panic!("unexpected end"),
                Ok(n) => out.extend_from_slice(&b[..n]),
                Err(e) => break e,
            }
        };
        // Everything decodable from the delivered input came out first.
        assert!(out.len() > 17_000, "{}", out.len());
        assert_eq!(out, data[..out.len()]);
        match err {
            DclError::Io(e) => {
                assert_eq!(e.kind(), io::ErrorKind::ConnectionReset);
                assert_eq!(e.to_string(), "boom-7f3a");
            }
            other => panic!("{other:?}"),
        }
        // Later calls: a generic error, never the original twice.
        match r.read_decoded(&mut b) {
            Err(DclError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::Other),
            other => panic!("{other:?}"),
        }
        assert!(r.check_end().is_err());
    }

    #[test]
    fn io_error_without_pending_bytes_is_returned_directly() {
        let mut r = DclReader::new(FailAfter(&[]));
        match r.read_decoded(&mut [0u8; 8]) {
            Err(DclError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::ConnectionReset),
            other => panic!("{other:?}"),
        }
    }

    /// Claims to have read one byte more than the buffer holds.
    struct Liar;
    impl Read for Liar {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            Ok(buf.len() + 1)
        }
    }

    #[test]
    fn lying_inner_reader_is_an_error_not_a_panic() {
        let mut r = DclReader::new(Liar);
        match r.read_decoded(&mut [0u8; 8]) {
            Err(DclError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::InvalidData),
            other => panic!("{other:?}"),
        }
        // Lie after the valid stream was handed out (the read-ahead past
        // the end code, or check_end's probe, sees the lie).
        struct ThenLie<'a>(&'a [u8]);
        impl Read for ThenLie<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0.is_empty() {
                    return Ok(buf.len() + 1);
                }
                let n = buf.len().min(self.0.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        for size in 1..=ADLER.len() + 1 {
            let mut r = DclReader::with_input_buffer(ThenLie(&ADLER), size);
            let mut b = [0u8; 64];
            let err = loop {
                match r.read_decoded(&mut b) {
                    Ok(0) => break r.check_end().unwrap_err(),
                    Ok(_) => {}
                    Err(e) => break e,
                }
            };
            match err {
                DclError::Io(e) => assert_eq!(e.kind(), io::ErrorKind::InvalidData),
                other => panic!("size {size}: {other:?}"),
            }
        }
    }

    #[test]
    fn encoder_reproduces_adler_example() {
        use testing::{Token, encode};
        let toks = [
            Token::Literal(b'A'),
            Token::Literal(b'I'),
            Token::Match { len: 11, dist: 2 },
        ];
        assert_eq!(
            decompress_all(&encode(&toks, true, 4)).unwrap(),
            b"AIAIAIAIAIAIA"
        );
    }
}

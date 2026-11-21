//! CRC-32/ISO-HDLC (the IEEE CRC used by zlib, PNG, gzip, ...).
//!
//! Reflected polynomial `0xEDB88320`, initial register `0xFFFFFFFF`, final
//! XOR `0xFFFFFFFF`. Check value for `b"123456789"` is `0xCBF43926`.
//!
//! Table-driven, slicing-by-8: eight 256-entry tables (8 KiB) built at
//! compile time.

const POLY: u32 = 0xEDB8_8320;

const fn make_tables() -> [[u32; 256]; 8] {
    let mut t = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut i = 0;
    while i < 256 {
        let mut s = 1;
        while s < 8 {
            let prev = t[s - 1][i];
            t[s][i] = (prev >> 8) ^ t[0][(prev & 0xFF) as usize];
            s += 1;
        }
        i += 1;
    }
    t
}

static TABLES: [[u32; 256]; 8] = make_tables();

/// Incremental CRC-32 hasher.
///
/// ```
/// let mut c = read_dbc_format::Crc32::new();
/// c.update(b"1234");
/// c.update(b"56789");
/// assert_eq!(c.finalize(), 0xCBF4_3926);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Crc32 {
    reg: u32,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// A fresh hasher (register `0xFFFFFFFF`).
    pub const fn new() -> Self {
        Crc32 { reg: 0xFFFF_FFFF }
    }

    /// Feed `data` into the running CRC.
    pub fn update(&mut self, data: &[u8]) {
        let t = &TABLES;
        let mut crc = self.reg;
        let (chunks, rest) = data.as_chunks::<8>();
        for c in chunks {
            let lo = crc ^ u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            let hi = u32::from_le_bytes([c[4], c[5], c[6], c[7]]);
            crc = t[7][(lo & 0xFF) as usize]
                ^ t[6][((lo >> 8) & 0xFF) as usize]
                ^ t[5][((lo >> 16) & 0xFF) as usize]
                ^ t[4][(lo >> 24) as usize]
                ^ t[3][(hi & 0xFF) as usize]
                ^ t[2][((hi >> 8) & 0xFF) as usize]
                ^ t[1][((hi >> 16) & 0xFF) as usize]
                ^ t[0][(hi >> 24) as usize];
        }
        for &b in rest {
            crc = (crc >> 8) ^ t[0][((crc ^ b as u32) & 0xFF) as usize];
        }
        self.reg = crc;
    }

    /// The CRC of everything fed so far (does not reset the hasher).
    pub const fn finalize(&self) -> u32 {
        !self.reg
    }
}

/// One-shot CRC-32 of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitwise(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ POLY
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn matches_bitwise_and_is_incremental() {
        let data: Vec<u8> = (0..1000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let whole = crc32(&data);
        assert_eq!(whole, bitwise(&data));
        for split in [0, 1, 7, 8, 9, 63, 500, 999, 1000] {
            let mut c = Crc32::new();
            c.update(&data[..split]);
            c.update(&data[split..]);
            assert_eq!(c.finalize(), whole, "split {split}");
        }
    }
}

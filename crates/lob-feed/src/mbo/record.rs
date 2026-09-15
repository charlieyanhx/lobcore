//! Databento DBN file header and the MBO record (`MboMsg`) byte layout, decoded with
//! `from_le_bytes` on fixed-size sub-slices (no unaligned pointer reads, no `unsafe`).
//!
//! File: `b"DBN"` | version u8 | metadata length u32 LE | metadata | records. Version-3 metadata
//! starts with the dataset code (16 bytes, NUL padded), the schema u16 and the start timestamp
//! u64; only those three are exposed. Record: `RecordHeader` 16 bytes {length u8 in 32-bit words,
//! rtype u8 (MBO = 0xA0), publisher_id u16, instrument_id u32, ts_event u64 ns UNIX} then
//! order_id u64 | price i64 (1e-9 fixed, `UNDEF_PRICE` = i64::MAX) | size u32 | flags u8 |
//! channel_id u8 | action u8 | side u8 | ts_recv u64 | ts_in_delta i32 | sequence u32 = 56 bytes.
//! Layout verified against databento/dbn `rust/dbn/src/record.rs` and the two vendored stubs.
//!
//! This is a format test surface only: no book-apply rules (`R` clear, `F_SNAPSHOT`, `F_TOB`,
//! `M` on an unknown id) ship in v0.1.

/// Size of an MBO record in bytes.
pub const MBO_LEN: usize = 56;
/// `RecordHeader.rtype` of an MBO record.
pub const RTYPE_MBO: u8 = 0xA0;
/// Sentinel for an undefined price.
pub const UNDEF_PRICE: i64 = i64::MAX;
/// Sentinel for an undefined order size.
pub const UNDEF_ORDER_SIZE: u32 = u32::MAX;
/// Last record of the venue event for this instrument.
pub const F_LAST: u8 = 128;
/// Top-of-book record, not an order.
pub const F_TOB: u8 = 64;
/// From a replay / snapshot server.
pub const F_SNAPSHOT: u8 = 32;
/// Aggregated MBP record.
pub const F_MBP: u8 = 16;
/// `ts_recv` is unreliable.
pub const F_BAD_TS_RECV: u8 = 8;
/// An unrecoverable gap precedes this record.
pub const F_MAYBE_BAD_BOOK: u8 = 4;

/// Layout errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbnError {
    /// Fewer than 8 bytes or the first three are not `DBN`.
    BadMagic,
    /// The metadata length runs past the end of the input.
    Metadata { len: u32, have: usize },
    /// A record's length byte says more bytes than remain, or zero.
    Record { at: usize, len: usize, have: usize },
}

impl std::fmt::Display for DbnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbnError::BadMagic => write!(f, "not a DBN file"),
            DbnError::Metadata { len, have } => {
                write!(f, "metadata length {len} exceeds the {have} bytes present")
            }
            DbnError::Record { at, len, have } => {
                write!(f, "record at {at}: length {len} bytes, {have} present")
            }
        }
    }
}

impl std::error::Error for DbnError {}

/// The 8-byte file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbnHeader {
    /// DBN version byte.
    pub version: u8,
    /// Metadata length in bytes (follows the header).
    pub metadata_len: u32,
}

/// The first three fields of version-3 metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbnMetadata {
    /// Dataset code, NUL padded (e.g. `XNAS.ITCH`).
    pub dataset: [u8; 16],
    /// Schema id (MBO = 0).
    pub schema: u16,
    /// Start timestamp, ns UNIX.
    pub start: u64,
}

impl DbnMetadata {
    /// Dataset as a string (up to the first NUL).
    pub fn dataset_str(&self) -> &str {
        let n = self.dataset.iter().position(|&b| b == 0).unwrap_or(16);
        std::str::from_utf8(&self.dataset[..n]).unwrap_or("")
    }
}

impl DbnHeader {
    /// Parse the header; returns it with the metadata bytes and the record bytes.
    pub fn parse(bytes: &[u8]) -> Result<(DbnHeader, &[u8], &[u8]), DbnError> {
        if bytes.len() < 8 || &bytes[..3] != b"DBN" {
            return Err(DbnError::BadMagic);
        }
        let version = bytes[3];
        let metadata_len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let end = 8 + metadata_len as usize;
        if end > bytes.len() {
            return Err(DbnError::Metadata {
                len: metadata_len,
                have: bytes.len() - 8,
            });
        }
        Ok((
            DbnHeader {
                version,
                metadata_len,
            },
            &bytes[8..end],
            &bytes[end..],
        ))
    }

    /// The first three metadata fields, if the metadata is long enough.
    pub fn metadata(meta: &[u8]) -> Option<DbnMetadata> {
        if meta.len() < 26 {
            return None;
        }
        let mut dataset = [0u8; 16];
        dataset.copy_from_slice(&meta[..16]);
        Some(DbnMetadata {
            dataset,
            schema: u16::from_le_bytes([meta[16], meta[17]]),
            start: le64(meta, 18),
        })
    }
}

#[inline]
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

#[inline]
fn le64(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

/// One decoded MBO record (all 15 fields, in file order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MboMsg {
    /// Record length in 32-bit words (14 for MBO).
    pub length: u8,
    /// Record type (0xA0).
    pub rtype: u8,
    /// Publisher id.
    pub publisher_id: u16,
    /// Instrument id.
    pub instrument_id: u32,
    /// Matching-engine received timestamp, ns UNIX.
    pub ts_event: u64,
    /// Venue order id.
    pub order_id: u64,
    /// Price in 1e-9 units; `UNDEF_PRICE` when absent.
    pub price: i64,
    /// Order size; `UNDEF_ORDER_SIZE` when absent.
    pub size: u32,
    /// Bit flags (`F_*`).
    pub flags: u8,
    /// Channel id.
    pub channel_id: u8,
    /// `A` add, `C` cancel, `M` modify, `R` clear, `T` trade, `F` fill, `N` none.
    pub action: u8,
    /// `A` ask, `B` bid, `N` none.
    pub side: u8,
    /// Capture-server receive timestamp, ns UNIX.
    pub ts_recv: u64,
    /// `ts_recv - matching-engine sending timestamp`, ns.
    pub ts_in_delta: i32,
    /// Venue sequence number.
    pub sequence: u32,
}

impl MboMsg {
    /// Decode 56 little-endian bytes.
    pub fn decode(b: &[u8; MBO_LEN]) -> MboMsg {
        MboMsg {
            length: b[0],
            rtype: b[1],
            publisher_id: u16::from_le_bytes([b[2], b[3]]),
            instrument_id: le32(b, 4),
            ts_event: le64(b, 8),
            order_id: le64(b, 16),
            price: le64(b, 24) as i64,
            size: le32(b, 32),
            flags: b[36],
            channel_id: b[37],
            action: b[38],
            side: b[39],
            ts_recv: le64(b, 40),
            ts_in_delta: le32(b, 48) as i32,
            sequence: le32(b, 52),
        }
    }

    /// Encode back to the 56-byte layout (round-trip test surface).
    pub fn encode(&self) -> [u8; MBO_LEN] {
        let mut b = [0u8; MBO_LEN];
        b[0] = self.length;
        b[1] = self.rtype;
        b[2..4].copy_from_slice(&self.publisher_id.to_le_bytes());
        b[4..8].copy_from_slice(&self.instrument_id.to_le_bytes());
        b[8..16].copy_from_slice(&self.ts_event.to_le_bytes());
        b[16..24].copy_from_slice(&self.order_id.to_le_bytes());
        b[24..32].copy_from_slice(&self.price.to_le_bytes());
        b[32..36].copy_from_slice(&self.size.to_le_bytes());
        b[36] = self.flags;
        b[37] = self.channel_id;
        b[38] = self.action;
        b[39] = self.side;
        b[40..48].copy_from_slice(&self.ts_recv.to_le_bytes());
        b[48..52].copy_from_slice(&self.ts_in_delta.to_le_bytes());
        b[52..56].copy_from_slice(&self.sequence.to_le_bytes());
        b
    }

    /// Price as a float (1e-9 units); `None` when undefined.
    pub fn price_f64(&self) -> Option<f64> {
        (self.price != UNDEF_PRICE).then(|| self.price as f64 / 1e9)
    }
}

/// One record of the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Record {
    /// An MBO record.
    Mbo(MboMsg),
    /// Any other record type (skipped by its length byte).
    Other {
        /// Record type byte.
        rtype: u8,
        /// Length in bytes.
        len: usize,
    },
}

/// Iterate the records after the metadata.
#[derive(Debug, Clone)]
pub struct Records<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Iterator for Records<'_> {
    type Item = Result<Record, DbnError>;

    fn next(&mut self) -> Option<Self::Item> {
        let rest = &self.bytes[self.pos..];
        if rest.is_empty() {
            return None;
        }
        let len = rest[0] as usize * 4;
        if len == 0 || len > rest.len() {
            self.pos = self.bytes.len();
            return Some(Err(DbnError::Record {
                at: self.pos,
                len,
                have: rest.len(),
            }));
        }
        let at = self.pos;
        self.pos += len;
        let rec = &self.bytes[at..at + len];
        if rec[1] == RTYPE_MBO && len == MBO_LEN {
            let mut a = [0u8; MBO_LEN];
            a.copy_from_slice(rec);
            Some(Ok(Record::Mbo(MboMsg::decode(&a))))
        } else {
            Some(Ok(Record::Other { rtype: rec[1], len }))
        }
    }
}

/// Records of a complete DBN file (header + metadata + records).
pub fn records(file: &[u8]) -> Result<(DbnHeader, Option<DbnMetadata>, Records<'_>), DbnError> {
    let (h, meta, recs) = DbnHeader::parse(file)?;
    Ok((
        h,
        DbnHeader::metadata(meta),
        Records {
            bytes: recs,
            pos: 0,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_56_bytes_and_round_trips() {
        let m = MboMsg {
            length: 14,
            rtype: RTYPE_MBO,
            publisher_id: 2,
            instrument_id: 6155,
            ts_event: 1_609_146_088_297_098_637,
            order_id: 196_851,
            price: 453_120_000_000, // 453.12
            size: 2,
            flags: 0x82,
            channel_id: 0,
            action: b'A',
            side: b'B',
            ts_recv: 1_609_146_088_297_109_209,
            ts_in_delta: 10_572,
            sequence: 289_000,
        };
        let b = m.encode();
        assert_eq!(b.len(), MBO_LEN);
        assert_eq!(&b[24..32], &453_120_000_000i64.to_le_bytes());
        assert_eq!(MboMsg::decode(&b), m);
        assert_eq!(m.price_f64(), Some(453.12));
        let u = MboMsg {
            price: UNDEF_PRICE,
            ..m
        };
        assert_eq!(u.price_f64(), None);
        assert_eq!(UNDEF_PRICE, 9_223_372_036_854_775_807);
    }

    #[test]
    fn header_errors() {
        assert_eq!(
            DbnHeader::parse(b"DBX\x03\x00\x00\x00\x00"),
            Err(DbnError::BadMagic)
        );
        assert_eq!(DbnHeader::parse(b"DBN"), Err(DbnError::BadMagic));
        assert_eq!(
            DbnHeader::parse(b"DBN\x03\x09\x00\x00\x00\x01"),
            Err(DbnError::Metadata { len: 9, have: 1 })
        );
        let ok = DbnHeader::parse(b"DBN\x03\x01\x00\x00\x00\x07\xaa").unwrap();
        assert_eq!(
            ok,
            (
                DbnHeader {
                    version: 3,
                    metadata_len: 1
                },
                &b"\x07"[..],
                &b"\xaa"[..]
            )
        );
        let mut r = Records {
            bytes: &[3, 0xA0, 1],
            pos: 0,
        };
        assert!(matches!(
            r.next(),
            Some(Err(DbnError::Record { len: 12, .. }))
        ));
        assert!(r.next().is_none());
    }
}

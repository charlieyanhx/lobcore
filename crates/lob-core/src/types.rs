//! Frozen public types: sides, ids, prices, quantities, the 34-byte event record and the
//! event log.
//!
//! Units and sign conventions
//! - `Px` is an `i32` in 1e-4 price units (ITCH Price(4)); it is always positive in the public
//!   API. The sign-as-side trick (`+px` bid, `-px` ask) is internal to the order slot.
//! - `Qty` is a `u32`; a level summing past `u32::MAX` is a data error and is rejected.
//! - `Event` fields are widened (`i64` price, `u64` qty) so that the 34-byte record is the same
//!   for every producer (book, matcher, simulator).
//!
//! Event-log hash contract (docs/DESIGN.md sec 8): record = 34 bytes little-endian,
//! `u8 type | u64 a | u64 b | i64 price (1e-4) | u64 qty | u8 side`; type codes add=1 cancel=2
//! exec=3 delete=4 replace=5 modify=6 unknown=7 (b = kind code) reject=8 trade=9 stp_cancel=10
//! ioc_cancel=11 fok_reject=12; side byte 0 = bid, 1 = ask; sha256 over a leading format-version
//! byte (=1) then every record in emission order.
//!
//! Field conventions per kind as emitted by the books in this crate (documented so the Python
//! reference can reproduce them):
//! - add:     a = id,      b = 0,              px = order px, qty = order qty,    side = order side
//! - cancel:  a = id,      b = remaining after, px = order px, qty = qty removed,  side = order side
//! - exec:    a = id,      b = remaining after, px = order px, qty = qty executed, side = order side
//! - delete:  a = id,      b = 0,              px = order px, qty = qty removed,  side = order side
//! - replace: a = old id,  b = new id,         px = new px,   qty = new qty,      side = carried side
//! - unknown: a = id,      b = kind code of the attempted op, px = 0, qty = 0,    side = 0
//! - reject:  a = id,      b = kind code of the rejected op,  px = 0, qty = requested qty, side = 0
//!   (`BadQty` / `BadPrice` rejects carry a = 0, b = 0 because the error carries no id).

use sha2::{Digest, Sha256};

/// Book side. The event-record side byte is `Bid = 0`, `Ask = 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
    Bid,
    Ask,
}

impl Side {
    /// Event-record side byte.
    #[inline]
    pub const fn code(self) -> u8 {
        match self {
            Side::Bid => 0,
            Side::Ask => 1,
        }
    }

    /// Inverse of [`Side::code`]; any byte other than 0 or 1 is `None`.
    #[inline]
    pub const fn from_code(b: u8) -> Option<Side> {
        match b {
            0 => Some(Side::Bid),
            1 => Some(Side::Ask),
            _ => None,
        }
    }

    #[inline]
    pub const fn opposite(self) -> Side {
        match self {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
        }
    }
}

/// Venue order reference (ITCH order reference number, MBO order_id).
pub type OrderId = u64;
/// Quantity in shares / lots.
pub type Qty = u32;
/// Price in 1e-4 units; positive in the public API.
pub type Px = i32;

/// Event type codes of the hash contract (1..=12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EventKind {
    Add = 1,
    Cancel = 2,
    Exec = 3,
    Delete = 4,
    Replace = 5,
    Modify = 6,
    Unknown = 7,
    Reject = 8,
    Trade = 9,
    StpCancel = 10,
    IocCancel = 11,
    FokReject = 12,
}

impl EventKind {
    /// The one-byte type code hashed into the record.
    #[inline]
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Inverse of [`EventKind::code`].
    pub const fn from_code(c: u8) -> Option<EventKind> {
        Some(match c {
            1 => EventKind::Add,
            2 => EventKind::Cancel,
            3 => EventKind::Exec,
            4 => EventKind::Delete,
            5 => EventKind::Replace,
            6 => EventKind::Modify,
            7 => EventKind::Unknown,
            8 => EventKind::Reject,
            9 => EventKind::Trade,
            10 => EventKind::StpCancel,
            11 => EventKind::IocCancel,
            12 => EventKind::FokReject,
            _ => return None,
        })
    }
}

/// Size in bytes of one encoded event record.
pub const EVENT_RECORD_LEN: usize = 34;
/// Format version byte hashed before the first record.
pub const EVENT_LOG_VERSION: u8 = 1;

/// One canonical event. See the module docs for the per-kind field conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Event {
    pub kind: EventKind,
    pub a: u64,
    pub b: u64,
    pub px: i64,
    pub qty: u64,
    pub side: u8,
}

impl Event {
    /// Generic constructor; the book methods use the named helpers below.
    #[inline]
    pub const fn new(kind: EventKind, a: u64, b: u64, px: i64, qty: u64, side: u8) -> Event {
        Event {
            kind,
            a,
            b,
            px,
            qty,
            side,
        }
    }

    #[inline]
    pub const fn add(id: OrderId, side: Side, px: Px, qty: Qty) -> Event {
        Event::new(EventKind::Add, id, 0, px as i64, qty as u64, side.code())
    }

    #[inline]
    pub const fn cancel(id: OrderId, side: Side, px: Px, removed: Qty, remaining: Qty) -> Event {
        Event::new(
            EventKind::Cancel,
            id,
            remaining as u64,
            px as i64,
            removed as u64,
            side.code(),
        )
    }

    #[inline]
    pub const fn exec(id: OrderId, side: Side, px: Px, executed: Qty, remaining: Qty) -> Event {
        Event::new(
            EventKind::Exec,
            id,
            remaining as u64,
            px as i64,
            executed as u64,
            side.code(),
        )
    }

    #[inline]
    pub const fn delete(id: OrderId, side: Side, px: Px, removed: Qty) -> Event {
        Event::new(
            EventKind::Delete,
            id,
            0,
            px as i64,
            removed as u64,
            side.code(),
        )
    }

    #[inline]
    pub const fn replace(old: OrderId, new: OrderId, side: Side, px: Px, qty: Qty) -> Event {
        Event::new(
            EventKind::Replace,
            old,
            new,
            px as i64,
            qty as u64,
            side.code(),
        )
    }

    /// `('unknown', kind, id)`: b carries the code of the op that named the unknown id.
    #[inline]
    pub const fn unknown(kind: EventKind, id: OrderId) -> Event {
        Event::new(EventKind::Unknown, id, kind.code() as u64, 0, 0, 0)
    }

    /// Reject of `kind` on `id` asking for `want` (0 when the request had no quantity).
    #[inline]
    pub const fn reject(kind: EventKind, id: OrderId, want: u64) -> Event {
        Event::new(EventKind::Reject, id, kind.code() as u64, 0, want, 0)
    }

    /// The 34-byte little-endian record of the hash contract.
    #[inline]
    pub fn encode(&self) -> [u8; EVENT_RECORD_LEN] {
        let mut out = [0u8; EVENT_RECORD_LEN];
        out[0] = self.kind.code();
        out[1..9].copy_from_slice(&self.a.to_le_bytes());
        out[9..17].copy_from_slice(&self.b.to_le_bytes());
        out[17..25].copy_from_slice(&self.px.to_le_bytes());
        out[25..33].copy_from_slice(&self.qty.to_le_bytes());
        out[33] = self.side;
        out
    }

    /// Inverse of [`Event::encode`]; `None` if the type code is not 1..=12.
    pub fn decode(rec: &[u8; EVENT_RECORD_LEN]) -> Option<Event> {
        let kind = EventKind::from_code(rec[0])?;
        let u64_at = |i: usize| u64::from_le_bytes(rec[i..i + 8].try_into().expect("8 bytes"));
        Some(Event {
            kind,
            a: u64_at(1),
            b: u64_at(9),
            px: u64_at(17) as i64,
            qty: u64_at(25),
            side: rec[33],
        })
    }
}

/// Streaming sha256 over `EVENT_LOG_VERSION` then every pushed record, in emission order.
/// `digest()` may be called at any time; it does not consume the log.
#[derive(Clone)]
pub struct EventLog {
    hasher: Sha256,
    len: u64,
}

impl Default for EventLog {
    fn default() -> Self {
        Self::new()
    }
}

impl EventLog {
    pub fn new() -> EventLog {
        let mut hasher = Sha256::new();
        hasher.update([EVENT_LOG_VERSION]);
        EventLog { hasher, len: 0 }
    }

    /// Append one record. No allocation.
    #[inline]
    pub fn push(&mut self, e: Event) {
        self.hasher.update(e.encode());
        self.len += 1;
    }

    /// sha256 of the version byte and every record pushed so far.
    pub fn digest(&self) -> [u8; 32] {
        self.hasher.clone().finalize().into()
    }

    /// Number of records pushed.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl std::fmt::Debug for EventLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLog").field("len", &self.len).finish()
    }
}

/// Lower-case hex of a digest, for test pins and `replay --stats` output.
pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_record_is_the_research_oracle() {
        // report_core: trade(taker 5, maker 4, px 10001, qty 30, side BID)
        let e = Event::new(EventKind::Trade, 5, 4, 10001, 30, Side::Bid.code());
        assert_eq!(
            hex(&e.encode()),
            "090500000000000000040000000000000011270000000000001e0000000000000000"
        );
        assert_eq!(Event::decode(&e.encode()), Some(e));
    }

    #[test]
    fn negative_price_round_trips() {
        let e = Event::new(EventKind::Modify, 1, 2, -3, 4, 1);
        assert_eq!(Event::decode(&e.encode()), Some(e));
        assert_eq!(Event::decode(&[0u8; 34]), None);
    }

    #[test]
    fn kind_codes_round_trip() {
        for c in 1..=12u8 {
            assert_eq!(EventKind::from_code(c).map(EventKind::code), Some(c));
        }
        assert_eq!(EventKind::from_code(0), None);
        assert_eq!(EventKind::from_code(13), None);
        assert_eq!(Side::from_code(1), Some(Side::Ask));
        assert_eq!(Side::from_code(2), None);
    }
}

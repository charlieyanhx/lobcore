//! `ops.bin`: a fixed-width op-sequence format so an op stream dumped by the Python reference
//! model (or by a proptest failure) can be replayed through either book and its event-log and
//! book-state hashes compared.
//!
//! Record = 25 bytes little-endian: `u8 kind | u64 id | u64 id2 | i32 px | u32 qty`.
//! kind 1 add (id, side = id2: 0 bid / 1 ask, px, qty) | 2 cancel (id, qty) | 3 execute (id, qty)
//! | 4 delete (id) | 5 replace (old = id, new = id2, px, qty). Unused fields are zero.
//! Kind codes are the `EventKind` codes of the op they perform.

use crate::api::{BookError, OrderBook};
use crate::types::{Event, EventKind, EventLog, OrderId, Px, Qty, Side};

pub const OP_RECORD_LEN: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Add {
        id: OrderId,
        side: Side,
        px: Px,
        qty: Qty,
    },
    Cancel {
        id: OrderId,
        qty: Qty,
    },
    Execute {
        id: OrderId,
        qty: Qty,
    },
    Delete {
        id: OrderId,
    },
    Replace {
        old: OrderId,
        new: OrderId,
        px: Px,
        qty: Qty,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpsError {
    /// Byte length is not a multiple of `OP_RECORD_LEN`.
    Length(usize),
    /// Record `index` has an unknown kind or side byte.
    Record { index: usize, kind: u8 },
}

impl std::fmt::Display for OpsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpsError::Length(n) => {
                write!(f, "ops.bin length {n} is not a multiple of {OP_RECORD_LEN}")
            }
            OpsError::Record { index, kind } => {
                write!(f, "ops.bin record {index}: bad kind/side {kind}")
            }
        }
    }
}

impl std::error::Error for OpsError {}

impl Op {
    pub fn encode(&self) -> [u8; OP_RECORD_LEN] {
        let (kind, id, id2, px, qty): (EventKind, u64, u64, i32, u32) = match *self {
            Op::Add { id, side, px, qty } => (EventKind::Add, id, side.code() as u64, px, qty),
            Op::Cancel { id, qty } => (EventKind::Cancel, id, 0, 0, qty),
            Op::Execute { id, qty } => (EventKind::Exec, id, 0, 0, qty),
            Op::Delete { id } => (EventKind::Delete, id, 0, 0, 0),
            Op::Replace { old, new, px, qty } => (EventKind::Replace, old, new, px, qty),
        };
        let mut out = [0u8; OP_RECORD_LEN];
        out[0] = kind.code();
        out[1..9].copy_from_slice(&id.to_le_bytes());
        out[9..17].copy_from_slice(&id2.to_le_bytes());
        out[17..21].copy_from_slice(&px.to_le_bytes());
        out[21..25].copy_from_slice(&qty.to_le_bytes());
        out
    }

    pub fn decode(rec: &[u8; OP_RECORD_LEN]) -> Option<Op> {
        let id = u64::from_le_bytes(rec[1..9].try_into().expect("8 bytes"));
        let id2 = u64::from_le_bytes(rec[9..17].try_into().expect("8 bytes"));
        let px = i32::from_le_bytes(rec[17..21].try_into().expect("4 bytes"));
        let qty = u32::from_le_bytes(rec[21..25].try_into().expect("4 bytes"));
        Some(match EventKind::from_code(rec[0])? {
            EventKind::Add => Op::Add {
                id,
                side: Side::from_code(u8::try_from(id2).ok()?)?,
                px,
                qty,
            },
            EventKind::Cancel => Op::Cancel { id, qty },
            EventKind::Exec => Op::Execute { id, qty },
            EventKind::Delete => Op::Delete { id },
            EventKind::Replace => Op::Replace {
                old: id,
                new: id2,
                px,
                qty,
            },
            _ => return None,
        })
    }

    /// Apply to a book; the returned `Result` is exactly what the book returned.
    #[inline]
    pub fn apply<B: OrderBook + ?Sized>(&self, book: &mut B) -> Result<Event, BookError> {
        match *self {
            Op::Add { id, side, px, qty } => book.add(id, side, px, qty),
            Op::Cancel { id, qty } => book.cancel(id, qty),
            Op::Execute { id, qty } => book.execute(id, qty),
            Op::Delete { id } => book.delete(id),
            Op::Replace { old, new, px, qty } => book.replace(old, new, px, qty),
        }
    }
}

/// Serialise an op sequence to `ops.bin` bytes.
pub fn encode_ops(ops: &[Op]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ops.len() * OP_RECORD_LEN);
    for op in ops {
        out.extend_from_slice(&op.encode());
    }
    out
}

/// Parse `ops.bin` bytes.
pub fn decode_ops(bytes: &[u8]) -> Result<Vec<Op>, OpsError> {
    if !bytes.len().is_multiple_of(OP_RECORD_LEN) {
        return Err(OpsError::Length(bytes.len()));
    }
    bytes
        .as_chunks::<OP_RECORD_LEN>()
        .0
        .iter()
        .enumerate()
        .map(|(index, rec)| {
            Op::decode(rec).ok_or(OpsError::Record {
                index,
                kind: rec[0],
            })
        })
        .collect()
}

/// The event to log for an op result: the event on success, `err.event()` on failure.
#[inline]
pub fn logged_event(r: &Result<Event, BookError>) -> Event {
    match r {
        Ok(e) => *e,
        Err(e) => e.event(),
    }
}

/// Replay every op into `book`, pushing one record per op into `log`. Returns the number of
/// ops that returned `Err`.
pub fn replay<B: OrderBook + ?Sized>(book: &mut B, ops: &[Op], log: &mut EventLog) -> usize {
    let mut errors = 0usize;
    for op in ops {
        let r = op.apply(book);
        errors += r.is_err() as usize;
        log.push(logged_event(&r));
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_round_trip_and_reject_bad_bytes() {
        let ops = [
            Op::Add {
                id: 1,
                side: Side::Ask,
                px: 10050,
                qty: 7,
            },
            Op::Cancel { id: 1, qty: 3 },
            Op::Execute { id: 1, qty: 2 },
            Op::Replace {
                old: 1,
                new: 2,
                px: 10049,
                qty: 9,
            },
            Op::Delete { id: 2 },
        ];
        let bytes = encode_ops(&ops);
        assert_eq!(bytes.len(), 5 * OP_RECORD_LEN);
        assert_eq!(decode_ops(&bytes).unwrap(), ops);
        assert_eq!(decode_ops(&bytes[..10]), Err(OpsError::Length(10)));
        let mut bad = bytes.clone();
        bad[0] = 9;
        assert_eq!(
            decode_ops(&bad),
            Err(OpsError::Record { index: 0, kind: 9 })
        );
        let mut bad_side = bytes.clone();
        bad_side[9] = 2;
        assert_eq!(
            decode_ops(&bad_side),
            Err(OpsError::Record { index: 0, kind: 1 })
        );
    }
}

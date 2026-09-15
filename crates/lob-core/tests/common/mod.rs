//! Shared test machinery: abstract ops (what a strategy or a seeded RNG produces) and the
//! interpreter that resolves them into concrete `Op`s against the current book state,
//! the nanolob differential pattern in proptest form.
//!
//! Resolution rules (docs/PLAN.md, invariant list): `pick` -> a live id (`pick % live.len()`),
//! or an unknown id when `pick % 20 == 0` (5 %); bid price = `min(best_ask - 1, mid) - offset`,
//! ask price = `max(best_bid + 1, mid) + offset` (never crosses the opposite best unless the
//! clamp forces it); the in-range strategy clamps prices into the window, the overflow-heavy
//! one does not; `mid += drift` every 500 ops.

#![allow(dead_code)]

use std::collections::HashMap;

use lob_core::{BookError, Event, EventKind, LevelView, Op, OrderId, Px, Qty, Side};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbsOp {
    Add { side: Side, offset: u8, qty: Qty },
    Cancel { pick: u16, qty: Qty },
    Execute { pick: u16, qty: Qty },
    Delete { pick: u16 },
    Replace { pick: u16, offset: u8, qty: Qty },
}

impl AbsOp {
    /// Decode one op from a `u64` so proptest can generate `Vec<u64>` (cheap value trees that
    /// shrink element-wise toward 0 = `Add { Bid, offset 0, qty 1 }`). Mix: 40 % add, 20 %
    /// cancel, 12 % execute, 12 % delete, 16 % replace; offsets 0..40, qty 1..=100 (adds and
    /// replaces) or 1..=60 (cancels and executes).
    pub fn from_u64(x: u64) -> AbsOp {
        let roll = x % 100;
        let y = x / 100;
        let pick = (y & 0xFFFF) as u16;
        let y = y >> 16;
        let offset = (y % 40) as u8;
        let y = y / 40;
        let qty100 = 1 + (y % 100) as Qty;
        let qty60 = 1 + (y % 60) as Qty;
        let side = if (y / 100) & 1 == 0 {
            Side::Bid
        } else {
            Side::Ask
        };
        match roll {
            0..40 => AbsOp::Add {
                side,
                offset,
                qty: qty100,
            },
            40..60 => AbsOp::Cancel { pick, qty: qty60 },
            60..72 => AbsOp::Execute { pick, qty: qty60 },
            72..84 => AbsOp::Delete { pick },
            _ => AbsOp::Replace {
                pick,
                offset,
                qty: qty100,
            },
        }
    }
}

pub const DRIFT_EVERY: usize = 500;
pub const UNKNOWN_EVERY: u16 = 20;
pub const UNKNOWN_BASE: OrderId = 1_000_000_000;

#[derive(Debug, Clone)]
pub struct Interp {
    pub mid: Px,
    pub drift: Px,
    /// `Some((lo, hi))` clamps every generated price into `lo..=hi`.
    pub clamp: Option<(Px, Px)>,
    pub next_id: OrderId,
    pub live: Vec<OrderId>,
    pub orders: HashMap<OrderId, (Side, Qty)>,
    pub ops_seen: usize,
}

impl Interp {
    pub fn new(mid: Px, drift: Px, clamp: Option<(Px, Px)>) -> Interp {
        Interp {
            mid,
            drift,
            clamp,
            next_id: 1,
            live: Vec::new(),
            orders: HashMap::new(),
            ops_seen: 0,
        }
    }

    fn price(&self, side: Side, offset: u8, l1: (Option<LevelView>, Option<LevelView>)) -> Px {
        let raw = match side {
            Side::Bid => {
                l1.1.map_or(self.mid, |(ask, _, _)| (ask - 1).min(self.mid)) - offset as Px
            }
            Side::Ask => {
                l1.0.map_or(self.mid, |(bid, _, _)| (bid + 1).max(self.mid)) + offset as Px
            }
        };
        let px = match self.clamp {
            Some((lo, hi)) => raw.clamp(lo, hi),
            None => raw,
        };
        px.max(1)
    }

    fn pick(&self, pick: u16) -> OrderId {
        if pick.is_multiple_of(UNKNOWN_EVERY) || self.live.is_empty() {
            UNKNOWN_BASE + pick as OrderId
        } else {
            self.live[pick as usize % self.live.len()]
        }
    }

    /// Whether `op` will name an unknown id or over-execute (so the caller can assert I9/I10).
    pub fn predicts_error(&self, op: &Op) -> bool {
        match *op {
            Op::Add { .. } => false,
            Op::Cancel { id, .. } | Op::Delete { id } | Op::Replace { old: id, .. } => {
                !self.orders.contains_key(&id)
            }
            Op::Execute { id, qty } => self.orders.get(&id).is_none_or(|&(_, have)| qty > have),
        }
    }

    /// Resolve one abstract op given the reference book's current L1.
    pub fn resolve(&mut self, a: AbsOp, l1: (Option<LevelView>, Option<LevelView>)) -> Op {
        if self.ops_seen > 0 && self.ops_seen.is_multiple_of(DRIFT_EVERY) {
            self.mid += self.drift;
        }
        self.ops_seen += 1;
        match a {
            AbsOp::Add { side, offset, qty } => {
                let id = self.next_id;
                self.next_id += 1;
                Op::Add {
                    id,
                    side,
                    px: self.price(side, offset, l1),
                    qty,
                }
            }
            AbsOp::Cancel { pick, qty } => Op::Cancel {
                id: self.pick(pick),
                qty,
            },
            AbsOp::Execute { pick, qty } => Op::Execute {
                id: self.pick(pick),
                qty,
            },
            AbsOp::Delete { pick } => Op::Delete {
                id: self.pick(pick),
            },
            AbsOp::Replace { pick, offset, qty } => {
                let old = self.pick(pick);
                let side = self.orders.get(&old).map_or(Side::Bid, |&(s, _)| s);
                let new = self.next_id;
                self.next_id += 1;
                Op::Replace {
                    old,
                    new,
                    px: self.price(side, offset, l1),
                    qty,
                }
            }
        }
    }

    /// Update the live set from what the book actually did.
    pub fn observe(&mut self, r: &Result<Event, BookError>) {
        let Ok(e) = r else { return };
        let side = Side::from_code(e.side).expect("book events carry a valid side");
        match e.kind {
            EventKind::Add => self.insert(e.a, side, e.qty as Qty),
            EventKind::Cancel | EventKind::Exec => {
                if e.b == 0 {
                    self.remove(e.a);
                } else if let Some(o) = self.orders.get_mut(&e.a) {
                    o.1 = e.b as Qty;
                }
            }
            EventKind::Delete => self.remove(e.a),
            EventKind::Replace => {
                self.remove(e.a);
                self.insert(e.b, side, e.qty as Qty);
            }
            other => panic!("book emitted {other:?} on success"),
        }
    }

    fn insert(&mut self, id: OrderId, side: Side, qty: Qty) {
        self.live.push(id);
        self.orders.insert(id, (side, qty));
    }

    fn remove(&mut self, id: OrderId) {
        let pos = self
            .live
            .iter()
            .position(|&x| x == id)
            .expect("removed id was live");
        self.live.swap_remove(pos);
        self.orders.remove(&id);
    }
}

/// splitmix64: a tiny deterministic generator for the seeded (non-proptest) op streams.
#[derive(Debug, Clone)]
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// One abstract op from the next word (see [`AbsOp::from_u64`]).
    pub fn abs_op(&mut self) -> AbsOp {
        AbsOp::from_u64(self.next_u64())
    }
}

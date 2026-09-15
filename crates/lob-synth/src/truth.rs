//! The generator's own minimal L3 truth book and the book-state hash contract.
//!
//! One `TruthBook` per locate: `BTreeMap<i32 price, VecDeque<(u64 ref, u32 qty)>>` per side plus
//! `HashMap<ref, (Side, price)>`. Prices are ITCH 1e-4 units, positive on both sides. This crate does
//! not depend on lob-core; the sidecar it produces is what lob-core's replay is compared against.
//!
//! Book-state hash (docs/DESIGN.md section 8, "book-state hash"): sha256 over, per side bid then ask,
//! levels in ascending price, `i64 price LE | u32 count LE | (u64 id LE, u32 qty LE) in FIFO order`.
//! No side marker, no version byte; an empty book hashes to sha256 of the empty string.

use std::collections::{BTreeMap, HashMap, VecDeque};

use sha2::{Digest, Sha256};

/// Order side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// Buy ('B').
    Bid,
    /// Sell ('S').
    Ask,
}

/// One side: ascending price -> FIFO of (ref, qty).
pub type SideBook = BTreeMap<i32, VecDeque<(u64, u32)>>;

/// Minimal L3 book used as the truth sidecar source.
#[derive(Clone, Debug, Default)]
pub struct TruthBook {
    bids: SideBook,
    asks: SideBook,
    orders: HashMap<u64, (Side, i32)>,
}

impl TruthBook {
    /// Empty book.
    pub fn new() -> Self {
        Self::default()
    }

    fn side(&self, side: Side) -> &SideBook {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    fn side_mut(&mut self, side: Side) -> &mut SideBook {
        match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    /// Bid side, ascending price.
    pub fn bids(&self) -> &SideBook {
        &self.bids
    }

    /// Ask side, ascending price.
    pub fn asks(&self) -> &SideBook {
        &self.asks
    }

    /// Number of live orders.
    pub fn live_orders(&self) -> usize {
        self.orders.len()
    }

    /// `(side, price)` of a live order.
    pub fn lookup(&self, id: u64) -> Option<(Side, i32)> {
        self.orders.get(&id).copied()
    }

    /// Remaining quantity of a live order (linear in the level's FIFO length).
    pub fn qty(&self, id: u64) -> Option<u32> {
        let (side, px) = self.lookup(id)?;
        self.side(side)
            .get(&px)?
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, q)| *q)
    }

    /// Add at the tail of the level. Panics on a duplicate ref (a generator bug, not data).
    pub fn add(&mut self, id: u64, side: Side, px: i32, qty: u32) {
        assert!(qty > 0, "add with zero qty");
        let prev = self.orders.insert(id, (side, px));
        assert!(prev.is_none(), "duplicate ref {id}");
        self.side_mut(side)
            .entry(px)
            .or_default()
            .push_back((id, qty));
    }

    /// Remove the order entirely. Returns false when the ref is unknown (no state change).
    pub fn delete(&mut self, id: u64) -> bool {
        let Some((side, px)) = self.orders.remove(&id) else {
            return false;
        };
        let book = self.side_mut(side);
        let level = book.get_mut(&px).expect("level of a live order exists");
        let pos = level
            .iter()
            .position(|(i, _)| *i == id)
            .expect("live order sits in its level");
        level.remove(pos);
        if level.is_empty() {
            book.remove(&px);
        }
        true
    }

    /// Reduce the order by `qty` in place (ITCH X / E / C). Removes it when it reaches zero. Returns
    /// false (no change) when the ref is unknown or `qty` exceeds the remaining quantity.
    pub fn reduce(&mut self, id: u64, qty: u32) -> bool {
        let Some((side, px)) = self.lookup(id) else {
            return false;
        };
        let level = self
            .side_mut(side)
            .get_mut(&px)
            .expect("level of a live order exists");
        let Some(slot) = level.iter_mut().find(|(i, _)| *i == id) else {
            return false;
        };
        if qty > slot.1 {
            return false;
        }
        slot.1 -= qty;
        if slot.1 == 0 {
            self.delete(id);
        }
        true
    }

    /// ITCH U: remove `old`, add `new` at the tail of `px` on the same side (spec 1.4.5, always loses
    /// priority). Returns false (no change, `new` not added) when `old` is unknown.
    pub fn replace(&mut self, old: u64, new: u64, px: i32, qty: u32) -> bool {
        let Some((side, _)) = self.lookup(old) else {
            return false;
        };
        self.delete(old);
        self.add(new, side, px, qty);
        true
    }

    /// Best bid `(price, level qty)`.
    pub fn best_bid(&self) -> Option<(i32, u32)> {
        self.bids
            .iter()
            .next_back()
            .map(|(px, lvl)| (*px, level_qty(lvl)))
    }

    /// Best ask `(price, level qty)`.
    pub fn best_ask(&self) -> Option<(i32, u32)> {
        self.asks
            .iter()
            .next()
            .map(|(px, lvl)| (*px, level_qty(lvl)))
    }

    /// Head order `(ref, qty)` of the best level on `side`.
    pub fn head_of_best(&self, side: Side) -> Option<(u64, u32)> {
        let lvl = match side {
            Side::Bid => self.bids.iter().next_back().map(|(_, l)| l),
            Side::Ask => self.asks.iter().next().map(|(_, l)| l),
        }?;
        lvl.front().copied()
    }

    /// Level quantities of the five best levels on `side`, best first, zero-padded.
    pub fn l5(&self, side: Side) -> [u32; 5] {
        let mut out = [0u32; 5];
        let it: Box<dyn Iterator<Item = &VecDeque<(u64, u32)>>> = match side {
            Side::Bid => Box::new(self.bids.values().rev()),
            Side::Ask => Box::new(self.asks.values()),
        };
        for (slot, lvl) in out.iter_mut().zip(it) {
            *slot = level_qty(lvl);
        }
        out
    }

    /// The book-state hash contract.
    pub fn hash(&self) -> [u8; 32] {
        hash_book(&self.bids, &self.asks)
    }
}

/// Sum of the FIFO's quantities (u32, checked: a level over 4.29e9 shares is a data error).
pub fn level_qty(level: &VecDeque<(u64, u32)>) -> u32 {
    level
        .iter()
        .try_fold(0u32, |acc, (_, q)| acc.checked_add(*q))
        .expect("level qty overflows u32")
}

/// Book-state hash: sha256 over bid then ask side, ascending price, `i64 price LE | u32 count LE |
/// (u64 id LE, u32 qty LE)*` in FIFO order.
pub fn hash_book(bids: &SideBook, asks: &SideBook) -> [u8; 32] {
    let mut h = Sha256::new();
    for side in [bids, asks] {
        for (px, lvl) in side {
            h.update(i64::from(*px).to_le_bytes());
            h.update(
                u32::try_from(lvl.len())
                    .expect("level count fits u32")
                    .to_le_bytes(),
            );
            for (id, qty) in lvl {
                h.update(id.to_le_bytes());
                h.update(qty.to_le_bytes());
            }
        }
    }
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_book_hashes_to_sha256_of_nothing() {
        let b = TruthBook::new();
        let expect: [u8; 32] = Sha256::digest([]).into();
        assert_eq!(b.hash(), expect);
        assert_eq!(b.best_bid(), None);
        assert_eq!(b.l5(Side::Bid), [0; 5]);
    }

    #[test]
    fn hash_matches_hand_built_record_layout() {
        let mut b = TruthBook::new();
        b.add(7, Side::Bid, 10000, 100);
        b.add(8, Side::Bid, 10000, 50);
        b.add(9, Side::Ask, 10002, 80);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&10000i64.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&7u64.to_le_bytes());
        bytes.extend_from_slice(&100u32.to_le_bytes());
        bytes.extend_from_slice(&8u64.to_le_bytes());
        bytes.extend_from_slice(&50u32.to_le_bytes());
        bytes.extend_from_slice(&10002i64.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&9u64.to_le_bytes());
        bytes.extend_from_slice(&80u32.to_le_bytes());
        let expect: [u8; 32] = Sha256::digest(&bytes).into();
        assert_eq!(b.hash(), expect);
    }

    #[test]
    fn fifo_semantics_x_keeps_priority_u_loses_it() {
        // Oracle from the research report: FIFO [1,2,3] qty 300/100/200; X(1,50) -> [1,2,3] qty 550;
        // U(1 -> 9, same price, 250) -> [2,3,9].
        let mut b = TruthBook::new();
        b.add(1, Side::Bid, 10050, 300);
        b.add(2, Side::Bid, 10050, 100);
        b.add(3, Side::Bid, 10050, 200);
        assert!(b.reduce(1, 50));
        let ids: Vec<u64> = b.bids()[&10050].iter().map(|(i, _)| *i).collect();
        assert_eq!(ids, [1, 2, 3]);
        assert_eq!(b.best_bid(), Some((10050, 550)));
        assert!(b.replace(1, 9, 10050, 250));
        let ids: Vec<u64> = b.bids()[&10050].iter().map(|(i, _)| *i).collect();
        assert_eq!(ids, [2, 3, 9]);
        assert_eq!(b.head_of_best(Side::Bid), Some((2, 100)));
        // unknown refs are no-ops
        let before = b.hash();
        assert!(!b.delete(777));
        assert!(!b.reduce(777, 1));
        assert!(!b.replace(777, 10, 10050, 1));
        assert!(!b.reduce(2, 101));
        assert_eq!(b.hash(), before);
        assert_eq!(b.live_orders(), 3);
        // reduce to zero removes; empty level disappears
        assert!(b.reduce(2, 100));
        assert!(b.reduce(3, 200));
        assert!(b.reduce(9, 250));
        assert_eq!(b.best_bid(), None);
        assert!(b.bids().is_empty());
    }

    #[test]
    fn l5_and_bests() {
        let mut b = TruthBook::new();
        for (i, px) in [10000, 9999, 9998, 9997, 9996, 9995].iter().enumerate() {
            b.add(i as u64 + 1, Side::Bid, *px, 10 * (i as u32 + 1));
        }
        b.add(100, Side::Ask, 10001, 7);
        assert_eq!(b.l5(Side::Bid), [10, 20, 30, 40, 50]);
        assert_eq!(b.l5(Side::Ask), [7, 0, 0, 0, 0]);
        assert_eq!(b.best_bid(), Some((10000, 10)));
        assert_eq!(b.best_ask(), Some((10001, 7)));
        assert_eq!(b.qty(3), Some(30));
        assert_eq!(b.qty(999), None);
    }
}

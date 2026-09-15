//! `RefBook`: the BTreeMap reference book with the identical `OrderBook` API. It is the
//! differential oracle for `ArrayBook` (I11), the tree comparator for the bounded-vs-tree
//! benchmark table, and the book every non-watchlist locate runs in replay.
//!
//! Layout: `BTreeMap<Px, VecDeque<(OrderId, Qty)>>` per side + `HashMap<OrderId, (Side, Px)>`.
//! Cancels and executes scan the level's deque for the id (O(level length)); nothing here is
//! optimised, it is meant to be obviously correct. Level totals are capped at `u32::MAX` so the
//! `BadQty` rule matches `ArrayBook` exactly.

use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::api::{BookError, LevelView, OrderBook, Snapshot};
use crate::hash::StateHasher;
use crate::types::{Event, EventKind, OrderId, Px, Qty, Side};

type Fifo = VecDeque<(OrderId, Qty)>;

#[derive(Debug, Default)]
pub struct RefBook {
    bids: BTreeMap<Px, Fifo>,
    asks: BTreeMap<Px, Fifo>,
    ids: HashMap<OrderId, (Side, Px)>,
}

impl RefBook {
    pub fn new() -> RefBook {
        RefBook::default()
    }

    fn side(&self, side: Side) -> &BTreeMap<Px, Fifo> {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    fn side_mut(&mut self, side: Side) -> &mut BTreeMap<Px, Fifo> {
        match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    fn level_qty(fifo: &Fifo) -> u64 {
        fifo.iter().map(|&(_, q)| q as u64).sum()
    }

    fn level_view(px: Px, fifo: &Fifo) -> LevelView {
        (px, Self::level_qty(fifo) as Qty, fifo.len() as u32)
    }

    /// Levels best first: bids descending, asks ascending.
    fn best_first(&self, side: Side) -> Box<dyn Iterator<Item = (&Px, &Fifo)> + '_> {
        match side {
            Side::Bid => Box::new(self.bids.iter().rev()),
            Side::Ask => Box::new(self.asks.iter()),
        }
    }

    /// Position of `id` in its level; the id map says it is live.
    fn find(&self, id: OrderId) -> Option<(Side, Px, usize, Qty)> {
        let &(side, px) = self.ids.get(&id)?;
        let fifo = self.side(side).get(&px)?;
        let (pos, &(_, qty)) = fifo.iter().enumerate().find(|&(_, &(i, _))| i == id)?;
        Some((side, px, pos, qty))
    }

    /// Remove `by` from the order at `pos`; drop it (and an emptied level) at zero.
    fn take(&mut self, id: OrderId, side: Side, px: Px, pos: usize, by: Qty) -> Qty {
        let levels = self.side_mut(side);
        let fifo = levels.get_mut(&px).expect("level exists for a live id");
        let remaining = fifo[pos].1 - by;
        if remaining == 0 {
            fifo.remove(pos);
            if fifo.is_empty() {
                levels.remove(&px);
            }
            self.ids.remove(&id);
        } else {
            fifo[pos].1 = remaining;
        }
        remaining
    }

    fn push_tail(&mut self, id: OrderId, side: Side, px: Px, qty: Qty) {
        self.side_mut(side)
            .entry(px)
            .or_default()
            .push_back((id, qty));
        self.ids.insert(id, (side, px));
    }

    fn would_overflow(&self, side: Side, px: Px, minus: u64, plus: Qty) -> bool {
        let existing = self.side(side).get(&px).map_or(0, Self::level_qty) - minus;
        existing + plus as u64 > u32::MAX as u64
    }
}

impl OrderBook for RefBook {
    fn add(&mut self, id: OrderId, side: Side, px: Px, qty: Qty) -> Result<Event, BookError> {
        if qty == 0 {
            return Err(BookError::BadQty);
        }
        if px <= 0 {
            return Err(BookError::BadPrice);
        }
        // Same precedence as ArrayBook::add: level overflow before the duplicate check.
        if self.would_overflow(side, px, 0, qty) {
            return Err(BookError::BadQty);
        }
        if self.ids.contains_key(&id) {
            return Err(BookError::DuplicateId(id));
        }
        self.push_tail(id, side, px, qty);
        Ok(Event::add(id, side, px, qty))
    }

    fn cancel(&mut self, id: OrderId, qty: Qty) -> Result<Event, BookError> {
        let (side, px, pos, have) = self.find(id).ok_or(BookError::UnknownId {
            kind: EventKind::Cancel,
            id,
        })?;
        if qty == 0 {
            return Err(BookError::BadQty);
        }
        let removed = qty.min(have);
        let remaining = self.take(id, side, px, pos, removed);
        Ok(Event::cancel(id, side, px, removed, remaining))
    }

    fn delete(&mut self, id: OrderId) -> Result<Event, BookError> {
        let (side, px, pos, have) = self.find(id).ok_or(BookError::UnknownId {
            kind: EventKind::Delete,
            id,
        })?;
        self.take(id, side, px, pos, have);
        Ok(Event::delete(id, side, px, have))
    }

    fn execute(&mut self, id: OrderId, qty: Qty) -> Result<Event, BookError> {
        let (side, px, pos, have) = self.find(id).ok_or(BookError::UnknownId {
            kind: EventKind::Exec,
            id,
        })?;
        if qty == 0 {
            return Err(BookError::BadQty);
        }
        if qty > have {
            return Err(BookError::OverExecute {
                id,
                have,
                want: qty,
            });
        }
        let remaining = self.take(id, side, px, pos, qty);
        Ok(Event::exec(id, side, px, qty, remaining))
    }

    fn replace(
        &mut self,
        old: OrderId,
        new: OrderId,
        px: Px,
        qty: Qty,
    ) -> Result<Event, BookError> {
        let (side, old_px, pos, have) = self.find(old).ok_or(BookError::UnknownId {
            kind: EventKind::Replace,
            id: old,
        })?;
        if self.ids.contains_key(&new) {
            return Err(BookError::DuplicateId(new));
        }
        if qty == 0 {
            return Err(BookError::BadQty);
        }
        if px <= 0 {
            return Err(BookError::BadPrice);
        }
        let minus = if px == old_px { have as u64 } else { 0 };
        if self.would_overflow(side, px, minus, qty) {
            return Err(BookError::BadQty);
        }
        self.take(old, side, old_px, pos, have);
        self.push_tail(new, side, px, qty);
        Ok(Event::replace(old, new, side, px, qty))
    }

    fn l1(&self) -> (Option<LevelView>, Option<LevelView>) {
        (
            self.bids
                .last_key_value()
                .map(|(&px, f)| Self::level_view(px, f)),
            self.asks
                .first_key_value()
                .map(|(&px, f)| Self::level_view(px, f)),
        )
    }

    fn l2(&self, depth: usize) -> (Vec<LevelView>, Vec<LevelView>) {
        let take = |side| -> Vec<LevelView> {
            self.best_first(side)
                .take(depth)
                .map(|(&px, f)| Self::level_view(px, f))
                .collect()
        };
        (take(Side::Bid), take(Side::Ask))
    }

    fn queue_ahead(&self, id: OrderId) -> Option<Qty> {
        let (side, px, pos, _) = self.find(id)?;
        let fifo = self.side(side).get(&px)?;
        Some(fifo.iter().take(pos).map(|&(_, q)| q).sum())
    }

    fn live_orders(&self) -> usize {
        self.ids.len()
    }

    fn state_hash(&self) -> [u8; 32] {
        let mut h = StateHasher::new();
        for levels in [&self.bids, &self.asks] {
            for (&px, fifo) in levels {
                h.level(px, fifo.len() as u32);
                for &(id, qty) in fifo {
                    h.order(id, qty);
                }
            }
        }
        h.finish()
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Snapshot::new();
        for side in [Side::Bid, Side::Ask] {
            for (&px, fifo) in self.best_first(side) {
                out.push((side, px, fifo.iter().copied().collect()));
            }
        }
        out
    }
}

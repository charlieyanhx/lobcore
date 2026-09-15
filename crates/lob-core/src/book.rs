//! `ArrayBook`: the bounded-array L3 book. Levels live in a per-side array indexed by tick
//! offset (two-level bitmap for best tracking), orders in a slab with intrusive FIFO links,
//! ids in an `FxHashMap`, and off-grid / out-of-range prices in a per-side overflow `BTreeMap`.
//!
//! Hot-path allocation policy: after `with_capacity` / `reserve` sized for the live-order
//! high-water mark, add / cancel / delete / execute / replace at in-range prices allocate
//! nothing (tested by `tests/alloc_count.rs`). The overflow map is the documented allocating
//! path (a new overflow level allocates a tree node; an emptied one is removed).
//!
//! Invariants I1-I8 are checked by [`ArrayBook::check`]; I9/I10 (unknown id, over-execute)
//! are guaranteed by validating every input before the first mutation.

use std::collections::hash_map::Entry;

use crate::api::{BookConfig, BookError, LevelView, OrderBook, Snapshot};
use crate::hash::StateHasher;
use crate::levels::{Grid, Level, Locus, MAX_LEVELS, SideLevels};
use crate::slab::{IdMap, NIL, Slab, Slot};
use crate::types::{Event, EventKind, OrderId, Px, Qty, Side};

#[derive(Debug)]
pub struct ArrayBook {
    grid: Grid,
    bids: SideLevels,
    asks: SideLevels,
    slab: Slab,
    ids: IdMap,
}

/// Split-borrow helper: the side's levels and the slab as two disjoint `&mut`.
struct Parts<'a> {
    side: &'a mut SideLevels,
    slab: &'a mut Slab,
}

impl ArrayBook {
    /// Panics on an invalid config (`n_levels` in `1..=4096`, `tick > 0`, `base_px > 0`, window
    /// top `base_px + (n_levels - 1) * tick` within `Px`).
    pub fn new(cfg: BookConfig) -> ArrayBook {
        assert!(
            cfg.n_levels >= 1 && cfg.n_levels <= MAX_LEVELS,
            "n_levels must be in 1..={MAX_LEVELS}, got {}",
            cfg.n_levels
        );
        assert!(cfg.tick > 0, "tick must be positive, got {}", cfg.tick);
        assert!(
            cfg.base_px > 0,
            "base_px must be positive, got {}",
            cfg.base_px
        );
        let top = cfg.base_px as i64 + (cfg.n_levels as i64 - 1) * cfg.tick as i64;
        assert!(
            top <= Px::MAX as i64,
            "window top {top} exceeds Px::MAX; shrink n_levels or tick"
        );
        ArrayBook {
            grid: Grid {
                base_px: cfg.base_px,
                n_levels: cfg.n_levels,
                tick: cfg.tick,
            },
            bids: SideLevels::new(Side::Bid, cfg.n_levels),
            asks: SideLevels::new(Side::Ask, cfg.n_levels),
            slab: Slab::new(),
            ids: IdMap::new(),
        }
    }

    /// `new` plus `reserve(orders)`.
    pub fn with_capacity(cfg: BookConfig, orders: usize) -> ArrayBook {
        let mut b = ArrayBook::new(cfg);
        b.reserve(orders);
        b
    }

    /// Pre-size the slab and the id map for `orders` simultaneously live orders. The map is
    /// reserved at 2x so hashbrown's tombstone churn rehashes in place instead of resizing.
    pub fn reserve(&mut self, orders: usize) {
        self.slab.reserve(orders);
        self.ids.reserve(orders.saturating_mul(2));
    }

    pub fn config(&self) -> BookConfig {
        BookConfig::new(self.grid.base_px, self.grid.n_levels, self.grid.tick)
    }

    /// Adds routed to the overflow map, both sides.
    pub fn overflow_hits(&self) -> u64 {
        self.bids.overflow_hits() + self.asks.overflow_hits()
    }

    /// Largest `|(px - base) / tick|` seen on any add, both sides.
    pub fn max_abs_offset(&self) -> i64 {
        self.bids.max_abs_offset().max(self.asks.max_abs_offset())
    }

    /// Prices currently held in the overflow map of `side`, ascending.
    pub fn overflow_prices(&self, side: Side) -> Vec<Px> {
        self.side_ref(side).overflow.keys().copied().collect()
    }

    /// Slots ever allocated (live + free).
    pub fn slab_capacity(&self) -> usize {
        self.slab.capacity()
    }

    #[inline]
    fn side_ref(&self, side: Side) -> &SideLevels {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    #[inline]
    fn parts(&mut self, side: Side) -> Parts<'_> {
        let side = match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        };
        Parts {
            side,
            slab: &mut self.slab,
        }
    }

    /// Append `slot` to the tail of the level at `locus`. `slot.qty` must already be set.
    #[inline]
    fn link_tail(p: Parts<'_>, locus: Locus, slot: u32) {
        let qty = p.slab.get(slot).qty;
        let lvl = p.side.get_or_insert(locus);
        let before = lvl.count;
        let tail = lvl.tail;
        lvl.tail = slot;
        if tail == NIL {
            lvl.head = slot;
        }
        lvl.qty += qty; // checked by the caller before any mutation
        lvl.count += 1;
        if tail != NIL {
            p.slab.get_mut(tail).next = slot;
            p.slab.get_mut(slot).prev = tail;
        }
        p.side.after_count_change(locus, before);
    }

    /// Unlink `slot` from the level at `locus` and free it.
    #[inline]
    fn unlink(p: Parts<'_>, locus: Locus, slot: u32) {
        let s = *p.slab.get(slot);
        let lvl = p.side.get_mut(locus);
        let before = lvl.count;
        if s.prev == NIL {
            lvl.head = s.next;
        }
        if s.next == NIL {
            lvl.tail = s.prev;
        }
        lvl.qty -= s.qty;
        lvl.count -= 1;
        if s.prev != NIL {
            p.slab.get_mut(s.prev).next = s.next;
        }
        if s.next != NIL {
            p.slab.get_mut(s.next).prev = s.prev;
        }
        p.side.after_count_change(locus, before);
        p.slab.free(slot);
    }

    /// Reduce `slot` in place by `by` (< remaining).
    #[inline]
    fn reduce(p: Parts<'_>, locus: Locus, slot: u32, by: Qty) {
        p.side.get_mut(locus).qty -= by;
        p.slab.get_mut(slot).qty -= by;
    }

    /// Shared body of cancel / execute: remove `by` from the order, unlinking at zero.
    #[inline]
    fn take(&mut self, slot: u32, s: Slot, by: Qty) -> Qty {
        let locus = self.grid.locate(s.abs_px());
        let remaining = s.qty - by;
        if remaining == 0 {
            self.ids.remove(s.id);
            Self::unlink(self.parts(s.side()), locus, slot);
        } else {
            Self::reduce(self.parts(s.side()), locus, slot, by);
        }
        remaining
    }

    fn level_view(px: Px, l: &Level) -> LevelView {
        (px, l.qty, l.count)
    }

    fn fifo(&self, l: &Level) -> Vec<(OrderId, Qty)> {
        let mut v = Vec::with_capacity(l.count as usize);
        let mut i = l.head;
        while i != NIL {
            let s = self.slab.get(i);
            v.push((s.id, s.qty));
            i = s.next;
        }
        v
    }

    /// Verify invariants I1-I8; `Err(String)` names the first violation.
    pub fn check(&self) -> Result<(), String> {
        let mut seen_live = 0usize;
        let mut side_sum = [0u64; 2];
        for sl in [&self.bids, &self.asks] {
            let si = sl.side.code() as usize;
            for (i, l) in sl.levels.iter().enumerate() {
                let bit = sl.bitmap.is_set(i as u32);
                if bit != (l.count > 0) {
                    return Err(format!(
                        "I6: {:?} level {i} bit {bit} count {}",
                        sl.side, l.count
                    ));
                }
                seen_live += self.check_level(sl.side, self.grid.px_of(i as u32), l)?;
                side_sum[si] += l.qty as u64;
            }
            for (w, word) in sl.bitmap.words().iter().enumerate() {
                if ((sl.bitmap.summary() >> w) & 1 == 1) != (*word != 0) {
                    return Err(format!(
                        "I6: {:?} summary bit {w} vs word {word:#x}",
                        sl.side
                    ));
                }
            }
            for (&px, l) in &sl.overflow {
                if l.count == 0 {
                    return Err(format!("I6: {:?} empty overflow level {px}", sl.side));
                }
                if !matches!(self.grid.locate(px), Locus::Overflow(_)) {
                    return Err(format!(
                        "I4: {:?} in-range price {px} in overflow map",
                        sl.side
                    ));
                }
                seen_live += self.check_level(sl.side, px, l)?;
                side_sum[si] += l.qty as u64;
            }
        }
        if seen_live != self.slab.live() || self.ids.len() != self.slab.live() {
            return Err(format!(
                "I2/I7: linked {seen_live}, live {}, ids {}",
                self.slab.live(),
                self.ids.len()
            ));
        }
        let free = self.slab.free_len().map_err(|e| format!("I7: {e}"))?;
        if self.slab.live() + free != self.slab.capacity() {
            return Err(format!(
                "I7: live {} + free {free} != capacity {}",
                self.slab.live(),
                self.slab.capacity()
            ));
        }
        let mut order_sum = [0u64; 2];
        for (id, slot) in self.ids.iter() {
            let s = self.slab.get(slot);
            if s.id != id {
                return Err(format!("I7: id map {id} -> slot {slot} holds id {}", s.id));
            }
            if s.qty == 0 {
                return Err(format!("I5: order {id} has qty 0"));
            }
            order_sum[s.side().code() as usize] += s.qty as u64;
        }
        if side_sum != order_sum {
            return Err(format!(
                "I8: level sums {side_sum:?} != order sums {order_sum:?}"
            ));
        }
        Ok(())
    }

    /// I1-I5 for one level; returns the FIFO length.
    fn check_level(&self, side: Side, px: Px, l: &Level) -> Result<usize, String> {
        let mut n = 0usize;
        let mut qty = 0u64;
        let mut i = l.head;
        let mut prev = NIL;
        while i != NIL {
            n += 1;
            if n > self.slab.capacity() {
                return Err(format!(
                    "I3: {side:?}@{px} FIFO longer than the slab (cycle?)"
                ));
            }
            let s = self.slab.get(i);
            if s.prev != prev {
                return Err(format!(
                    "I3: {side:?}@{px} slot {i} prev {} != {prev}",
                    s.prev
                ));
            }
            if s.side() != side || s.abs_px() != px {
                return Err(format!(
                    "I4: slot {i} ({:?}@{}) sits in {side:?}@{px}",
                    s.side(),
                    s.abs_px()
                ));
            }
            if s.qty == 0 {
                return Err(format!("I5: {side:?}@{px} slot {i} qty 0"));
            }
            if self.ids.get(s.id) != Some(i) {
                return Err(format!(
                    "I7: {side:?}@{px} slot {i} id {} not mapped to it",
                    s.id
                ));
            }
            qty += s.qty as u64;
            prev = i;
            i = s.next;
        }
        if prev != l.tail {
            return Err(format!("I3: {side:?}@{px} tail {} != last {prev}", l.tail));
        }
        if n as u32 != l.count {
            return Err(format!("I2: {side:?}@{px} count {} != FIFO {n}", l.count));
        }
        if qty != l.qty as u64 {
            return Err(format!("I1: {side:?}@{px} qty {} != sum {qty}", l.qty));
        }
        Ok(n)
    }
}

impl OrderBook for ArrayBook {
    fn add(&mut self, id: OrderId, side: Side, px: Px, qty: Qty) -> Result<Event, BookError> {
        if qty == 0 {
            return Err(BookError::BadQty);
        }
        if px <= 0 {
            return Err(BookError::BadPrice);
        }
        let locus = self.grid.locate(px);
        let grid = self.grid;
        let sl = match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        };
        if sl
            .get(locus)
            .is_some_and(|l| l.qty.checked_add(qty).is_none())
        {
            return Err(BookError::BadQty);
        }
        // One hash probe for both the duplicate check and the insert (add is ~40 % of ITCH).
        let slot = match self.ids.entry(id) {
            Entry::Occupied(_) => return Err(BookError::DuplicateId(id)),
            Entry::Vacant(v) => *v.insert(self.slab.alloc(id, qty, Slot::signed_px(side, px))),
        };
        sl.note_add(&grid, locus, px);
        Self::link_tail(self.parts(side), locus, slot);
        Ok(Event::add(id, side, px, qty))
    }

    fn cancel(&mut self, id: OrderId, qty: Qty) -> Result<Event, BookError> {
        let slot = self.ids.get(id).ok_or(BookError::UnknownId {
            kind: EventKind::Cancel,
            id,
        })?;
        if qty == 0 {
            return Err(BookError::BadQty);
        }
        let s = *self.slab.get(slot);
        let removed = qty.min(s.qty);
        let remaining = self.take(slot, s, removed);
        Ok(Event::cancel(id, s.side(), s.abs_px(), removed, remaining))
    }

    fn delete(&mut self, id: OrderId) -> Result<Event, BookError> {
        let slot = self.ids.get(id).ok_or(BookError::UnknownId {
            kind: EventKind::Delete,
            id,
        })?;
        let s = *self.slab.get(slot);
        self.take(slot, s, s.qty);
        Ok(Event::delete(id, s.side(), s.abs_px(), s.qty))
    }

    fn execute(&mut self, id: OrderId, qty: Qty) -> Result<Event, BookError> {
        let slot = self.ids.get(id).ok_or(BookError::UnknownId {
            kind: EventKind::Exec,
            id,
        })?;
        if qty == 0 {
            return Err(BookError::BadQty);
        }
        let s = *self.slab.get(slot);
        if qty > s.qty {
            return Err(BookError::OverExecute {
                id,
                have: s.qty,
                want: qty,
            });
        }
        let remaining = self.take(slot, s, qty);
        Ok(Event::exec(id, s.side(), s.abs_px(), qty, remaining))
    }

    fn replace(
        &mut self,
        old: OrderId,
        new: OrderId,
        px: Px,
        qty: Qty,
    ) -> Result<Event, BookError> {
        let slot = self.ids.get(old).ok_or(BookError::UnknownId {
            kind: EventKind::Replace,
            id: old,
        })?;
        if self.ids.contains(new) {
            return Err(BookError::DuplicateId(new));
        }
        if qty == 0 {
            return Err(BookError::BadQty);
        }
        if px <= 0 {
            return Err(BookError::BadPrice);
        }
        let s = *self.slab.get(slot);
        let side = s.side();
        let old_locus = self.grid.locate(s.abs_px());
        let new_locus = self.grid.locate(px);
        let grid = self.grid;
        let sl = match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        };
        let existing = if new_locus == old_locus {
            sl.get(old_locus).map_or(0, |l| l.qty) - s.qty
        } else {
            sl.get(new_locus).map_or(0, |l| l.qty)
        };
        if existing.checked_add(qty).is_none() {
            return Err(BookError::BadQty);
        }
        // Nothing below can fail: the freed slot is reused, the map has room for one more.
        sl.note_add(&grid, new_locus, px);
        self.ids.remove(old);
        Self::unlink(self.parts(side), old_locus, slot);
        let ns = self.slab.alloc(new, qty, Slot::signed_px(side, px));
        self.ids.insert(new, ns);
        Self::link_tail(self.parts(side), new_locus, ns);
        Ok(Event::replace(old, new, side, px, qty))
    }

    fn l1(&self) -> (Option<LevelView>, Option<LevelView>) {
        (
            self.bids
                .best(&self.grid)
                .map(|(px, l)| Self::level_view(px, l)),
            self.asks
                .best(&self.grid)
                .map(|(px, l)| Self::level_view(px, l)),
        )
    }

    fn l2(&self, depth: usize) -> (Vec<LevelView>, Vec<LevelView>) {
        let take = |sl: &SideLevels| -> Vec<LevelView> {
            sl.iter_best_first(&self.grid)
                .take(depth)
                .map(|(px, l)| Self::level_view(px, l))
                .collect()
        };
        (take(&self.bids), take(&self.asks))
    }

    fn queue_ahead(&self, id: OrderId) -> Option<Qty> {
        let slot = self.ids.get(id)?;
        let s = self.slab.get(slot);
        let locus = self.grid.locate(s.abs_px());
        let lvl = self.side_ref(s.side()).get(locus)?;
        let mut ahead = 0u32;
        let mut i = lvl.head;
        while i != slot {
            debug_assert!(i != NIL, "slot not reachable from its level head");
            let o = self.slab.get(i);
            ahead += o.qty;
            i = o.next;
        }
        Some(ahead)
    }

    fn live_orders(&self) -> usize {
        self.slab.live()
    }

    fn state_hash(&self) -> [u8; 32] {
        let mut h = StateHasher::new();
        for sl in [&self.bids, &self.asks] {
            for (px, l) in sl.iter_ascending(&self.grid) {
                h.level(px, l.count);
                let mut i = l.head;
                while i != NIL {
                    let s = self.slab.get(i);
                    h.order(s.id, s.qty);
                    i = s.next;
                }
            }
        }
        h.finish()
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Snapshot::new();
        for sl in [&self.bids, &self.asks] {
            for (px, l) in sl.iter_best_first(&self.grid) {
                out.push((sl.side, px, self.fifo(l)));
            }
        }
        out
    }
}

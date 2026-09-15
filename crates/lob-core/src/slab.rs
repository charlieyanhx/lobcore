//! Order slab: a `Vec<Slot>` arena with a `u32` free list, plus the id -> slot map.
//!
//! Invariant kept here (I7): live slots + free-list length == `slots.len()`.
//! The slot is 24 bytes: `{id u64, qty u32, px i32, prev u32, next u32}`; the side lives in the
//! sign of `px` (`+` bid, `-` ask). `NIL` (= `u32::MAX`) terminates the FIFO links and the free
//! list, which is threaded through `next`.
//!
//! Allocation policy: `alloc` reuses a freed slot when one exists and otherwise pushes onto the
//! `Vec`; after `reserve(n)` no push reallocates until more than `n` orders are live at once.

use std::collections::hash_map::Entry;

use rustc_hash::FxHashMap;

use crate::types::{OrderId, Px, Qty, Side};

/// Link terminator for `prev`, `next` and the free list.
pub const NIL: u32 = u32::MAX;

/// One resting order. `px` is signed: positive = bid, negative = ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Slot {
    pub id: OrderId,
    pub qty: Qty,
    pub px: Px,
    pub prev: u32,
    pub next: u32,
}

impl Slot {
    /// Decode the side from the sign of `px`.
    #[inline]
    pub const fn side(&self) -> Side {
        if self.px < 0 { Side::Ask } else { Side::Bid }
    }

    /// The unsigned public price.
    #[inline]
    pub const fn abs_px(&self) -> Px {
        self.px.abs()
    }

    /// Encode a side into the sign of a (positive) price.
    #[inline]
    pub const fn signed_px(side: Side, px: Px) -> Px {
        match side {
            Side::Bid => px,
            Side::Ask => -px,
        }
    }
}

/// Slab of slots with an intrusive free list.
#[derive(Debug, Default)]
pub struct Slab {
    slots: Vec<Slot>,
    free_head: u32,
    live: usize,
}

impl Slab {
    pub fn new() -> Slab {
        Slab {
            slots: Vec::new(),
            free_head: NIL,
            live: 0,
        }
    }

    pub fn with_capacity(n: usize) -> Slab {
        let mut s = Slab::new();
        s.reserve(n);
        s
    }

    /// Ensure `n` orders can be live without a reallocation.
    pub fn reserve(&mut self, n: usize) {
        let want = n.saturating_sub(self.slots.len() - self.live);
        self.slots.reserve(want);
    }

    /// Take a slot (from the free list first) and initialise it. Links are `NIL`.
    #[inline]
    pub fn alloc(&mut self, id: OrderId, qty: Qty, signed_px: Px) -> u32 {
        self.live += 1;
        let slot = Slot {
            id,
            qty,
            px: signed_px,
            prev: NIL,
            next: NIL,
        };
        if self.free_head != NIL {
            let i = self.free_head;
            self.free_head = self.slots[i as usize].next;
            self.slots[i as usize] = slot;
            i
        } else {
            let i = self.slots.len();
            assert!(i < NIL as usize, "slab full: more than u32::MAX-1 slots");
            self.slots.push(slot);
            i as u32
        }
    }

    /// Return a slot to the free list. The caller must have unlinked it from its level.
    #[inline]
    pub fn free(&mut self, i: u32) {
        debug_assert!(self.live > 0);
        self.live -= 1;
        let s = &mut self.slots[i as usize];
        s.id = 0;
        s.qty = 0;
        s.px = 0;
        s.prev = NIL;
        s.next = self.free_head;
        self.free_head = i;
    }

    #[inline]
    pub fn get(&self, i: u32) -> &Slot {
        &self.slots[i as usize]
    }

    #[inline]
    pub fn get_mut(&mut self, i: u32) -> &mut Slot {
        &mut self.slots[i as usize]
    }

    /// Number of live orders.
    #[inline]
    pub fn live(&self) -> usize {
        self.live
    }

    /// Total slots ever allocated (live + free).
    #[inline]
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Length of the free list, walked; `Err` if the list is cyclic or runs past capacity.
    pub fn free_len(&self) -> Result<usize, String> {
        let mut n = 0usize;
        let mut i = self.free_head;
        while i != NIL {
            n += 1;
            if n > self.slots.len() {
                return Err("free list longer than the slab (cycle?)".into());
            }
            i = self.slots[i as usize].next;
        }
        Ok(n)
    }
}

/// id -> slot index. `FxHashMap` because venue references are sparse, not dense.
#[derive(Debug, Default)]
pub struct IdMap {
    map: FxHashMap<OrderId, u32>,
}

impl IdMap {
    pub fn new() -> IdMap {
        IdMap {
            map: FxHashMap::default(),
        }
    }

    /// Ensure room for `n` live ids. hashbrown resizes only when live items exceed half the
    /// table capacity after tombstone churn, so reserve twice the expected high-water mark for
    /// an allocation-free steady state.
    pub fn reserve(&mut self, n: usize) {
        self.map.reserve(n);
    }

    #[inline]
    pub fn get(&self, id: OrderId) -> Option<u32> {
        self.map.get(&id).copied()
    }

    #[inline]
    pub fn contains(&self, id: OrderId) -> bool {
        self.map.contains_key(&id)
    }

    /// One-lookup duplicate check + insert for the add path.
    #[inline]
    pub fn entry(&mut self, id: OrderId) -> Entry<'_, OrderId, u32> {
        self.map.entry(id)
    }

    /// Insert a fresh id; the caller has already checked it is unknown.
    #[inline]
    pub fn insert(&mut self, id: OrderId, slot: u32) {
        let prev = self.map.insert(id, slot);
        debug_assert!(prev.is_none(), "duplicate id inserted into IdMap");
    }

    #[inline]
    pub fn remove(&mut self, id: OrderId) -> Option<u32> {
        self.map.remove(&id)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (OrderId, u32)> + '_ {
        self.map.iter().map(|(&k, &v)| (k, v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_is_24_bytes() {
        assert_eq!(std::mem::size_of::<Slot>(), 24);
        assert_eq!(std::mem::align_of::<Slot>(), 8);
    }

    #[test]
    fn free_list_reuses_most_recent_slot() {
        let mut s = Slab::new();
        let a = s.alloc(1, 10, 100);
        let b = s.alloc(2, 10, -100);
        assert_eq!((a, b), (0, 1));
        assert_eq!(s.get(b).side(), Side::Ask);
        assert_eq!(s.get(b).abs_px(), 100);
        s.free(a);
        assert_eq!(s.free_len(), Ok(1));
        assert_eq!(s.live(), 1);
        let c = s.alloc(3, 5, 7);
        assert_eq!(c, a);
        assert_eq!(s.capacity(), 2);
        assert_eq!(s.live() + s.free_len().unwrap(), s.capacity());
    }

    #[test]
    fn reserve_prevents_growth() {
        let mut s = Slab::with_capacity(8);
        let cap = s.slots.capacity();
        for i in 0..8 {
            s.alloc(i, 1, 1);
        }
        assert_eq!(s.slots.capacity(), cap);
    }
}

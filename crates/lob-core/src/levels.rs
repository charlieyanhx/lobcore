//! Per-side price levels: a bounded array indexed by tick offset from a base price, a two-level
//! `u64` bitmap for O(1) best tracking, and an overflow `BTreeMap` for off-grid or out-of-range
//! prices.
//!
//! Layout: `Level = {head u32, tail u32, qty u32, count u32}` (16 bytes). Index
//! `i = (px - base) / tick` for `0 <= i < n_levels` and `(px - base) % tick == 0`; every other
//! positive price goes to the overflow map, keyed by the public (positive) price. `n_levels`
//! <= 4096 so one `u64` summary word covers the <= 64 bitmap words.
//!
//! Invariant kept here (I6): bitmap bit i == (levels[i].count > 0); summary bit w ==
//! (words[w] != 0). Overflow levels are removed from the map when their count reaches 0, so
//! every map entry is non-empty and best() is `first_key_value` / `last_key_value`.
//!
//! Counters: `overflow_hits` = adds routed to the overflow map; `max_abs_offset` = the largest
//! `|(px - base) / tick|` seen on any add (in-range or not), so v0.2 can size a re-centring policy.

use std::collections::BTreeMap;

use crate::slab::NIL;
use crate::types::{Px, Qty, Side};

/// Largest supported window; one summary word covers 64 words of 64 bits.
pub const MAX_LEVELS: u32 = 4096;

/// One price level: FIFO head/tail slot indices, total quantity, order count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Level {
    pub head: u32,
    pub tail: u32,
    pub qty: Qty,
    pub count: u32,
}

impl Level {
    pub const EMPTY: Level = Level {
        head: NIL,
        tail: NIL,
        qty: 0,
        count: 0,
    };

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }
}

impl Default for Level {
    fn default() -> Self {
        Level::EMPTY
    }
}

/// Two-level occupancy bitmap over `n <= 4096` indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    words: Vec<u64>,
    summary: u64,
}

impl Bitmap {
    pub fn new(n: u32) -> Bitmap {
        assert!(
            n <= MAX_LEVELS,
            "bitmap supports at most {MAX_LEVELS} levels"
        );
        Bitmap {
            words: vec![0u64; n.div_ceil(64) as usize],
            summary: 0,
        }
    }

    #[inline]
    pub fn set(&mut self, i: u32) {
        let w = (i >> 6) as usize;
        self.words[w] |= 1u64 << (i & 63);
        self.summary |= 1u64 << w;
    }

    #[inline]
    pub fn clear(&mut self, i: u32) {
        let w = (i >> 6) as usize;
        self.words[w] &= !(1u64 << (i & 63));
        if self.words[w] == 0 {
            self.summary &= !(1u64 << w);
        }
    }

    #[inline]
    pub fn is_set(&self, i: u32) -> bool {
        (self.words[(i >> 6) as usize] >> (i & 63)) & 1 == 1
    }

    /// Highest set index (best bid): two `leading_zeros` ops, no scan.
    #[inline]
    pub fn max(&self) -> Option<u32> {
        if self.summary == 0 {
            return None;
        }
        let w = 63 - self.summary.leading_zeros();
        let b = 63 - self.words[w as usize].leading_zeros();
        Some(w * 64 + b)
    }

    /// Lowest set index (best ask): two `trailing_zeros` ops, no scan.
    #[inline]
    pub fn min(&self) -> Option<u32> {
        if self.summary == 0 {
            return None;
        }
        let w = self.summary.trailing_zeros();
        let b = self.words[w as usize].trailing_zeros();
        Some(w * 64 + b)
    }

    /// Next set index strictly below `i`.
    #[inline]
    pub fn prev_set(&self, i: u32) -> Option<u32> {
        let mut w = (i >> 6) as usize;
        let bit = i & 63;
        let mut word = if bit == 0 {
            0
        } else {
            self.words[w] & ((1u64 << bit) - 1)
        };
        loop {
            if word != 0 {
                return Some(w as u32 * 64 + (63 - word.leading_zeros()));
            }
            if w == 0 {
                return None;
            }
            let below = self.summary & ((1u64 << w) - 1);
            if below == 0 {
                return None;
            }
            w = (63 - below.leading_zeros()) as usize;
            word = self.words[w];
        }
    }

    /// Next set index strictly above `i`.
    #[inline]
    pub fn next_set(&self, i: u32) -> Option<u32> {
        let mut w = (i >> 6) as usize;
        let bit = i & 63;
        let mut word = if bit == 63 {
            0
        } else {
            self.words[w] & !((2u64 << bit) - 1)
        };
        loop {
            if word != 0 {
                return Some(w as u32 * 64 + word.trailing_zeros());
            }
            if w >= 63 {
                return None;
            }
            let above = self.summary & !((2u64 << w) - 1);
            if above == 0 {
                return None;
            }
            w = above.trailing_zeros() as usize;
            word = self.words[w];
        }
    }

    pub fn words(&self) -> &[u64] {
        &self.words
    }

    pub fn summary(&self) -> u64 {
        self.summary
    }
}

/// Where a public price lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locus {
    InRange(u32),
    Overflow(Px),
}

/// Grid arithmetic shared by both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub base_px: Px,
    pub n_levels: u32,
    pub tick: Px,
}

impl Grid {
    #[inline]
    pub fn locate(&self, px: Px) -> Locus {
        let d = px as i64 - self.base_px as i64;
        let t = self.tick as i64;
        if d >= 0 && d % t == 0 {
            let i = d / t;
            if i < self.n_levels as i64 {
                return Locus::InRange(i as u32);
            }
        }
        Locus::Overflow(px)
    }

    /// Signed tick offset (integer division toward zero) used for `max_abs_offset`.
    #[inline]
    pub fn offset(&self, px: Px) -> i64 {
        (px as i64 - self.base_px as i64) / self.tick as i64
    }

    #[inline]
    pub fn px_of(&self, i: u32) -> Px {
        self.base_px + i as Px * self.tick
    }
}

/// One side of the book.
#[derive(Debug)]
pub struct SideLevels {
    pub side: Side,
    pub levels: Vec<Level>,
    pub bitmap: Bitmap,
    pub overflow: BTreeMap<Px, Level>,
    overflow_hits: u64,
    max_abs_offset: i64,
}

impl SideLevels {
    pub fn new(side: Side, n_levels: u32) -> SideLevels {
        SideLevels {
            side,
            levels: vec![Level::EMPTY; n_levels as usize],
            bitmap: Bitmap::new(n_levels),
            overflow: BTreeMap::new(),
            overflow_hits: 0,
            max_abs_offset: 0,
        }
    }

    pub fn overflow_hits(&self) -> u64 {
        self.overflow_hits
    }

    pub fn max_abs_offset(&self) -> i64 {
        self.max_abs_offset
    }

    /// Record the counters for an add at `px`.
    #[inline]
    pub fn note_add(&mut self, grid: &Grid, locus: Locus, px: Px) {
        let off = grid.offset(px).abs();
        if off > self.max_abs_offset {
            self.max_abs_offset = off;
        }
        if matches!(locus, Locus::Overflow(_)) {
            self.overflow_hits += 1;
        }
    }

    /// Existing level at `locus`, if any (overflow levels exist only while non-empty).
    #[inline]
    pub fn get(&self, locus: Locus) -> Option<&Level> {
        match locus {
            Locus::InRange(i) => Some(&self.levels[i as usize]),
            Locus::Overflow(px) => self.overflow.get(&px),
        }
    }

    /// Mutable level at `locus`, creating an empty overflow entry when needed (allocates on the
    /// overflow path only).
    #[inline]
    pub fn get_or_insert(&mut self, locus: Locus) -> &mut Level {
        match locus {
            Locus::InRange(i) => &mut self.levels[i as usize],
            Locus::Overflow(px) => self.overflow.entry(px).or_insert(Level::EMPTY),
        }
    }

    /// Mutable level at `locus`; the caller guarantees it exists.
    #[inline]
    pub fn get_mut(&mut self, locus: Locus) -> &mut Level {
        match locus {
            Locus::InRange(i) => &mut self.levels[i as usize],
            Locus::Overflow(px) => self.overflow.get_mut(&px).expect("overflow level exists"),
        }
    }

    /// Maintain the bitmap / map after a level's count changed from `before` to its new value.
    #[inline]
    pub fn after_count_change(&mut self, locus: Locus, before: u32) {
        match locus {
            Locus::InRange(i) => {
                let now = self.levels[i as usize].count;
                if before == 0 && now > 0 {
                    self.bitmap.set(i);
                } else if before > 0 && now == 0 {
                    self.bitmap.clear(i);
                }
            }
            Locus::Overflow(px) => {
                if self.overflow.get(&px).is_some_and(Level::is_empty) {
                    self.overflow.remove(&px);
                }
            }
        }
    }

    /// Best price and level, comparing the array best with the overflow extreme.
    #[inline]
    pub fn best(&self, grid: &Grid) -> Option<(Px, &Level)> {
        let arr = match self.side {
            Side::Bid => self.bitmap.max(),
            Side::Ask => self.bitmap.min(),
        }
        .map(|i| (grid.px_of(i), &self.levels[i as usize]));
        let ovf = match self.side {
            Side::Bid => self.overflow.last_key_value(),
            Side::Ask => self.overflow.first_key_value(),
        }
        .map(|(&px, l)| (px, l));
        match (arr, ovf) {
            (Some(a), Some(o)) => Some(match self.side {
                Side::Bid if o.0 > a.0 => o,
                Side::Ask if o.0 < a.0 => o,
                _ => a,
            }),
            (a, o) => a.or(o),
        }
    }

    /// Non-empty levels, best first, merging the array and the overflow map.
    pub fn iter_best_first<'a>(&'a self, grid: &'a Grid) -> BestFirst<'a> {
        let arr_cursor = match self.side {
            Side::Bid => self.bitmap.max(),
            Side::Ask => self.bitmap.min(),
        };
        BestFirst {
            side: self,
            grid,
            arr_cursor,
            ovf: self.overflow.iter(),
            ovf_peek: None,
            primed: false,
        }
    }

    /// Non-empty levels, ascending price (the book-state hash order).
    pub fn iter_ascending<'a>(
        &'a self,
        grid: &'a Grid,
    ) -> Box<dyn Iterator<Item = (Px, &'a Level)> + 'a> {
        match self.side {
            Side::Ask => Box::new(self.iter_best_first(grid)),
            Side::Bid => {
                let mut v: Vec<(Px, &Level)> = self.iter_best_first(grid).collect();
                v.reverse();
                Box::new(v.into_iter())
            }
        }
    }
}

/// Merge iterator over array levels (via the bitmap) and overflow levels, best first.
pub struct BestFirst<'a> {
    side: &'a SideLevels,
    grid: &'a Grid,
    arr_cursor: Option<u32>,
    ovf: std::collections::btree_map::Iter<'a, Px, Level>,
    ovf_peek: Option<(Px, &'a Level)>,
    primed: bool,
}

impl std::fmt::Debug for BestFirst<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BestFirst")
            .field("side", &self.side.side)
            .field("arr_cursor", &self.arr_cursor)
            .field("ovf_peek", &self.ovf_peek)
            .finish()
    }
}

impl<'a> BestFirst<'a> {
    fn pull_ovf(&mut self) -> Option<(Px, &'a Level)> {
        match self.side.side {
            Side::Bid => self.ovf.next_back().map(|(&p, l)| (p, l)),
            Side::Ask => self.ovf.next().map(|(&p, l)| (p, l)),
        }
    }
}

impl<'a> Iterator for BestFirst<'a> {
    type Item = (Px, &'a Level);

    fn next(&mut self) -> Option<Self::Item> {
        if !self.primed {
            self.ovf_peek = self.pull_ovf();
            self.primed = true;
        }
        let arr = self
            .arr_cursor
            .map(|i| (self.grid.px_of(i), &self.side.levels[i as usize]));
        let take_ovf = match (arr, self.ovf_peek) {
            (None, None) => return None,
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (Some(a), Some(o)) => match self.side.side {
                Side::Bid => o.0 > a.0,
                Side::Ask => o.0 < a.0,
            },
        };
        if take_ovf {
            let o = self.ovf_peek.take();
            self.ovf_peek = self.pull_ovf();
            o
        } else {
            let i = self.arr_cursor.expect("array cursor present");
            self.arr_cursor = match self.side.side {
                Side::Bid => self.side.bitmap.prev_set(i),
                Side::Ask => self.side.bitmap.next_set(i),
            };
            arr
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_is_16_bytes() {
        assert_eq!(std::mem::size_of::<Level>(), 16);
    }

    #[test]
    fn bitmap_oracle_5_130_1000() {
        // report_core bitmap_best_tracking: 2048 levels; set 5, 130, 1000.
        let mut b = Bitmap::new(2048);
        for i in [5, 130, 1000] {
            b.set(i);
        }
        let set_words: Vec<usize> = b
            .words()
            .iter()
            .enumerate()
            .filter(|(_, w)| **w != 0)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(set_words, vec![0, 2, 15]);
        assert_eq!(b.summary(), 0b1000000000000101);
        assert_eq!(b.max(), Some(1000));
        assert_eq!(b.min(), Some(5));
        b.clear(1000);
        assert_eq!(b.max(), Some(130));
        assert_eq!(b.summary(), 0b101);
        assert_eq!(b.prev_set(130), Some(5));
        assert_eq!(b.prev_set(5), None);
        assert_eq!(b.next_set(5), Some(130));
        assert_eq!(b.next_set(130), None);
        b.clear(5);
        b.clear(130);
        assert_eq!(b.summary(), 0);
        assert_eq!(b.max(), None);
    }

    #[test]
    fn bitmap_neighbours_cross_word_boundaries() {
        let mut b = Bitmap::new(4096);
        for i in [0, 63, 64, 127, 4095] {
            b.set(i);
        }
        assert_eq!(b.next_set(0), Some(63));
        assert_eq!(b.next_set(63), Some(64));
        assert_eq!(b.next_set(64), Some(127));
        assert_eq!(b.next_set(127), Some(4095));
        assert_eq!(b.next_set(4095), None);
        assert_eq!(b.prev_set(4095), Some(127));
        assert_eq!(b.prev_set(64), Some(63));
        assert_eq!(b.prev_set(63), Some(0));
        assert_eq!(b.prev_set(0), None);
    }

    #[test]
    fn grid_locates_off_grid_and_out_of_range_to_overflow() {
        let g = Grid {
            base_px: 10000,
            n_levels: 64,
            tick: 100,
        };
        assert_eq!(g.locate(10000), Locus::InRange(0));
        assert_eq!(g.locate(16300), Locus::InRange(63));
        assert_eq!(g.locate(16400), Locus::Overflow(16400));
        assert_eq!(g.locate(10050), Locus::Overflow(10050));
        assert_eq!(g.locate(9900), Locus::Overflow(9900));
        assert_eq!(g.offset(9900), -1);
        assert_eq!(g.offset(9950), 0);
    }
}

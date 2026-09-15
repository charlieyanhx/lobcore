//! "No per-message heap allocation" is a tested claim, not a slogan: a counting
//! `#[global_allocator]` wraps `System`, the book is warmed up (`reserve` for the live-order
//! high-water mark, then a burst that touches every level and settles the slab and map), and
//! 100,000 in-range ops must then run with the allocation counter unchanged.
//!
//! The op stream is generated inline (splitmix64) and the live-id set lives in a pre-sized
//! `Vec` with `swap_remove`, so the harness itself allocates nothing during the measured window.
//! The overflow map is deliberately not exercised: it is the documented allocating path.
//!
//! This is the one `unsafe` in lob-core's tree and it is test-only: a `GlobalAlloc`
//! implementation cannot be written without it. The library crate keeps `deny(unsafe_code)`.

#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use lob_core::{ArrayBook, BookConfig, EventKind, OrderBook, Side};

struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to `System` unchanged and only bumps an atomic counter, so
// the allocator contract (valid layouts in, matching allocations out) is exactly `System`'s.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        REALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn counters() -> (usize, usize) {
    (
        ALLOCS.load(Ordering::SeqCst),
        REALLOCS.load(Ordering::SeqCst),
    )
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

const N_LEVELS: u32 = 2048;
const BASE: i32 = 10_000;
const MAX_LIVE: usize = 4_096;

/// Allocation-free driver: keeps the live set in a pre-sized `Vec`, prices inside the window.
struct Driver {
    rng: SplitMix64,
    live: Vec<u64>,
    next_id: u64,
    ops: u64,
    errors: u64,
}

impl Driver {
    fn new(seed: u64) -> Driver {
        Driver {
            rng: SplitMix64(seed),
            live: Vec::with_capacity(MAX_LIVE),
            next_id: 1,
            ops: 0,
            errors: 0,
        }
    }

    fn step(&mut self, book: &mut ArrayBook) {
        let r = self.rng.next();
        let roll = r % 100;
        let px = BASE + (r / 100 % N_LEVELS as u64) as i32;
        let qty = 1 + (r / 100_000 % 100) as u32;
        let side = if (r >> 40) & 1 == 0 {
            Side::Bid
        } else {
            Side::Ask
        };
        let pick = (r >> 41) as usize;
        self.ops += 1;
        // Keep the live set under MAX_LIVE: once it is large, bias toward removals.
        let add_share = if self.live.len() > MAX_LIVE / 2 {
            20
        } else {
            55
        };
        let res = if roll < add_share || self.live.is_empty() {
            let id = self.next_id;
            self.next_id += 1;
            let res = book.add(id, side, px, qty);
            if res.is_ok() {
                self.live.push(id);
            }
            res
        } else {
            let pos = pick % self.live.len();
            let id = self.live[pos];
            let res = match roll % 4 {
                0 => book.cancel(id, qty),
                1 => book.execute(id, qty.min(qty / 2 + 1)),
                2 => book.delete(id),
                _ => {
                    let new = self.next_id;
                    self.next_id += 1;
                    book.replace(id, new, px, qty)
                }
            };
            if let Ok(e) = res {
                match e.kind {
                    EventKind::Replace => self.live[pos] = e.b,
                    EventKind::Delete => {
                        self.live.swap_remove(pos);
                    }
                    _ if e.b == 0 => {
                        self.live.swap_remove(pos);
                    }
                    _ => {}
                }
            }
            res
        };
        if res.is_err() {
            // Only over-execute can fail here (qty > remaining); count it, nothing else to do.
            self.errors += 1;
        }
    }
}

#[test]
fn hot_path_makes_zero_allocations_after_warm_up() {
    let mut book = ArrayBook::with_capacity(BookConfig::new(BASE, N_LEVELS, 1), MAX_LIVE);
    let mut drv = Driver::new(7);
    // Warm-up: touch every level on both sides, then churn until the slab and map settle.
    for i in 0..N_LEVELS {
        book.add(drv.next_id, Side::Bid, BASE + i as i32, 1)
            .unwrap();
        drv.live.push(drv.next_id);
        drv.next_id += 1;
        book.add(drv.next_id, Side::Ask, BASE + i as i32, 1)
            .unwrap();
        drv.live.push(drv.next_id);
        drv.next_id += 1;
    }
    for _ in 0..50_000 {
        drv.step(&mut book);
    }
    book.check().unwrap();
    let live_before = book.live_orders();
    assert!(
        live_before > 0 && live_before <= MAX_LIVE,
        "warm-up live {live_before}"
    );

    let before = counters();
    let ops_before = drv.ops;
    for _ in 0..100_000 {
        drv.step(&mut book);
    }
    let after = counters();
    assert_eq!(drv.ops - ops_before, 100_000);
    assert_eq!(
        after, before,
        "allocations during 100,000 hot-path ops: {before:?} -> {after:?}"
    );

    book.check().unwrap();
    assert!(book.live_orders() <= MAX_LIVE);
    assert_eq!(book.overflow_hits(), 0);
    assert!(book.slab_capacity() <= MAX_LIVE + 2 * N_LEVELS as usize);
    // l1 does not allocate either; l2 / snapshot return Vecs and are excluded by design.
    let before = counters();
    let _ = book.l1();
    let _ = book.queue_ahead(drv.live[0]);
    assert_eq!(counters(), before);
}

//! "No per-message heap allocation" for the feed path: a counting `#[global_allocator]` wraps
//! `System`; a synthetic day of 1,000,000 messages (placeholders off, 4,096-level windows so no
//! price leaves the array) is framed from memory and applied by a session with every locate on
//! the array book. The first 100,000 messages are the warm-up (directory, window creation,
//! slab and id-map growth); the remaining 900,000 must run with the allocation and
//! reallocation counters unchanged. Overflow hits are asserted zero so the test is meaningful:
//! the overflow map is the documented allocating path.
//!
//! The only `unsafe` in lob-feed's tree, test-only, mirroring lob-core's test: a
//! `GlobalAlloc` cannot be written without it.

#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use lob_feed::{FrameSource, Session, SessionConfig, SliceFrames, Watchlist};
use lob_synth::{SynthConfig, synth_itch_bytes};

struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to `System` unchanged and only bumps an atomic counter, so
// the allocator contract is exactly `System`'s.
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

const N: u64 = 1_000_000;
const WARM: u64 = 100_000;

#[test]
fn one_million_messages_allocate_nothing_after_warm_up() {
    let cfg = SynthConfig {
        placeholder_rate: 0.0,
        subpenny: false,
        ..Default::default()
    };
    let bytes = synth_itch_bytes(11, N, &cfg);
    let mut s = Session::new(SessionConfig {
        watchlist: Watchlist::all(),
        array_window: 4096,
        array_reserve: 16_384,
        ..Default::default()
    });
    let mut f = SliceFrames::new(&bytes);
    let mut n = 0u64;
    while n < WARM {
        let p = f.next_frame().unwrap().expect("warm-up frames");
        s.apply_payload(p).unwrap();
        n += 1;
    }
    let before = counters();
    while let Some(p) = f.next_frame().unwrap() {
        s.apply_payload(p).unwrap();
        n += 1;
    }
    let after = counters();
    assert_eq!(n, N);
    assert_eq!(
        after, before,
        "allocations (allocs, reallocs) during the measured 900,000 messages"
    );
    s.finish();
    let st = s.stats();
    assert_eq!(st.unknown_id, 0);
    assert_eq!(st.negative_qty(), 0);
    for (l, ls) in st.locates_with_book() {
        assert!(ls.array, "locate {l} on the array book");
        assert_eq!(ls.overflow_hits, 0, "locate {l}: no price left the window");
        assert!(
            ls.live_hwm as usize <= 16_384,
            "locate {l}: reserve covers the HWM"
        );
    }
    // the session's own allocations after warm-up would also show up above; l1 and
    // queue_ahead on the array book are allocation-free as well
    let b = counters();
    let book = s.book(1).unwrap();
    let _ = book.l1();
    let _ = book.queue_ahead(1);
    assert_eq!(counters(), b);
}

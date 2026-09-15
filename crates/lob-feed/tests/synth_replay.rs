//! The synthetic day through the session: every framed length equals the size table (0
//! mismatches), the committed fixture's type histogram is pinned, and after EVERY message the
//! session's book for that message's locate equals the generator's truth row (best bid / ask,
//! five-level quantities, live orders, book-state hash) — for the committed seed-7 fixture with
//! every locate on the array book, and for seeds 1-3 generated at test time (all-array, all-
//! reference, and a two-symbol watchlist) with a 5 % stale-reference rate and C/P/L in the mix.

use lob_core::OrderBook;
use lob_feed::itch::msg::{parse, spec_len};
use lob_feed::{FrameSource, Session, SessionConfig, SliceFrames, Watchlist};
use lob_synth::{SynthConfig, SynthDay, Truth, synth_itch};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/synth_s7_100k.itch"
);

fn fixture_bytes() -> Vec<u8> {
    std::fs::read(FIXTURE).expect("committed fixture tests/fixtures/synth_s7_100k.itch")
}

fn l5(book: &dyn OrderBook) -> ([u32; 5], [u32; 5]) {
    let (b, a) = book.l2(5);
    let mut lb = [0u32; 5];
    let mut la = [0u32; 5];
    for (i, l) in b.iter().enumerate() {
        lb[i] = l.1;
    }
    for (i, l) in a.iter().enumerate() {
        la[i] = l.1;
    }
    (lb, la)
}

const EMPTY_HASH: [u8; 32] = [
    0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24,
    0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
];

fn assert_truth(s: &Session, t: &Truth, i: usize) {
    match s.book(t.locate) {
        None => {
            assert_eq!(t.best_bid, None, "msg {i}: no book but truth has a bid");
            assert_eq!(t.best_ask, None, "msg {i}");
            assert_eq!(t.live_orders, 0, "msg {i}");
            assert_eq!(t.book_hash, EMPTY_HASH, "msg {i}");
        }
        Some(book) => {
            let (b, a) = book.l1();
            assert_eq!(b.map(|l| (l.0, l.1)), t.best_bid, "msg {i} best bid");
            assert_eq!(a.map(|l| (l.0, l.1)), t.best_ask, "msg {i} best ask");
            let (lb, la) = l5(book);
            assert_eq!(lb, t.l5_bid, "msg {i} l5 bid");
            assert_eq!(la, t.l5_ask, "msg {i} l5 ask");
            assert_eq!(book.live_orders() as u32, t.live_orders, "msg {i} live");
            assert_eq!(book.state_hash(), t.book_hash, "msg {i} hash");
        }
    }
}

/// Replay `day` through a session with `watchlist`, checking the truth after every message.
fn replay_against_truth(day: &SynthDay, watchlist: Watchlist, label: &str) -> Session {
    let mut s = Session::new(SessionConfig {
        watchlist,
        source: label.into(),
        ..Default::default()
    });
    let mut f = SliceFrames::new(&day.bytes);
    let mut i = 0usize;
    while let Some(p) = f.next_frame().unwrap() {
        s.apply_payload(p).unwrap();
        assert_truth(&s, &day.truth[i], i);
        i += 1;
    }
    assert_eq!(i, day.truth.len(), "{label}: one truth row per frame");
    assert_eq!(f.truncated(), 0);
    s.finish();
    s
}

#[test]
fn fixture_lengths_match_the_size_table_and_histogram_is_pinned() {
    let bytes = fixture_bytes();
    let mut hist = [0u64; 256];
    let mut mismatches = 0u64;
    let mut n = 0u64;
    let mut frames = SliceFrames::new(&bytes);
    for p in frames.by_ref() {
        n += 1;
        hist[p[0] as usize] += 1;
        if spec_len(p[0]) != Some(p.len()) {
            mismatches += 1;
        }
        assert!(parse(p).is_ok());
    }
    assert_eq!(frames.truncated(), 0);
    assert_eq!(mismatches, 0);
    assert_eq!(n, 100_000);
    let pinned: &[(u8, u64)] = &[
        (b'S', 6),
        (b'R', 8),
        (b'H', 8),
        (b'A', 40086),
        (b'F', 943),
        (b'D', 37219),
        (b'X', 11134),
        (b'U', 4845),
        (b'E', 668),
        (b'P', 96),
        (b'L', 4987),
    ];
    let mut total = 0;
    for &(t, c) in pinned {
        assert_eq!(hist[t as usize], c, "type {}", t as char);
        total += c;
    }
    assert_eq!(total, 100_000);
}

#[test]
fn fixture_replay_matches_truth_after_every_message() {
    let bytes = fixture_bytes();
    let day = synth_itch(7, 100_000, &SynthConfig::default());
    assert_eq!(
        day.bytes, bytes,
        "the generator no longer reproduces the fixture"
    );
    let s = replay_against_truth(&day, Watchlist::all(), "synth_s7_100k.itch");
    let st = s.stats();
    assert_eq!(st.unknown_id, 0);
    assert_eq!(st.negative_qty(), 0);
    assert_eq!(st.duplicate_id, 0);
    assert_eq!(st.bad_qty + st.bad_price + st.bad_side, 0);
    assert_eq!(st.no_directory, 0);
    assert_eq!(st.crossed_after_q_trading, 0, "uncrossed after Q");
    assert!(
        st.crossed_before_q > 0,
        "the default config crosses pre-open"
    );
    assert_eq!(st.ec_total, 668);
    assert_eq!(
        st.ec_at_head, 668,
        "the generator executes level heads only"
    );
    assert_eq!(st.live, 3_649);
    assert_eq!(st.live_hwm, 3_657);
    assert_eq!(st.locates_with_book().count(), 8);
    assert!(st.locates_with_book().all(|(_, l)| l.array && l.watched));
    for (l, ls) in st.locates_with_book() {
        let last = day.truth.iter().rev().find(|t| t.locate == l).unwrap();
        assert_eq!(ls.close_hash, last.book_hash, "close hash locate {l}");
        assert_eq!(ls.live, last.live_orders);
    }
    // 5 of the 8 locates are on the 1c grid at or above $1, 3 are sub-penny; every window is
    // centred on the first non-placeholder add.
    assert!(st.locates_with_book().all(|(_, l)| l.window.is_some()));
    let text = st.render();
    assert!(text.contains("| messages (complete frames) | 100,000 |"));
    assert!(text.contains("| unknown-id | 0 |"));
    eprintln!("{text}");
}

#[test]
fn seeds_1_to_3_with_stale_refs_match_truth_on_both_book_kinds() {
    let cfg = SynthConfig {
        unknown_ref_rate: 0.05,
        mix: [0.36, 0.02, 0.33, 0.11, 0.05, 0.03, 0.03, 0.02, 0.05],
        ..Default::default()
    };
    let lists = [
        Watchlist::all(),
        Watchlist::none(),
        Watchlist::symbols(["SYN0002", "SYN0005"]),
    ];
    for (seed, wl) in (1u64..=3).zip(lists) {
        let day = synth_itch(seed, 40_000, &cfg);
        let s = replay_against_truth(&day, wl.clone(), &format!("seed {seed}"));
        let st = s.stats();
        assert!(st.unknown_id > 0, "seed {seed}: stale refs must be counted");
        assert_eq!(st.negative_qty(), 0);
        assert!(st.by_type[b'C' as usize] > 0, "seed {seed}: C in the mix");
        let arrays = st.locates_with_book().filter(|(_, l)| l.array).count();
        let expect = if wl.all { 8 } else { wl.symbols.len() };
        assert_eq!(arrays, expect, "seed {seed}: array books");
        assert_eq!(st.ec_at_head, st.ec_total, "seed {seed}");
    }
}

#[test]
fn streaming_framer_gives_the_same_stats_as_the_slice_framer() {
    let bytes = fixture_bytes();
    let mut a = Session::new(SessionConfig::default());
    a.replay(&mut SliceFrames::new(&bytes)).unwrap();
    let mut b = Session::new(SessionConfig::default());
    b.replay(&mut lob_feed::Framer::with_buffer(&bytes[..], 1))
        .unwrap();
    let mut c = Session::new(SessionConfig::default());
    c.replay(&mut lob_feed::open(FIXTURE).unwrap()).unwrap();
    for s in [&a, &b, &c] {
        assert_eq!(s.stats().messages, 100_000);
        assert_eq!(s.stats().bytes_in, bytes.len() as u64);
        assert!(s.stats().timing.is_some());
    }
    let key = |s: &Session| {
        let st = s.stats();
        (
            st.event_log_hash,
            st.all_locates_hash,
            st.live_hwm,
            st.crossed_before_q,
            st.unknown_id,
        )
    };
    assert_eq!(key(&a), key(&b));
    assert_eq!(key(&a), key(&c));
    // reference books everywhere: the same close hashes as the array-book run
    let day = synth_itch(7, 100_000, &SynthConfig::default());
    for (l, ls) in a.stats().locates_with_book() {
        let last = day.truth.iter().rev().find(|t| t.locate == l).unwrap();
        assert_eq!(ls.close_hash, last.book_hash);
        assert!(!ls.array);
    }
}

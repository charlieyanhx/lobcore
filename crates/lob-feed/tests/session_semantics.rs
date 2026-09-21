//! Session semantics on hand-built streams that the synthetic day cannot produce (it has no
//! rejected messages): `apply_msg` reports whether the book changed, the E/C-at-head fraction
//! counts executions only, and a rejected add never centres a watched locate's array window.

use lob_feed::itch::msg::parse;
use lob_feed::{FrameSource, Session, SessionConfig, SliceFrames, Watchlist};
use lob_synth::itch::{self, Header, symbol8};

const LOC: u16 = 5;

fn hdr(ts: u64) -> Header {
    Header {
        locate: LOC,
        tracking: 0,
        ts,
    }
}

fn session(watch: Watchlist) -> Session {
    Session::new(SessionConfig {
        watchlist: watch,
        source: "hand-built".into(),
        ..Default::default()
    })
}

/// Apply every frame of `bytes`; returns `apply_msg`'s answer per frame.
fn apply_all(s: &mut Session, bytes: &[u8]) -> Vec<bool> {
    let mut f = SliceFrames::new(bytes);
    let mut changed = Vec::new();
    while let Some(p) = f.next_frame().unwrap() {
        changed.push(s.apply_msg(parse(p).unwrap()));
    }
    s.finish();
    changed
}

#[test]
fn apply_msg_is_true_only_when_the_book_changed() {
    let sym = symbol8("SYNX");
    let mut out = Vec::new();
    itch::system_event(
        &mut out,
        &Header {
            locate: 0,
            tracking: 0,
            ts: 1,
        },
        b'O',
    );
    itch::stock_directory(&mut out, &hdr(2), &sym);
    itch::add_order(&mut out, &hdr(3), 1, b'B', 100, &sym, 1_000_000); // accepted
    itch::add_order(&mut out, &hdr(4), 1, b'B', 100, &sym, 1_000_000); // duplicate id
    itch::order_delete(&mut out, &hdr(5), 999); // unknown id
    itch::add_order(&mut out, &hdr(6), 2, b'X', 100, &sym, 1_000_000); // bad side byte
    itch::order_executed(&mut out, &hdr(7), 1, 150, 1); // over-execute (100 resting)
    itch::order_replace(&mut out, &hdr(8), 1, 1, 50, 1_000_100); // new == old
    itch::order_cancel(&mut out, &hdr(9), 1, 40); // accepted
    itch::trade(&mut out, &hdr(10), 5, &sym, 1_000_000, 2); // P never touches the book
    itch::order_executed(&mut out, &hdr(11), 1, 60, 3); // accepted, fills the order
    let mut s = session(Watchlist::none());
    let changed = apply_all(&mut s, &out);
    assert_eq!(
        changed,
        [
            false, false, true, false, false, false, false, false, true, false, true
        ]
    );
    let st = s.stats();
    assert_eq!(st.duplicate_id, 2, "duplicate add + replace to itself");
    assert_eq!(st.unknown_id, 1);
    assert_eq!(st.bad_side, 1);
    assert_eq!(st.over_execute, 1);
    assert_eq!(
        st.event_count, 7,
        "every book call is logged, accepted or not (bad side never reaches the book)"
    );
    assert_eq!(st.live, 0);
}

#[test]
fn ec_at_head_fraction_counts_executions_not_attempts() {
    let sym = symbol8("SYNX");
    let mut out = Vec::new();
    itch::stock_directory(&mut out, &hdr(1), &sym);
    itch::add_order(&mut out, &hdr(2), 1, b'B', 100, &sym, 1_000_000);
    itch::add_order(&mut out, &hdr(3), 2, b'B', 100, &sym, 1_000_000); // behind id 1
    itch::order_executed(&mut out, &hdr(4), 2, 500, 1); // rejected: over-execute, not an execution
    itch::order_executed(&mut out, &hdr(5), 7, 10, 2); // rejected: unknown id
    itch::order_executed(&mut out, &hdr(6), 2, 10, 3); // accepted, 100 shares ahead
    itch::order_executed(&mut out, &hdr(7), 1, 10, 4); // accepted, at the head
    for watch in [Watchlist::none(), Watchlist::all()] {
        let mut s = session(watch);
        apply_all(&mut s, &out);
        let st = s.stats();
        assert_eq!((st.ec_total, st.ec_at_head), (2, 1));
        assert_eq!((st.over_execute, st.unknown_id), (1, 1));
    }
}

#[test]
fn a_rejected_add_never_centres_the_array_window() {
    let sym = symbol8("SYNX");
    let mut out = Vec::new();
    itch::stock_directory(&mut out, &hdr(1), &sym);
    itch::add_order(&mut out, &hdr(2), 1, b'B', 10, &sym, 100); // $0.01 placeholder
    itch::add_order(&mut out, &hdr(3), 1, b'S', 10, &sym, 5_000_000); // duplicate id at $500
    itch::order_replace(&mut out, &hdr(4), 9, 10, 10, 7_000_000); // unknown old ref at $700
    itch::add_order(&mut out, &hdr(5), 2, b'S', 0, &sym, 9_000_000); // qty 0 at $900
    itch::add_order(&mut out, &hdr(6), 2, b'S', 10, &sym, 1_000_000); // first accepted real add: $100
    itch::add_order(&mut out, &hdr(7), 3, b'S', 10, &sym, 1_000_000); // duplicate? no: id 3 is new
    itch::add_order(&mut out, &hdr(8), 3, b'B', 10, &sym, 9_990_000); // duplicate id 3 at $999
    let mut s = session(Watchlist::none().with_locates([LOC]));
    apply_all(&mut s, &out);
    let st = s.stats();
    let ls = &st.locates[LOC as usize];
    assert!(ls.array);
    // window centred on $100 with a 1c tick: base = $100 - 1024 ticks
    assert_eq!(ls.window, Some((1_000_000 - 1024 * 100, 2048, 100)));
    // the placeholder is the only overflow hit; the rejected $500 / $700 / $900 adds never rested
    assert_eq!(ls.overflow_hits, 1);
    assert_eq!(
        ls.max_abs_offset_real, 1024,
        "set by the centring add, not by the rejected $999 add"
    );
    assert_eq!((st.duplicate_id, st.unknown_id, st.bad_qty), (2, 1, 1));
    assert_eq!(ls.live, 3);
    // the same stream on the reference book gives the same close hash: migration is hash-transparent
    let mut r = session(Watchlist::none());
    apply_all(&mut r, &out);
    assert_eq!(r.stats().locates[LOC as usize].close_hash, ls.close_hash);
    assert_eq!(r.stats().event_log_hash, st.event_log_hash);
}

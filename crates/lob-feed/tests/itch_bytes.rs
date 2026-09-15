//! Hand-decoded bytes for every message type the views cover, the research report's `A`
//! message oracle as a hex literal, and the first frame of the real emi.nasdaq.com sample
//! (01302020, verified in the research pass) as a literal.

use lob_core::Side;
use lob_feed::itch::msg::{
    AddOrder, AddOrderMpid, BrokenTrade, CrossTrade, Msg, OrderCancel, OrderDelete, OrderExecuted,
    OrderExecutedWithPrice, OrderReplace, ParseError, SPEC_LEN, StockDirectory, SystemEvent, Trade,
    TradingAction, parse, spec_len,
};
use lob_feed::{FrameSource, Session, SessionConfig, SliceFrames};

fn unhex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Header written by hand: type | locate u16 BE | tracking u16 BE | ts u48 BE.
fn hdr(ty: u8, locate: u16, tracking: u16, ts: u64) -> Vec<u8> {
    let mut v = vec![ty];
    v.extend_from_slice(&locate.to_be_bytes());
    v.extend_from_slice(&tracking.to_be_bytes());
    v.extend_from_slice(&ts.to_be_bytes()[2..]);
    v
}

const TS: u64 = 34_200_000_000_123;

#[test]
fn add_order_hex_oracle() {
    // Framed: len 36, locate 7, tracking 3, ts 34,200,000,000,123, ref 123456789, side B,
    // 100 shares, SPY, price 453.1200 (research report oracle itch_A_message_bytes).
    let framed =
        unhex("002441000700031f1aced9f07b00000000075bcd154200000064535059202020202000452400");
    assert_eq!(framed.len(), 38);
    let mut f = SliceFrames::new(&framed);
    let payload = f.next().unwrap();
    assert_eq!(payload.len(), 36);
    let a = AddOrder::new(payload).unwrap();
    assert_eq!(a.locate(), 7);
    assert_eq!(a.tracking(), 3);
    assert_eq!(a.ts(), TS);
    assert_eq!(a.order_ref(), 123_456_789);
    assert_eq!(a.side(), Some(Side::Bid));
    assert_eq!(a.side_byte(), b'B');
    assert_eq!(a.shares(), 100);
    assert_eq!(&a.stock(), b"SPY     ");
    assert_eq!(a.price(), 4_531_200);
    assert!(matches!(parse(payload), Ok(Msg::Add(_))));
    assert_eq!(f.next(), None);
    assert_eq!(f.truncated(), 0);
}

#[test]
fn real_sample_first_frame_literal() {
    // First bytes of 01302020.NASDAQ_ITCH50.gz (research pass, verified on the file): frame
    // length 0x000c, 'S', locate 0, tracking 0, ts 10,953,404,452,051 ns (03:02:33), event 'O';
    // then the first 'R' frame starts 0x0027 'R' locate 1.
    let head = unhex("000c 53 0000 0000 09f649c80cd3 4f 0027 52 0001");
    let mut f = SliceFrames::new(&head);
    let p = f.next().unwrap();
    assert_eq!(p.len(), 12);
    let s = SystemEvent::new(p).unwrap();
    assert_eq!(s.locate(), 0);
    assert_eq!(s.tracking(), 0);
    assert_eq!(s.ts(), 10_953_404_452_051);
    assert_eq!(s.event_code(), b'O');
    assert_eq!(f.next(), None);
    assert_eq!(f.truncated(), 1); // the dangling 'R' prefix is the counted truncated tail
}

#[test]
fn system_event() {
    let mut b = hdr(b'S', 0, 0, 10_953_404_452_051);
    b.push(b'Q');
    assert_eq!(b.len(), 12);
    let s = SystemEvent::new(&b).unwrap();
    assert_eq!(s.ts(), 10_953_404_452_051);
    assert_eq!(s.event_code(), b'Q');
    assert!(matches!(parse(&b), Ok(Msg::System(_))));
}

#[test]
fn stock_directory() {
    let mut b = hdr(b'R', 1, 2, TS);
    b.extend_from_slice(b"AAPL    ");
    b.push(b'Q'); // market category @19
    b.push(b'N'); // financial status @20
    b.extend_from_slice(&100u32.to_be_bytes()); // round lot @21
    b.push(b'N'); // round lots only @25
    b.push(b'C'); // issue classification @26
    b.extend_from_slice(b"Z "); // issue sub-type @27
    b.push(b'P'); // authenticity @29
    b.push(b'N'); // short sale threshold @30
    b.push(b'N'); // ipo @31
    b.push(b'1'); // luld tier @32
    b.push(b'N'); // etp flag @33
    b.extend_from_slice(&3u32.to_be_bytes()); // etp leverage @34
    b.push(b'N'); // inverse @38
    assert_eq!(b.len(), 39);
    let r = StockDirectory::new(&b).unwrap();
    assert_eq!(r.locate(), 1);
    assert_eq!(r.tracking(), 2);
    assert_eq!(&r.stock(), b"AAPL    ");
    assert_eq!(r.market_category(), b'Q');
    assert_eq!(r.financial_status(), b'N');
    assert_eq!(r.round_lot_size(), 100);
    assert_eq!(r.round_lots_only(), b'N');
    assert_eq!(r.issue_classification(), b'C');
    assert_eq!(&r.issue_subtype(), b"Z ");
    assert_eq!(r.authenticity(), b'P');
    assert_eq!(r.short_sale_threshold(), b'N');
    assert_eq!(r.ipo_flag(), b'N');
    assert_eq!(r.luld_tier(), b'1');
    assert_eq!(r.etp_flag(), b'N');
    assert_eq!(r.etp_leverage(), 3);
    assert_eq!(r.inverse(), b'N');
    assert!(matches!(parse(&b), Ok(Msg::Directory(_))));
}

#[test]
fn trading_action() {
    let mut b = hdr(b'H', 9, 0, TS);
    b.extend_from_slice(b"MSFT    ");
    b.push(b'H');
    b.push(b' ');
    b.extend_from_slice(b"T1  ");
    assert_eq!(b.len(), 25);
    let h = TradingAction::new(&b).unwrap();
    assert_eq!(h.locate(), 9);
    assert_eq!(&h.stock(), b"MSFT    ");
    assert_eq!(h.trading_state(), b'H');
    assert_eq!(h.reserved(), b' ');
    assert_eq!(&h.reason(), b"T1  ");
    assert!(matches!(parse(&b), Ok(Msg::Action(_))));
}

#[test]
fn add_order_with_mpid() {
    let mut b = hdr(b'F', 7, 3, TS);
    b.extend_from_slice(&123_456_789u64.to_be_bytes());
    b.push(b'S');
    b.extend_from_slice(&250u32.to_be_bytes());
    b.extend_from_slice(b"SPY     ");
    b.extend_from_slice(&4_531_300u32.to_be_bytes());
    b.extend_from_slice(b"NSDQ");
    assert_eq!(b.len(), 40);
    let f = AddOrderMpid::new(&b).unwrap();
    assert_eq!(f.order_ref(), 123_456_789);
    assert_eq!(f.side(), Some(Side::Ask));
    assert_eq!(f.shares(), 250);
    assert_eq!(&f.stock(), b"SPY     ");
    assert_eq!(f.price(), 4_531_300);
    assert_eq!(&f.attribution(), b"NSDQ");
    assert!(matches!(parse(&b), Ok(Msg::AddMpid(_))));
    // a bad side byte decodes to None
    b[19] = b'X';
    assert_eq!(AddOrderMpid::new(&b).unwrap().side(), None);
}

#[test]
fn order_executed_and_with_price() {
    let mut e = hdr(b'E', 7, 0, TS);
    e.extend_from_slice(&42u64.to_be_bytes());
    e.extend_from_slice(&30u32.to_be_bytes());
    e.extend_from_slice(&987_654_321u64.to_be_bytes());
    assert_eq!(e.len(), 31);
    let v = OrderExecuted::new(&e).unwrap();
    assert_eq!(v.order_ref(), 42);
    assert_eq!(v.executed(), 30);
    assert_eq!(v.match_number(), 987_654_321);
    assert!(matches!(parse(&e), Ok(Msg::Exec(_))));

    let mut c = hdr(b'C', 7, 0, TS);
    c.extend_from_slice(&42u64.to_be_bytes());
    c.extend_from_slice(&30u32.to_be_bytes());
    c.extend_from_slice(&987_654_322u64.to_be_bytes());
    c.push(b'N');
    c.extend_from_slice(&4_531_150u32.to_be_bytes());
    assert_eq!(c.len(), 36);
    let v = OrderExecutedWithPrice::new(&c).unwrap();
    assert_eq!(v.order_ref(), 42);
    assert_eq!(v.executed(), 30);
    assert_eq!(v.match_number(), 987_654_322);
    assert_eq!(v.printable(), b'N');
    assert_eq!(v.execution_price(), 4_531_150);
    assert!(matches!(parse(&c), Ok(Msg::ExecPx(_))));
}

#[test]
fn cancel_delete_replace() {
    let mut x = hdr(b'X', 7, 0, TS);
    x.extend_from_slice(&42u64.to_be_bytes());
    x.extend_from_slice(&50u32.to_be_bytes());
    assert_eq!(x.len(), 23);
    let v = OrderCancel::new(&x).unwrap();
    assert_eq!((v.order_ref(), v.cancelled()), (42, 50));
    assert!(matches!(parse(&x), Ok(Msg::Cancel(_))));

    let mut d = hdr(b'D', 7, 0, TS);
    d.extend_from_slice(&42u64.to_be_bytes());
    assert_eq!(d.len(), 19);
    assert_eq!(OrderDelete::new(&d).unwrap().order_ref(), 42);
    assert!(matches!(parse(&d), Ok(Msg::Delete(_))));

    let mut u = hdr(b'U', 7, 0, TS);
    u.extend_from_slice(&42u64.to_be_bytes());
    u.extend_from_slice(&43u64.to_be_bytes());
    u.extend_from_slice(&75u32.to_be_bytes());
    u.extend_from_slice(&4_531_000u32.to_be_bytes());
    assert_eq!(u.len(), 35);
    let v = OrderReplace::new(&u).unwrap();
    assert_eq!(v.original_ref(), 42);
    assert_eq!(v.new_ref(), 43);
    assert_eq!(v.shares(), 75);
    assert_eq!(v.price(), 4_531_000);
    assert!(matches!(parse(&u), Ok(Msg::Replace(_))));
}

#[test]
fn trade_cross_broken() {
    let mut p = hdr(b'P', 7, 0, TS);
    p.extend_from_slice(&0u64.to_be_bytes());
    p.push(b'B');
    p.extend_from_slice(&100u32.to_be_bytes());
    p.extend_from_slice(b"SPY     ");
    p.extend_from_slice(&4_531_200u32.to_be_bytes());
    p.extend_from_slice(&5u64.to_be_bytes());
    assert_eq!(p.len(), 44);
    let v = Trade::new(&p).unwrap();
    assert_eq!(v.order_ref(), 0);
    assert_eq!(v.side_byte(), b'B');
    assert_eq!(v.shares(), 100);
    assert_eq!(&v.stock(), b"SPY     ");
    assert_eq!(v.price(), 4_531_200);
    assert_eq!(v.match_number(), 5);
    assert!(matches!(parse(&p), Ok(Msg::Trade(_))));

    let mut q = hdr(b'Q', 7, 0, TS);
    q.extend_from_slice(&1_000_000u64.to_be_bytes());
    q.extend_from_slice(b"SPY     ");
    q.extend_from_slice(&4_531_200u32.to_be_bytes());
    q.extend_from_slice(&6u64.to_be_bytes());
    q.push(b'O');
    assert_eq!(q.len(), 40);
    let v = CrossTrade::new(&q).unwrap();
    assert_eq!(v.shares(), 1_000_000);
    assert_eq!(&v.stock(), b"SPY     ");
    assert_eq!(v.cross_price(), 4_531_200);
    assert_eq!(v.match_number(), 6);
    assert_eq!(v.cross_type(), b'O');
    assert!(matches!(parse(&q), Ok(Msg::Cross(_))));

    let mut b = hdr(b'B', 7, 0, TS);
    b.extend_from_slice(&6u64.to_be_bytes());
    assert_eq!(b.len(), 19);
    assert_eq!(BrokenTrade::new(&b).unwrap().match_number(), 6);
    assert!(matches!(parse(&b), Ok(Msg::Broken(_))));
}

#[test]
fn every_table_type_parses_at_its_length_and_fails_off_by_one() {
    for (t, n) in SPEC_LEN {
        let b = hdr(t, 1, 0, TS)
            .into_iter()
            .chain(std::iter::repeat_n(b' ', n - 11))
            .collect::<Vec<u8>>();
        assert_eq!(b.len(), n);
        let m = parse(&b).unwrap();
        assert_eq!(m.ty(), t);
        assert_eq!(m.locate(), Some(1));
        assert_eq!(m.ts(), Some(TS));
        let short = &b[..n - 1];
        assert_eq!(
            parse(short),
            Err(ParseError::Length { ty: t, got: n - 1 }),
            "type {}",
            t as char
        );
        assert_eq!(spec_len(t), Some(n));
    }
}

#[test]
fn views_reject_wrong_type_or_length() {
    let a = hdr(b'A', 1, 0, TS)
        .into_iter()
        .chain(std::iter::repeat_n(0, 25))
        .collect::<Vec<u8>>();
    assert!(AddOrder::new(&a).is_some());
    assert!(AddOrderMpid::new(&a).is_none());
    assert!(AddOrder::new(&a[..35]).is_none());
}

#[test]
fn session_applies_hand_built_messages_with_itch_semantics() {
    // R, H, A(1) 300 @ 100.00 bid, A(2) 100, A(3) 200 (FIFO [1,2,3]); X(1,50) keeps priority,
    // level 550; U(1 -> 9, same px, 250) moves to the tail [2,3,9]; E(2, 100) empties order 2 at
    // the head; C(3, 50) printable N reduces by id; D(9); an unknown D(777) is counted, not applied;
    // P/Q/B leave the book alone; a bad side byte is skipped.
    let mut s = Session::new(SessionConfig {
        watchlist: lob_feed::Watchlist::symbols(["SPY"]),
        ..Default::default()
    });
    let mut r = hdr(b'R', 7, 0, TS);
    r.extend_from_slice(b"SPY     ");
    r.extend_from_slice(&[b' '; 20]);
    s.apply_payload(&r).unwrap();
    let mut h = hdr(b'H', 7, 0, TS);
    h.extend_from_slice(b"SPY     ");
    h.push(b'T');
    h.push(b' ');
    h.extend_from_slice(b"    ");
    s.apply_payload(&h).unwrap();
    let add = |id: u64, side: u8, qty: u32, px: u32| {
        let mut b = hdr(b'A', 7, 0, TS);
        b.extend_from_slice(&id.to_be_bytes());
        b.push(side);
        b.extend_from_slice(&qty.to_be_bytes());
        b.extend_from_slice(b"SPY     ");
        b.extend_from_slice(&px.to_be_bytes());
        b
    };
    s.apply_payload(&add(1, b'B', 300, 1_000_000)).unwrap();
    s.apply_payload(&add(2, b'B', 100, 1_000_000)).unwrap();
    s.apply_payload(&add(3, b'B', 200, 1_000_000)).unwrap();
    s.apply_payload(&add(4, b'S', 10, 1_000_100)).unwrap();
    s.apply_payload(&add(5, b'Z', 10, 1_000_100)).unwrap(); // bad side
    assert!(matches!(
        s.locate_book(7),
        Some(lob_feed::itch::apply::LocateBook::Array(_))
    ));
    let book = s.book(7).unwrap();
    assert_eq!(book.live_orders(), 4);
    assert_eq!(book.l1().0, Some((1_000_000, 600, 3)));
    assert_eq!(book.queue_ahead(3), Some(400));

    let mut x = hdr(b'X', 7, 0, TS);
    x.extend_from_slice(&1u64.to_be_bytes());
    x.extend_from_slice(&50u32.to_be_bytes());
    s.apply_payload(&x).unwrap();
    let book = s.book(7).unwrap();
    assert_eq!(book.l1().0, Some((1_000_000, 550, 3)));
    assert_eq!(book.queue_ahead(1), Some(0));

    let mut u = hdr(b'U', 7, 0, TS);
    u.extend_from_slice(&1u64.to_be_bytes());
    u.extend_from_slice(&9u64.to_be_bytes());
    u.extend_from_slice(&250u32.to_be_bytes());
    u.extend_from_slice(&1_000_000u32.to_be_bytes());
    s.apply_payload(&u).unwrap();
    let book = s.book(7).unwrap();
    let snap = book.snapshot();
    assert_eq!(
        snap[0],
        (Side::Bid, 1_000_000, vec![(2, 100), (3, 200), (9, 250)])
    );
    assert_eq!(book.queue_ahead(9), Some(300));

    let mut e = hdr(b'E', 7, 0, TS);
    e.extend_from_slice(&2u64.to_be_bytes());
    e.extend_from_slice(&100u32.to_be_bytes());
    e.extend_from_slice(&1u64.to_be_bytes());
    s.apply_payload(&e).unwrap();
    let mut c = hdr(b'C', 7, 0, TS);
    c.extend_from_slice(&3u64.to_be_bytes());
    c.extend_from_slice(&50u32.to_be_bytes());
    c.extend_from_slice(&2u64.to_be_bytes());
    c.push(b'N');
    c.extend_from_slice(&999_900u32.to_be_bytes()); // exec price is not book state
    s.apply_payload(&c).unwrap();
    let book = s.book(7).unwrap();
    assert_eq!(
        book.snapshot()[0],
        (Side::Bid, 1_000_000, vec![(3, 150), (9, 250)])
    );
    assert_eq!(book.live_orders(), 3);

    let mut d = hdr(b'D', 7, 0, TS);
    d.extend_from_slice(&777u64.to_be_bytes());
    s.apply_payload(&d).unwrap();
    d[11..19].copy_from_slice(&9u64.to_be_bytes());
    s.apply_payload(&d).unwrap();

    let mut p = hdr(b'P', 7, 0, TS);
    p.extend_from_slice(&[0u8; 33]);
    s.apply_payload(&p).unwrap();
    let mut q = hdr(b'Q', 7, 0, TS);
    q.extend_from_slice(&[0u8; 29]);
    s.apply_payload(&q).unwrap();
    let mut b = hdr(b'B', 7, 0, TS);
    b.extend_from_slice(&[0u8; 8]);
    s.apply_payload(&b).unwrap();
    s.apply_payload(&[b'Z', 1, 2, 3]).unwrap(); // unknown type

    s.finish();
    let book = s.book(7).unwrap();
    assert_eq!(book.snapshot()[0], (Side::Bid, 1_000_000, vec![(3, 150)]));
    assert_eq!(book.live_orders(), 2);
    let st = s.stats();
    assert_eq!(st.unknown_id, 1);
    assert_eq!(st.bad_side, 1);
    assert_eq!(st.unknown_type, 1);
    assert_eq!(st.ec_total, 2);
    assert_eq!(st.ec_at_head, 2);
    assert_eq!(st.over_cancel, 0);
    assert_eq!(st.negative_qty(), 0);
    assert_eq!(st.live_hwm, 4);
    assert_eq!(st.live, 2);
    assert_eq!(st.locates[7].live_hwm, 4);
    assert_eq!(st.locates[7].h_state, b'T');
    assert!(st.locates[7].array);
    assert_eq!(st.locates[7].window, Some((897_600, 2048, 100)));
    assert_eq!(st.event_count, 10); // 4 adds (the bad-side add is skipped) + X + U + E + C + D(777) + D(9)
    assert_eq!(st.by_type[b'P' as usize], 1);
    assert_eq!(st.by_type[b'Q' as usize], 1);
    assert_eq!(st.by_type[b'B' as usize], 1);
    assert_eq!(st.by_type[b'Z' as usize], 1);
    assert_eq!(st.locates[7].msgs, 2 + 5 + 1 + 1 + 1 + 1 + 2 + 3);
    assert_eq!(st.locates[7].close_hash, book.state_hash());
    assert_eq!(st.event_log_hash, s.event_log().digest());
    let text = st.render();
    assert!(text.contains("| unknown-id | 1 |"));
    assert!(text.contains("| E/C at level head | 2 / 2 = 1.0000 |"));
    assert!(text.contains("| 7 | SPY | array |"));
}

#[test]
fn over_cancel_over_execute_and_no_directory_are_counted() {
    let mut s = Session::new(SessionConfig::default());
    let mut a = hdr(b'A', 3, 0, TS);
    a.extend_from_slice(&1u64.to_be_bytes());
    a.push(b'S');
    a.extend_from_slice(&100u32.to_be_bytes());
    a.extend_from_slice(b"X       ");
    a.extend_from_slice(&500_000u32.to_be_bytes());
    s.apply_payload(&a).unwrap(); // no R before it
    let mut e = hdr(b'E', 3, 0, TS);
    e.extend_from_slice(&1u64.to_be_bytes());
    e.extend_from_slice(&150u32.to_be_bytes());
    e.extend_from_slice(&1u64.to_be_bytes());
    s.apply_payload(&e).unwrap(); // over-execute: rejected, order intact
    assert_eq!(s.book(3).unwrap().live_orders(), 1);
    let mut x = hdr(b'X', 3, 0, TS);
    x.extend_from_slice(&1u64.to_be_bytes());
    x.extend_from_slice(&150u32.to_be_bytes());
    s.apply_payload(&x).unwrap(); // over-cancel: the book removes the order; counted
    assert_eq!(s.book(3).unwrap().live_orders(), 0);
    s.finish();
    let st = s.stats();
    assert_eq!(st.no_directory, 1);
    assert_eq!(st.over_execute, 1);
    assert_eq!(st.over_cancel, 1);
    assert_eq!(st.negative_qty(), 2);
    assert_eq!(st.live_hwm, 1);
    assert_eq!(st.live, 0);
    assert_eq!(st.event_count, 3);
    assert!(!st.locates[3].array);
}

#[test]
fn crossed_snapshots_before_and_after_q() {
    let mut s = Session::new(SessionConfig::default());
    let mut r = hdr(b'R', 1, 0, TS);
    r.extend_from_slice(b"ABC     ");
    r.extend_from_slice(&[b' '; 20]);
    s.apply_payload(&r).unwrap();
    let mut h = hdr(b'H', 1, 0, TS);
    h.extend_from_slice(b"ABC     ");
    h.extend_from_slice(b"T     ");
    s.apply_payload(&h).unwrap();
    let add = |id: u64, side: u8, px: u32| {
        let mut b = hdr(b'A', 1, 0, TS);
        b.extend_from_slice(&id.to_be_bytes());
        b.push(side);
        b.extend_from_slice(&10u32.to_be_bytes());
        b.extend_from_slice(b"ABC     ");
        b.extend_from_slice(&px.to_be_bytes());
        b
    };
    s.apply_payload(&add(1, b'B', 1_000_100)).unwrap();
    s.apply_payload(&add(2, b'S', 1_000_000)).unwrap(); // crossed (bid 100.01 > ask 100.00)
    s.apply_payload(&add(3, b'S', 1_000_200)).unwrap(); // still crossed
    let mut q = hdr(b'S', 0, 0, TS);
    q.push(b'Q');
    s.apply_payload(&q).unwrap();
    s.apply_payload(&add(4, b'B', 999_900)).unwrap(); // still crossed after Q
    let mut d = hdr(b'D', 1, 0, TS);
    d.extend_from_slice(&2u64.to_be_bytes());
    s.apply_payload(&d).unwrap(); // uncrossed: bid 100.01 < ask 100.02
    s.apply_payload(&add(5, b'B', 1_000_200)).unwrap(); // locked (bid == ask) counts as crossed
    s.finish();
    let st = s.stats();
    assert_eq!(st.crossed_before_q, 2);
    assert_eq!(st.crossed_after_q, 2);
    assert_eq!(st.crossed_after_q_trading, 2);
    assert_eq!(s.system_state(), b'Q');
    assert_eq!(st.s_events, vec![(b'Q', TS)]);
}

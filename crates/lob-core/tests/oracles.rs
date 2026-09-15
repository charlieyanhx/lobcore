//! Book oracles from the research report (report_core, section ORACLES), run against both
//! `ArrayBook` and `RefBook`. Where the report's fixture is under-specified (the 650/100 book
//! behind `unknown_id_no_op`) the fixture below is stated in full and reproduces the report's
//! numbers.

use lob_core::{
    ArrayBook, BookConfig, BookError, Event, EventKind, EventLog, OrderBook, RefBook, Side, hex,
};

fn cfg() -> BookConfig {
    BookConfig::new(10000, 128, 1)
}

fn books() -> Vec<(&'static str, Box<dyn OrderBook>)> {
    vec![
        ("array", Box::new(ArrayBook::new(cfg()))),
        ("ref", Box::new(RefBook::new())),
    ]
}

fn fifo_ids(b: &dyn OrderBook, side: Side, px: i32) -> Vec<u64> {
    b.snapshot()
        .into_iter()
        .find(|(s, p, _)| *s == side && *p == px)
        .map(|(_, _, f)| f.into_iter().map(|(id, _)| id).collect())
        .unwrap_or_default()
}

fn level_qty(b: &dyn OrderBook, side: Side, px: i32) -> u32 {
    b.snapshot()
        .into_iter()
        .find(|(s, p, _)| *s == side && *p == px)
        .map(|(_, _, f)| f.into_iter().map(|(_, q)| q).sum())
        .unwrap_or(0)
}

/// FIFO [1,2,3] with qty 300/100/200 at bid 10050.
fn seed_fifo(b: &mut dyn OrderBook) {
    b.add(1, Side::Bid, 10050, 300).unwrap();
    b.add(2, Side::Bid, 10050, 100).unwrap();
    b.add(3, Side::Bid, 10050, 200).unwrap();
}

#[test]
fn fifo_partial_cancel_keeps_priority_and_replace_loses_it() {
    for (name, mut b) in books() {
        seed_fifo(b.as_mut());
        // ITCH X: cancel 50 on id 1 -> [1,2,3], level qty 550, priority kept.
        let e = b.cancel(1, 50).unwrap();
        assert_eq!(e, Event::cancel(1, Side::Bid, 10050, 50, 250), "{name}");
        assert_eq!(
            fifo_ids(b.as_ref(), Side::Bid, 10050),
            vec![1, 2, 3],
            "{name}"
        );
        assert_eq!(level_qty(b.as_ref(), Side::Bid, 10050), 550, "{name}");
        assert_eq!(b.queue_ahead(3), Some(350), "{name}");
        // ITCH U: replace 1 -> 9 at the same price, qty 250 -> tail: [2,3,9] (spec 1.4.5).
        let e = b.replace(1, 9, 10050, 250).unwrap();
        assert_eq!(e, Event::replace(1, 9, Side::Bid, 10050, 250), "{name}");
        assert_eq!(
            fifo_ids(b.as_ref(), Side::Bid, 10050),
            vec![2, 3, 9],
            "{name}"
        );
        assert_eq!(level_qty(b.as_ref(), Side::Bid, 10050), 550, "{name}");
        assert_eq!(b.queue_ahead(9), Some(300), "{name}");
        assert_eq!(b.queue_ahead(1), None, "{name}");
        assert_eq!(b.l1().0, Some((10050, 550, 3)), "{name}");
    }
}

/// Book behind the report's unknown-id / over-execute / empty-side oracles:
/// bid 10050 [2:200, 3:200, 9:250] = 650, bid 10049 [4:100], ask 10051 [5:10]; 4 live bids.
fn seed_650(b: &mut dyn OrderBook) {
    b.add(2, Side::Bid, 10050, 200).unwrap();
    b.add(3, Side::Bid, 10050, 200).unwrap();
    b.add(9, Side::Bid, 10050, 250).unwrap();
    b.add(4, Side::Bid, 10049, 100).unwrap();
    b.add(5, Side::Ask, 10051, 10).unwrap();
}

#[test]
fn unknown_id_is_a_no_op_with_an_unknown_event() {
    for (name, mut b) in books() {
        seed_650(b.as_mut());
        let before = b.state_hash();
        let err = b.cancel(777, 10).unwrap_err();
        assert_eq!(
            err,
            BookError::UnknownId {
                kind: EventKind::Cancel,
                id: 777
            },
            "{name}"
        );
        assert_eq!(
            err.event(),
            Event::unknown(EventKind::Cancel, 777),
            "{name}"
        );
        assert_eq!(err.event().b, 2, "{name}: b carries the cancel kind code");
        assert_eq!(
            b.delete(777).unwrap_err().event(),
            Event::unknown(EventKind::Delete, 777)
        );
        assert_eq!(
            b.execute(777, 1).unwrap_err().event(),
            Event::unknown(EventKind::Exec, 777)
        );
        assert_eq!(
            b.replace(777, 778, 10050, 1).unwrap_err().event(),
            Event::unknown(EventKind::Replace, 777)
        );
        assert_eq!(level_qty(b.as_ref(), Side::Bid, 10050), 650, "{name}");
        assert_eq!(level_qty(b.as_ref(), Side::Bid, 10049), 100, "{name}");
        assert_eq!(b.live_orders(), 5, "{name}");
        assert_eq!(b.state_hash(), before, "{name}: I9 zero state change");
    }
}

#[test]
fn over_execute_rejects_with_no_change() {
    for (name, mut b) in books() {
        seed_650(b.as_mut());
        let before = b.state_hash();
        let err = b.execute(9, 999).unwrap_err();
        assert_eq!(
            err,
            BookError::OverExecute {
                id: 9,
                have: 250,
                want: 999
            },
            "{name}"
        );
        assert_eq!(
            err.event(),
            Event::reject(EventKind::Exec, 9, 999),
            "{name}"
        );
        assert_eq!(level_qty(b.as_ref(), Side::Bid, 10050), 650, "{name}");
        assert_eq!(b.state_hash(), before, "{name}: I10 zero state change");
        // Exact execute empties the order; the level keeps the other two.
        let e = b.execute(9, 250).unwrap();
        assert_eq!(e, Event::exec(9, Side::Bid, 10050, 250, 0), "{name}");
        assert_eq!(fifo_ids(b.as_ref(), Side::Bid, 10050), vec![2, 3], "{name}");
    }
}

#[test]
fn empty_side_best_is_none_and_the_other_side_survives() {
    for (name, mut b) in books() {
        seed_650(b.as_mut());
        for id in [2, 3, 9, 4] {
            b.delete(id).unwrap();
        }
        assert_eq!(b.l1(), (None, Some((10051, 10, 1))), "{name}");
        assert_eq!(b.l2(5), (vec![], vec![(10051, 10, 1)]), "{name}");
        assert_eq!(b.live_orders(), 1, "{name}");
    }
    // The array side's bitmap summary is 0 once every bid is gone.
    let mut a = ArrayBook::new(cfg());
    seed_650(&mut a);
    for id in [2, 3, 9, 4] {
        a.delete(id).unwrap();
    }
    a.check().unwrap();
    assert_eq!(a.snapshot(), vec![(Side::Ask, 10051, vec![(5, 10)])]);
}

#[test]
fn bitmap_best_tracking_through_the_book() {
    // Levels at tick offsets 5, 130, 1000 of a 2048-level bid side.
    let mut a = ArrayBook::new(BookConfig::new(10000, 2048, 1));
    a.add(1, Side::Bid, 10005, 1).unwrap();
    a.add(2, Side::Bid, 10130, 1).unwrap();
    a.add(3, Side::Bid, 10000 + 1000, 1).unwrap();
    assert_eq!(a.l1().0, Some((11000, 1, 1)));
    a.delete(3).unwrap();
    assert_eq!(a.l1().0, Some((10130, 1, 1)));
    assert_eq!(a.l2(8).0, vec![(10130, 1, 1), (10005, 1, 1)]);
    a.check().unwrap();
}

#[test]
fn overflow_window_oracle() {
    // 64-level window [10000, 10064): bid 10030, ask 10031, ask 10999 (overflow), bid 9000
    // (overflow), bid 10029 -> best (10030, 10031), 2 overflow pushes; then the overflow map
    // serves best once every in-range order is gone.
    let mut a = ArrayBook::new(BookConfig::new(10000, 64, 1));
    let mut r = RefBook::new();
    let ops: [(u64, Side, i32); 5] = [
        (1, Side::Bid, 10030),
        (2, Side::Ask, 10031),
        (3, Side::Ask, 10999),
        (4, Side::Bid, 9000),
        (5, Side::Bid, 10029),
    ];
    for (id, side, px) in ops {
        assert_eq!(a.add(id, side, px, 10), r.add(id, side, px, 10));
    }
    assert_eq!(a.l1(), (Some((10030, 10, 1)), Some((10031, 10, 1))));
    assert_eq!(a.overflow_hits(), 2);
    assert_eq!(a.max_abs_offset(), 1000);
    assert_eq!(a.overflow_prices(Side::Bid), vec![9000]);
    assert_eq!(a.overflow_prices(Side::Ask), vec![10999]);
    assert_eq!(a.snapshot(), r.snapshot());
    assert_eq!(a.state_hash(), r.state_hash());
    assert_eq!(
        a.l2(3),
        (
            vec![(10030, 10, 1), (10029, 10, 1), (9000, 10, 1)],
            vec![(10031, 10, 1), (10999, 10, 1)]
        )
    );
    a.check().unwrap();
    for id in [1, 2, 5] {
        assert_eq!(a.delete(id), r.delete(id));
    }
    assert_eq!(a.l1(), (Some((9000, 10, 1)), Some((10999, 10, 1))));
    assert_eq!(a.snapshot(), r.snapshot());
    a.check().unwrap();
    // Emptying an overflow level removes it from the map.
    a.execute(4, 10).unwrap();
    assert_eq!(a.overflow_prices(Side::Bid), Vec::<i32>::new());
    assert_eq!(a.l1().0, None);
    a.check().unwrap();
    // Off-grid prices route to the overflow map too (tick 100 grid, price 10050).
    let mut g = ArrayBook::new(BookConfig::new(10000, 64, 100));
    g.add(1, Side::Ask, 10050, 1).unwrap();
    g.add(2, Side::Ask, 10100, 1).unwrap();
    assert_eq!(g.overflow_prices(Side::Ask), vec![10050]);
    assert_eq!(g.l1().1, Some((10050, 1, 1)));
    g.check().unwrap();
}

#[test]
fn replace_across_levels_and_sides_carries_the_side() {
    for (name, mut b) in books() {
        seed_650(b.as_mut());
        let e = b.replace(4, 40, 10048, 7).unwrap();
        assert_eq!(e, Event::replace(4, 40, Side::Bid, 10048, 7), "{name}");
        assert_eq!(level_qty(b.as_ref(), Side::Bid, 10049), 0, "{name}");
        assert_eq!(fifo_ids(b.as_ref(), Side::Bid, 10048), vec![40], "{name}");
        // new id must be unknown; old must be live; nothing changes on the errors.
        let before = b.state_hash();
        assert_eq!(
            b.replace(40, 2, 10048, 1),
            Err(BookError::DuplicateId(2)),
            "{name}"
        );
        assert_eq!(
            b.replace(40, 41, 10048, 0),
            Err(BookError::BadQty),
            "{name}"
        );
        assert_eq!(b.replace(40, 41, 0, 1), Err(BookError::BadPrice), "{name}");
        assert_eq!(
            b.add(40, Side::Ask, 10052, 1),
            Err(BookError::DuplicateId(40)),
            "{name}"
        );
        assert_eq!(
            b.add(41, Side::Ask, 10052, 0),
            Err(BookError::BadQty),
            "{name}"
        );
        assert_eq!(
            b.add(41, Side::Ask, -1, 1),
            Err(BookError::BadPrice),
            "{name}"
        );
        assert_eq!(b.cancel(40, 0), Err(BookError::BadQty), "{name}");
        assert_eq!(b.execute(40, 0), Err(BookError::BadQty), "{name}");
        assert_eq!(b.state_hash(), before, "{name}");
    }
}

#[test]
fn cancel_at_or_above_remaining_removes_the_order() {
    for (name, mut b) in books() {
        seed_fifo(b.as_mut());
        let e = b.cancel(2, 100).unwrap();
        assert_eq!(e, Event::cancel(2, Side::Bid, 10050, 100, 0), "{name}");
        let e = b.cancel(3, 5000).unwrap();
        assert_eq!(e, Event::cancel(3, Side::Bid, 10050, 200, 0), "{name}");
        assert_eq!(fifo_ids(b.as_ref(), Side::Bid, 10050), vec![1], "{name}");
        assert_eq!(b.live_orders(), 1, "{name}");
        let e = b.delete(1).unwrap();
        assert_eq!(e, Event::delete(1, Side::Bid, 10050, 300), "{name}");
        assert_eq!(b.l1(), (None, None), "{name}");
        assert_eq!(b.live_orders(), 0, "{name}");
    }
}

#[test]
fn level_total_past_u32_is_rejected_identically() {
    for (name, mut b) in books() {
        b.add(1, Side::Ask, 10051, u32::MAX - 5).unwrap();
        assert_eq!(
            b.add(2, Side::Ask, 10051, 6),
            Err(BookError::BadQty),
            "{name}"
        );
        // Precedence: level overflow is reported before a duplicate id, on both books.
        assert_eq!(
            b.add(1, Side::Ask, 10051, 6),
            Err(BookError::BadQty),
            "{name}"
        );
        assert_eq!(
            b.add(1, Side::Ask, 10051, 5),
            Err(BookError::DuplicateId(1)),
            "{name}"
        );
        b.add(2, Side::Ask, 10051, 5).unwrap();
        assert_eq!(b.replace(2, 3, 10051, 6), Err(BookError::BadQty), "{name}");
        assert!(b.replace(2, 3, 10052, 6).is_ok(), "{name}");
        assert_eq!(b.l1().1, Some((10051, u32::MAX - 5, 1)), "{name}");
    }
}

#[test]
fn state_hashes_agree_and_the_empty_book_hash_is_sha256_of_nothing() {
    let a = ArrayBook::new(cfg());
    let r = RefBook::new();
    assert_eq!(a.state_hash(), r.state_hash());
    assert_eq!(
        hex(&a.state_hash()),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn event_log_digest_matches_hashlib() {
    // hashlib.sha256(b"\x01").hexdigest() and with the report's trade record appended.
    let mut log = EventLog::new();
    assert_eq!(
        hex(&log.digest()),
        "4bf5122f344554c53bde2ebb8cd2b7e3d1600ad631c385a5d7cce23c7785459a"
    );
    log.push(Event::new(EventKind::Trade, 5, 4, 10001, 30, 0));
    assert_eq!(log.len(), 1);
    assert_eq!(
        hex(&log.digest()),
        "e5cf8f41ab7824f948d25eb59f855f9e5f7a50e3238bbce226b5e28b522bb88f"
    );
}

#[test]
fn queue_ahead_is_exact_across_replace_and_partial_fills() {
    for (name, mut b) in books() {
        seed_fifo(b.as_mut());
        assert_eq!(b.queue_ahead(1), Some(0), "{name}");
        assert_eq!(b.queue_ahead(2), Some(300), "{name}");
        b.execute(1, 120).unwrap();
        assert_eq!(b.queue_ahead(2), Some(180), "{name}");
        b.execute(1, 180).unwrap();
        assert_eq!(b.queue_ahead(2), Some(0), "{name}");
        assert_eq!(b.queue_ahead(3), Some(100), "{name}");
    }
}

#[test]
fn array_book_config_is_validated() {
    let r = std::panic::catch_unwind(|| ArrayBook::new(BookConfig::new(10000, 4097, 1)));
    assert!(r.is_err());
    let r = std::panic::catch_unwind(|| ArrayBook::new(BookConfig::new(10000, 0, 1)));
    assert!(r.is_err());
    let r = std::panic::catch_unwind(|| ArrayBook::new(BookConfig::new(10000, 8, 0)));
    assert!(r.is_err());
    let r = std::panic::catch_unwind(|| ArrayBook::new(BookConfig::new(i32::MAX - 10, 64, 1)));
    assert!(r.is_err());
    let a = ArrayBook::with_capacity(BookConfig::new(10000, 4096, 1), 1024);
    assert_eq!(a.config(), BookConfig::new(10000, 4096, 1));
    assert_eq!(a.live_orders(), 0);
}

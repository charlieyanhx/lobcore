//! I11/I12: proptest differential test, `ArrayBook` vs `RefBook` (the nanolob pattern).
//! Identical `Result<Event, BookError>` after EVERY op; identical snapshot and
//! `ArrayBook::check()` (I1-I8) every 64 ops, identical state hash every 256 ops, all three at
//! the end; I9/I10 (unknown id,
//! over-execute) asserted as snapshot-before == snapshot-after on the predicted-error ops.
//! Two strategies: in-range (2048-tick window, prices clamped into it) and overflow-heavy
//! (64-tick window, unclamped, mid drift +-40 every 500 ops). `PROPTEST_CASES` is honoured
//! (proptest reads it; default 256).

mod common;

use common::{AbsOp, Interp};
use lob_core::{ArrayBook, BookConfig, OrderBook, RefBook};
use proptest::prelude::*;

const CHECK_EVERY: usize = 64;
/// The sha256 state hash is the one costly check in a debug build (~0.1 s per 5,000-op case
/// at every 64 ops), so it runs at a quarter of the snapshot cadence and at the end.
const HASH_EVERY: usize = 256;

/// Ops are generated as `Vec<u64>` and decoded by `AbsOp::from_u64`: a `u64` value tree is a
/// small struct, a `prop_oneof` of tuples is a boxed union, and 5,000-element vectors of the
/// latter cost ~0.2 s per case to generate in a debug build (measured: 12.7 s vs 0.9 s for
/// 64 cases). Shrinking is still element-wise (drop ops, then shrink each toward `Add`).
fn ops() -> impl Strategy<Value = Vec<AbsOp>> {
    prop::collection::vec(any::<u64>(), 1..5000)
        .prop_map(|v| v.into_iter().map(AbsOp::from_u64).collect())
}

fn run(
    ops: &[AbsOp],
    cfg: BookConfig,
    mid: i32,
    drift: i32,
    clamp: bool,
) -> Result<(), TestCaseError> {
    let mut array = ArrayBook::new(cfg);
    let mut reference = RefBook::new();
    let lo = cfg.base_px;
    let hi = cfg.base_px + (cfg.n_levels as i32 - 1) * cfg.tick;
    let mut interp = Interp::new(mid, drift, clamp.then_some((lo, hi)));

    for (i, &a) in ops.iter().enumerate() {
        let op = interp.resolve(a, reference.l1());
        let expect_err = interp.predicts_error(&op);
        let before = expect_err.then(|| array.snapshot());
        let ra = op.apply(&mut array);
        let rr = op.apply(&mut reference);
        prop_assert_eq!(&ra, &rr, "op {} {:?}: array vs reference result", i, op);
        if let Some(before) = before {
            prop_assert!(
                ra.is_err(),
                "op {} {:?} predicted to fail but succeeded",
                i,
                op
            );
            prop_assert_eq!(
                before,
                array.snapshot(),
                "op {} {:?}: I9/I10 state changed on Err",
                i,
                op
            );
        }
        interp.observe(&ra);
        if (i + 1).is_multiple_of(CHECK_EVERY) {
            prop_assert_eq!(
                array.snapshot(),
                reference.snapshot(),
                "snapshot at op {}",
                i
            );
            prop_assert!(
                array.check().is_ok(),
                "check at op {}: {:?}",
                i,
                array.check()
            );
        }
        if (i + 1).is_multiple_of(HASH_EVERY) {
            prop_assert_eq!(
                array.state_hash(),
                reference.state_hash(),
                "state hash at op {}",
                i
            );
        }
    }
    prop_assert_eq!(array.snapshot(), reference.snapshot(), "final snapshot");
    prop_assert_eq!(
        array.state_hash(),
        reference.state_hash(),
        "final state hash"
    );
    prop_assert_eq!(array.l1(), reference.l1(), "final l1");
    prop_assert_eq!(array.l2(5), reference.l2(5), "final l2");
    prop_assert_eq!(array.live_orders(), reference.live_orders(), "final live");
    for &id in &interp.live {
        prop_assert_eq!(
            array.queue_ahead(id),
            reference.queue_ahead(id),
            "queue_ahead {}",
            id
        );
    }
    array
        .check()
        .map_err(|e| TestCaseError::fail(format!("final check: {e}")))?;
    Ok(())
}

proptest! {
    // `ProptestConfig::default()` reads PROPTEST_CASES (default 256; CI sets 64).
    #![proptest_config(ProptestConfig {
        max_shrink_iters: 2000,
        .. ProptestConfig::default()
    })]

    /// In-range strategy: 2048-tick window, prices clamped into it, mid drift +-40.
    #[test]
    fn array_matches_reference_in_range(
        ops in ops(),
        drift in -40i32..=40,
    ) {
        run(&ops, BookConfig::new(10_000, 2048, 1), 11_024, drift, true)?;
    }

    /// Overflow-heavy strategy: 64-tick window, unclamped prices, mid drift +-40 every 500 ops,
    /// so most levels land in the overflow map; every invariant must still hold.
    #[test]
    fn array_matches_reference_overflow_heavy(
        ops in ops(),
        drift in -40i32..=40,
    ) {
        run(&ops, BookConfig::new(10_000, 64, 1), 10_032, drift, false)?;
    }

    /// Sub-dollar grid (tick 1 = $0.0001) with a coarse-tick window: every price is on-grid.
    #[test]
    fn array_matches_reference_coarse_tick(
        ops in ops(),
        drift in -40i32..=40,
    ) {
        run(&ops, BookConfig::new(1_000, 512, 3), 1_700, drift, false)?;
    }
}

#[test]
fn overflow_heavy_strategy_actually_overflows() {
    let mut rng = common::SplitMix64(11);
    let ops: Vec<AbsOp> = (0..3000).map(|_| rng.abs_op()).collect();
    let cfg = BookConfig::new(10_000, 64, 1);
    let mut array = ArrayBook::new(cfg);
    let mut reference = RefBook::new();
    let mut interp = Interp::new(10_032, 40, None);
    for a in ops {
        let op = interp.resolve(a, reference.l1());
        let ra = op.apply(&mut array);
        assert_eq!(ra, op.apply(&mut reference));
        interp.observe(&ra);
    }
    assert!(
        array.overflow_hits() > 100,
        "overflow hits {}",
        array.overflow_hits()
    );
    assert!(
        array.max_abs_offset() >= 64,
        "max |offset| {}",
        array.max_abs_offset()
    );
    assert_eq!(array.snapshot(), reference.snapshot());
    array.check().unwrap();
}

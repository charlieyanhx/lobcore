//! The lob-synth guarantees, each as a test over the independent reader in `common`.

mod common;

use std::sync::OnceLock;

use common::{check_day, frames};
use lob_synth::{SynthConfig, SynthDay, sha256, synth_itch, synth_itch_bytes, truth_csv};

const N_FIXTURE: u64 = 100_000;

fn day_s7() -> &'static SynthDay {
    static DAY: OnceLock<SynthDay> = OnceLock::new();
    DAY.get_or_init(|| synth_itch(7, N_FIXTURE, &SynthConfig::default()))
}

#[test]
fn seed7_default_is_deterministic_and_bytes_only_matches() {
    let a = day_s7();
    let b = synth_itch(7, N_FIXTURE, &SynthConfig::default());
    assert_eq!(a, &b);
    let c = synth_itch_bytes(7, N_FIXTURE, &SynthConfig::default());
    assert_eq!(a.bytes, c);
    let d = synth_itch_bytes(8, N_FIXTURE, &SynthConfig::default());
    assert_ne!(a.bytes, d);
}

#[test]
fn seed7_default_round_trips_against_the_independent_book() {
    let day = day_s7();
    let cfg = SynthConfig::default();
    let unknown = check_day(day, &cfg, N_FIXTURE);
    assert_eq!(unknown, 0);
    // every mix type appears with the default weights
    let msgs = frames(&day.bytes);
    for t in *b"AFDXUEPL" {
        assert!(msgs.iter().any(|m| m.ty == t), "type {} missing", t as char);
    }
    assert!(
        msgs.iter().all(|m| m.ty != b'C'),
        "C has weight 0 in the default mix"
    );
    // sub-penny names exist (locates 1 and 5 are in the sub-$1 bucket) and the placeholders too
    assert!(
        msgs.iter()
            .any(|m| matches!(m.ty, b'A' | b'F') && m.add_price() % 100 != 0)
    );
    // the pre-open crossed and was uncrossed by E at the open
    let q_pos = msgs
        .iter()
        .position(|m| m.ty == b'S' && m.event_code() == b'Q')
        .unwrap();
    let crossed_pre_q = day.truth[..q_pos]
        .iter()
        .filter(|t| matches!((t.best_bid, t.best_ask), (Some((b, _)), Some((a, _))) if b >= a))
        .count();
    assert!(
        crossed_pre_q > 0,
        "crossed_preopen produced no crossed snapshot"
    );
    assert_eq!(
        msgs[q_pos - 1].ty,
        b'E',
        "the open ends with the uncrossing executions"
    );
}

#[test]
fn seeds_1_to_3_round_trip_with_every_knob() {
    for seed in 1..=3u64 {
        let cfg = SynthConfig {
            locates: 3,
            unknown_ref_rate: 0.05,
            mix: [0.35, 0.02, 0.30, 0.10, 0.08, 0.05, 0.05, 0.02, 0.03],
            ..SynthConfig::default()
        };
        let day = synth_itch(seed, 20_000, &cfg);
        let unknown = check_day(&day, &cfg, 20_000);
        assert!(
            unknown > 0,
            "unknown_ref_rate 0.05 produced no unknown-ref message"
        );
        assert!(unknown < 20_000 / 4);
    }
}

#[test]
fn uncrossed_preopen_and_penny_grid_configs() {
    let cfg = SynthConfig {
        locates: 5,
        subpenny: false,
        crossed_preopen: false,
        placeholder_rate: 0.0,
        ..SynthConfig::default()
    };
    let day = synth_itch(11, 20_000, &cfg);
    check_day(&day, &cfg, 20_000);
    let crossed = day
        .truth
        .iter()
        .filter(|t| matches!((t.best_bid, t.best_ask), (Some((b, _)), Some((a, _))) if b >= a))
        .count();
    assert_eq!(crossed, 0, "crossed snapshot with crossed_preopen = false");
    let msgs = frames(&day.bytes);
    assert!(
        msgs.iter()
            .all(|m| !matches!(m.ty, b'A' | b'F') || m.add_price() % 100 == 0)
    );
    assert!(msgs.iter().all(|m| !matches!(m.ty, b'A' | b'F')
        || (m.add_price() != 100 && m.add_price() != 1_999_999_900)));
}

#[test]
fn minimum_stream_is_header_q_and_trailer() {
    let cfg = SynthConfig {
        locates: 2,
        ..SynthConfig::default()
    };
    let day = synth_itch(1, 10, &cfg);
    let msgs = frames(&day.bytes);
    let types: Vec<u8> = msgs.iter().map(|m| m.ty).collect();
    assert_eq!(types, *b"SSRRHHSSSS");
    let codes: Vec<u8> = msgs
        .iter()
        .filter(|m| m.ty == b'S')
        .map(|m| m.event_code())
        .collect();
    assert_eq!(codes, *b"OSQMEC");
    assert_eq!(day.truth.len(), 10);
    assert!(day.truth.iter().all(|t| t.live_orders == 0));
}

#[test]
#[should_panic(expected = "n_msgs must be at least")]
fn too_few_messages_panics() {
    let _ = synth_itch(1, 5, &SynthConfig::default());
}

#[test]
fn truth_csv_has_one_row_per_message() {
    let cfg = SynthConfig {
        locates: 2,
        ..SynthConfig::default()
    };
    let day = synth_itch(3, 500, &cfg);
    let csv = truth_csv(&day);
    let text = std::str::from_utf8(&csv).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 501);
    assert!(lines[0].starts_with("ts,locate,bid_px,bid_qty,ask_px,ask_qty,bid1,"));
    assert!(lines[0].ends_with(",live_orders,book_hash"));
    let cols: Vec<&str> = lines[1].split(',').collect();
    assert_eq!(cols.len(), 18);
    assert_eq!(cols[17].len(), 64);
    assert_eq!(cols[1], "0");
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("truth_s3_500.csv");
    lob_synth::write_truth_csv(&day, &path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), csv);
    assert_eq!(sha256(&csv), sha256(&truth_csv(&day)));
}

#[test]
fn invalid_configs_are_named_by_check_args_before_any_generation() {
    use lob_synth::{MAX_MSGS, check_args};
    let ok = SynthConfig::default();
    assert_eq!(check_args(22, &ok), Ok(()));
    assert_eq!(check_args(MAX_MSGS, &ok), Ok(()));
    let cases: [(&str, SynthConfig); 9] = [
        (
            "locates",
            SynthConfig {
                locates: 0,
                ..ok.clone()
            },
        ),
        (
            "mix weights must not all be zero",
            SynthConfig {
                mix: [0.0; 9],
                ..ok.clone()
            },
        ),
        (
            "mix weights must be finite",
            SynthConfig {
                mix: [-1.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5],
                ..ok.clone()
            },
        ),
        (
            "mix weights must be finite",
            SynthConfig {
                mix: [f32::NAN; 9],
                ..ok.clone()
            },
        ),
        (
            "placeholder_rate must be in [0, 1], got 2",
            SynthConfig {
                placeholder_rate: 2.0,
                ..ok.clone()
            },
        ),
        (
            "placeholder_rate must be in [0, 1], got NaN",
            SynthConfig {
                placeholder_rate: f32::NAN,
                ..ok.clone()
            },
        ),
        (
            "unknown_ref_rate must be in [0, 1], got 1.5",
            SynthConfig {
                unknown_ref_rate: 1.5,
                ..ok.clone()
            },
        ),
        (
            "open_ns (10) must precede close_ns (10)",
            SynthConfig {
                open_ns: 10,
                close_ns: 10,
                ..ok.clone()
            },
        ),
        (
            "must fit 48 bits",
            SynthConfig {
                close_ns: 1 << 48,
                ..ok.clone()
            },
        ),
    ];
    for (text, cfg) in cases {
        let err = cfg.validate().unwrap_err();
        assert!(err.contains(text), "{err:?} should contain {text:?}");
        assert_eq!(check_args(1_000, &cfg).unwrap_err(), err);
    }
    assert_eq!(
        check_args(21, &ok).unwrap_err(),
        "n_msgs must be at least 2 * locates + 6 = 22, got 21"
    );
    assert!(
        check_args(MAX_MSGS + 1, &ok)
            .unwrap_err()
            .contains("at most")
    );
}

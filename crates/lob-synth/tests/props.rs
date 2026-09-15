//! Property test: every guarantee holds for random small configurations (locates, n, knobs, mix).

mod common;

use common::check_day;
use lob_synth::{SynthConfig, synth_itch};
use proptest::prelude::*;

fn config() -> impl Strategy<Value = SynthConfig> {
    (
        1u16..=4,
        proptest::array::uniform9(0.01f32..1.0),
        prop_oneof![Just(0.0f32), Just(0.05f32)],
        prop_oneof![Just(0.0f32), Just(0.1f32)],
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(locates, mix, placeholder_rate, unknown_ref_rate, subpenny, crossed_preopen)| {
                SynthConfig {
                    locates,
                    mix,
                    placeholder_rate,
                    unknown_ref_rate,
                    subpenny,
                    crossed_preopen,
                    ..SynthConfig::default()
                }
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(256),
        .. ProptestConfig::default()
    })]

    #[test]
    fn random_small_days_keep_every_guarantee(seed in any::<u64>(), cfg in config(), extra in 0u64..400) {
        let n = 2 * u64::from(cfg.locates) + 6 + extra;
        let day = synth_itch(seed, n, &cfg);
        let again = synth_itch(seed, n, &cfg);
        prop_assert_eq!(&day, &again);
        check_day(&day, &cfg, n);
    }
}

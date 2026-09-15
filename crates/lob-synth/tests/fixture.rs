//! The committed fixture `tests/fixtures/synth_s7_100k.itch` (seed 7, 100,000 messages, default
//! config) and its pinned sha256. Regenerate with
//! `cargo run --release -p lob-synth --example write_fixture` after any deliberate generator change.

use std::path::PathBuf;

use lob_synth::{SynthConfig, hex, sha256, synth_itch, truth_csv};

/// sha256 of the framed bytes for `synth_itch(7, 100_000, &SynthConfig::default())`.
const PINNED_BYTES_SHA256: &str =
    "20cc1acc13a893c060d0fd1658a544c54dff96cf66688a6be9ace78d9d752268";
/// sha256 of `truth_csv` for the same day (the sidecar is not committed; its hash is).
const PINNED_TRUTH_SHA256: &str =
    "41488ad1640f1e163983ed38bfd6c5d2f656c1ab1f0ba234f6b5ad7f62a39029";
const FIXTURE_BYTES: usize = 2_968_812;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

#[test]
fn seed7_100k_default_matches_the_pinned_sha256_and_the_committed_file() {
    let day = synth_itch(7, 100_000, &SynthConfig::default());
    assert_eq!(day.bytes.len(), FIXTURE_BYTES);
    assert_eq!(hex(&sha256(&day.bytes)), PINNED_BYTES_SHA256);
    assert_eq!(hex(&sha256(&truth_csv(&day))), PINNED_TRUTH_SHA256);

    let file = std::fs::read(fixtures_dir().join("synth_s7_100k.itch")).expect("committed fixture");
    assert_eq!(file.len(), FIXTURE_BYTES);
    assert!(
        file == day.bytes,
        "committed fixture differs from the generator output"
    );

    let sidecar = std::fs::read_to_string(fixtures_dir().join("synth_s7_100k.sha256"))
        .expect("sha256 sidecar");
    let lines: Vec<&str> = sidecar.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[0],
        format!("{PINNED_BYTES_SHA256}  synth_s7_100k.itch")
    );
    assert_eq!(
        lines[1],
        format!("{PINNED_TRUTH_SHA256}  synth_s7_100k.truth.csv")
    );
}

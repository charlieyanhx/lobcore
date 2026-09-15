//! Regenerates the committed fixture: `tests/fixtures/synth_s7_100k.itch` (seed 7, 100,000 messages,
//! `SynthConfig::default()`) and `synth_s7_100k.sha256` (sha256 of the bytes and of the truth CSV).
//!
//! `cargo run --release -p lob-synth --example write_fixture [OUT_DIR]`

use std::path::PathBuf;
use std::time::Instant;

use lob_synth::{SynthConfig, hex, sha256, synth_itch, truth_csv};

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures"));
    std::fs::create_dir_all(&out_dir).expect("create fixture dir");
    let t = Instant::now();
    let day = synth_itch(7, 100_000, &SynthConfig::default());
    let gen_s = t.elapsed().as_secs_f64();
    let bytes_hash = hex(&sha256(&day.bytes));
    let csv = truth_csv(&day);
    let csv_hash = hex(&sha256(&csv));
    std::fs::write(out_dir.join("synth_s7_100k.itch"), &day.bytes).expect("write fixture");
    std::fs::write(
        out_dir.join("synth_s7_100k.sha256"),
        format!("{bytes_hash}  synth_s7_100k.itch\n{csv_hash}  synth_s7_100k.truth.csv\n"),
    )
    .expect("write sha256");
    println!(
        "wrote {} bytes ({} messages) in {gen_s:.2}s\nbytes  sha256 {bytes_hash}\ntruth  sha256 {csv_hash} ({} bytes, not committed)",
        day.bytes.len(),
        day.truth.len(),
        csv.len()
    );
}

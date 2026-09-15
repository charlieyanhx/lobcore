//! The `lobcore` binary: clap parses every subcommand; `replay --stats` on the committed
//! fixture reproduces the pinned counts and hashes (identical with and without an array
//! watchlist); `synth` reproduces the fixture bytes; `--write-readme` / `--check-readme` round
//! trip on a temporary README and detect drift; `bench-once` prints a parseable line.

use std::path::PathBuf;
use std::process::Command;

use clap::Parser;
use lob_bench::bench::OnceResult;
use lob_bench::cli::{Cli, Command as Sub, Mode};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/synth_s7_100k.itch"
);
const EVENT_LOG_HASH: &str = "fd52410fc6465823184edb2830ae6969fb27a5d7f2a4ea4497012a2c16245fcc";
const ALL_LOCATES_HASH: &str = "851e617c74e7a148336501e55073f060783565de218818a853275f69e891ae34";

fn lobcore() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lobcore"))
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lobcore-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn clap_parses_every_subcommand() {
    let c = Cli::try_parse_from([
        "lobcore",
        "replay",
        "--stats",
        "x.itch",
        "--symbol",
        "SPY",
        "--symbol",
        "AAPL",
        "--locate",
        "7",
        "--array-window",
        "1024",
        "--check-readme",
        "README.md",
    ])
    .unwrap();
    let Sub::Replay(a) = c.command else { panic!() };
    assert!(a.stats);
    assert_eq!(a.file, PathBuf::from("x.itch"));
    assert_eq!(a.symbols, vec!["SPY", "AAPL"]);
    assert_eq!(a.locates, vec![7]);
    assert_eq!(a.array_window, 1024);
    assert_eq!(a.check_readme, Some(PathBuf::from("README.md")));
    assert_eq!(a.write_readme, None);
    assert!(!a.all);

    let c = Cli::try_parse_from(["lobcore", "replay", "x.itch"]).unwrap();
    let Sub::Replay(a) = c.command else { panic!() };
    assert!(!a.stats);
    assert_eq!(a.array_window, 2048);

    let c = Cli::try_parse_from([
        "lobcore", "synth", "--seed", "7", "--n", "100000", "--out", "o.itch", "--truth", "t.csv",
    ])
    .unwrap();
    let Sub::Synth(a) = c.command else { panic!() };
    assert_eq!((a.seed, a.n, a.locates), (7, 100_000, 8));
    assert_eq!(a.out, PathBuf::from("o.itch"));
    assert_eq!(a.truth, Some(PathBuf::from("t.csv")));

    let c =
        Cli::try_parse_from(["lobcore", "bench-quick", "f.itch", "--array-window", "512"]).unwrap();
    let Sub::BenchQuick(a) = c.command else {
        panic!()
    };
    assert_eq!(a.runs, 5);
    assert_eq!(a.array_window, 512);
    assert!(a.symbols.is_empty());

    let c = Cli::try_parse_from([
        "lobcore",
        "bench-once",
        "f.itch",
        "--mode",
        "array-log",
        "--all",
    ])
    .unwrap();
    let Sub::BenchOnce(a) = c.command else {
        panic!()
    };
    assert_eq!(a.mode, Mode::ArrayLog);
    assert!(a.all);

    assert!(Cli::try_parse_from(["lobcore", "synth", "--seed", "1"]).is_err());
    assert!(Cli::try_parse_from(["lobcore", "nope"]).is_err());
}

fn stats_output(extra: &[&str]) -> String {
    let out = lobcore()
        .args(["replay", "--stats", FIXTURE])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn replay_stats_reproduces_the_pinned_counts_and_hashes() {
    let text = stats_output(&[]);
    for line in [
        "| messages (complete frames) | 100,000 |",
        "| truncated final message | 0 |",
        "| inflated bytes consumed | 2,968,812 |",
        "| by type | S 6 · R 8 · H 8 · L 4,987 · A 40,086 · F 943 · E 668 · X 11,134 · D 37,219 · U 4,845 · P 96 |",
        "| locates with a book | 8 (0 array, 8 reference); watchlist: none; array window 2048 levels/side |",
        "| live orders: high-water mark / at close | 3,657 / 3,649 |",
        "| crossed snapshots before Q | 9,819 |",
        "| crossed snapshots after Q (all / trading-state T) | 0 / 0 |",
        "| unknown-id | 0 |",
        "| negative-qty attempts (over-execute + over-cancel) | 0 (0 + 0) |",
        "| E/C at level head | 668 / 668 = 1.0000 |",
        "| total | 4,263 | 14,040 | 13,706 | 13,628 | 13,676 | 13,579 | 13,633 | 13,472 | 3 | 100,000 |",
    ] {
        assert!(text.contains(line), "missing {line:?} in\n{text}");
    }
    assert!(text.contains(EVENT_LOG_HASH));
    assert!(text.contains(ALL_LOCATES_HASH));
    assert!(text.contains("| msgs/s parse+apply + event-log sha256, excluding gunzip/read |"));

    // the same hashes with three locates on the array book, and the rows say so
    let arr = stats_output(&[
        "--symbol", "SYN0003", "--locate", "5", "--symbol", "SYN0008",
    ]);
    assert!(arr.contains(EVENT_LOG_HASH));
    assert!(arr.contains(ALL_LOCATES_HASH));
    assert!(arr.contains(
        "| locates with a book | 8 (3 array, 5 reference); watchlist: SYN0003 SYN0008 #5;"
    ));
    assert!(arr.contains("| 3 | SYN0003 | array |"));
    assert!(arr.contains("| 5 | SYN0005 | array |"));
    assert!(arr.contains("| 8 | SYN0008 | array |"));
    // with a watchlist only its locates are listed
    assert!(!arr.contains("| 4 | SYN0004 |"));
    let det = |t: &str| lob_feed::stats::deterministic_lines(t).unwrap();
    let (a, b) = (det(&text), det(&arr));
    // everything above the per-locate table agrees except the watchlist row
    let head = |v: &[String]| {
        v.iter()
            .take_while(|l| !l.starts_with("| locate |"))
            .filter(|l| !l.starts_with("| locates with a book"))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(head(&a), head(&b));
    assert_eq!(a.iter().filter(|l| l.contains("| reference |")).count(), 8);
    assert_eq!(b.iter().filter(|l| l.contains("| array |")).count(), 3);
}

#[test]
fn synth_reproduces_the_fixture_and_writes_truth() {
    let out = tmp("s7.itch");
    let truth = tmp("s7.csv");
    let st = lobcore()
        .args(["synth", "--seed", "7", "--n", "100000"])
        .arg("--out")
        .arg(&out)
        .arg("--truth")
        .arg(&truth)
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(
        std::fs::read(&out).unwrap(),
        std::fs::read(FIXTURE).unwrap()
    );
    let csv = std::fs::read_to_string(&truth).unwrap();
    assert!(csv.starts_with("ts,locate,bid_px,"));
    assert_eq!(csv.lines().count(), 100_001);
    let bad = lobcore()
        .args(["synth", "--seed", "1", "--n", "5", "--out"])
        .arg(tmp("bad.itch"))
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("at least"));
}

#[test]
fn readme_write_then_check_round_trip_and_drift() {
    let readme = tmp("README.md");
    std::fs::write(&readme, "# lobcore\n\nintro\n\n## Results\n").unwrap();
    // no block yet: check fails
    let out = lobcore()
        .args(["replay", "--stats", FIXTURE, "--check-readme"])
        .arg(&readme)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no stats block"));
    // write, then check passes
    let st = lobcore()
        .args(["replay", FIXTURE, "--write-readme"])
        .arg(&readme)
        .status()
        .unwrap();
    assert!(st.success());
    let text = std::fs::read_to_string(&readme).unwrap();
    assert!(text.starts_with("# lobcore\n\nintro\n\n## Results\n"));
    assert!(text.contains(lob_feed::stats::BEGIN_MARK));
    assert!(text.contains(EVENT_LOG_HASH));
    let st = lobcore()
        .args(["replay", FIXTURE, "--check-readme"])
        .arg(&readme)
        .status()
        .unwrap();
    assert!(st.success());
    // a changed msgs/s row is not drift; a changed count is
    let faster = text.replace("| msgs/s parse+apply", "| msgs/s parse+apply (999 M)");
    std::fs::write(&readme, &faster).unwrap();
    let st = lobcore()
        .args(["replay", FIXTURE, "--check-readme"])
        .arg(&readme)
        .status()
        .unwrap();
    assert!(st.success());
    let drift = text.replace("| unknown-id | 0 |", "| unknown-id | 1 |");
    std::fs::write(&readme, &drift).unwrap();
    let out = lobcore()
        .args(["replay", FIXTURE, "--check-readme"])
        .arg(&readme)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("drifted"));
    assert!(err.contains("| unknown-id | 1 |"));
    // writing again replaces the block in place (one block, prefix intact)
    let st = lobcore()
        .args(["replay", FIXTURE, "--write-readme"])
        .arg(&readme)
        .status()
        .unwrap();
    assert!(st.success());
    let again = std::fs::read_to_string(&readme).unwrap();
    assert_eq!(again.matches(lob_feed::stats::BEGIN_MARK).count(), 1);
    assert!(again.starts_with("# lobcore\n\nintro\n\n## Results\n"));
    assert_eq!(
        lob_feed::stats::deterministic_lines(&again),
        lob_feed::stats::deterministic_lines(&text)
    );
    // a different watchlist is drift (the watchlist row is deterministic)
    let out = lobcore()
        .args(["replay", FIXTURE, "--symbol", "SYN0001", "--check-readme"])
        .arg(&readme)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn bench_once_prints_a_parseable_line() {
    for mode in ["parse", "array", "ref", "array-log"] {
        let out = lobcore()
            .args(["bench-once", FIXTURE, "--mode", mode, "--all"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        let r = text.lines().find_map(OnceResult::parse_line).unwrap();
        assert_eq!(r.mode.flag(), mode);
        assert_eq!(r.n, 100_000);
        assert!(r.ns > 0);
    }
}

#[test]
fn bench_quick_refuses_an_unknown_symbol_and_runs_one_round() {
    let out = lobcore()
        .args(["bench-quick", FIXTURE, "--runs", "1", "--symbol", "NOPE"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not in the file's directory"));
    let out = lobcore()
        .args(["bench-quick", FIXTURE, "--runs", "1", "--symbol", "SYN0002"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("| preflight | value |"));
    assert!(text.contains("| 1-min load average |"));
    assert!(text.contains("| parse only | 100,000 | 1 |"));
    assert!(text.contains("| parse+apply, ArrayBook watchlist | 100,000 | 1 |"));
    assert!(text.contains("| parse+apply, RefBook everywhere | 100,000 | 1 |"));
    assert!(text.contains("ArrayBook / RefBook parse+apply throughput ratio"));
}

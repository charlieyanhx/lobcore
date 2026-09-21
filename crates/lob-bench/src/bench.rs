//! `lobcore bench-quick`: messages per second for four modes (parse only, parse+apply on the
//! array book, parse+apply on the reference book, array book + event-log sha256) over `runs`
//! fresh processes (modes interleaved run by run, so drift hits every mode alike), reported as
//! the median with min-max, plus the array / reference ratio both as a ratio of medians and as
//! the min / median / max of the per-run paired ratios (run i array over run i reference). The
//! input is inflated into memory once per process, so every number excludes gunzip and file
//! I/O. `bench-once` is the child.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use lob_feed::itch::msg::parse;
use lob_feed::{Session, SessionConfig, SliceFrames, Watchlist, read_all};

use crate::Error;
use crate::cli::{BenchOnceArgs, BenchQuickArgs, Mode};
use crate::preflight;

/// Refuse to persist results above this 1-minute load average.
pub const LOAD_GATE: f64 = 1.0;
/// Watchlist default: every locate when the file has at most this many.
pub const ALL_LOCATES_LIMIT: usize = 64;

/// One child's result line: `lobcore-bench-once mode=<m> n=<n> ns=<ns>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnceResult {
    pub mode: Mode,
    pub n: u64,
    pub ns: u64,
}

impl OnceResult {
    pub fn msgs_per_s(&self) -> f64 {
        self.n as f64 * 1e9 / self.ns.max(1) as f64
    }

    pub fn ns_per_msg(&self) -> f64 {
        self.ns as f64 / self.n.max(1) as f64
    }

    /// Parse the child's line.
    pub fn parse_line(line: &str) -> Option<OnceResult> {
        let mut mode = None;
        let mut n = None;
        let mut ns = None;
        if !line.starts_with("lobcore-bench-once ") {
            return None;
        }
        for kv in line.split_whitespace().skip(1) {
            let (k, v) = kv.split_once('=')?;
            match k {
                "mode" => {
                    mode = Some(match v {
                        "parse" => Mode::Parse,
                        "array" => Mode::Array,
                        "ref" => Mode::Ref,
                        "array-log" => Mode::ArrayLog,
                        _ => return None,
                    })
                }
                "n" => n = v.parse().ok(),
                "ns" => ns = v.parse().ok(),
                _ => {}
            }
        }
        Some(OnceResult {
            mode: mode?,
            n: n?,
            ns: ns?,
        })
    }
}

fn load(file: &Path) -> Result<Vec<u8>, Error> {
    let (bytes, _cut) =
        read_all(file).map_err(|e| Error(format!("cannot read {}: {e}", file.display())))?;
    Ok(bytes)
}

/// Time one pass over in-memory bytes. Returns `(messages, ns)`.
pub fn time_once(bytes: &[u8], mode: Mode, watchlist: Watchlist, window: u32) -> (u64, u64) {
    match mode {
        Mode::Parse => {
            let mut counts = [0u64; 256];
            let mut ts_sum = 0u64;
            let mut n = 0u64;
            let t0 = Instant::now();
            for p in SliceFrames::new(bytes) {
                let m = parse(p).expect("length table");
                counts[m.ty() as usize] += 1;
                ts_sum = ts_sum.wrapping_add(m.ts().unwrap_or(0));
                n += 1;
            }
            let ns = t0.elapsed().as_nanos() as u64;
            // keep the decoded values alive so the loop is not optimised away
            std::hint::black_box((counts, ts_sum));
            (n, ns)
        }
        Mode::Array | Mode::Ref | Mode::ArrayLog => {
            let wl = if mode == Mode::Ref {
                Watchlist::none()
            } else {
                watchlist
            };
            let mut s = Session::new(SessionConfig {
                watchlist: wl,
                array_window: window,
                log_events: mode == Mode::ArrayLog,
                ..Default::default()
            });
            s.replay(&mut SliceFrames::new(bytes)).expect("replay");
            let st = s.stats();
            (st.messages, st.timing.map(|t| t.wall_ns).unwrap_or(0))
        }
    }
}

/// `lobcore bench-once` (child).
pub fn run_once(a: &BenchOnceArgs) -> Result<i32, Error> {
    let bytes = load(&a.file)?;
    let wl = if a.all {
        Watchlist::all()
    } else {
        Watchlist::symbols(&a.symbols)
    };
    let (n, ns) = time_once(&bytes, a.mode, wl, a.array_window);
    println!("lobcore-bench-once mode={} n={n} ns={ns}", a.mode.flag());
    Ok(0)
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Summary of one mode over its runs.
#[derive(Debug, Clone)]
pub struct ModeSummary {
    pub mode: Mode,
    pub n: u64,
    pub runs: usize,
    pub median_mps: f64,
    pub min_mps: f64,
    pub max_mps: f64,
    pub median_ns: f64,
}

/// Aggregate results per mode.
pub fn summarise(results: &[OnceResult]) -> Vec<ModeSummary> {
    Mode::ALL
        .into_iter()
        .filter_map(|mode| {
            let rs: Vec<&OnceResult> = results.iter().filter(|r| r.mode == mode).collect();
            if rs.is_empty() {
                return None;
            }
            let mut mps: Vec<f64> = rs.iter().map(|r| r.msgs_per_s()).collect();
            let mut nspm: Vec<f64> = rs.iter().map(|r| r.ns_per_msg()).collect();
            Some(ModeSummary {
                mode,
                n: rs[0].n,
                runs: rs.len(),
                median_mps: median(&mut mps),
                min_mps: mps[0],
                max_mps: mps[mps.len() - 1],
                median_ns: median(&mut nspm),
            })
        })
        .collect()
}

/// Per-run paired ratios array / reference: run i of the array mode over run i of the
/// reference mode, in run order (the two modes ran back to back in that run, so a load spike
/// hits both). Empty when either mode is missing.
pub fn paired_ratios(results: &[OnceResult]) -> Vec<f64> {
    let arr = results.iter().filter(|r| r.mode == Mode::Array);
    let rf = results.iter().filter(|r| r.mode == Mode::Ref);
    arr.zip(rf)
        .map(|(a, r)| a.msgs_per_s() / r.msgs_per_s())
        .collect()
}

/// Markdown table of the summaries plus the array / reference ratio (of the medians, and the
/// min / median / max of the per-run paired ratios).
pub fn render_table(
    sum: &[ModeSummary],
    paired: &[f64],
    source: &str,
    window: u32,
    watchlist: &str,
) -> String {
    let mut o = String::new();
    let _ = writeln!(
        o,
        "| mode | messages | runs | median msgs/s | min – max msgs/s | median ns/msg |"
    );
    let _ = writeln!(o, "|---|---:|---:|---:|---|---:|");
    for s in sum {
        let _ = writeln!(
            o,
            "| {} | {} | {} | {:.2} M | {:.2} – {:.2} M | {:.1} |",
            s.mode.label(),
            lob_feed::stats::commas(s.n),
            s.runs,
            s.median_mps / 1e6,
            s.min_mps / 1e6,
            s.max_mps / 1e6,
            s.median_ns
        );
    }
    let arr = sum.iter().find(|s| s.mode == Mode::Array);
    let rf = sum.iter().find(|s| s.mode == Mode::Ref);
    if let (Some(a), Some(r)) = (arr, rf) {
        let mut p = paired.to_vec();
        let (lo, hi) = (
            p.iter().cloned().fold(f64::INFINITY, f64::min),
            p.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        );
        let mid = median(&mut p);
        let _ = writeln!(
            o,
            "\nArrayBook / RefBook parse+apply throughput ratio (medians): {:.2}x; per-run paired ratio min / median / max over {} runs: {lo:.2}x / {mid:.2}x / {hi:.2}x; source `{source}`, array window {window} levels/side, watchlist {watchlist}; in-memory bytes, gunzip and file I/O excluded; fresh process per run, modes interleaved; the array and reference rows do not hash the event log, the last row does.",
            a.median_mps / r.median_mps,
            paired.len()
        );
    }
    o
}

/// `lobcore bench-quick` (parent).
pub fn run_quick(a: &BenchQuickArgs) -> Result<i32, Error> {
    if a.runs == 0 {
        return Err(Error("--runs must be at least 1".into()));
    }
    let pf = preflight::gather();
    let mut report = String::new();
    let _ = writeln!(report, "## lobcore bench-quick\n");
    report.push_str(&pf.render());
    report.push('\n');

    // one warm pass in-process: message count, locate count, watchlist decision
    let bytes = load(&a.file)?;
    let mut probe = Session::new(SessionConfig::default());
    probe.replay(&mut SliceFrames::new(&bytes))?;
    let n_loc = probe.stats().locates_with_book().count();
    let (all, wl_desc) = if !a.symbols.is_empty() {
        for s in &a.symbols {
            if probe.locate_of(s).is_none() {
                return Err(Error(format!("symbol {s} not in the file's directory")));
            }
        }
        (false, a.symbols.join(" "))
    } else if n_loc <= ALL_LOCATES_LIMIT {
        (true, format!("all {n_loc} locates"))
    } else {
        return Err(Error(format!(
            "{n_loc} locates in the file: pass --symbol SYM for the array-book watchlist (a full-market array window does not fit in memory)"
        )));
    };
    let _ = writeln!(
        report,
        "Input `{}`: {} messages, {} locates, {} inflated bytes; array watchlist: {wl_desc}; window {} levels/side.\n",
        a.file.display(),
        lob_feed::stats::commas(probe.stats().messages),
        n_loc,
        lob_feed::stats::commas(bytes.len() as u64),
        a.array_window
    );
    drop(bytes);

    let exe = std::env::current_exe()?;
    let mut results = Vec::new();
    for run in 0..a.runs {
        for mode in Mode::ALL {
            let mut c = Command::new(&exe);
            c.arg("bench-once")
                .arg(&a.file)
                .arg("--mode")
                .arg(mode.flag())
                .arg("--array-window")
                .arg(a.array_window.to_string());
            if all {
                c.arg("--all");
            }
            for s in &a.symbols {
                c.arg("--symbol").arg(s);
            }
            let out = c.output()?;
            if !out.status.success() {
                return Err(Error(format!(
                    "bench-once failed (run {run}, {}): {}",
                    mode.flag(),
                    String::from_utf8_lossy(&out.stderr)
                )));
            }
            let text = String::from_utf8_lossy(&out.stdout);
            let r = text
                .lines()
                .find_map(OnceResult::parse_line)
                .ok_or_else(|| Error(format!("bench-once printed no result: {text}")))?;
            eprintln!(
                "run {} {}: {:.2} M msgs/s",
                run + 1,
                mode.flag(),
                r.msgs_per_s() / 1e6
            );
            results.push(r);
        }
    }
    let sum = summarise(&results);
    report.push_str(&render_table(
        &sum,
        &paired_ratios(&results),
        &a.file
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        a.array_window,
        &wl_desc,
    ));
    print!("{report}");

    if let Some(out) = &a.out {
        match pf.load1 {
            Some(l) if l > LOAD_GATE => {
                eprintln!(
                    "not persisting: 1-minute load average {l:.2} > {LOAD_GATE} (results printed only)"
                );
                return Ok(3);
            }
            None => {
                eprintln!("not persisting: load average unavailable (results printed only)");
                return Ok(3);
            }
            _ => {}
        }
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(out)
            .map_err(|e| Error(format!("cannot open {}: {e}", out.display())))?;
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        writeln!(
            f,
            "\n<!-- lobcore bench-quick, unix time {secs} -->\n{report}"
        )?;
        eprintln!("appended the report to {}", out.display());
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn once_result_line_round_trip_and_median() {
        let r =
            OnceResult::parse_line("lobcore-bench-once mode=array n=100000 ns=25000000").unwrap();
        assert_eq!(r.mode, Mode::Array);
        assert_eq!(r.msgs_per_s(), 4_000_000.0);
        assert_eq!(r.ns_per_msg(), 250.0);
        assert!(OnceResult::parse_line("noise").is_none());
        assert!(OnceResult::parse_line("lobcore-bench-once mode=zzz n=1 ns=1").is_none());
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&mut [4.0, 1.0, 2.0, 3.0]), 2.5);
        let rs = [
            OnceResult {
                mode: Mode::Ref,
                n: 10,
                ns: 100,
            },
            OnceResult {
                mode: Mode::Ref,
                n: 10,
                ns: 50,
            },
            OnceResult {
                mode: Mode::Array,
                n: 10,
                ns: 20,
            },
            OnceResult {
                mode: Mode::Ref,
                n: 10,
                ns: 200,
            },
        ];
        let s = summarise(&rs);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].mode, Mode::Array);
        assert_eq!(s[1].runs, 3);
        assert_eq!(s[1].median_ns, 10.0);
        assert_eq!(s[1].min_mps, 5e7);
        assert_eq!(s[1].max_mps, 2e8);
        // one array run pairs with the first reference run only (100 ns vs 20 ns -> 5x)
        let paired = paired_ratios(&rs);
        assert_eq!(paired, vec![5.0]);
        let t = render_table(&s, &paired, "x", 2048, "all");
        assert!(t.contains("ratio (medians): 5.00x"));
        assert!(t.contains(
            "per-run paired ratio min / median / max over 1 runs: 5.00x / 5.00x / 5.00x"
        ));
        // three interleaved runs: pairs are (run 1 array, run 1 ref) ... in run order
        let once = |mode, ns| OnceResult { mode, n: 10, ns };
        let three = [
            once(Mode::Array, 10),
            once(Mode::Ref, 40), // 4x
            once(Mode::Array, 100),
            once(Mode::Ref, 100), // 1x, a load spike hit both
            once(Mode::Array, 20),
            once(Mode::Ref, 50), // 2.5x
        ];
        assert_eq!(paired_ratios(&three), vec![4.0, 1.0, 2.5]);
        let t = render_table(&summarise(&three), &paired_ratios(&three), "x", 2048, "all");
        assert!(t.contains("ratio (medians): 2.50x"), "{t}");
        assert!(t.contains(
            "per-run paired ratio min / median / max over 3 runs: 1.00x / 2.50x / 4.00x"
        ));
    }
}

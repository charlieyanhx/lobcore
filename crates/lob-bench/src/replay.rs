//! `lobcore replay` and `lobcore synth`.

use std::path::Path;

use lob_feed::{Session, SessionConfig, Watchlist, open};
use lob_synth::{SynthConfig, synth_itch, write_truth_csv};

use crate::Error;
use crate::cli::{ReplayArgs, SynthArgs};
use crate::readme;

fn watchlist(symbols: &[String], locates: &[u16], all: bool) -> Watchlist {
    if all {
        return Watchlist::all();
    }
    Watchlist::symbols(symbols).with_locates(locates.iter().copied())
}

fn basename(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Replay `file` with the given watchlist and return the finished session.
pub fn replay_file(
    file: &Path,
    symbols: &[String],
    locates: &[u16],
    all: bool,
    array_window: u32,
) -> Result<Session, Error> {
    if !(1..=4096).contains(&array_window) {
        return Err(Error(format!(
            "--array-window must be in 1..=4096, got {array_window}"
        )));
    }
    let mut s = Session::new(SessionConfig {
        watchlist: watchlist(symbols, locates, all),
        array_window,
        source: basename(file),
        ..Default::default()
    });
    let mut src = open(file).map_err(|e| Error(format!("cannot open {}: {e}", file.display())))?;
    s.replay(&mut src)?;
    Ok(s)
}

/// `lobcore replay`.
pub fn run(a: &ReplayArgs) -> Result<i32, Error> {
    let s = replay_file(&a.file, &a.symbols, &a.locates, a.all, a.array_window)?;
    let st = s.stats();
    let block = st.render();
    if a.stats {
        print!("{block}");
    } else {
        let t = st.timing.unwrap_or_default();
        println!(
            "{}: {} messages, {} truncated, unknown-id {}, negative-qty {}, live HWM {}, {:.2} M msgs/s excluding read, event log {}",
            st.source,
            st.messages,
            st.truncated,
            st.unknown_id,
            st.negative_qty(),
            st.live_hwm,
            t.msgs_per_s_excl(st.messages) / 1e6,
            lob_core::hex(&st.event_log_hash)
        );
    }
    let mut code = 0;
    if let Some(p) = &a.write_readme {
        readme::write(p, &st.render_block())?;
        eprintln!("wrote the stats block into {}", p.display());
    }
    if let Some(p) = &a.check_readme {
        match readme::check(p, &st.render_block())? {
            readme::Check::Same => eprintln!("{}: stats block up to date", p.display()),
            readme::Check::Missing => {
                eprintln!(
                    "{}: no stats block (markers {} / {} absent)",
                    p.display(),
                    lob_feed::stats::BEGIN_MARK,
                    lob_feed::stats::END_MARK
                );
                code = 1;
            }
            readme::Check::Drift { line, readme, now } => {
                eprintln!(
                    "{}: stats block drifted at deterministic line {line}:\n  README: {readme}\n  now:    {now}",
                    p.display()
                );
                code = 1;
            }
        }
    }
    Ok(code)
}

/// `lobcore synth`.
pub fn synth(a: &SynthArgs) -> Result<i32, Error> {
    let cfg = SynthConfig {
        locates: a.locates,
        ..Default::default()
    };
    // `lob-synth` panics on bad arguments (and this binary is `panic = "abort"`): reject here
    // with the `lobcore: <message>` / exit 1 contract instead. `--locates 0` and `--n` below
    // `2 * locates + 6` are the two reachable cases.
    lob_synth::check_args(a.n, &cfg).map_err(|e| Error(format!("--{e}")))?;
    let day = synth_itch(a.seed, a.n, &cfg);
    std::fs::write(&a.out, &day.bytes)?;
    if let Some(t) = &a.truth {
        write_truth_csv(&day, t)?;
    }
    eprintln!(
        "wrote {} ({} bytes, {} messages, sha256 {})",
        a.out.display(),
        day.bytes.len(),
        day.truth.len(),
        lob_synth::hex(&lob_synth::sha256(&day.bytes))
    );
    Ok(0)
}

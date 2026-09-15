//! `lobcore.replay_itch` (batch numpy snapshots + features) and `lobcore.replay_stats` (the
//! replay-statistics contract), both driven by `lob_feed::Session` with the GIL released.

use std::path::PathBuf;

use lob_core::OrderBook;
use lob_features::{L1, L5};
use lob_feed::itch::msg::{Msg, parse};
use lob_feed::stats::{LocateStats, Stats};
use lob_feed::{FrameSource, Session, SessionConfig, Watchlist, open};
use numpy::PyArray1;
use pyo3::exceptions::{PyIOError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};

/// Columns of one `replay_itch` batch (struct of arrays so the numpy conversion is one copy
/// per column and the loop never touches a Python object).
#[derive(Default)]
struct Columns {
    ts: Vec<u64>,
    locate: Vec<u16>,
    bid_px: Vec<i32>,
    bid_qty: Vec<u32>,
    ask_px: Vec<i32>,
    ask_qty: Vec<u32>,
    depth5_bid: Vec<u64>,
    depth5_ask: Vec<u64>,
    wmid: Vec<f64>,
    imb1: Vec<f64>,
    imb5: Vec<f64>,
    spread: Vec<i32>,
    ofi: Vec<i64>,
}

/// Per-locate sampling state.
#[derive(Clone, Copy, Default)]
struct Sampler {
    /// Book-changing messages seen since the last emitted row.
    since: u64,
    /// Timestamp of the last emitted row (`every_ns` mode).
    last_ts: Option<u64>,
    /// L1 of the last emitted row, when both sides were present (for the OFI step).
    prev_l1: Option<L1>,
}

#[derive(Clone, Copy)]
enum Cadence {
    EveryN(u64),
    EveryNs(u64),
}

fn watchlist(symbols: &Option<Vec<String>>, locates: &Option<Vec<u16>>) -> Watchlist {
    let mut w = Watchlist::none();
    if let Some(s) = symbols {
        w = Watchlist::symbols(s);
    }
    if let Some(l) = locates {
        w = w.with_locates(l.iter().copied());
    }
    w
}

fn check_window(array_window: u32) -> PyResult<()> {
    if !(1..=lob_core::MAX_LEVELS).contains(&array_window) {
        return Err(PyValueError::new_err(format!(
            "array_window must be in 1..={}, got {array_window}",
            lob_core::MAX_LEVELS
        )));
    }
    Ok(())
}

fn is_book_msg(m: &Msg<'_>) -> bool {
    matches!(
        m,
        Msg::Add(_)
            | Msg::AddMpid(_)
            | Msg::Exec(_)
            | Msg::ExecPx(_)
            | Msg::Cancel(_)
            | Msg::Delete(_)
            | Msg::Replace(_)
    )
}

/// Append one row for `locate` from its book.
fn emit(
    cols: &mut Columns,
    sampler: &mut Sampler,
    features: bool,
    ts: u64,
    locate: u16,
    book: &dyn OrderBook,
) {
    let (bid, ask) = book.l1();
    let (bpx, bq) = bid.map_or((0, 0), |l| (l.0, l.1));
    let (apx, aq) = ask.map_or((0, 0), |l| (l.0, l.1));
    cols.ts.push(ts);
    cols.locate.push(locate);
    cols.bid_px.push(bpx);
    cols.bid_qty.push(bq);
    cols.ask_px.push(apx);
    cols.ask_qty.push(aq);
    if !features {
        return;
    }
    let (b5, a5) = book.l2(5);
    let mut l5 = L5 {
        bid: [0; 5],
        ask: [0; 5],
    };
    for (i, l) in b5.iter().enumerate() {
        l5.bid[i] = l.1;
    }
    for (i, l) in a5.iter().enumerate() {
        l5.ask[i] = l.1;
    }
    cols.depth5_bid.push(lob_features::depth5(&l5.bid));
    cols.depth5_ask.push(lob_features::depth5(&l5.ask));
    cols.imb5
        .push(lob_features::imb5(&l5).map_or(f64::NAN, |r| r.as_f64()));
    let both = bid.is_some() && ask.is_some();
    let cur = both.then(|| L1::new(bpx as i64, bq, apx as i64, aq));
    cols.wmid.push(
        cur.and_then(|l| lob_features::wmid(l.bid_px, l.bid_qty, l.ask_px, l.ask_qty))
            .map_or(f64::NAN, |w| w.as_f64()),
    );
    cols.imb1.push(
        cur.and_then(|l| lob_features::imb1(l.bid_qty, l.ask_qty))
            .map_or(f64::NAN, |r| r.as_f64()),
    );
    cols.spread
        .push(cur.map_or(0, |l| lob_features::spread(&l) as i32));
    let step = match (sampler.prev_l1, cur) {
        (Some(p), Some(c)) => lob_features::ofi_step(&p, &c),
        _ => 0,
    };
    cols.ofi.push(step);
    sampler.prev_l1 = cur;
}

/// The whole loop: frames -> session -> sampled rows. Runs without the GIL.
fn run_replay(
    path: &PathBuf,
    wl: Watchlist,
    array_window: u32,
    cadence: Cadence,
    features: bool,
) -> Result<(Columns, Stats), String> {
    let sample_all = wl == Watchlist::none();
    let mut session = Session::new(SessionConfig {
        watchlist: wl,
        array_window,
        source: path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        log_events: false,
        ..Default::default()
    });
    let mut src = open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut cols = Columns::default();
    let mut samplers: Vec<Sampler> = Vec::new();
    let mut at = 0u64;
    loop {
        let payload = match src.next_frame() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(e) => return Err(format!("read error: {e}")),
        };
        let m = parse(payload).map_err(|e| format!("message #{at}: {e}"))?;
        at += 1;
        let book_msg = is_book_msg(&m);
        let locate = m.locate();
        let ts = m.ts();
        session.apply_msg(m);
        let (Some(locate), Some(ts)) = (locate, ts) else {
            continue;
        };
        if !book_msg {
            continue;
        }
        let stats = session.stats();
        let sampled = sample_all
            || stats
                .locates
                .get(locate as usize)
                .is_some_and(|l| l.watched);
        if !sampled {
            continue;
        }
        if samplers.len() <= locate as usize {
            samplers.resize(locate as usize + 1, Sampler::default());
        }
        let s = &mut samplers[locate as usize];
        s.since += 1;
        let due = match cadence {
            Cadence::EveryN(n) => s.since >= n,
            Cadence::EveryNs(d) => s.last_ts.is_none_or(|t| ts.saturating_sub(t) >= d),
        };
        if !due {
            continue;
        }
        s.since = 0;
        s.last_ts = Some(ts);
        if let Some(book) = session.book(locate) {
            emit(&mut cols, s, features, ts, locate, book);
        }
    }
    session.finish();
    Ok((cols, session.into_stats()))
}

/// Replay an ITCH 5.0 file (plain or `.gz`) and return sampled L1 snapshots with book features
/// as a dict of numpy arrays.
///
/// - `symbols` / `locates`: the locates to sample; they run on the bounded-array book. With
///   neither given every locate is sampled on the reference book.
/// - `every_n`: emit a row after every n-th book-changing message of a sampled locate;
///   `every_ns`: emit when at least that many nanoseconds passed since the locate's last row.
///   Default (both `None`) is `every_n=1`; passing both is an error.
/// - `features=False` returns only `ts, locate, bid_px, bid_qty, ask_px, ask_qty`.
/// - An empty side has `px = 0, qty = 0`; `wmid`, `imb1`, `imb5` are NaN when undefined;
///   `spread` is 0 unless both sides are present; `ofi` is the Cont-Kukanov-Stoikov step
///   between this row and the locate's previous row (0 when either row lacked a side).
#[pyfunction]
#[pyo3(signature = (path, symbols = None, locates = None, every_n = None, every_ns = None, features = true, array_window = 1024))]
#[allow(clippy::too_many_arguments)]
pub fn replay_itch<'py>(
    py: Python<'py>,
    path: PathBuf,
    symbols: Option<Vec<String>>,
    locates: Option<Vec<u16>>,
    every_n: Option<u64>,
    every_ns: Option<u64>,
    features: bool,
    array_window: u32,
) -> PyResult<Bound<'py, PyDict>> {
    check_window(array_window)?;
    let cadence = match (every_n, every_ns) {
        (Some(_), Some(_)) => {
            return Err(PyValueError::new_err("pass every_n or every_ns, not both"));
        }
        (Some(0), _) | (_, Some(0)) => {
            return Err(PyValueError::new_err("every_n / every_ns must be positive"));
        }
        (Some(n), None) => Cadence::EveryN(n),
        (None, Some(d)) => Cadence::EveryNs(d),
        (None, None) => Cadence::EveryN(1),
    };
    let wl = watchlist(&symbols, &locates);
    let (cols, _stats) = py
        .detach(|| run_replay(&path, wl, array_window, cadence, features))
        .map_err(PyIOError::new_err)?;
    let d = PyDict::new(py);
    d.set_item("ts", PyArray1::from_vec(py, cols.ts))?;
    d.set_item("locate", PyArray1::from_vec(py, cols.locate))?;
    d.set_item("bid_px", PyArray1::from_vec(py, cols.bid_px))?;
    d.set_item("bid_qty", PyArray1::from_vec(py, cols.bid_qty))?;
    d.set_item("ask_px", PyArray1::from_vec(py, cols.ask_px))?;
    d.set_item("ask_qty", PyArray1::from_vec(py, cols.ask_qty))?;
    if features {
        d.set_item("depth5_bid", PyArray1::from_vec(py, cols.depth5_bid))?;
        d.set_item("depth5_ask", PyArray1::from_vec(py, cols.depth5_ask))?;
        d.set_item("wmid", PyArray1::from_vec(py, cols.wmid))?;
        d.set_item("imb1", PyArray1::from_vec(py, cols.imb1))?;
        d.set_item("imb5", PyArray1::from_vec(py, cols.imb5))?;
        d.set_item("spread", PyArray1::from_vec(py, cols.spread))?;
        d.set_item("ofi", PyArray1::from_vec(py, cols.ofi))?;
    }
    Ok(d)
}

fn sym(s: &[u8; 8]) -> String {
    String::from_utf8_lossy(s).trim_end().to_string()
}

fn locate_dict<'py>(py: Python<'py>, i: u16, l: &LocateStats) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("locate", i)?;
    d.set_item("symbol", sym(&l.symbol))?;
    d.set_item("watched", l.watched)?;
    d.set_item("array", l.array)?;
    d.set_item("window", l.window)?;
    d.set_item("msgs", l.msgs)?;
    d.set_item("live", l.live)?;
    d.set_item("live_hwm", l.live_hwm)?;
    d.set_item("overflow_hits", l.overflow_hits)?;
    d.set_item("max_abs_offset", l.max_abs_offset)?;
    d.set_item("max_abs_offset_real", l.max_abs_offset_real)?;
    d.set_item(
        "h_state",
        if l.h_state == 0 {
            String::new()
        } else {
            (l.h_state as char).to_string()
        },
    )?;
    d.set_item("close_hash", PyBytes::new(py, &l.close_hash))?;
    Ok(d)
}

fn stats_dict<'py>(py: Python<'py>, st: &Stats) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("source", &st.source)?;
    d.set_item("array_window", st.array_window)?;
    d.set_item("watchlist", &st.watchlist)?;
    d.set_item("messages", st.messages)?;
    d.set_item("truncated", st.truncated)?;
    d.set_item("source_cut", st.source_cut)?;
    d.set_item("bytes_in", st.bytes_in)?;
    d.set_item("unknown_type", st.unknown_type)?;
    let by_type = PyDict::new(py);
    let by_type_hour = PyDict::new(py);
    for t in 0..=255u8 {
        if st.by_type[t as usize] > 0 {
            let key = (t as char).to_string();
            by_type.set_item(&key, st.by_type[t as usize])?;
            by_type_hour.set_item(&key, PyArray1::from_slice(py, &st.by_type_hour[t as usize]))?;
        }
    }
    d.set_item("by_type", by_type)?;
    d.set_item("by_type_hour", by_type_hour)?;
    d.set_item("first_ts", st.first_ts)?;
    d.set_item("last_ts", st.last_ts)?;
    let events = PyList::empty(py);
    for (c, ts) in &st.s_events {
        events.append(((*c as char).to_string(), *ts))?;
    }
    d.set_item("s_events", events)?;
    d.set_item("live", st.live)?;
    d.set_item("live_hwm", st.live_hwm)?;
    d.set_item("crossed_before_q", st.crossed_before_q)?;
    d.set_item("crossed_after_q", st.crossed_after_q)?;
    d.set_item("crossed_after_q_trading", st.crossed_after_q_trading)?;
    d.set_item("unknown_id", st.unknown_id)?;
    d.set_item("over_execute", st.over_execute)?;
    d.set_item("over_cancel", st.over_cancel)?;
    d.set_item("negative_qty", st.negative_qty())?;
    d.set_item("duplicate_id", st.duplicate_id)?;
    d.set_item("bad_qty", st.bad_qty)?;
    d.set_item("bad_price", st.bad_price)?;
    d.set_item("bad_side", st.bad_side)?;
    d.set_item("no_directory", st.no_directory)?;
    d.set_item("ec_total", st.ec_total)?;
    d.set_item("ec_at_head", st.ec_at_head)?;
    d.set_item("event_count", st.event_count)?;
    d.set_item("event_log_hash", PyBytes::new(py, &st.event_log_hash))?;
    d.set_item("all_locates_hash", PyBytes::new(py, &st.all_locates_hash))?;
    let locates = PyList::empty(py);
    for (i, l) in st.locates_with_book() {
        locates.append(locate_dict(py, i, l)?)?;
    }
    d.set_item("locates", locates)?;
    if let Some(t) = st.timing {
        d.set_item("wall_ns", t.wall_ns)?;
        d.set_item("io_ns", t.io_ns)?;
        d.set_item("msgs_per_s_excl", t.msgs_per_s_excl(st.messages))?;
        d.set_item("msgs_per_s_incl", t.msgs_per_s_incl(st.messages))?;
    }
    d.set_item("render", st.render())?;
    Ok(d)
}

/// The replay-statistics contract as a dict (`lobcore replay --stats` in Python): every
/// counter, the close hashes as `bytes`, per-locate rows under `"locates"` and the rendered
/// markdown block under `"render"`.
#[pyfunction]
#[pyo3(signature = (path, symbols = None, locates = None, array_window = 1024))]
pub fn replay_stats<'py>(
    py: Python<'py>,
    path: PathBuf,
    symbols: Option<Vec<String>>,
    locates: Option<Vec<u16>>,
    array_window: u32,
) -> PyResult<Bound<'py, PyDict>> {
    check_window(array_window)?;
    let wl = watchlist(&symbols, &locates);
    let stats = py
        .detach(|| -> Result<Stats, String> {
            let mut session = Session::new(SessionConfig {
                watchlist: wl,
                array_window,
                source: path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                ..Default::default()
            });
            let mut src =
                open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
            session.replay(&mut src).map_err(|e| e.to_string())?;
            Ok(session.into_stats())
        })
        .map_err(PyIOError::new_err)?;
    stats_dict(py, &stats)
}

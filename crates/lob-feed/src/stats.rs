//! The replay-statistics contract (`lobcore replay --stats`): counts by type and by hour,
//! messages per second excluding and including gunzip / file read (both labelled), live-order
//! high-water marks (global and per locate), overflow-map hits and max |tick offset| per
//! watchlist locate, crossed-snapshot counts before and after the `Q` system event, unknown-id
//! and negative-quantity attempt counts, the E/C-at-level-head fraction, the close book-state
//! hash per watchlist locate (plus one hash over every locate) and the event-log hash.
//!
//! [`Stats::render`] writes the markdown block between `<!-- lobcore:begin:stats -->` and
//! `<!-- lobcore:end:stats -->`; [`deterministic_lines`] extracts the lines a README check
//! compares (every count and hash; never a msgs/s or wall-clock row).

use std::fmt::Write as _;

use lob_core::hex;

use crate::itch::msg::ParseError;

/// Opening marker of the README block.
pub const BEGIN_MARK: &str = "<!-- lobcore:begin:stats -->";
/// Closing marker of the README block.
pub const END_MARK: &str = "<!-- lobcore:end:stats -->";

const NS_PER_HOUR: u64 = 3_600_000_000_000;

/// Type-table order used for every histogram row.
pub const TYPE_ORDER: [u8; 23] = *b"SRHYLVWKJhAFECXDUPQBINO";

/// Why a replay stopped early.
#[derive(Debug)]
pub enum ReplayError {
    /// The source failed (file read or gunzip).
    Io(std::io::Error),
    /// A layout error at 0-based framed-message index `at`.
    Parse { at: u64, err: ParseError },
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplayError::Io(e) => write!(f, "read error: {e}"),
            ReplayError::Parse { at, err } => write!(f, "message #{at}: {err}"),
        }
    }
}

impl std::error::Error for ReplayError {}

impl From<std::io::Error> for ReplayError {
    fn from(e: std::io::Error) -> Self {
        ReplayError::Io(e)
    }
}

/// Wall-clock figures of one replay (never compared by the README check).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timing {
    /// Wall time of the replay loop including source reads.
    pub wall_ns: u64,
    /// Time inside the source's `read` calls (gunzip and/or file I/O); 0 for a memory slice.
    pub io_ns: u64,
}

impl Timing {
    /// Messages per second with the source reads excluded from the denominator.
    pub fn msgs_per_s_excl(&self, n: u64) -> f64 {
        let ns = self.wall_ns.saturating_sub(self.io_ns).max(1);
        n as f64 * 1e9 / ns as f64
    }

    /// Messages per second over the full wall time.
    pub fn msgs_per_s_incl(&self, n: u64) -> f64 {
        n as f64 * 1e9 / self.wall_ns.max(1) as f64
    }
}

/// Per-locate counters and close state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocateStats {
    /// Symbol from the `R` message, space padded; all spaces if no `R` was seen.
    pub symbol: [u8; 8],
    /// Whether the locate is on the watchlist.
    pub watched: bool,
    /// Whether the locate ended in an `ArrayBook` (a watched locate with at least one
    /// non-placeholder add).
    pub array: bool,
    /// `(base_px, n_levels, tick)` of the array window, once created.
    pub window: Option<(i32, u32, i32)>,
    /// Messages carrying this locate.
    pub msgs: u64,
    /// Live orders now.
    pub live: u32,
    /// Live-order high-water mark.
    pub live_hwm: u32,
    /// Adds / replaces that went to the overflow map (array books only).
    pub overflow_hits: u64,
    /// Largest |(px - base) / tick| seen by an add (array books only, placeholders included).
    pub max_abs_offset: i64,
    /// Same, over non-placeholder adds and replaces only (the number a re-centring policy
    /// would look at; placeholders at $0.01 / $199,999.99 always overflow).
    pub max_abs_offset_real: i64,
    /// Last `H` trading state (0 if none).
    pub h_state: u8,
    /// Book-state hash at close (filled by `finish`).
    pub close_hash: [u8; 32],
    /// Whether a book exists for this locate.
    pub has_book: bool,
}

impl Default for LocateStats {
    fn default() -> Self {
        LocateStats {
            symbol: [b' '; 8],
            watched: false,
            array: false,
            window: None,
            msgs: 0,
            live: 0,
            live_hwm: 0,
            overflow_hits: 0,
            max_abs_offset: 0,
            max_abs_offset_real: 0,
            h_state: 0,
            close_hash: [0; 32],
            has_book: false,
        }
    }
}

/// Every counter of the contract. All fields are public so a caller can pin them in tests.
#[derive(Debug, Clone)]
pub struct Stats {
    /// Input name (basename only, so the README block is machine independent).
    pub source: String,
    /// Array window (levels per side) used for watchlist locates.
    pub array_window: u32,
    /// Human description of the watchlist.
    pub watchlist: String,
    /// Complete framed messages.
    pub messages: u64,
    /// Incomplete trailing messages discarded.
    pub truncated: u32,
    /// The source ended mid-stream (a cut gzip member); see `FrameSource::source_cut`.
    pub source_cut: bool,
    /// Bytes after the last gzip member that were not another member (ignored, counted); see
    /// `FrameSource::trailing_bytes`.
    pub trailing_bytes: u64,
    /// Bytes consumed from the (inflated) source.
    pub bytes_in: u64,
    /// Messages whose type byte is outside the size table.
    pub unknown_type: u64,
    /// Count per type byte.
    pub by_type: [u64; 256],
    /// Count per type byte and hour of day (ns since midnight / 3.6e12, clamped to 23).
    pub by_type_hour: Box<[[u64; 24]; 256]>,
    /// Timestamp of the first and last message carrying a header.
    pub first_ts: Option<u64>,
    /// See `first_ts`.
    pub last_ts: Option<u64>,
    /// System events `(code, ts)` in order.
    pub s_events: Vec<(u8, u64)>,
    /// Live orders across all locates now.
    pub live: u64,
    /// Global live-order high-water mark.
    pub live_hwm: u64,
    /// Book-changing messages after which the locate's book was crossed (bid >= ask), before `Q`.
    pub crossed_before_q: u64,
    /// Same, after `Q`.
    pub crossed_after_q: u64,
    /// Same, after `Q` and only for locates whose last `H` state is `T` (the hard check).
    pub crossed_after_q_trading: u64,
    /// Cancel / delete / execute / replace naming an id that is not live.
    pub unknown_id: u64,
    /// `E` / `C` asking for more than the order's remaining quantity (rejected, no change).
    pub over_execute: u64,
    /// `X` asking for more than the remaining quantity (the book removes the order; counted).
    pub over_cancel: u64,
    /// `A` / `F` / `U` naming an id that is already live.
    pub duplicate_id: u64,
    /// Zero quantity or a level total beyond `u32::MAX`.
    pub bad_qty: u64,
    /// Zero price.
    pub bad_price: u64,
    /// Add with a side byte other than `B` / `S` (skipped).
    pub bad_side: u64,
    /// Order messages for a locate with no prior `R` (a reference book is created on demand).
    pub no_directory: u64,
    /// `A` / `F` messages whose price is a placeholder (`is_placeholder`: at or below $0.01,
    /// at or above $199,900); they rest via the overflow map and never centre a window.
    pub placeholder_adds: u64,
    /// `E` / `C` on a live id.
    pub ec_total: u64,
    /// Of those, the order was at the head of its level.
    pub ec_at_head: u64,
    /// Per locate, indexed by locate number.
    pub locates: Vec<LocateStats>,
    /// Whether events were logged (false = the event-log row is not meaningful).
    pub event_logged: bool,
    /// Records in the event log.
    pub event_count: u64,
    /// sha256 of the event log (version byte then 34-byte records).
    pub event_log_hash: [u8; 32],
    /// sha256 over `(locate u16 LE || close book-state hash)` for every locate with a book,
    /// ascending.
    pub all_locates_hash: [u8; 32],
    /// Wall-clock figures, if measured.
    pub timing: Option<Timing>,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            source: String::new(),
            array_window: 0,
            watchlist: String::new(),
            messages: 0,
            truncated: 0,
            source_cut: false,
            trailing_bytes: 0,
            bytes_in: 0,
            unknown_type: 0,
            by_type: [0; 256],
            by_type_hour: Box::new([[0; 24]; 256]),
            first_ts: None,
            last_ts: None,
            s_events: Vec::with_capacity(16),
            live: 0,
            live_hwm: 0,
            crossed_before_q: 0,
            crossed_after_q: 0,
            crossed_after_q_trading: 0,
            unknown_id: 0,
            over_execute: 0,
            over_cancel: 0,
            duplicate_id: 0,
            bad_qty: 0,
            bad_price: 0,
            bad_side: 0,
            no_directory: 0,
            placeholder_adds: 0,
            ec_total: 0,
            ec_at_head: 0,
            locates: Vec::new(),
            event_logged: true,
            event_count: 0,
            event_log_hash: [0; 32],
            all_locates_hash: [0; 32],
            timing: None,
        }
    }
}

/// `12345678` -> `12,345,678`.
pub fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// ns since midnight as `HH:MM:SS.nnnnnnnnn`.
pub fn hms(ts: u64) -> String {
    let s = ts / 1_000_000_000;
    format!(
        "{:02}:{:02}:{:02}.{:09}",
        s / 3600,
        (s / 60) % 60,
        s % 60,
        ts % 1_000_000_000
    )
}

fn sym(s: &[u8; 8]) -> String {
    String::from_utf8_lossy(s).trim_end().to_string()
}

impl Stats {
    /// Hour bucket of a timestamp.
    #[inline]
    pub fn hour(ts: u64) -> usize {
        ((ts / NS_PER_HOUR) as usize).min(23)
    }

    /// Negative-quantity attempts: over-executes plus over-cancels.
    pub fn negative_qty(&self) -> u64 {
        self.over_execute + self.over_cancel
    }

    /// Locates in `R` order that have a book.
    pub fn locates_with_book(&self) -> impl Iterator<Item = (u16, &LocateStats)> {
        self.locates
            .iter()
            .enumerate()
            .filter(|(_, l)| l.has_book)
            .map(|(i, l)| (i as u16, l))
    }

    /// Locates whose close hash is listed individually: the watchlist, or every locate with a
    /// book when the watchlist is empty and there are at most 16 of them.
    pub fn listed_locates(&self) -> Vec<u16> {
        let watched: Vec<u16> = self
            .locates_with_book()
            .filter(|(_, l)| l.watched)
            .map(|(i, _)| i)
            .collect();
        if !watched.is_empty() {
            return watched;
        }
        let all: Vec<u16> = self.locates_with_book().map(|(i, _)| i).collect();
        if all.len() <= 16 { all } else { Vec::new() }
    }

    /// Hours (0..24) with at least one message.
    fn hours_present(&self) -> Vec<usize> {
        (0..24)
            .filter(|&h| self.by_type_hour.iter().any(|row| row[h] > 0))
            .collect()
    }

    /// Types with at least one message: the table order first, then any unknown bytes.
    fn types_present(&self) -> Vec<u8> {
        let mut v: Vec<u8> = TYPE_ORDER
            .iter()
            .copied()
            .filter(|&t| self.by_type[t as usize] > 0)
            .collect();
        for t in 0..=255u8 {
            if self.by_type[t as usize] > 0 && !TYPE_ORDER.contains(&t) {
                v.push(t);
            }
        }
        v
    }

    /// Compact `A 40086 · D 37219 · ...` line in table order.
    pub fn type_histogram_line(&self) -> String {
        self.types_present()
            .iter()
            .map(|&t| format!("{} {}", type_label(t), commas(self.by_type[t as usize])))
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// The markdown block, markers included, with the two msgs/s rows (for stdout).
    pub fn render(&self) -> String {
        self.render_with(true)
    }

    /// The markdown block WITHOUT the msgs/s rows: what `--write-readme` splices into a README, so a
    /// second run is byte-identical (timings belong in the throughput section, outside any diffed block).
    pub fn render_block(&self) -> String {
        self.render_with(false)
    }

    fn render_with(&self, timing: bool) -> String {
        let mut o = String::with_capacity(4096);
        o.push_str(BEGIN_MARK);
        o.push('\n');
        let _ = writeln!(o, "| replay | value |");
        let _ = writeln!(o, "|---|---|");
        let _ = writeln!(o, "| source | `{}` |", self.source);
        let _ = writeln!(
            o,
            "| messages (complete frames) | {} |",
            commas(self.messages)
        );
        let _ = writeln!(
            o,
            "| truncated final message | {}{} |",
            self.truncated,
            if self.source_cut {
                " (source cut mid-stream: range-downloaded gzip prefix)"
            } else {
                ""
            }
        );
        let _ = writeln!(
            o,
            "| inflated bytes consumed | {}{} |",
            commas(self.bytes_in),
            if self.trailing_bytes > 0 {
                format!(
                    " ({} bytes after the last gzip member ignored)",
                    commas(self.trailing_bytes)
                )
            } else {
                String::new()
            }
        );
        let _ = writeln!(
            o,
            "| unknown message types (skipped by length) | {} |",
            commas(self.unknown_type)
        );
        let _ = writeln!(o, "| by type | {} |", self.type_histogram_line());
        match (self.first_ts, self.last_ts) {
            (Some(a), Some(b)) => {
                let _ = writeln!(
                    o,
                    "| time span (ns since midnight) | {} – {} |",
                    hms(a),
                    hms(b)
                );
            }
            _ => {
                let _ = writeln!(o, "| time span | none |");
            }
        }
        let ev = self
            .s_events
            .iter()
            .map(|(c, ts)| format!("{}@{}", *c as char, hms(*ts)))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = writeln!(
            o,
            "| system events | {} |",
            if ev.is_empty() { "none".into() } else { ev }
        );
        let n_loc = self.locates_with_book().count();
        let n_arr = self.locates_with_book().filter(|(_, l)| l.array).count();
        let _ = writeln!(
            o,
            "| locates with a book | {} ({} array, {} reference); watchlist: {}; array window {} levels/side |",
            commas(n_loc as u64),
            n_arr,
            n_loc - n_arr,
            self.watchlist,
            self.array_window
        );
        let _ = writeln!(
            o,
            "| order messages without a prior R | {} |",
            commas(self.no_directory)
        );
        let adds = self.by_type[b'A' as usize] + self.by_type[b'F' as usize];
        let _ = writeln!(
            o,
            "| adds at placeholder prices (<= $0.01 or >= $199,900) | {} of {} = {:.2} % |",
            commas(self.placeholder_adds),
            commas(adds),
            if adds > 0 {
                100.0 * self.placeholder_adds as f64 / adds as f64
            } else {
                0.0
            }
        );
        let _ = writeln!(
            o,
            "| live orders: high-water mark / at close | {} / {} |",
            commas(self.live_hwm),
            commas(self.live)
        );
        let _ = writeln!(
            o,
            "| crossed snapshots before Q | {} |",
            commas(self.crossed_before_q)
        );
        let _ = writeln!(
            o,
            "| crossed snapshots after Q (all / trading-state T) | {} / {} |",
            commas(self.crossed_after_q),
            commas(self.crossed_after_q_trading)
        );
        let _ = writeln!(o, "| unknown-id | {} |", commas(self.unknown_id));
        let _ = writeln!(
            o,
            "| negative-qty attempts (over-execute + over-cancel) | {} ({} + {}) |",
            commas(self.negative_qty()),
            commas(self.over_execute),
            commas(self.over_cancel)
        );
        let _ = writeln!(
            o,
            "| duplicate id / bad qty / bad price / bad side | {} / {} / {} / {} |",
            self.duplicate_id, self.bad_qty, self.bad_price, self.bad_side
        );
        let frac = if self.ec_total == 0 {
            "n/a".to_string()
        } else {
            format!("{:.4}", self.ec_at_head as f64 / self.ec_total as f64)
        };
        let _ = writeln!(
            o,
            "| E/C at level head | {} / {} = {} |",
            commas(self.ec_at_head),
            commas(self.ec_total),
            frac
        );
        if self.event_logged {
            let _ = writeln!(
                o,
                "| event log | {} records, sha256 `{}` |",
                commas(self.event_count),
                hex(&self.event_log_hash)
            );
        } else {
            let _ = writeln!(o, "| event log | not recorded (log_events = false) |");
        }
        let _ = writeln!(
            o,
            "| close book-state hash, all locates | `{}` |",
            hex(&self.all_locates_hash)
        );
        if let Some(t) = self.timing.filter(|_| timing) {
            let _ = writeln!(
                o,
                "| msgs/s parse+apply{}, excluding gunzip/read | {:.2} M (parse+apply {:.1} ms, read {:.1} ms) |",
                if self.event_logged {
                    " + event-log sha256"
                } else {
                    ""
                },
                t.msgs_per_s_excl(self.messages) / 1e6,
                (t.wall_ns - t.io_ns.min(t.wall_ns)) as f64 / 1e6,
                t.io_ns as f64 / 1e6
            );
            let _ = writeln!(
                o,
                "| msgs/s parse+apply{}, including gunzip/read | {:.2} M (wall {:.1} ms) |",
                if self.event_logged {
                    " + event-log sha256"
                } else {
                    ""
                },
                t.msgs_per_s_incl(self.messages) / 1e6,
                t.wall_ns as f64 / 1e6
            );
        }
        o.push('\n');
        self.render_type_hour(&mut o);
        o.push('\n');
        self.render_locates(&mut o);
        o.push_str(END_MARK);
        o.push('\n');
        o
    }

    fn render_type_hour(&self, o: &mut String) {
        let hours = self.hours_present();
        let types = self.types_present();
        let _ = write!(o, "| type \\ hour |");
        for h in &hours {
            let _ = write!(o, " {h:02} |");
        }
        let _ = writeln!(o, " total |");
        let _ = write!(o, "|---|");
        for _ in &hours {
            let _ = write!(o, "---:|");
        }
        let _ = writeln!(o, "---:|");
        let mut col_tot = vec![0u64; hours.len()];
        for &t in &types {
            let _ = write!(o, "| {} |", type_label(t));
            for (i, &h) in hours.iter().enumerate() {
                let c = self.by_type_hour[t as usize][h];
                col_tot[i] += c;
                let _ = write!(o, " {} |", commas(c));
            }
            let _ = writeln!(o, " {} |", commas(self.by_type[t as usize]));
        }
        let _ = write!(o, "| total |");
        for c in &col_tot {
            let _ = write!(o, " {} |", commas(*c));
        }
        let _ = writeln!(o, " {} |", commas(col_tot.iter().sum()));
    }

    fn render_locates(&self, o: &mut String) {
        let listed = self.listed_locates();
        if listed.is_empty() {
            let _ = writeln!(
                o,
                "Per-locate rows: none (no watchlist and more than 16 locates); the all-locates hash above covers every book."
            );
            return;
        }
        let _ = writeln!(
            o,
            "| locate | symbol | book | msgs | live HWM / close | H | overflow hits | max abs offset (all / ex-placeholder) | window (base, levels, tick) | close book-state hash |"
        );
        let _ = writeln!(o, "|---:|---|---|---:|---:|---|---:|---:|---|---|");
        for l in listed {
            let s = &self.locates[l as usize];
            let (ov, mo, win) = if s.array {
                let (b, n, t) = s.window.unwrap_or((0, 0, 0));
                (
                    commas(s.overflow_hits),
                    format!("{} / {}", s.max_abs_offset, s.max_abs_offset_real),
                    format!("{b}, {n}, {t}"),
                )
            } else {
                ("–".into(), "–".into(), "–".into())
            };
            let _ = writeln!(
                o,
                "| {} | {} | {} | {} | {} / {} | {} | {} | {} | {} | `{}` |",
                l,
                sym(&s.symbol),
                if s.array { "array" } else { "reference" },
                commas(s.msgs),
                commas(s.live_hwm as u64),
                commas(s.live as u64),
                if s.h_state == 0 {
                    '-'
                } else {
                    s.h_state as char
                },
                ov,
                mo,
                win,
                hex(&s.close_hash)
            );
        }
    }
}

/// Printable label for a type byte.
pub fn type_label(t: u8) -> String {
    if t.is_ascii_graphic() {
        (t as char).to_string()
    } else {
        format!("0x{t:02x}")
    }
}

/// True for rows the README check must ignore (timing).
pub fn is_volatile(line: &str) -> bool {
    line.starts_with("| msgs/s")
}

/// The non-empty lines between the markers (exclusive) with volatile rows removed; `None` if
/// either marker is missing or they are out of order.
pub fn deterministic_lines(text: &str) -> Option<Vec<String>> {
    let b = text.find(BEGIN_MARK)?;
    let e = text[b..].find(END_MARK)? + b;
    let inner = &text[b + BEGIN_MARK.len()..e];
    Some(
        inner
            .lines()
            .map(|l| l.trim_end())
            .filter(|l| !l.is_empty() && !is_volatile(l))
            .map(str::to_string)
            .collect(),
    )
}

/// Replace the block in `text` with `block` (which must carry both markers); appends a new
/// block when none exists.
pub fn splice_block(text: &str, block: &str) -> String {
    let block = block.trim_end_matches('\n');
    if let Some(b) = text.find(BEGIN_MARK)
        && let Some(e) = text[b..].find(END_MARK)
    {
        let e = b + e + END_MARK.len();
        return format!("{}{}{}", &text[..b], block, &text[e..]);
    }
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(block);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commas_and_hms() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1000), "1,000");
        assert_eq!(commas(4_330_679), "4,330,679");
        assert_eq!(hms(34_200_000_000_123), "09:30:00.000000123");
        assert_eq!(Stats::hour(34_200_000_000_123), 9);
        assert_eq!(Stats::hour(u64::MAX), 23);
    }

    #[test]
    fn render_and_filter_round_trip() {
        let mut s = Stats {
            source: "x.itch".into(),
            watchlist: "none".into(),
            messages: 3_000,
            ..Default::default()
        };
        s.by_type[b'A' as usize] = 2;
        s.by_type[b'S' as usize] = 1;
        s.by_type_hour[b'A' as usize][9] = 2;
        s.by_type_hour[b'S' as usize][3] = 1;
        s.timing = Some(Timing {
            wall_ns: 1_000_000,
            io_ns: 250_000,
        });
        let r = s.render();
        assert!(r.starts_with(BEGIN_MARK));
        assert!(r.trim_end().ends_with(END_MARK));
        assert!(r.contains("| by type | S 1 · A 2 |"));
        assert!(r.contains("| type \\ hour | 03 | 09 | total |"));
        // the two rows name their denominators: parse+apply = wall - read, and the gross wall
        assert!(r.contains(
            "| msgs/s parse+apply + event-log sha256, excluding gunzip/read | 4.00 M (parse+apply 0.8 ms, read 0.2 ms) |"
        ));
        assert!(r.contains(
            "| msgs/s parse+apply + event-log sha256, including gunzip/read | 3.00 M (wall 1.0 ms) |"
        ));
        let det = deterministic_lines(&r).unwrap();
        assert!(det.iter().all(|l| !l.contains("msgs/s")));
        s.timing = None;
        assert_eq!(deterministic_lines(&s.render()).unwrap(), det);
        let readme = "# t\n\nintro\n";
        let spliced = splice_block(readme, &r);
        assert_eq!(deterministic_lines(&spliced).unwrap(), det);
        let again = splice_block(&spliced, &r);
        assert_eq!(again, spliced);
        assert!(deterministic_lines("no markers").is_none());
    }
}

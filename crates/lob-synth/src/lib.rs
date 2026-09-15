//! Seeded synthetic Nasdaq TotalView-ITCH 5.0 day with a per-message truth sidecar.
//!
//! `synth_itch(seed, n_msgs, &cfg)` writes exactly `n_msgs` framed messages (emi.nasdaq.com
//! BinaryFILE framing: 2-byte big-endian payload length before each message) and one [`Truth`] row
//! per message, recorded after that message was applied to the generator's own book for that
//! message's locate (system events carry locate 0 and an empty book).
//!
//! Stream shape: `S 'O'`, `S 'S'`, one `R` per locate (symbols `SYN0001..`), one `H` per locate
//! (state `T`), then the message mix drawn from `SynthConfig::mix`; at the first message timestamp at
//! or after `open_ns` any crossed resting orders are executed from the level heads (`E`) and the
//! `S 'Q'` event is written (both at exactly `open_ns`); the stream ends with `S 'M'`, `S 'E'`,
//! `S 'C'` at `close_ns`. A stream too short for its clock to reach `open_ns` still carries `S 'Q'`
//! (after the mix, without an uncross), so the six system events are always present in order.
//!
//! Guarantees (each is a test in `tests/`):
//! - same `(seed, cfg)` -> identical bytes on every platform (ChaCha8, integer draws only);
//! - order reference numbers strictly increasing (A/F and the new ref of U), timestamps monotone
//!   non-decreasing 48-bit ns since midnight, every framed length equals the spec size table;
//! - `X`/`D`/`E`/`C`/`U` target live refs only when `unknown_ref_rate == 0`; with a positive rate
//!   they may target a previously deleted ref of the same locate (the truth is then unchanged);
//! - `X` is a strict partial cancel (1..qty-1, a one-share order is deleted instead); `E`/`C` consume
//!   1..qty from the head order of the best level with strictly increasing match numbers; `U`
//!   removes the old ref and appends the new ref at the tail of its level;
//! - placeholder adds at $0.01 (bid) / $199,999.99 (ask) at `placeholder_rate`; prices off the 1c
//!   grid only for locates whose base is under $1 when `subpenny`; crossed quotes only before
//!   `open_ns` and only when `crossed_preopen`.
//!
//! Prices are ITCH u32 with four implied decimals; the truth keeps them as `i32`, positive on both
//! sides. `n_msgs` counts every framed message including the header, `S 'Q'` and the three closing
//! events, so `n_msgs >= 2 * locates + 6`.
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod itch;
pub mod truth;

mod generator;

use std::io::Write;
use std::path::Path;

use sha2::{Digest, Sha256};

pub use truth::{Side, SideBook, TruthBook, hash_book};

/// Generator parameters. `mix` weights are over `A F D X U E C P L` in that order and are normalised.
#[derive(Clone, Debug, PartialEq)]
pub struct SynthConfig {
    /// Number of symbols; locates are `1..=locates`, symbols `SYN0001..`.
    pub locates: u16,
    /// Message-type weights over A/F/D/X/U/E/C/P/L (need not sum to one).
    pub mix: [f32; 9],
    /// Share of adds priced at $0.01 (bid) or $199,999.99 (ask). Default 0.01.
    pub placeholder_rate: f32,
    /// Share of X/D/E/C/U messages that target an already-deleted ref. Default 0.
    pub unknown_ref_rate: f32,
    /// Quote locates with a base under $1 on the 0.0001 grid instead of the 0.01 grid.
    pub subpenny: bool,
    /// Allow resting orders to cross the opposite best before `open_ns` (uncrossed by `E` at open).
    pub crossed_preopen: bool,
    /// Start of market hours in ns since midnight (`S 'Q'`). Default 09:30.
    pub open_ns: u64,
    /// End of market hours in ns since midnight (`S 'M'`). Default 16:00.
    pub close_ns: u64,
}

/// Default mix: the pre-open type histogram measured on the real 2019-12-30 sample head
/// (A 39.9 / D 37.3 / X 11.0 / L 5.0 / U 4.8 / F 0.9 / E 0.4 / P 0.1 %; no C before 09:30).
pub const DEFAULT_MIX: [f32; 9] = [0.399, 0.009, 0.373, 0.110, 0.048, 0.004, 0.0, 0.001, 0.050];

impl Default for SynthConfig {
    fn default() -> Self {
        Self {
            locates: 8,
            mix: DEFAULT_MIX,
            placeholder_rate: 0.01,
            unknown_ref_rate: 0.0,
            subpenny: true,
            crossed_preopen: true,
            open_ns: 34_200_000_000_000,
            close_ns: 57_600_000_000_000,
        }
    }
}

/// Book state of one locate after one message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Truth {
    /// Message timestamp, ns since midnight.
    pub ts: u64,
    /// Stock locate of the message (0 for system events).
    pub locate: u16,
    /// Best bid `(price, level qty)`.
    pub best_bid: Option<(i32, u32)>,
    /// Best ask `(price, level qty)`.
    pub best_ask: Option<(i32, u32)>,
    /// Level quantities of the five best bid levels, best first, zero-padded.
    pub l5_bid: [u32; 5],
    /// Level quantities of the five best ask levels, best first, zero-padded.
    pub l5_ask: [u32; 5],
    /// Live orders on this locate.
    pub live_orders: u32,
    /// Book-state hash (see [`truth::hash_book`]).
    pub book_hash: [u8; 32],
}

/// A generated day: the framed ITCH bytes and one truth row per message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SynthDay {
    /// Framed ITCH 5.0 bytes.
    pub bytes: Vec<u8>,
    /// One row per framed message, in stream order.
    pub truth: Vec<Truth>,
}

/// Generate `n_msgs` framed messages and the truth sidecar. Panics on an invalid config or when
/// `n_msgs < 2 * locates + 6`.
pub fn synth_itch(seed: u64, n_msgs: u64, cfg: &SynthConfig) -> SynthDay {
    generator::Generator::new(seed, cfg, true, n_msgs).run(n_msgs)
}

/// Same bytes as [`synth_itch`] without recording (or hashing) the truth; use for large bench days.
pub fn synth_itch_bytes(seed: u64, n_msgs: u64, cfg: &SynthConfig) -> Vec<u8> {
    generator::Generator::new(seed, cfg, false, n_msgs)
        .run(n_msgs)
        .bytes
}

/// The truth sidecar as CSV: `ts,locate,bid_px,bid_qty,ask_px,ask_qty,bid1..bid5,ask1..ask5,
/// live_orders,book_hash` with empty fields for an absent side and the hash in lowercase hex.
pub fn truth_csv(day: &SynthDay) -> Vec<u8> {
    let mut out = Vec::with_capacity(day.truth.len() * 120);
    out.extend_from_slice(
        b"ts,locate,bid_px,bid_qty,ask_px,ask_qty,bid1,bid2,bid3,bid4,bid5,ask1,ask2,ask3,ask4,ask5,live_orders,book_hash\n",
    );
    for t in &day.truth {
        let _ = write!(out, "{},{}", t.ts, t.locate);
        for side in [t.best_bid, t.best_ask] {
            match side {
                Some((px, q)) => {
                    let _ = write!(out, ",{px},{q}");
                }
                None => out.extend_from_slice(b",,"),
            }
        }
        for q in t.l5_bid.iter().chain(t.l5_ask.iter()) {
            let _ = write!(out, ",{q}");
        }
        let _ = writeln!(out, ",{},{}", t.live_orders, hex(&t.book_hash));
    }
    out
}

/// Write [`truth_csv`] to `path`.
pub fn write_truth_csv(day: &SynthDay, path: impl AsRef<Path>) -> std::io::Result<()> {
    std::fs::write(path, truth_csv(day))
}

/// sha256 of a byte slice.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

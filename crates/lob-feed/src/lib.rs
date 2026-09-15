//! lob-feed: the market-data side of lobcore.
//!
//! - [`itch::frame`]: 2-byte big-endian length framing over any `Read` (gzip via flate2/zlib-rs
//!   when the path ends in `.gz`) with a 4 MB buffer, and over an in-memory `&[u8]`; a truncated
//!   final message is discarded and counted.
//! - [`itch::msg`]: borrowed, allocation-free views over Nasdaq TotalView-ITCH 5.0 payloads for
//!   S R H A F E C X D U P Q B with the spec offsets, the size table for all 23 types, u48
//!   timestamps; a length mismatch is a fatal [`itch::msg::ParseError::Length`], an unknown type
//!   is skipped by its framed length and counted.
//! - [`itch::apply`]: [`itch::apply::Session`] — one book per stock locate (`ArrayBook` for the
//!   watchlist, `RefBook` for everyone else), ITCH semantics from the spec (U = delete + tail add
//!   with the side carried from the original, X in place, E and C identical for state, P/Q/B
//!   no-ops), and every counter the replay-statistics contract needs.
//! - [`stats`]: the contract's markdown block between `<!-- lobcore:begin:stats -->` and
//!   `<!-- lobcore:end:stats -->`, plus the deterministic-line filter the README check uses.
//! - [`mbo::record`]: the Databento DBN header and the 56-byte little-endian `MboMsg` record
//!   decoder (format test only; no book-apply rules in v0.1).
//!
//! Every integer is read with `from_be_bytes` / `from_le_bytes` on fixed-size slices; the crate
//! needs no `unsafe`.

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod itch;
pub mod mbo;
pub mod stats;

pub use itch::apply::{Session, SessionConfig, Watchlist};
pub use itch::frame::{FrameSource, Framer, SliceFrames, open, read_all};
pub use itch::msg::{Msg, ParseError, SPEC_LEN, spec_len};
pub use mbo::record::{DbnError, DbnHeader, DbnMetadata, MboMsg, Record, records};
pub use stats::{ReplayError, Stats, Timing};

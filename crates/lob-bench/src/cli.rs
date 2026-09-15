//! Command-line surface (clap derive). Kept in the library so tests can parse without spawning.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// lobcore: bounded-array order book, ITCH replay and benchmarks.
#[derive(Debug, Parser)]
#[command(name = "lobcore", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Replay an ITCH file (plain or .gz) and print the statistics block.
    Replay(ReplayArgs),
    /// Write a seeded synthetic ITCH day (and optionally its truth sidecar).
    Synth(SynthArgs),
    /// Quick throughput bench: parse-only, parse+apply array, parse+apply reference; 5 fresh
    /// processes, median with min-max, machine preflight, load gate.
    BenchQuick(BenchQuickArgs),
    /// One timed pass in this process (spawned by bench-quick).
    #[command(hide = true)]
    BenchOnce(BenchOnceArgs),
}

#[derive(Debug, Args, Clone)]
pub struct ReplayArgs {
    /// Print the full statistics block (between the README markers).
    #[arg(long)]
    pub stats: bool,
    /// ITCH file; gzip when the name ends in .gz.
    pub file: PathBuf,
    /// Symbol whose locate gets the array book (repeatable).
    #[arg(long = "symbol", value_name = "SYM")]
    pub symbols: Vec<String>,
    /// Locate number whose book is the array book (repeatable).
    #[arg(long = "locate", value_name = "N")]
    pub locates: Vec<u16>,
    /// Every locate on the array book (small synthetic universes only).
    #[arg(long)]
    pub all: bool,
    /// Array window: levels per side (1..=4096).
    #[arg(long, default_value_t = 2048, value_name = "N")]
    pub array_window: u32,
    /// Compare the deterministic lines of the block against the README copy; exit 1 on drift.
    #[arg(long, value_name = "README")]
    pub check_readme: Option<PathBuf>,
    /// Replace the block in the README in place (append when absent).
    #[arg(long, value_name = "README")]
    pub write_readme: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
pub struct SynthArgs {
    /// RNG seed.
    #[arg(long)]
    pub seed: u64,
    /// Total framed messages (>= 2 * locates + 6).
    #[arg(long)]
    pub n: u64,
    /// Output file.
    #[arg(long)]
    pub out: PathBuf,
    /// Also write the truth sidecar as CSV.
    #[arg(long, value_name = "CSV")]
    pub truth: Option<PathBuf>,
    /// Number of locates.
    #[arg(long, default_value_t = 8)]
    pub locates: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Frame and classify every message; no book.
    Parse,
    /// Parse and apply with the watchlist on the array book (no event log).
    Array,
    /// Parse and apply with every locate on the reference book (no event log).
    Ref,
    /// `Array` plus the incremental event-log sha256 (what `replay --stats` runs).
    ArrayLog,
}

impl Mode {
    /// Every mode in report order.
    pub const ALL: [Mode; 4] = [Mode::Parse, Mode::Array, Mode::Ref, Mode::ArrayLog];

    /// Label for tables.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Parse => "parse only",
            Mode::Array => "parse+apply, ArrayBook watchlist",
            Mode::Ref => "parse+apply, RefBook everywhere",
            Mode::ArrayLog => "parse+apply, ArrayBook watchlist + event-log sha256",
        }
    }

    /// Command-line spelling.
    pub fn flag(self) -> &'static str {
        match self {
            Mode::Parse => "parse",
            Mode::Array => "array",
            Mode::Ref => "ref",
            Mode::ArrayLog => "array-log",
        }
    }
}

#[derive(Debug, Args, Clone)]
pub struct BenchQuickArgs {
    /// ITCH file; inflated into memory once per run so timings exclude gunzip.
    pub file: PathBuf,
    /// Array window: levels per side.
    #[arg(long, default_value_t = 2048, value_name = "N")]
    pub array_window: u32,
    /// Symbols for the array-book watchlist. Default: every locate when the file has at most
    /// 64, otherwise required.
    #[arg(long = "symbol", value_name = "SYM")]
    pub symbols: Vec<String>,
    /// Fresh processes per mode.
    #[arg(long, default_value_t = 5)]
    pub runs: u32,
    /// Append the report to this file (refused when the 1-minute load average exceeds 1.0).
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
pub struct BenchOnceArgs {
    pub file: PathBuf,
    #[arg(long, value_enum)]
    pub mode: Mode,
    #[arg(long, default_value_t = 2048)]
    pub array_window: u32,
    #[arg(long = "symbol")]
    pub symbols: Vec<String>,
    #[arg(long)]
    pub all: bool,
}

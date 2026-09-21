//! lob-bench: the `lobcore` binary. `replay --stats` renders the replay-statistics block and
//! checks or rewrites the README copy; `synth` writes a seeded synthetic ITCH day; `bench-quick`
//! measures messages per second in four modes (parse only, parse+apply on the array book,
//! parse+apply on the reference book, array book + event-log sha256) over five fresh processes
//! behind a machine preflight and a load gate. criterion / hdrhistogram harnesses are v0.3.

#![deny(unsafe_code)]

pub mod bench;
pub mod cli;
pub mod preflight;
pub mod readme;
pub mod replay;

use std::fmt;

use cli::{Cli, Command};

/// Any failure of a subcommand, printed as `lobcore: <message>` with exit code 1.
#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

impl From<lob_feed::ReplayError> for Error {
    fn from(e: lob_feed::ReplayError) -> Self {
        Error(e.to_string())
    }
}

/// Run a parsed command line; `Ok(exit code)`.
pub fn run(cli: Cli) -> Result<i32, Error> {
    match cli.command {
        Command::Replay(a) => replay::run(&a),
        Command::Synth(a) => replay::synth(&a),
        Command::BenchQuick(a) => bench::run_quick(&a),
        Command::BenchOnce(a) => bench::run_once(&a),
    }
}

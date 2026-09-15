//! `lobcore`: the command-line front end (see `lob_bench::cli`).

use clap::Parser;

fn main() {
    let cli = lob_bench::cli::Cli::parse();
    match lob_bench::run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("lobcore: {e}");
            std::process::exit(1);
        }
    }
}

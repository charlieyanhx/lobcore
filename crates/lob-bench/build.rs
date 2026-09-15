//! Records the compiler and the encoded RUSTFLAGS the binary was built with, so the bench
//! preflight can print them without invoking `rustc` at run time.

use std::process::Command;

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let version = Command::new(&rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "rustc (version unknown)".into());
    let flags = std::env::var("CARGO_ENCODED_RUSTFLAGS")
        .map(|f| f.split('\x1f').collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let target = std::env::var("TARGET").unwrap_or_default();
    let profile = std::env::var("PROFILE").unwrap_or_default();
    println!("cargo:rustc-env=LOBCORE_RUSTC={version}");
    println!("cargo:rustc-env=LOBCORE_RUSTFLAGS={flags}");
    println!("cargo:rustc-env=LOBCORE_TARGET={target}");
    println!("cargo:rustc-env=LOBCORE_PROFILE={profile}");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-changed=build.rs");
}

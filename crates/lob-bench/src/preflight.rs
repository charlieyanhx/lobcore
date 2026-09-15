//! Machine preflight for the bench: model, cores, OS, compiler, RUSTFLAGS / target, cache line,
//! P-cluster L2, memory, timer frequency and the 1-minute load average. Read through `sysctl`
//! and `sw_vers` on macOS and `/proc` + `/sys` on Linux (no libc binding), so every value is
//! best effort and printed as `unknown` when unavailable.

use std::fmt::Write as _;
use std::process::Command;

/// The block's values.
#[derive(Debug, Clone, Default)]
pub struct Preflight {
    pub model: String,
    pub cpu: String,
    pub cores: String,
    pub os: String,
    pub rustc: String,
    pub rustflags: String,
    pub target: String,
    pub profile: String,
    pub cacheline: String,
    pub l2: String,
    pub memory: String,
    pub timer_hz: String,
    pub load1: Option<f64>,
}

fn cmd(program: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(program).args(args).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(target_os = "macos")]
fn sysctl(key: &str) -> Option<String> {
    cmd("sysctl", &["-n", key])
}

fn unknown(v: Option<String>) -> String {
    v.unwrap_or_else(|| "unknown".into())
}

/// 1-minute load average.
#[cfg(target_os = "macos")]
pub fn load1() -> Option<f64> {
    // "{ 1.23 1.45 1.50 }"
    let s = sysctl("vm.loadavg")?;
    s.trim_matches(|c| c == '{' || c == '}' || c == ' ')
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// 1-minute load average.
#[cfg(not(target_os = "macos"))]
pub fn load1() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/loadavg").ok()?;
    s.split_whitespace().next()?.parse().ok()
}

/// Gather everything.
pub fn gather() -> Preflight {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let mut p = Preflight {
        cores,
        rustc: env!("LOBCORE_RUSTC").to_string(),
        rustflags: {
            let f = env!("LOBCORE_RUSTFLAGS");
            if f.is_empty() {
                "(none)".into()
            } else {
                f.into()
            }
        },
        target: env!("LOBCORE_TARGET").to_string(),
        profile: env!("LOBCORE_PROFILE").to_string(),
        load1: load1(),
        ..Default::default()
    };
    #[cfg(target_os = "macos")]
    {
        p.model = unknown(sysctl("hw.model"));
        p.cpu = unknown(sysctl("machdep.cpu.brand_string"));
        if let (Some(pc), Some(ec)) = (
            sysctl("hw.perflevel0.physicalcpu"),
            sysctl("hw.perflevel1.physicalcpu"),
        ) {
            p.cores = format!("{} ({pc}P + {ec}E)", p.cores);
        }
        p.os = format!(
            "macOS {} ({})",
            unknown(cmd("sw_vers", &["-productVersion"])),
            unknown(cmd("sw_vers", &["-buildVersion"]))
        );
        p.cacheline = unknown(sysctl("hw.cachelinesize"));
        p.l2 = unknown(sysctl("hw.perflevel0.l2cachesize"));
        p.memory = unknown(sysctl("hw.memsize"));
        p.timer_hz = unknown(sysctl("hw.tbfrequency"));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        p.cpu = cpuinfo
            .lines()
            .find(|l| l.starts_with("model name"))
            .and_then(|l| l.split(':').nth(1))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".into());
        p.model = cmd("uname", &["-m"]).unwrap_or_else(|| "unknown".into());
        let pretty = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("PRETTY_NAME="))
                    .map(|l| l["PRETTY_NAME=".len()..].trim_matches('"').to_string())
            });
        p.os = format!("{} ({})", unknown(pretty), unknown(cmd("uname", &["-r"])));
        p.cacheline = unknown(
            std::fs::read_to_string(
                "/sys/devices/system/cpu/cpu0/cache/index0/coherency_line_size",
            )
            .ok()
            .map(|s| s.trim().to_string()),
        );
        p.l2 = unknown(
            std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cache/index2/size")
                .ok()
                .map(|s| s.trim().to_string()),
        );
        p.memory = unknown(
            std::fs::read_to_string("/proc/meminfo")
                .ok()
                .and_then(|s| s.lines().next().map(|l| l.trim().to_string())),
        );
        p.timer_hz = "n/a (Instant = CLOCK_MONOTONIC)".into();
    }
    p
}

impl Preflight {
    /// Timer tick in ns from `hw.tbfrequency` (24 MHz on Apple Silicon = 41.667 ns).
    pub fn timer_tick_ns(&self) -> Option<f64> {
        self.timer_hz.parse::<f64>().ok().map(|hz| 1e9 / hz)
    }

    /// Markdown table.
    pub fn render(&self) -> String {
        let mut o = String::new();
        let _ = writeln!(o, "| preflight | value |");
        let _ = writeln!(o, "|---|---|");
        let _ = writeln!(o, "| machine | {} / {} |", self.model, self.cpu);
        let _ = writeln!(o, "| cores | {} |", self.cores);
        let _ = writeln!(o, "| OS | {} |", self.os);
        let _ = writeln!(o, "| rustc | {} |", self.rustc);
        let _ = writeln!(
            o,
            "| RUSTFLAGS / target / profile | `{}` / {} / {} |",
            self.rustflags, self.target, self.profile
        );
        let _ = writeln!(o, "| hw.cachelinesize | {} |", self.cacheline);
        let _ = writeln!(o, "| hw.perflevel0.l2cachesize | {} |", self.l2);
        let _ = writeln!(o, "| memory (bytes) | {} |", self.memory);
        match self.timer_tick_ns() {
            Some(t) => {
                let _ = writeln!(
                    o,
                    "| timer | {} Hz (Instant tick {t:.3} ns) |",
                    self.timer_hz
                );
            }
            None => {
                let _ = writeln!(o, "| timer | {} |", self.timer_hz);
            }
        }
        let _ = writeln!(
            o,
            "| 1-min load average | {} |",
            self.load1
                .map(|l| format!("{l:.2}"))
                .unwrap_or_else(|| "unknown".into())
        );
        let _ = writeln!(
            o,
            "| affinity | none (thread_policy_set is KERN_NOT_SUPPORTED on Apple Silicon); no QoS request in v0.1 |"
        );
        o
    }
}

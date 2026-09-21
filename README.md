# lobcore

[![ci](https://github.com/charlieyanhx/lobcore/actions/workflows/ci.yml/badge.svg)](https://github.com/charlieyanhx/lobcore/actions/workflows/ci.yml)
![python 3.11 | 3.12](https://img.shields.io/badge/python-3.11%20%7C%203.12-blue)
![MIT](https://img.shields.io/badge/license-MIT-green)

lobcore is a compact L3 order book in Rust with a Nasdaq TotalView-ITCH 5.0 replay path, a
seeded synthetic ITCH day, book features (weighted mid, imbalance, Cont-Kukanov-Stoikov OFI)
and PyO3 bindings that hand numpy arrays back with the GIL released. The one design choice is
the book layout: a bounded flat array of 16-byte levels indexed by tick offset from a
per-symbol base, a two-level 64-bit bitmap that finds the best price with one `msb`/`ctz` per
level (no scan on depletion), 24-byte order slots in a slab with intrusive FIFO links, and a
`BTreeMap` overflow for every price outside the window, so correctness never depends on the
window and speed only does. It is justified against a `BTreeMap` + `VecDeque` reference book
with the identical API on the same stream: on the committed 100,000-message synthetic day the
array book parses and applies 9.15 M msgs/s against the reference book's 4.80 M, a 1.90x
ratio of medians over 5 fresh-process runs on an Apple M1 with `-C target-cpu=apple-m1`
(min-max 3.34-11.47 M and 3.68-5.44 M; the run's 1-minute load average was 15.15, far above
the harness's 1.0 gate, and repeated batches under load moved the ratio between 1.41x and
2.80x, so neither the absolutes nor the ratio is a result yet, only the ordering: the array
book was faster in every run of every batch, and an idle re-measurement is owed). What is
tested: the array book and the reference book return identical events after
every operation and identical snapshots every 64 operations over proptest sequences; a
counting global allocator sees 0 allocations over 900,000 in-window messages; the event log
and book-state hashes are reproduced by a stdlib-only Python reference on the fixture; the
synthetic day's truth sidecar matches after every one of its 100,000 messages. What it is not:
there is no matcher yet (v0.2), no wire path, no thread pinning, and it is not a production
system.

## Run it

```sh
export PATH=$HOME/.cargo/bin:$PATH          # rust 1.98.1 pinned by rust-toolchain.toml
cargo test --workspace                     # 103 tests (+1 ignored real-data test), 46 s wall on the M1 under load
cargo run --release -p lob-bench -- synth --seed 7 --n 100000 --out /tmp/s7.itch   # byte-equals tests/fixtures/synth_s7_100k.itch
cargo run --release -p lob-bench -- replay --stats tests/fixtures/synth_s7_100k.itch
cargo run --release -p lob-bench -- bench-quick tests/fixtures/synth_s7_100k.itch   # 5 fresh runs, preflight, load gate
python -m venv .venv && .venv/bin/pip install maturin==1.15.0 pytest numpy
cd python/lobcore && ../../.venv/bin/maturin develop --release && cd ../..
.venv/bin/pytest -q                        # 53 tests: hash parity, synth truth per row, book oracles, replay, stats, stub sync, README wording
scripts/fetch_itch.sh                      # local only: the md5-pinned 50 MB prefix of the public Nasdaq sample
cargo test -p lob-feed --release --test real_head -- --ignored   # replays it: 4,330,712 messages, unknown-id 0
```

Python, after `maturin develop`:

```python
import lobcore
b = lobcore.Book(base_px=1_000_000, n_levels=2048, tick=100)      # prices in 1e-4 units
b.add(1, "B", 1_000_500, 300); b.add(2, "S", 1_000_600, 100)
b.l1()                                   # ((1000500, 300, 1), (1000600, 100, 1))
b.l2(5)["bid"]                           # numpy structured array: px int32, qty uint32, count uint32
b.queue_ahead(1)                         # 0 shares ahead, exact FIFO walk
b.state_hash().hex()                     # the book-state hash of docs/DESIGN.md section 5
rows = lobcore.replay_itch("tests/fixtures/synth_s7_100k.itch", symbols=["SYN0003"], every_n=10)
rows["wmid"], rows["ofi"]                # numpy arrays, computed with the GIL released
stats = lobcore.replay_stats("tests/fixtures/synth_s7_100k.itch")
stats["event_log_hash"].hex()            # == the CLI's event-log sha256 below
```

## Replay statistics

The block below is written by `lobcore replay --stats tests/fixtures/synth_s7_100k.itch
--write-readme README.md` and checked byte for byte (every count and hash) by
`--check-readme README.md` in CI on the fixture the CI job regenerates from seed 7. The fixture
is 8 synthetic symbols, pre-open crossed quotes uncrossed at the `Q` event, 1 % placeholder adds
at $0.01 / $199,999.99 and sub-penny prices for the sub-dollar names. The block carries no
timing rows (a second `--write-readme` is a no-op); `lobcore replay --stats` prints two msgs/s
rows to stdout for the run at hand, and the Throughput section is the benchmark.

<!-- lobcore:begin:stats -->
| replay | value |
|---|---|
| source | `synth_s7_100k.itch` |
| messages (complete frames) | 100,000 |
| truncated final message | 0 |
| inflated bytes consumed | 2,968,812 |
| unknown message types (skipped by length) | 0 |
| by type | S 6 · R 8 · H 8 · L 4,987 · A 40,086 · F 943 · E 668 · X 11,134 · D 37,219 · U 4,845 · P 96 |
| time span (ns since midnight) | 08:41:15.000000000 – 16:00:00.000000000 |
| system events | O@08:41:15.000000000 S@08:41:15.000000000 Q@09:30:00.000000000 M@16:00:00.000000000 E@16:00:00.000000000 C@16:00:00.000000000 |
| locates with a book | 8 (0 array, 8 reference); watchlist: none; array window 2048 levels/side |
| order messages without a prior R | 0 |
| adds at placeholder prices (<= $0.01 or >= $199,900) | 413 of 41,029 = 1.01 % |
| live orders: high-water mark / at close | 3,657 / 3,649 |
| crossed snapshots before Q | 9,819 |
| crossed snapshots after Q (all / trading-state T) | 0 / 0 |
| unknown-id | 0 |
| negative-qty attempts (over-execute + over-cancel) | 0 (0 + 0) |
| duplicate id / bad qty / bad price / bad side | 0 / 0 / 0 / 0 |
| E/C at level head | 668 / 668 = 1.0000 |
| event log | 94,895 records, sha256 `fd52410fc6465823184edb2830ae6969fb27a5d7f2a4ea4497012a2c16245fcc` |
| close book-state hash, all locates | `851e617c74e7a148336501e55073f060783565de218818a853275f69e891ae34` |

| type \ hour | 08 | 09 | 10 | 11 | 12 | 13 | 14 | 15 | 16 | total |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| S | 2 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 3 | 6 |
| R | 8 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 8 |
| H | 8 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 8 |
| L | 204 | 672 | 693 | 682 | 691 | 642 | 679 | 724 | 0 | 4,987 |
| A | 1,749 | 5,462 | 5,567 | 5,528 | 5,417 | 5,519 | 5,404 | 5,440 | 0 | 40,086 |
| F | 29 | 116 | 141 | 121 | 126 | 118 | 142 | 150 | 0 | 943 |
| E | 9 | 337 | 46 | 50 | 53 | 62 | 57 | 54 | 0 | 668 |
| X | 450 | 1,553 | 1,540 | 1,533 | 1,559 | 1,439 | 1,552 | 1,508 | 0 | 11,134 |
| D | 1,604 | 5,195 | 5,000 | 5,073 | 5,174 | 5,139 | 5,111 | 4,923 | 0 | 37,219 |
| U | 195 | 688 | 703 | 631 | 645 | 647 | 673 | 663 | 0 | 4,845 |
| P | 5 | 16 | 16 | 10 | 11 | 13 | 15 | 10 | 0 | 96 |
| total | 4,263 | 14,040 | 13,706 | 13,628 | 13,676 | 13,579 | 13,633 | 13,472 | 3 | 100,000 |

| locate | symbol | book | msgs | live HWM / close | H | overflow hits | max abs offset (all / ex-placeholder) | window (base, levels, tick) | close book-state hash |
|---:|---|---|---:|---:|---|---:|---:|---|---|
| 1 | SYN0001 | reference | 12,503 | 425 / 413 | T | – | – | – | `9b9fa325b87c071b31b0009ddbdaaf428b4fdd9dccf5f705e56785377575cdc8` |
| 2 | SYN0002 | reference | 12,417 | 272 / 269 | T | – | – | – | `9dec91141e201ce8f246da68a3a25eed6dfa412b050f92cadbf3253841f59691` |
| 3 | SYN0003 | reference | 12,403 | 461 / 449 | T | – | – | – | `a9c7049069f16d3b11bfb32627163e625be82bc2f0c81e338b49eab71b69aa99` |
| 4 | SYN0004 | reference | 12,472 | 566 / 564 | T | – | – | – | `2698c411a56eb273b993d7e7717cda0955b4b63dcd433c1c8929379e8edd99ae` |
| 5 | SYN0005 | reference | 12,540 | 360 / 351 | T | – | – | – | `647c62049183ca5756fcfc27e7eb1745e965160d2264a5f9046e360093f6b452` |
| 6 | SYN0006 | reference | 12,629 | 525 / 525 | T | – | – | – | `d275fb5f4aee29428e45ac423612c779694b511c849728e42d8f8d17dc77d895` |
| 7 | SYN0007 | reference | 12,440 | 533 / 505 | T | – | – | – | `feb63e61441028ccca8bc2175c74e3d32953879f0339a854ef9c74f8283ae426` |
| 8 | SYN0008 | reference | 12,590 | 579 / 573 | T | – | – | – | `460bb4cc14d4ebe1b34e25a5ba1bc4858490c0695efa2085705924d498ae9747` |
<!-- lobcore:end:stats -->

### Real prefix (local, not CI)

`scripts/fetch_itch.sh` range-downloads the first 52,428,800 bytes of Nasdaq's public
`12302019.NASDAQ_ITCH50.gz` (md5 `8bd91e6f5b4a31d4d50dd6ac8a8fe7e2`) into the gitignored
`data/` directory; the bytes are Nasdaq's and are never committed, only these aggregates are.
The prefix covers 03:04-09:02 ET (pre-open only: no `C`, `Q` or `B` messages). zlib inflates
it to 128,451,559 bytes = 4,330,712 complete messages plus a 13-byte tail (`gzip -dc` stops
999 bytes earlier at 128,450,560 B / 4,330,679 messages because it discards the incomplete
final deflate block; lobcore ships flate2 with the zlib-rs backend and reports the zlib
figure). Machine: MacBookPro17,1 / Apple M1 (4P + 4E), 8 GB, macOS 26.0.1 (25A362), rustc
1.98.1, `-C target-cpu=apple-m1`. Command:
`lobcore replay --stats data/itch_12302019_head50m.gz --symbol AAPL --symbol MSFT --symbol QQQ --symbol SPY`;
every count and hash below is also asserted, literal for literal, by
`cargo test -p lob-feed --release --test real_head -- --ignored`. The "crossed snapshots" rows
count `bid >= ask` after a book change (locked or crossed; pre-open books are).

| replay | value |
|---|---|
| source | `itch_12302019_head50m.gz` |
| messages (complete frames) | 4,330,712 |
| truncated final message | 1 (source cut mid-stream: range-downloaded gzip prefix) |
| inflated bytes consumed | 128,451,559 |
| unknown message types (skipped by length) | 0 |
| by type | S 2 · R 8,906 · H 8,901 · Y 8,897 · L 215,087 · V 1 · K 3 · A 1,729,797 · F 38,901 · E 17,789 · X 477,211 · D 1,613,725 · U 208,061 · P 3,431 |
| time span (ns since midnight) | 03:04:32.057543747 – 09:01:59.442416600 |
| system events | O@03:04:32.057543747 S@04:00:00.000198145 |
| locates with a book | 8,906 (4 array, 8902 reference); watchlist: AAPL MSFT QQQ SPY; array window 2048 levels/side |
| order messages without a prior R | 0 |
| adds at placeholder prices (<= $0.01 or >= $199,900) | 15,232 of 1,768,698 = 0.86 % |
| live orders: high-water mark / at close | 145,701 / 145,691 |
| crossed snapshots before Q | 21 |
| crossed snapshots after Q (all / trading-state T) | 0 / 0 |
| unknown-id | 0 |
| negative-qty attempts (over-execute + over-cancel) | 0 (0 + 0) |
| duplicate id / bad qty / bad price / bad side | 0 / 0 / 0 / 0 |
| E/C at level head | 17,789 / 17,789 = 1.0000 |
| event log | 4,085,484 records, sha256 `3514db25d5e09cee6e05240eaa29d88ef554670e0402cac0a6e03ce5c61362f6` |
| close book-state hash, all locates | `19a984f0d6d7742eb9ae27185cdc006bbae7ceccef9dc96c804babe3a7cba584` |

| type \ hour | 03 | 04 | 05 | 06 | 07 | 08 | 09 | total |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| S | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 2 |
| R | 8,906 | 0 | 0 | 0 | 0 | 0 | 0 | 8,906 |
| H | 8,896 | 1 | 0 | 0 | 4 | 0 | 0 | 8,901 |
| Y | 8,893 | 4 | 0 | 0 | 0 | 0 | 0 | 8,897 |
| L | 215,036 | 0 | 0 | 3 | 25 | 23 | 0 | 215,087 |
| V | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 1 |
| K | 0 | 0 | 0 | 0 | 1 | 2 | 0 | 3 |
| A | 0 | 343,486 | 293,681 | 282,543 | 266,974 | 452,496 | 90,617 | 1,729,797 |
| F | 0 | 43 | 0 | 149 | 3,199 | 32,929 | 2,581 | 38,901 |
| E | 0 | 880 | 730 | 1,629 | 3,808 | 9,917 | 825 | 17,789 |
| X | 0 | 126,174 | 104,538 | 96,405 | 79,029 | 68,390 | 2,675 | 477,211 |
| D | 0 | 337,348 | 292,915 | 280,612 | 261,486 | 415,754 | 25,610 | 1,613,725 |
| U | 0 | 40,877 | 42,050 | 37,835 | 38,053 | 46,253 | 2,993 | 208,061 |
| P | 0 | 157 | 117 | 235 | 572 | 2,141 | 209 | 3,431 |
| total | 241,732 | 848,971 | 734,031 | 699,411 | 653,152 | 1,027,905 | 125,510 | 4,330,712 |

| locate | symbol | book | msgs | live HWM / close | H | overflow hits | max abs offset (all / ex-placeholder) | window (base, levels, tick) | close book-state hash |
|---:|---|---|---:|---:|---|---:|---:|---|---|
| 13 | AAPL | array | 6,316 | 717 / 717 | T | 1,841 | 71034 / 71034 | 2896600, 2048, 100 | `538c02205848c659cc3430338c832fec5c01c723579d7b8b02963df04affc8f6` |
| 5291 | MSFT | array | 4,095 | 1,018 / 1,009 | T | 2,446 | 19999998 / 25483 | 100, 2048, 100 | `2431e9865f64545fce41294bcea601bae14252f333aecc3cda0797e3c61eb9bd` |
| 6556 | QQQ | array | 94,408 | 298 / 296 | T | 148 | 19979638 / 20261 | 2036100, 2048, 100 | `a89c74ef8b40ff9fb0807ae9d63b75e0f73b4740064b4d676df55e35487626a1` |
| 7451 | SPY | array | 27,896 | 392 / 391 | T | 247 | 31294 / 31285 | 3129500, 2048, 100 | `569204099219277884c881e67ccfc058a19f0d7451a4d900b4a318216f4d7fb4` |

The pre-open drift is the v0.2 re-centring input: the 2048-level window centred on each
symbol's first real add saw its real prices wander 20,261-71,034 ticks away (MSFT's window
bottomed out at $0.01), and 148-2,446 adds per symbol went to the overflow map.

The same command prints two msgs/s rows for the run at hand, which are not in the block above
because they are single-run numbers: the run that produced the block read 1.86 M msgs/s
excluding gunzip/read (parse+apply + event-log sha256 2,334.1 ms, read 402.9 ms) and 1.58 M
including it (wall 2,736.9 ms), at a 1-minute load average of 11.7 with other processes
active; indicative only. Inflate is ~15 % of the wall here; the incremental sha256 and
parse+apply are the rest (Throughput section).

## Throughput

`lobcore bench-quick tests/fixtures/synth_s7_100k.itch --runs 5`: the fixture is inflated into
memory once per run, so gunzip and file I/O are excluded; each mode runs in a fresh process,
modes interleaved run by run; the table is the median with min-max over the 5 runs. Preflight
as printed: MacBookPro17,1 / Apple M1, 8 cores (4P + 4E), macOS 26.0.1 (25A362), rustc 1.98.1
(48a229cea 2026-09-01), RUSTFLAGS `-C target-cpu=apple-m1` (aarch64-apple-darwin, release),
hw.cachelinesize 128, hw.perflevel0.l2cachesize 12,582,912, memory 8,589,934,592 bytes, timer
24,000,000 Hz (Instant tick 41.667 ns), affinity none (thread_policy_set is KERN_NOT_SUPPORTED
on Apple Silicon), no QoS request in v0.1. **Load gate: the 1-minute load average was 15.15
against the harness's 1.0 limit (other processes were running), so `--out` would have refused
to persist this table. Nothing in it was measured under the gate, and the ratio is not more
robust than the absolutes: six 5-run batches under load (9-39) gave ratios of medians from
1.41x to 2.80x, the same 2x spread as the array row's medians. What survived every batch is
the ordering (array > reference in every run). `bench-quick` now prints the min / median / max
of the per-run paired ratio (run i array over run i reference) so the next, idle,
measurement quotes that; docs/DESIGN.md section 7.**

| mode | messages | runs | median msgs/s | min - max msgs/s | median ns/msg |
|---|---:|---:|---:|---|---:|
| parse only | 100,000 | 5 | 196.79 M | 107.24 - 247.78 M | 5.1 |
| parse+apply, ArrayBook watchlist | 100,000 | 5 | 9.15 M | 3.34 - 11.47 M | 109.3 |
| parse+apply, RefBook everywhere | 100,000 | 5 | 4.80 M | 3.68 - 5.44 M | 208.2 |
| parse+apply, ArrayBook watchlist + event-log sha256 | 100,000 | 5 | 3.30 M | 3.04 - 4.78 M | 302.7 |

ArrayBook / RefBook parse+apply ratio of the medians: 1.90x (all 8 locates on the array book,
window 2048 levels/side; the per-run paired ratio was not computed for this batch). The
incremental sha256 of the 34-byte event record costs more per message than the book operation
(302.7 - 109.3 = 193 ns against 109.3 ns), which is why the stats block's msgs/s rows carry the
"+ event-log sha256" label and the bench reports both. Per-message percentiles (K-batch
hdrhistogram, LATENCY.md) are v0.3; a single Instant tick is 41.667 ns on this machine, so no
per-operation tail is quoted here.

## Design rules

Tested:

- Differential (crates/lob-core/tests/differential.rs): `ArrayBook` and `RefBook` return the
  identical `Result<Event, BookError>` after every operation, identical snapshots and I1-I8
  every 64 operations, identical state hashes every 256, over proptest sequences on an
  in-range 2048-tick strategy, an overflow-heavy 64-tick strategy with +-40 drift, and a
  coarse-tick strategy (`PROPTEST_CASES` honoured, 256 locally, 64 in CI).
- Error is a no-op (I9, I10): unknown id / over-execute / duplicate id / bad qty / bad price
  leave the state hash unchanged and log an `unknown` / `reject` record derived from the error
  alone, so both books agree by construction.
- ITCH semantics from the spec text: `X` reduces in place and keeps priority, `U` deletes and
  rests the new reference at the tail (spec 1.4.5), `E` and `C` reduce by id at the display
  price, `P` / `Q` / `B` never touch the book (crates/lob-feed/tests/itch_bytes.rs).
- Allocation-free hot path: a counting `#[global_allocator]` sees 0 allocations and 0
  reallocations over 100,000 in-range book operations after warm-up, and over 900,000
  synthetic messages after a 100,000-message warm-up (crates/lob-core/tests/alloc_count.rs,
  crates/lob-feed/tests/alloc_count.rs).
- Hash contracts across three implementations: the seed-7 fixture's event-log sha256 and every
  locate's close book-state hash agree between the Rust session, `lobcore.Book` driven from
  Python, a stdlib-only Python dict + deque reference, and the generator's truth sidecar
  (python/tests/test_hash_parity.py; crates/lob-core/tests/fixtures/ops_pyref.py pins the
  5,000-op `ops.bin` digests the same way).
- Truth after every message: the synthetic day replays to its sidecar's best bid/ask, L5, live
  count and book hash after each of its 100,000 messages, on both book kinds and on seeds 1-3
  with stale references injected (crates/lob-feed/tests/synth_replay.rs); from Python,
  `synth_itch(..., truth=True)` hands the same sidecar back as numpy arrays (the hash column
  is an `(n, 32)` uint8 array, never an `S32` array that would strip a trailing NUL byte from
  1 digest in 256) and python/tests/test_synth.py checks every one of the 100,000 rows.
- Rejected messages are counted, not applied: a duplicate id, unknown reference, over-execute
  or bad side byte leaves the book unchanged, emits no `replay_itch` row, does not advance
  `every_n`, does not count as an execution in the E/C-at-head fraction and never centres a
  watched locate's array window (crates/lob-feed/tests/session_semantics.rs,
  python/tests/test_replay.py).
- No panic behind the Python boundary: the wheel is built with `panic = "abort"`, so every
  `synth_itch` argument is validated first and an invalid one raises `ValueError`
  (python/tests/test_synth.py); the CLI maps the same check to `lobcore: ...` / exit 1.
- gzip by magic, not by name: `open` sniffs `1f 8b`, decodes every member, and counts (rather
  than fails on) bytes after the last member (crates/lob-feed/src/itch/frame.rs tests).
- Byte oracles: the ITCH `A` message hex, the real sample's first frame, every type at its
  spec length and rejected at length-1, the 56-byte DBN MBO record round-trip on the two
  vendored stubs, the 34-byte event record hex, the empty-book hash = sha256 of nothing.
- Struct sizes: `size_of::<Slot>() == 24` and `size_of::<Level>() == 16` are unit tests.
- Bitmap oracle: levels {5, 130, 1000} set words {0, 2, 15} and summary
  `0b1000000000000101`, best 1000; deleting 1000 makes the best 130 with no scan.

By construction (not a test):

- Side lives in the sign of the slot price, not in a byte: one code path serves both sides.
- Prices outside the window are correct, not fast: the overflow `BTreeMap` is the only
  allocating path in the book, and its hits plus max |tick offset| are published per symbol.
- `bid < ask` is a replay statistic, not an invariant: real pre-open books are crossed until
  the `Q` event; the hard check applies only after `Q` and only in trading state `T`, and it
  becomes an invariant in the v0.2 matcher.
- Single writer: a `Session` owns its books and is never shared; there is no SPSC pipeline
  (docs/DESIGN.md section 4 says what was not built).
- The book-state hash has no side separator (the contract as written); a bid-only book and a
  bid + ask book with byte-identical level records would collide. docs/DESIGN.md section 5.
- Field decode copies bytes out of the frame (`from_be_bytes` on slices); the claim is no
  per-message heap allocation, and that one is tested.

## What is where

```text
lobcore/
├── Cargo.toml                  # workspace: pinned deps, release lto=fat, dev opt-level=3 for sha2 and lob-synth
├── rust-toolchain.toml         # 1.98.1 + clippy + rustfmt
├── ruff.toml                   # the CONVENTIONS ruff settings for python/tests (python/lobcore/pyproject.toml has the same)
├── .cargo/config.toml          # -C target-cpu=apple-m1 on aarch64-apple-darwin only
├── crates/
│   ├── lob-core/               # ArrayBook, RefBook, OrderBook trait, Event / EventLog, book-state hash, ops.bin
│   │   ├── src/{api,book,levels,slab,hash,types,ops,reference}.rs
│   │   └── tests/              # oracles, differential proptest, ops_seed7 pins (+ ops_pyref.py), alloc_count
│   ├── lob-feed/               # ITCH framing (plain / gz), borrowed message views, Session, stats block, DBN MBO decoder
│   │   ├── src/itch/{frame,msg,apply}.rs  src/mbo/record.rs  src/stats.rs
│   │   └── tests/              # itch_bytes, synth_replay (truth after every message), session_semantics, dbn stubs, alloc_count, real_head (#[ignore])
│   ├── lob-synth/              # seeded synthetic ITCH day + truth sidecar; examples/write_fixture.rs
│   ├── lob-features/           # wmid (exact rationals), imb1 / imb5, spread, depth5, CKS OFI
│   └── lob-bench/              # bin `lobcore`: replay --stats [--write-readme|--check-readme], synth, bench-quick (preflight.rs prints the machine block)
├── python/
│   ├── lobcore/                # PyO3 0.29 abi3-py311 crate: Book, RefBook, replay_itch, replay_stats, synth_itch; lobcore.pyi
│   └── tests/                  # test_hash_parity, test_synth, test_book, test_replay, test_stats, test_stub, test_readme_wording
├── tests/fixtures/             # synth_s7_100k.itch (2,968,812 B) + .sha256 sidecar (bytes and truth CSV)
├── scripts/fetch_itch.sh       # md5-pinned 50 MB range download of the public Nasdaq sample (local only)
├── docs/{PLAN,DESIGN}.md       # plan v2 (the contract) and the engineering note
├── NOTICE                      # the two Apache-2.0 DBN stubs, with repo + commit + sha256
└── .github/workflows/ci.yml    # fmt, clippy -D warnings, tests (PROPTEST_CASES=64), fixture + README regeneration, wheels + ruff + pytest
```

## Roadmap

- v0.2: `lob-match` (price-time, limit / market / IOC / FOK, self-trade prevention
  cancel_newest + cancel_oldest with the research fixture hashes), `lob-sim` (entry / response
  / feed latency with jitter, queue-position models with the research oracles), Databento MBO
  apply rules, PyO3 `Matcher` / `Sim` and the per-event iterator, and the re-centring decision
  made from the overflow numbers above.
- v0.3: criterion + hdrhistogram harness with K-batch percentiles, docs/LATENCY.md with the
  preflight protocol, a samply profile, the rtrb SPSC two-stage row (reported even if slower),
  the quotesim adapter (`Book` + `l2` + `queue_ahead` with a latency parameter), and the
  write-up "A bounded-array order book in Rust: design, measurements, and what I left out".
- Not planned: kernel bypass, FPGA parsing, feed arbitration, thread pinning, exchange
  conformance (docs/DESIGN.md section 8). Not in 0.1.0 either: a cargo-deny licence check
  (NOTICE lists the dependency licences by hand).

## Data and privacy

- Zero vendor bytes in the repository. The Nasdaq TotalView-ITCH 5.0 sample at emi.nasdaq.com is
  downloadable but not redistributable (Nasdaq Global Data Agreement; the 2008 notice says
  "for internal testing"), and files there vanish without notice. `scripts/fetch_itch.sh`
  fetches a 52,428,800-byte prefix with an md5 pin into the gitignored `data/`; only aggregates
  (counts, hashes, throughput) are published, in the "local, not CI" block above.
- The canonical inputs are synthetic: `tests/fixtures/synth_s7_100k.itch` is generated by
  `lob-synth` from seed 7 and regenerated byte-identically in CI; its 15.5 MB truth CSV is not
  committed, only its sha256 (line 2 of the `.sha256` sidecar).
- The two DBN fixtures under crates/lob-feed/tests/fixtures/dbn are Databento's own test
  files, published under Apache-2.0 in github.com/databento/dbn and databento-python; NOTICE
  records repo, path, commit and sha256. They are 2 and 4 records of byte layout, not market
  data.
- No LOBSTER, ever: the LOBSTER sample terms (version 2026-08-14, clause 5.1 (a) publication,
  (b) derived statistics, (f) redistribution, (g) benchmarking) forbid everything this
  repository does with data, so there is no reader, no fixture and no layout claim.
- Nothing private is used or stored; the build reads no environment secrets.

## Companion repos

- [tcakit](https://github.com/charlieyanhx/tcakit) - transaction-cost analysis and scheduling; consumes lobcore's fill log through its long-format frames (v0.2).
- [quotesim](https://github.com/charlieyanhx/quotesim) - market-making simulator; its v0.2 `book.py` will consume `Book`, `l2` and `queue_ahead` (v0.3 adapter).
- [tickq](https://github.com/charlieyanhx/tickq) - tick-data queries (DuckDB); owns the synthetic L1/L2 layouts lobcore does not read.
- [deskboard](https://github.com/charlieyanhx/deskboard) - trading-desk dashboard; consumes nothing from lobcore in v0.1-v0.3.
- [pricers](https://github.com/charlieyanhx/pricers), [riskkit](https://github.com/charlieyanhx/riskkit), [volsurf](https://github.com/charlieyanhx/volsurf) - option pricing, risk and surface tooling.
- [quant-research-agent](https://github.com/charlieyanhx/quant-research-agent) - the research-task runner.

MIT © Hanxiong (Charlie) Yan

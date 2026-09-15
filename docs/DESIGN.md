# lobcore v0.1 — design note

Every number below carries its basis (input, machine, run count, load). Numbers measured in
this build session were taken on a MacBookPro17,1 (Apple M1, 4P + 4E, 8 GB, macOS 26.0.1
25A362, rustc 1.98.1, `-C target-cpu=apple-m1`) while the 1-minute load average was between
12 and 70 from unrelated processes; none of them is an idle-machine benchmark, and section 9
records what that did to them.

## 0. Decision this serves

**Decision:** whether a bounded flat-array L3 book with a bitmap best-price index earns its
complexity over a tree book for ITCH replay on a laptop, and whether its correctness can be
pinned hard enough (differential tests, cross-language hashes) that a matcher and a simulator
can be built on it in v0.2 without re-auditing the book.

**Abstract.** lobcore is a compact L3 order book in Rust with an ITCH 5.0 replay path, a
seeded synthetic ITCH day, book features and PyO3 bindings. The one design choice is the book
layout: 16-byte levels in a flat array indexed by tick offset from a per-symbol base, a
two-level 64-bit bitmap for the best price, 24-byte order slots in a slab with intrusive FIFO
links, and a `BTreeMap` overflow for prices outside the window. The number: on the committed
100,000-message synthetic day the array book parses and applies 9.15 M msgs/s against the
`BTreeMap` reference book's 4.80 M, a 1.90x ratio, median of 5 fresh-process runs on the M1
above with a 1-minute load of 15.15 (min-max 3.34-11.47 M and 3.68-5.44 M), so the ratio is the
result and the absolute is provisional. What it is not: no matcher (v0.2), no wire path, no
thread pinning, not a production system.

## 1. Why another order book

The reference implementations read during the research pass do not do what lobcore does, so
the layout has to be justified on its own numbers rather than by citation. Published figures,
with the machine and the method that produced each:

| implementation | language / structure | figure | machine | method |
|---|---|---|---|---|
| nanolob (Hellblazer704) | C++20; flat hash map of levels + intrusive sorted level list, slab pool, 64-B padded nodes | add p50 33 ns, cancel p50 30 ns, mixed p99.9 437 ns, 34.8 M ops/s; std::map baseline 9.1 M/s | Intel Core Ultra 5 225H, GCC 16 -O3 | Google Benchmark, calibrated rdtsc, no pinning |
| limitbook (solarpx) | Rust; BTreeMap + VecDeque + HashMap | limit ~204 ns, crossing ~290 ns, market / cancel ~31 ns | unspecified | criterion |
| order-book-rs (farrellh1) | Rust; BTreeMap | ~104 ns median per matching limit order | Apple M3 Pro | criterion |
| OrderBook-rs 0.12 (joaquinbejar) | Rust; lock-free SkipMap + Arc levels | add p50 917 ns, p99 62.8 us, p99.9 97.7 us | Apple M4 Max | hdrhistogram, one Instant per op (see section 6) |
| orderbook-rs (rymnc) | Rust; bounded price range around a base | "> 1.5 M ops/s, sub-microsecond" | Apple M4 Pro | no distribution published |
| liquibook (enewhuis) | C++ | 2.0-2.5 M inserts/s sustained | ~2012 hardware | project README |
| itchy-rust (seanlane) | Rust ITCH 5.0 parser | ~20 M msgs/s | XPS 9370, i7-8550U | project README |
| **lobcore v0.1** | Rust; flat tick-indexed levels + two-level bitmap + slab + overflow BTreeMap | parse-only 196.79 M msgs/s; parse+apply array 9.15 M (109.3 ns/msg), reference 4.80 M (208.2 ns/msg) | Apple M1 4P+4E, 8 GB | `lobcore bench-quick`, 5 fresh processes interleaved, median with min-max, in-memory synthetic day, **load 15.15 (gate 1.0 not met)** |

None of the three repositories read line by line (nanolob, senzenn/rust-order-book,
Hoeppke/rust_order_book_simd) indexes levels by tick offset with a bitmap: nanolob keeps a
sorted intrusive list of levels (O(k) walk to insert a new level), senzenn scans every level
on cancel under a `RwLock`, Hoeppke is an L2 SIMD toy. The bounded array makes best-price
lookup, level creation and level depletion O(1) word operations; the price it pays is a window,
and the overflow map is what keeps that price a speed cost rather than a correctness cost.

## 2. Data structures and memory layout

Order slot, 24 B (`size_of` asserted by a unit test):

```text
Slot   { id: u64, qty: u32, px: i32, prev: u32, next: u32 }
        |<-8 B->|<-4 B->|<-4 B->|<-4 B->|<-4 B->|   px sign = side (+bid, -ask)
```

Level record, 16 B (asserted): `Level { head: u32, tail: u32, qty: u32, count: u32 }`, one per
grid price per side, indexed by `(px - base_px) / tick`; `qty` is `u32` with `checked_add`, a
level summing past 4,294,967,295 shares is rejected as a data error (both books, same
precedence). Best price: `words[n / 64]` of `u64` plus one `u64` summary; the bid best is
`msb(summary)` then `msb(word)`, the ask best `ctz` twice; set / clear on the level's count
0 <-> 1 transition. Oracle from the research pass, now a unit test: levels {5, 130, 1000} set
words {0, 2, 15} and summary `0b1000000000000101`, best 1000; deleting level 1000 gives best 130
and summary `0b101` with two `msb` operations and no scan.

Figure 1. Cache-line packing of the two records, drawn for a 64 B line (x86) and the 128 B
line the M1 reports (`hw.cachelinesize` = 128, verified by the preflight):

```text
64 B line   |slot 0 (24 B)     |slot 1 (24 B)     |slot 2 (16 of 24 B)      |   2.67 slots / line
            |L0  |L1  |L2  |L3  |                                              4 levels / line
            0    16   32   48   64

128 B line  |slot 0 |slot 1 |slot 2 |slot 3 |slot 4 |slot 5 (8 of 24 B)|      5.33 slots / line
            |L0 |L1 |L2 |L3 |L4 |L5 |L6 |L7 |                                  8 levels / line
            0   16  32  48  64  80  96  112 128

bitmap, 4096 levels: 64 words x 8 B = 512 B = 8 x 64 B lines = 4 x 128 B lines, + 1 summary word
```

An `add` at an in-window price touches: the id map (one `FxHashMap<u64, u32>` probe), the
slot (one line), the level record (one line, 4-8 levels share it, so a busy touch stays hot),
and at most two bitmap words. A `cancel` by id touches the id map, the slot, its two FIFO
neighbours' slots (unlink) and the level record. Nothing in that path allocates; the slab has
a `u32` free list and the id map is pre-sized by `reserve` (2x the order estimate) so its
steady state never rehashes; both facts are enforced by the counting-allocator tests.

Overflow: every positive price off the grid (sub-penny on a 1c grid) or outside
`[base_px, base_px + (n_levels - 1) * tick]` lives in a per-side `BTreeMap<Px, Level>` with
the same FIFO representation; `l1` compares the array best with the overflow extreme only when
the overflow map is non-empty; emptied overflow levels are removed (the documented allocating
path). The overflow count and the max |tick offset| are published per symbol. On the real
pre-open prefix this is not a corner case: 0.9 % of adds are $0.01 / $199,999.99 placeholders
(15,844 of 1,768,698 in the research pass), and the 2048-level window centred on each
watchlist symbol's first real add saw real prices 20,261-71,034 ticks away (README, real
prefix block: AAPL 1,841 overflow hits, MSFT 2,446, QQQ 148, SPY 247 over 4.33 M messages).

Why a window and not a full-range array: with 8,906 stock locates on the sample day a
4,096-tick window costs 4,096 x 16 B x 2 sides x 8,906 = 1.17 GB of level records, and a
65,536-tick full-market window would be 65,536 x 16 B x 8,906 = 9.3 GB per side, impossible on
8 GB. Hence arrays only for a watchlist (`--symbol` / `--locate`), and the `BTreeMap` reference
book for every other locate; the reference book is also the tree comparator in section 7. No
re-centring in v0.1; the overflow numbers above are the v0.2 input.

## 3. Message path

ITCH 5.0 from the spec, not assumed. The emi.nasdaq.com sample files carry a 2-byte big-endian
payload length before every message (verified on the real head: `000c 53 ... 4f`, an `S` `O`
event at 10,953,404,452,051 ns after midnight, on the 01302020 file read during research; the
12302019 head starts the same way and is checked live by the ignored test). Every message:
type @0, stock locate u16 @1, tracking u16 @3, timestamp u48 @5, body @11; the size table for
all 23 types (S12 R39 H25 Y20 L26 V35 W12 K28 J35 h21 A36 F40 E31 C36 X23 D19 U35 P44 Q40 B19
I50 N20 O48) had 0 mismatches over 4,330,712 real messages. A wrong length is a fatal
`ParseError::Length` (no resync); an unknown type is skipped by its framed length and counted.

Book semantics: `A` / `F` rest at the tail of the level (`F`'s attribution never touches the
book); `E` and `C` reduce **by id** at the order's display price (`Printable` and the execution
price only affect the tape); `X` reduces in place and keeps priority; `D` removes; `U` removes
the original reference and rests the new one at the tail of its (possibly same) level with the
side carried from the original, because spec 1.4.5 says the original's shares are "no longer
accessible" and a new reference number "will be used henceforth"; `P`, `Q`, `B` never touch a
book. The plan's original "replace keeps priority when the size decreases" is Databento's `M`
rule, scheduled for the v0.2 MBO apply layer, not an ITCH rule. Oracles as tests: FIFO
[1, 2, 3] with 300/100/200 -> `X(1, 50)` keeps [1, 2, 3] at level qty 550 -> `U(1 -> 9, 250)`
gives [2, 3, 9].

Framing runs over any `Read` through a 4 MB compacting buffer (gzip via flate2 with the
zlib-rs backend when the path ends in `.gz`) and over an in-memory slice; frames are returned
as borrowed slices and the message views are borrowed too, so parsing allocates nothing. A
cut deflate stream (the range-downloaded prefix) ends the input with `source_cut = true`
rather than failing; the incomplete final message is discarded and counted. Field decode uses
`from_be_bytes` on fixed slices: the crate needs no `unsafe`, so the plan's allowance of one
documented unaligned read in lob-feed went unused.

Databento MBO: the DBN header (`DBN`, version 3, u32 metadata length; 352 in both stubs) and
the 56-byte little-endian `MboMsg` record are decoded with `from_le_bytes` (no `repr(C)` cast,
so alignment is a non-issue) and re-encoded byte for byte on the two vendored stubs
(XNAS.ITCH NVDA 4 x `A`, GLBX.MDP3 ESH1 2 x `C`). The v0.2 apply rules from Databento's own
reference book: `R` clears; `A` appends; `C` reduces and drops at zero; `M` in place only for a
same-price size decrease, else to the tail; `M` on an unknown id is an add; `F_TOB` replaces
the side; snapshots emit on `F_LAST`; one book per (instrument, publisher).

"No per-message heap allocation" means exactly this: after warm-up, a counting
`#[global_allocator]` records 0 allocations and 0 reallocations across 100,000 in-range book
operations (lob-core) and across 900,000 synthetic messages after a 100,000-message warm-up
with placeholders off and a 4,096-level window (lob-feed), with `l1` and `queue_ahead` also
allocation-free. It does not mean zero copies: field decode copies integers out of the frame,
and the event log copies 34 bytes per event into a sha256 state.

## 4. Threading model: single writer, and the SPSC pipeline I did not build

A `Session` owns its books, its event log and its counters, and one thread drives it from one
frame source; nothing is shared, nothing is locked, `ArrayBook` and `RefBook` are plain `Send`
values. That is the single-writer principle from the LMAX Disruptor paper and Thompson's 2011
note, and it is the whole threading model of v0.1 and v0.2.

What was not built: a decode thread handing parsed messages to a book thread through an SPSC
ring, and a book thread publishing versioned top-N snapshots to readers. The research pass
priced it at "rtrb 0.4.0 two-stage experiment, reported even if slower", and the reason it is a
v0.3 optional row rather than a v0.1 feature is arithmetic: at 109 ns per parse+apply message
(section 7, loaded machine) a handoff that costs tens of nanoseconds of cache traffic per
message buys nothing unless the two stages are genuinely balanced, and parse-only runs at
5.1 ns per message here, so they are not. OrderBook-rs's 917 ns p50 for a "thread-safe" add
(table, section 1) is the cost of paying for concurrency the replay does not use.

## 5. Correctness

**Reference model.** `RefBook` is `BTreeMap<Px, VecDeque<(OrderId, Qty)>>` per side plus
`HashMap<OrderId, (Side, Px)>`, implementing the same `OrderBook` trait with the same error
precedence (including the level-total cap). It is the differential comparator, the
tree comparator for the throughput ratio, and the book of every non-watchlist locate.

**Invariants I1-I12** (`ArrayBook::check`, run every 64 operations and at the end of every
differential case): I1 level qty == sum of FIFO qtys; I2 level count == FIFO length; I3 tail
reachable from head, prev/next consistent; I4 an order's (side, |px|) equals its level; I5
order qty > 0; I6 bitmap bit == (count > 0), summary bit == (word != 0); I7 live + free slots
== capacity; I8 per-side level sum == live-order sum; I9 unknown-id cancel / delete / execute
/ replace -> `Err` + `unknown` record, state hash unchanged; I10 over-execute -> `Err`, state
unchanged; I11 array snapshot == reference snapshot and identical `Result<Event, BookError>`
after **every** operation; I12 the in-range (2048 ticks, clamped) and overflow-heavy
(64 ticks, unclamped, +-40 drift per 500 ops; a fixed-seed case asserts > 100 overflows) and
coarse-tick strategies all pass. `bid < ask` is deliberately absent: it is a replay statistic
(9,819 crossed snapshots before `Q` and 0 after on the fixture; 21 before `Q` on the real
pre-open prefix) and becomes an invariant only in the v0.2 matcher, where prices are generated
relative to the opposite best.

**Differential pattern** (nanolob's `test_differential.cpp`, in proptest form): an abstract
op stream (`Vec<u64>` decoded into Add / Cancel / Execute / Delete / Replace with 5 % unknown
ids, prices offset from the opposite best, drift every 500 ops) is interpreted against both
books; the event suffix is compared after every op, snapshots and `check()` every 64,
state hashes every 256 and at the end, plus `queue_ahead` for every live id. `PROPTEST_CASES`
is honoured (256 locally, 64 in CI). A committed 5,000-op `ops.bin` (sha256
`1fa8bb02...`) pins the event-log digest `824b07d9...`, the close state hash `0ac37ba1...`,
358 errors and 999 live orders, and a stdlib-only Python reference
(`crates/lob-core/tests/fixtures/ops_pyref.py`) reproduces all four independently.

**Event-log hash contract.** Record = 34 bytes little-endian: `u8 type | u64 a | u64 b |
i64 px (1e-4 units) | u64 qty | u8 side`; type codes add=1 cancel=2 exec=3 delete=4 replace=5
modify=6 unknown=7 reject=8 trade=9 stp_cancel=10 ioc_cancel=11 fok_reject=12; side 0 = bid,
1 = ask; the digest is sha256 over one leading format-version byte (= 1) then every record in
emission order, so `hashlib` reproduces it. Field conventions per kind (add: a = id; cancel /
exec: b = remaining after, qty = removed; delete: qty = removed; replace: a = old, b = new;
unknown: b = kind code; reject: b = kind code, qty = requested) are in `lob_core::types`. Oracle:
`trade(5, 4, 10001, 30, bid)` encodes to
`090500000000000000040000000000000011270000000000001e0000000000000000`. The research
pass's seed 1-3 digests (`d2c9d0d8...`, `af0921d6...`, `a7b7edab...`) are **not** reproduced:
they came from a Python model whose field conventions were not preserved, so the seed-7
`ops.bin` pins replaced them (section 9).

**Book-state hash contract.** sha256 over the bid side then the ask side, ascending price,
each level as `i64 px LE, u32 count LE` then `(u64 id LE, u32 qty LE)` in FIFO order, no side
separator, no version byte; the empty book hashes to sha256 of nothing
(`e3b0c442...`). Prices are hashed positive on both sides (the sign-as-side trick never leaves
the slot). It is the `replay --stats` close hash per locate, the generator's per-message truth,
and the key of the three-way parity test: on the seed-7 fixture the Rust session, `lobcore.Book`
driven from Python, and the pure-Python dict + deque reference agree on all 8 close hashes, the
all-locates hash `851e617c...` and the event-log hash `fd52410f...` (94,895 records). Known
weakness, kept because the contract says so: with no side separator a bid-only book and a
bid + ask book whose level bytes coincide collide; changing it means changing `hash.rs`,
`ops_pyref.py`, `lob-synth`'s truth hash and the pins together.

**Truth after every message.** The synthetic generator keeps its own book and writes best
bid/ask, L5 quantities, live count and book hash after each of its `n` messages; lob-feed
replays the seed-7 fixture and seeds 1-3 (40k messages, stale references injected, `C`/`P`/`L`
in the mix) on both book kinds and compares after every message.

## 6. Measurement methodology

Summary; the full protocol is v0.3's LATENCY.md. Facts measured in the research pass on this
machine and printed by `bench-quick`'s preflight: `Instant::now()` is `mach_absolute_time`, a
24 MHz counter, so one tick is 41.667 ns and back-to-back deltas read 0 most of the time; a
20-100 ns operation timed one at a time is quantisation noise, which is why per-operation
percentiles are not quoted in v0.1 and why v0.3 times batches of K = 64 messages into an
hdrhistogram with an empty-loop overhead row. `thread_policy_set(THREAD_AFFINITY_POLICY)`
returns `KERN_NOT_SUPPORTED` (46) on Apple Silicon: no pinning; QoS is the only lever and v0.1
does not request it. `hw.l2cachesize` reports the E-cluster's 4 MiB; the preflight prints
`hw.perflevel0.l2cachesize` (12 MiB) instead. Load gate: `bench-quick --out` refuses to persist
when the 1-minute load average exceeds 1.0 (exit 3), prints the report anyway, and records the
load in the preflight. Five runs, each mode in a fresh process, modes interleaved run by run,
median with min-max. Throughput on the compressed real prefix is reported both excluding and
including gunzip and both rows say so, because the streamed number is gzip-bound: on the real
head 480.6 ms of the 3,271 ms wall was inside `read`.

## 7. Results

**Table 1 — replay statistics.** The CI block for the seed-7 fixture and the local block for
the real prefix are in README.md ("Replay statistics"); the pins are: fixture 100,000
messages, live 3,657 / 3,649, crossed before `Q` 9,819 and 0 after, unknown-id 0, negative-qty
0, E/C at head 668 / 668, event log 94,895 records `fd52410f...`, all-locates close hash
`851e617c...`; real prefix 4,330,712 messages + 1 truncated, 8,906 locates, live HWM 145,701,
crossed before `Q` 21, unknown-id 0, negative-qty 0, E/C at head 17,789 / 17,789, event log
4,085,484 records `3514db25...`, all-locates hash `19a984f0...`, 1.55 M msgs/s excluding
gunzip with the event log on (single run, load 12.2).

**Table 2 — bounded array vs tree on the same stream** (`bench-quick`, seed-7 fixture in
memory, all 8 locates on the array book, window 2048, 5 fresh processes interleaved, Apple M1,
`-C target-cpu=apple-m1`, **load 15.15**):

| mode | median msgs/s | min - max | median ns/msg |
|---|---:|---|---:|
| parse only | 196.79 M | 107.24 - 247.78 M | 5.1 |
| parse+apply, ArrayBook | 9.15 M | 3.34 - 11.47 M | 109.3 |
| parse+apply, RefBook | 4.80 M | 3.68 - 5.44 M | 208.2 |
| parse+apply, ArrayBook + event-log sha256 | 3.30 M | 3.04 - 4.78 M | 302.7 |

Ratio of medians ArrayBook / RefBook 1.90x. Two caveats that are results in themselves: the
array row's min-max spans 3.4x under this load, so the ratio is the quotable number; and the
incremental sha256 of the 34-byte event record costs more than the book operation (3.30 M vs
9.15 M), which is why the stats block labels its msgs/s rows "+ event-log sha256" and the bench
reports the book with and without it. The `RefBook` `l1()` allocates a boxed iterator per call
(used by the crossed-snapshot check after every book change), which flatters the ratio in the
array book's favour by an amount not yet measured.

Figure 2 (latency histogram) and Figure 3 (throughput vs window width / overflow share) are
v0.3 deliverables and do not exist yet; the overflow inputs for Figure 3 are already published
per symbol (README real-prefix rows). No profile was taken: cargo-flamegraph on macOS needs
xctrace (full Xcode, absent); samply or perf on Linux CI is the v0.3 route.

## 8. What I left out

Kernel bypass (OpenOnload / ef_vi / DPDK); FPGA parsing; A/B feed arbitration and gap
recovery; isolcpus, IRQ pinning and busy-polling; hugepages; SPSC rings (section 4);
hardware timestamps and PTP; cache warming; colocation; exchange conformance testing;
re-centring of the window (v0.2 decision from the published overflow numbers); MBO replay
(decoder only, apply rules v0.2); the matcher and simulator (v0.2). Also left out on
purpose: Stoikov's micro-price (a fitted Markov table, not a closed form; the weighted mid is
the deterministic feature shipped), and any LOBSTER reader (its sample terms forbid the use).

## 9. Measurement pitfalls, logged verbatim from build sessions

Wrong number -> corrected number, in the order they were hit; nothing composed.

1. Real-prefix oracle: the plan pinned "128,450,560 B = 4,330,679 messages" from `gzip -dc`
   of the 52,428,800-byte head. flate2/zlib-rs (and Python's `zlib`) inflate the same bytes to
   128,451,559 B = 4,330,712 messages + a 13-byte tail: gzip discards the output of the
   incomplete final deflate block, zlib emits the 999 bytes it had decoded. lob-feed pins the
   zlib figure and the README states the decoder next to the number.
2. Synthetic fixture live count: the generator's build report said "3,810 live orders at
   close"; the truth sidecar sums to 3,649 across the 8 locates and the replay agrees. 3,649
   is what is quoted.
3. Debug-profile sha256: the 100k-message truth-after-every-message test took ~120 s in the
   dev profile because sha256 over the whole truth book ran unoptimised after every message;
   `[profile.dev.package.sha2]` and `[profile.dev.package.lob-synth]` at `opt-level = 3` took
   the crate's suite to ~11 s. A test-time number, but it decided a workspace setting.
4. Differential strategy cost: generating proptest cases as `prop_oneof` tuples cost 12.7 s
   for 64 cases in a debug build versus 0.9 s decoding a `Vec<u64>`; same op mix, same
   element-wise shrinking. State-hash comparison moved from every 64 to every 256 ops for the
   same reason (debug sha256 was the single largest cost).
5. Every wall-clock number in this build was taken under a 1-minute load average of 12-70
   from other processes: the lob-core suite ran in 26 s at load 60-70, the workspace in 38 s at
   load 14, and `bench-quick` printed a load of 15.15 against its 1.0 gate. The array row's
   min-max (3.34-11.47 M msgs/s) is the visible damage; an earlier 3-run bench by the lob-feed
   builder at load 17 read ~9.7 M without the event log and 4.0 M with it, and a second 5-run
   batch at load 14.29 gave medians of 7.45 M (array), 3.54 M (reference) and 3.56 M (array +
   log), ratio 2.10x; the three batches agree with the quoted one (9.15 / 4.80 / 3.30 M, 1.90x)
   only in ratio.
6. The throughput headline would have been the hash: with the event log on, the
   parse+apply number was 4.0 M and the book looked slow; separating the rows showed the sha256
   costs more per message than the book (~140 ns vs ~100 ns on this M1). The bench now reports
   both and the stats block labels its rows.
7. README timing rows: the stats block written by `--write-readme` includes two msgs/s rows,
   and a second `--write-readme` run rewrote them (2.55 M -> 2.12 M, single runs). The
   `--check-readme` comparison ignores rows starting `| msgs/s`, so CI is stable; the README
   says those two rows are a single run and not a benchmark.
8. Python vs CLI window default: `lobcore.replay_stats` defaults `array_window` to 1024 (the
   contract) while the CLI defaults to 2048, and the window is printed in a deterministic line
   of the block, so the Python render did not match the CLI's block until the parity test
   passed `array_window=2048` explicitly.
9. Research-pass digests that could not be reproduced: the seed 1-3 event-log digests
   (`d2c9d0d8...`, `af0921d6...`, `a7b7edab...`) depend on a Python model's field conventions
   that were not preserved; rather than guess them, the build pinned its own seed-7 `ops.bin`
   and cross-checked it with a fresh stdlib-only Python reference.

## 10. Reproduce

Machine block for every number in this note: MacBookPro17,1 / Apple M1 (4P + 4E), 8 GB,
macOS 26.0.1 (25A362), rustc 1.98.1 (48a229cea 2026-09-01), `-C target-cpu=apple-m1` via
`.cargo/config.toml` (aarch64-apple-darwin only), hw.cachelinesize 128, hw.perflevel0.l2cachesize
12,582,912, Instant tick 41.667 ns; 1-minute load 12-70 (see section 9).

```sh
export PATH=$HOME/.cargo/bin:$PATH
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                                        # PROPTEST_CASES=64 in CI
cargo run --release -p lob-bench -- synth --seed 7 --n 100000 --out /tmp/s7.itch
cmp /tmp/s7.itch tests/fixtures/synth_s7_100k.itch            # byte-identical fixture
cargo run --release -p lob-bench -- replay --stats tests/fixtures/synth_s7_100k.itch --check-readme README.md
cargo run --release -p lob-bench -- bench-quick tests/fixtures/synth_s7_100k.itch --runs 5
cd python/lobcore && maturin develop --release && cd ../.. && pytest -q   # 37 tests
scripts/fetch_itch.sh                                         # local only, ~1 min at ~0.8 MB/s
cargo run --release -p lob-bench -- replay --stats data/itch_12302019_head50m.gz --symbol AAPL --symbol MSFT --symbol QQQ --symbol SPY
cargo test -p lob-feed --release --test real_head -- --ignored
```

Fixture hashes: `tests/fixtures/synth_s7_100k.itch` 2,968,812 B sha256
`20cc1acc13a893c060d0fd1658a544c54dff96cf66688a6be9ace78d9d752268`; its truth CSV (not
committed) sha256 `41488ad1640f1e163983ed38bfd6c5d2f656c1ab1f0ba234f6b5ad7f62a39029`;
`crates/lob-core/tests/fixtures/ops_seed7.bin` 125,000 B sha256 `1fa8bb02...`; real prefix
md5 `8bd91e6f5b4a31d4d50dd6ac8a8fe7e2` (52,428,800 B). CI (ubuntu + macOS) runs everything
above except the two `data/` commands; the real prefix is local only and its numbers are
labelled so.

## 11. References

- Nasdaq TotalView-ITCH 5.0 specification (NQTVITCHspecification.pdf), sections 1.1-1.5;
  Order Replace semantics in 1.4.5.
- Nasdaq sample files: https://emi.nasdaq.com/ITCH/Nasdaq%20ITCH/ (12302019.NASDAQ_ITCH50.gz,
  3,524,013,057 B gz, ISIZE-derived 8,251,407,909 B inflated); Nasdaq Data News 2008-91
  ("for internal testing purposes").
- Databento DBN: https://github.com/databento/dbn, commit
  a7d8ee93a5d5d0b7082d0ca15ab85c01e3cce550 (`rust/dbn/src/record.rs`, `enums.rs`, `flags.rs`;
  fixture `tests/data/test_data.mbo.v3.dbn`); https://github.com/databento/databento-python,
  commit e675ef30369f5ee9f68b04606080c519adf3be69 (fixture
  `tests/data/XNAS.ITCH/test_data.mbo.dbn.zst`); Databento docs, "limit-order-book" and
  "queue-position" examples.
- R. Cont, A. Kukanov, S. Stoikov, "The price impact of order book events", arXiv:1011.6402
  (2014), section 2.1 (the OFI definition `e_n`).
- S. Stoikov, "The micro-price: a high-frequency estimator of future prices", Quantitative
  Finance 18(12), 2018 (why the micro-price is a fitted table and not shipped).
- U. Drepper, "What Every Programmer Should Know About Memory" (2007), cache-line and
  false-sharing arithmetic.
- M. Thompson, D. Farley, M. Barker, P. Gee, A. Stewart, "Disruptor" (LMAX, 2011); M.
  Thompson, "Single Writer Principle" (2011).
- Apple, "Thread Affinity API" release note (affinity is a hint; `KERN_NOT_SUPPORTED` on Apple
  Silicon measured here); SEC Regulation NMS Rule 612 (sub-penny quoting below $1).
- Order-book implementations read (commit hashes not recorded in the research pass; URLs as
  fetched 2026-09-15): https://github.com/Hellblazer704/nanolob (`include/nanolob/*.hpp`,
  `tests/test_differential.cpp`, BENCHMARKS.md); https://github.com/senzenn/rust-order-book;
  https://github.com/Hoeppke/rust_order_book_simd; https://github.com/solarpx/limitbook;
  https://github.com/farrellh1/order-book-rs; https://github.com/joaquinbejar/OrderBook-rs
  (BENCH.md); https://github.com/rymnc/orderbook-rs; https://github.com/enewhuis/liquibook;
  https://github.com/seanlane/itchy-rust; https://github.com/nkaz001/hftbacktest
  (`hftbacktest/src/backtest/models/queue.rs`, the v0.2 queue models).
- Crates: pyo3 0.29.2, numpy 0.29.0, maturin 1.15.0, proptest 1.11.0, flate2 1.1.10 (zlib-rs),
  rustc-hash 2, sha2 0.10, rand 0.9 / rand_chacha 0.9, clap 4; criterion 0.8.2 and
  hdrhistogram 7.6.0 pinned for v0.3.

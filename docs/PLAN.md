# lobcore — plan v2 (2026-09-15)

Plan v1 (the user's): a compact, benchmarked order-book and execution core in Rust with PyO3
bindings and an engineering write-up (DESIGN.md, LATENCY.md). Four research passes (book data
structures and invariants — with a Python reference model and a differential runner; data,
licensing and toolchain; benchmark methodology and the design-note content; a low-latency hiring
critic) and a cross-check changed the plan as follows. Every change carries a measured or verified
fact.

## What changed and why

1. **No LOBSTER anywhere.** The sample is gated and its 2026-08-14 terms forbid redistribution,
   derived statistics and benchmarking (5.1 a/b/f/g). tickq's synthetic LOBSTER layout is L1/L2
   (fresh id per quote), useless for an L3 book (replaying it as L3 gave 1,994 phantom orders and a
   crossed book). The v0.1 milestone reads "replay the synthetic ITCH day and the md5-pinned real
   prefix", not "one LOBSTER/ITCH day".
2. **The real data is the Nasdaq TotalView-ITCH 5.0 sample** at emi.nasdaq.com — downloadable, NOT
   redistributable (Nasdaq Global Data Agreement; the 2008 notice: "for internal testing"), files
   vanish without notice (every 2018 day is gone). Policy: commit ZERO vendor bytes; `scripts/fetch_itch.sh`
   range-downloads the first 52,428,800 bytes of `12302019.NASDAQ_ITCH50.gz` (md5
   `8bd91e6f5b4a31d4d50dd6ac8a8fe7e2`; inflates to 128,450,560 B = 4,330,679 complete messages,
   03:04–09:02 ET pre-open) and every number from it is an aggregate labelled "local, not CI". The
   canonical benchmark input is the synthetic ITCH day lobcore writes itself. A full day is 8.25 GB
   inflated (ISIZE trailer, ~278 M messages) — streamed only, never inflated to disk (17 GiB free).
3. **ITCH semantics were read from the spec, not assumed.** `U` (Replace) is "cancel-replaced with a
   NEW reference number" — it always loses priority (spec 1.4.5); priority-keeping reductions arrive as
   `X` on the same reference. `E` and `C` are identical for book state (Printable only affects tape);
   `P`, `Q`, `B` never touch the book. The plan's "replace preserves priority when qty decreases" is
   Databento's `M` rule, which is v0.2 MBO-apply, not ITCH.
4. **The bounded array is not what any reference book does** (nanolob: hash map of levels + intrusive
   sorted list; senzenn: BTreeMap + DashMap + O(levels) cancels; Hoeppke: L2 SIMD toy) — so it is
   justified on its own numbers, against a BTreeMap reference book on the same stream. Layout: 16-B
   level `{head u32, tail u32, qty u32, count u32}` indexed by tick offset from a per-locate base; a
   two-level u64 bitmap gives best bid/ask by msb/ctz with no scan on depletion; a 24-B order slot
   `{id u64, qty u32, px i32 (sign = side), prev u32, next u32}` in a slab with a u32 free list;
   `FxHashMap<u64, u32>` id → slot; an overflow `BTreeMap` for off-grid / out-of-range prices (0.9 % of
   pre-open adds are $0.01 / $199,999 placeholders, so the overflow map is mandatory for correctness;
   speed never depends on the window). Flat arrays only for a watchlist of locates; every other locate
   runs the reference book (a full-market 4,096-tick window is 1.17 GB for both sides; 65,536 ticks is
   9.3 GB — impossible on 8 GB). No re-centring in v0.1; overflow hits and max |offset| are published
   so v0.2 can decide.
5. **Cache-line arithmetic is written for both 64 B (x86) and 128 B (Apple M1: `hw.cachelinesize`
   = 128, verified)**; `hw.l2cachesize` reports the E-cluster 4 MiB — the preflight prints
   `hw.perflevel0.l2cachesize` (12 MiB). No thread affinity on Apple Silicon (KERN_NOT_SUPPORTED); the
   harness uses QoS, a load gate, K-batch timing (Instant tick 41.667 ns), an empty-loop overhead row and
   5 interleaved fresh-process runs reported as median with min–max. cargo-flamegraph on macOS needs
   xctrace (full Xcode, absent here); samply or perf on Linux CI instead.
6. **Correctness is a differential test, the nanolob pattern**: a BTreeMap reference book with the
   identical API; identical event suffix after EVERY op and identical snapshot every 64 ops over
   proptest sequences (in-range and overflow-heavy strategies), plus invariants I1–I12 below. Python
   reference values (sha256 of canonical event logs for seeds 1–3, the six-order matching fixture,
   bitmap states, queue-position models) were computed independently during research and are the
   test oracles. `bid < ask` is a replay STATISTIC pre-open (the real prefix is crossed before the `Q`
   event) and a hard invariant only in the matcher (v0.2).
7. **"Zero-copy" is not claimed** (field decode copies); the claim is "no per-message heap allocation,
   enforced by a counting global allocator test".
8. **Python API is batch-first**: `replay_itch(...)` returns numpy structured arrays computed under
   `Python::detach`; the per-event iterator is secondary; both msg/s are printed side by side, so the
   Python overhead is a number, not a caveat.
9. **Threading model section is honest**: "single writer, and the SPSC pipeline I did not build"; an
   rtrb two-stage experiment is a v0.3 optional row reported even if slower.
10. **Siblings**: v0.3 integrates quotesim only (its v0.2 `book.py` consumes `Book`, `l2`, `queue_ahead`);
    tcakit consumes the fill log through its existing frames; deskboard is deferred.
11. **Toolchain (installed 2026-09-15)**: rust 1.98.1 stable via rustup (`~/.cargo/bin`, PATH not
    modified — `export PATH=$HOME/.cargo/bin:$PATH`), clippy, rustfmt. Pinned crates (crates.io, verified
    today): criterion 0.8.2, hdrhistogram 7.6.0, proptest 1.11.0, pyo3 0.29.2, numpy 0.29.0, maturin
    1.15.0, dbn 0.69.0, flate2 1.1.10 (`zlib-rs` feature — zlib-ng needs cmake, absent), rtrb 0.4.0,
    slab 0.4.12, rustc-hash. `.cargo/config.toml` sets `target-cpu=apple-m1` for aarch64-apple-darwin
    only and every table states it.

## Cross-check resolutions (verbatim)

- LOBSTER: core keeps a 'LOBSTER path' (L2 orderbook-file replay + reconstruction-vs-snapshot harness, tickq dialect) while data (terms v2026-08-14 cl. 5.1a/b/f/g verified in the site bundle) and critic say drop it. RESOLVE: zero LOBSTER strings in lobcore; tickq's synthetic layout is a file format, not vendor Data, but it is L1/L2 (fresh id per quote, level-size rows; core's replay showed 1,994 phantom orders + a crossed book) so it is useless for L3 and out of v0.1 anyway.
- Order slot size: core 32 B {id,prev,next,qty,price i32,side,gen,pad} vs bench 24 B {id u64,qty u32,price u32,prev,next} 'asserted by unit test'. RESOLVE: 24 B = {id u64, qty u32, px i32 (ITCH 1e-4 units, max 2,000,000,000 < 2^31, sign = side), prev u32, next u32}; generation counter only under cfg(debug_assertions). Side lives in the sign, not in a byte.
- Level record: core 16 B {head,tail,qty u32,count u32} vs bench 24 B with qty u64. RESOLVE: 16 B with u32 qty + checked_add (a level summing >4.29e9 shares is a data error worth surfacing, not silently widened); memory is the binding constraint on 8 GB.
- Full-market array memory: data lens '4,096 slots x 16 B x 8,906 = 570 MB' counts ONE side; two sides = 1.17 GB (core's 1.1 GiB is right). RESOLVE: flat arrays only for a watchlist (default: locates named on the CLI); every other locate uses the BTreeMap reference book, which is also the tree comparator for the bounded-vs-tree table.
- Flamegraph tooling: bench (sec 5 and 6) and critic say cargo-flamegraph on macOS needs dtrace/sudo; data lens says xctrace/Xcode. VERIFIED flamegraph 0.6.14 src/lib.rs: #[cfg(target_os="macos")] use inferno::collapse::xctrace, error text 'could not spawn xctrace'. Data lens is right; no Xcode.app here (CommandLineTools only) so cargo-flamegraph cannot run; samply 0.13.1 (2025-02-01, unverified on macOS 26) or perf on ubuntu CI.
- Full-day sizes: bench '12302019 ~9.1 GB / ~305 M msgs; 01302020 ~14.4 GB / ~485 M' vs data lens 8.25 GB / 278 M and 12.95 GB; core says 01302020 = 423,285,709 msgs. VERIFIED gzip ISIZE trailers by range GET: 12302019 -> 3,956,440,613 mod 2^32 -> 8,251,407,909 B (k=1, ratio 2.34); 01302020 -> 67,148,866 -> 12,952,050,754 B (k=3, ratio 2.31). Bench's extrapolation from the pre-open ratio 2.57 is wrong; core's 423 M is consistent with 12.95 GB at 30.6 B/msg (regular-session mix is heavier than the 29.66 B/msg pre-open average), so 278 M for 12302019 is +-5%, not a count.
- 50 MB head oracle: data lens 4,330,712 msgs / 128,451,546 B vs bench 4,330,679 msgs / 128,450,560 B. VERIFIED locally: md5 of the 52,428,800-byte head = 8bd91e6f5b4a31d4d50dd6ac8a8fe7e2 and gzip -dc yields exactly 128,450,560 B; data lens used a slightly longer range. Pin the oracle to the 52,428,800-byte head: 128,450,560 B, 4,330,679 complete messages (last partial message discarded), type histogram to be regenerated by lobcore itself.
- ITCH message mix: critic '55-60% adds, ~35% deletes, <10% executions' (unmeasured) vs data lens measured pre-open A 39.9 / D 37.3 / X 11.0 / L 5.0 / U 4.8 / F 0.9 / E 0.4. Neither is the regular-session mix (no C/Q/B before 09:30). RESOLVE: the synthetic generator's default mix is a parameter filled from `lobcore replay --stats` on a regular-session slice, and DESIGN.md quotes both hours separately.
- Self-match prevention: core built and hashed 4 STP modes; critic cuts to one ('reject incoming'). RESOLVE: lob-match is v0.2; ship cancel_newest (= critic's reject incoming) and cancel_oldest (fixtures + sha256 already exist, ~40 lines); cancel_both and FOK-under-STP semantics stay optional and are stated as venue choices.
- Databento MBO: core specifies full apply semantics (R clear, F_SNAPSHOT/F_LAST, F_TOB, M-on-unknown = add) and data lens adds a DBN-v3 MBO writer twin in v0.1; critic reduces to a stub-level record decoder. RESOLVE: v0.1 = 56-byte LE MboMsg decoder with unaligned loads + round-trip on the two Apache-2.0 stubs (format test only); book-apply rules v0.2; DBN writer deferred until Databento's normalisation of ITCH 'U' is verified (data lens flags it unknown; the stubs do not cover it).
- Bounded window: critic 'must be RELATIVE with an explicit re-centering policy'; core 'centred on first in-session add, overflow BTreeMap, no rebase in v0.1'. Both are relative to a per-locate base; the disagreement is re-centring. RESOLVE: v0.1 no re-centring; publish overflow hits and max |offset| per locate-day; re-centring is a v0.2 decision made from those numbers (data lens: 0.9% of pre-open adds are $0.01/$199,999 placeholders that no window catches, so the overflow map is mandatory either way).
- Threading: bench 'stay single-threaded in v0.2, state the single-writer principle' vs critic 'implement decode->SPSC->book with measured handoff OR title the section none'. RESOLVE: v0.1-v0.2 single writer; DESIGN.md section 4 titled 'Threading model: single writer, and the SPSC pipeline I did not build'; an rtrb 0.4.0 two-stage experiment is a v0.3 optional row reported even if slower.
- Sibling integration: plan says tcakit + quotesim + deskboard; critic says one (quotesim v0.2 'book.py L2/FIFO queue with a latency parameter' - verified in quotesim/README.md Roadmap). core/data/bench silent. RESOLVE: v0.3 integrates quotesim only; tcakit consumes lobcore's fill log through its existing orders/fills/market long-format frames (tcakit README 'Your own fills'); deskboard deferred.
- bid<ask: all four agree it fails pre-open; core scopes 'assert 0 only after System Event Q', critic '09:30-16:00 outside halts'. RESOLVE: crossed-count is a replay statistic always; the hard check applies only after 'Q' and only for locates whose last H state is 'T'; in lob-match it is a proptest invariant.
- Python API: plan 'replay(file) -> iterator of snapshots/events'; bench + critic 'batch numpy primary'. RESOLVE: batch (every_n | every_ns) numpy structured arrays computed under Python::detach is the primary API (v0.2); per-event iterator secondary; both msg/s numbers printed side by side.
- Free disk: data 20 GiB, critic/bench 18 GB, df now 17 GiB. rustup default profile + target dirs (~6 GB) + one 3.5 GB gz leaves ~7 GB; never download two days; never inflate a day to disk.
- gzip backend: bench says flate2 'zlib-ng', data lens says 'zlib-rs'. zlib-ng needs cmake, which the critic measured ABSENT on this Mac. RESOLVE: flate2 = { features = ["zlib-rs"] } (pure Rust).
- Cache reasoning: core says slot '32 B fits 2 per cache line' and cites nanolob's 64-B padding; bench/sysctl: hw.cachelinesize = 128 on M1 (verified), perflevel0 L2 = 12 MiB (hw.l2cachesize reports the 4 MiB E-cluster value - the preflight must print hw.perflevel0.l2cachesize). Write layout arithmetic for both 64 B (x86) and 128 B (M1).
- Databento queue-position oracle: core's k=0 value 225 (p_front 0.75) uses others_size = post-cancel depth 400; with the pre-cancel 500 it is 240 (= proportional). oracles.py itself notes the ambiguity. Pin which definition Databento's example uses before porting, or drop the Databento estimator from the fixture.

## Contracts the build must follow (from the cross-check's "missing" list, verbatim)

- Cargo workspace: lobcore/Cargo.toml [workspace] members = crates/lob-core, crates/lob-feed, crates/lob-synth, crates/lob-features, crates/lob-bench (bin), python/lobcore (cdylib, maturin); rust-toolchain.toml channel = "1.98.1"; workspace.package edition 2024, license MIT, rust-version 1.89 (maturin MSRV is the max); [profile.release] lto = "fat", codegen-units = 1, panic = "abort"; [profile.bench] inherits release + debug = "line-tables-only" (samply symbols); RUSTFLAGS=-C target-cpu=apple-m1 only via .cargo/config.toml [target.aarch64-apple-darwin] and stated in every table; deny(unsafe_code) everywhere except one documented unaligned read in lob-feed; cargo-deny licence check (MIT/Apache-2.0/BSD-2 only).
- CI matrix (.github/workflows/ci.yml): jobs {rust: ubuntu-latest + macos-latest, dtolnay/rust-toolchain@1.98.1, Swatinem/rust-cache@v2, cargo fmt --check, cargo clippy --all-targets -D warnings, cargo test --workspace with PROPTEST_CASES=64, cargo bench --no-run}; {python: ubuntu + macos x py3.11/3.12, /opt/homebrew/bin/python3.12 locally, maturin build --release --features abi3-py311 via PyO3/maturin-action@v1.51.0, pip install the wheel, pytest -q python/tests}; {repro: regenerate the synthetic fixture and `lobcore replay --stats` on it, git diff --exit-code}. House rule: whole suite < 3 min; the 50 MB real head is NOT downloaded in CI (61 s at 0.82 MB/s, emi files vanish) - it is a #[ignore] test run locally with the md5 pin.
- Synthetic ITCH writer contract (lob-synth): fn synth_itch(seed: u64, n_msgs: u64, cfg: SynthConfig{locates: u16, mix: [f32; 9] over A/F/D/X/U/E/C/P/L, placeholder_rate (default 0.01), unknown_ref_rate (default 0), subpenny (bool), crossed_preopen (bool), open_ns, close_ns}) -> SynthDay{bytes: Vec<u8> (emi 2-byte BE length framing, S:O/S/Q/M/E/C, R per locate, one H per locate), truth: Vec<Truth{ts u48, locate, best_bid, best_ask, l1..l5 qty per side, live_orders u32, book_hash [u8;32]}> written at every message}. Guarantees: same (seed, cfg) -> identical bytes (sha256 pinned in tests for seed 7, n 100_000); refs strictly increasing u64; ts monotone; X/D/E/U only on live refs unless unknown_ref_rate > 0; U removes old ref and appends new ref at the tail; E consumes from the level head with match numbers; sub-penny only when base < $1. Fixture committed: one seed-7 100k-message file (~3 MB) + its truth sidecar hash; larger days generated at test/bench start. Exposed as lobcore.synth_itch(seed, n) -> bytes.
- Event-log hash contract (docs/DESIGN.md sec 8, tested in Rust and Python): record = 34 bytes LE: u8 type | u64 a | u64 b | i64 price (1e-4 units) | u64 qty | u8 side; type codes add=1 cancel=2 exec=3 delete=4 replace=5 modify=6 unknown=7 (b = kind) reject=8 trade=9 stp_cancel=10 ioc_cancel=11 fok_reject=12; 'rest' hashes as add; accept not hashed; hash = sha256 over records in emission order (sha256, not blake3, so hashlib reproduces it); a leading 1-byte format version (=1) is hashed first so the contract can change. Book-state hash (separate): sha256 over per side (bid then ask), ascending price, (i64 price, u32 count, then (u64 id, u32 qty) in FIFO order) - this is the `replay --stats` close hash and the Rust-vs-Python-vs-reference equality key. Expected values from core's oracles_out.json (six-order fixture c6207d5f...; seeds 1-3) become tests fed by a dumped ops.bin, never by porting Python's RNG.
- Exact proptest invariant list (lob-core, every 64 ops and at end): I1 level.qty == sum(order.qty in FIFO); I2 level.count == FIFO length; I3 tail reachable from head and prev/next consistent; I4 order's (side,|px|) equals the level it sits in; I5 order.qty > 0; I6 bitmap bit == (count>0), summary bit == (word != 0); I7 live slots + free slots == capacity; I8 per-side sum(levels) == sum(live orders); I9 unknown-id cancel/delete/execute/replace returns Err + emits ('unknown', kind, id) with zero state change (state hash before == after); I10 over-execute (qty > remaining) rejects with no change; I11 array-book snapshot == BTreeMap reference snapshot (Vec<(side, price, [(id,qty)])>) and event suffix == reference suffix after EVERY op; I12 in-range and overflow-heavy strategies both pass (window 64 ticks, drift +-40). bid<ask is NOT here; it is a lob-match invariant (v0.2) plus a replay statistic. Strategy: proptest::collection::vec(op, 1..5000), Op = Add{side, offset 0..40, qty 1..100} | Cancel{pick,qty 1..60} | Execute{pick,qty} | Delete{pick} | Replace{pick,offset,qty}; interpreter resolves pick -> live id (5% unknown), price = opposite_best -/+ 1 - offset, mid drift every 500 ops.
- Book-state and event hashes must also be produced by the Python reference model (model.py) so the cross-implementation equality the critic asks for is testable: ship python/tests/test_hash_parity.py replaying the committed synthetic fixture through lobcore and through a 100-line pure-Python BTree-free reference (dict+deque).
- PyO3 API surface the siblings need (python/lobcore, abi3-py311, .pyi via pyo3-stub-gen): lobcore.Book(base_px, n_levels) with add(id, side, px, qty) / cancel(id, qty) / delete(id) / execute(id, qty) / replace(old, new, px, qty) -> event tuple, l1(), l2(depth) -> numpy (px,qty,count) per side, queue_ahead(id) -> int (exact, from the FIFO), state_hash(); lobcore.replay_itch(path, symbols|locates, every_n=None, every_ns=None, features=True) -> dict of numpy arrays {ts, locate, bid_px, bid_qty, ask_px, ask_qty, depth5, wmid, imb1, imb5, spread, ofi}; lobcore.replay_stats(path, symbols=None) -> dict (the README headline block); lobcore.synth_itch(seed, n, **cfg) -> bytes; v0.2 adds Matcher, Sim (fills as a long-format frame with columns order_id, ts, px, qty, side, liquidity_flag matching tcakit's fills schema) and the per-event iterator. quotesim v0.2 book.py consumes Book + l2 + queue_ahead with a latency parameter; deskboard consumes nothing in v0.1-v0.3.
- lob-feed details none specified: ITCH parser reads u16 BE length then dispatches on byte[0] with the size table (S12 R39 H25 Y20 L26 V35 W12 K28 J35 h21 A36 F40 E31 C36 X23 D19 U35 P44 Q40 B19 I50 N20 O48); length != table -> ParseError::Length{type, got} and stop (do not resync); unknown type -> skip by length + counter; u48 timestamp read as 6 BE bytes; the truncated final message of the head file is discarded and counted; streaming gz via flate2/zlib-rs with a 4 MB ring buffer; locate -> Book via Vec<Option<Box<Book>>> sized from R messages; H state per locate; a counting #[global_allocator] test asserts zero allocations across 1e6 messages after warm-up.
- Replay statistics contract (`lobcore replay --stats`): messages by type and by hour, msgs/s excluding and including gunzip (both labelled), live-order high-water mark (global and per locate), overflow-map hits and max |tick offset| per watchlist locate, crossed-snapshot count before and after 'Q', unknown-id count (must be 0 on a from-message-1 replay), E/C-at-level-head fraction, negative-qty attempts (must be 0), close book-state hash per watchlist locate, event-log hash. This block is the README headline, regenerated in CI on the synthetic fixture and locally on the real head.
- Data policy files: .gitignore data/; scripts/fetch_itch.sh (curl -r 0-52428799, md5 pin 8bd91e6f5b4a31d4d50dd6ac8a8fe7e2, size 52,428,800); NOTICE for the two Apache-2.0 dbn stubs with repo + commit hash; README 'Data and privacy' section: Nasdaq sample is downloadable but not redistributable, only aggregates published.
- Bench harness spec (v0.3 but decided now so v0.1 code is measurable): bench/hdr.rs times K=64-message batches with Instant::now (tick 41.667 ns, tbfrequency 24 MHz), records batch_ns/K into hdrhistogram(1..1e9, 3 sigfigs), prints p50/p90/p99/p99.9/max + an empty-loop row; preflight prints M1 4P+4E, 8 GB, macOS 26.0.1, rustc, RUSTFLAGS, hw.cachelinesize 128, hw.perflevel0.l2cachesize, getloadavg, effective-GHz probe before/after; refuses to persist if 1-min load > 1.0; QOS_CLASS_USER_INTERACTIVE via libc; 5 fresh-process runs interleaved A/B, median with min-max. criterion 0.8.2 with Throughput::Elements and --save-baseline for slopes only.
- Site publication preconditions (data lens): ~/Downloads/charlieyan-site is not a git repo and has two deploy configs (wrangler + vercel.json); add src/content/projects/lobcore.md + src/content/writing/<slug>.md, run node scripts/og.mjs; gate on Linux CI green and regenerated numbers.
- Rust install step for the orchestrator: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile default (~116 MB, 1.1-1.4 GB installed; writes ~/.zshenv); then cargo install --locked samply; python venv at scratchpad/plan7/venv (already exists with numpy) + pip install maturin==1.15.0 pytest.

## v0.1 scope (reconciled)

lobcore v0.1 (1 week) - Rust workspace, MIT, rust 1.98.1, no LOBSTER, no matcher, no PyO3 beyond the read-only surface.

1. crates/lob-core
   1.1 types.rs: Side, Price (i32, 1e-4 units, sign = side inside slots), OrderId u64, Qty u32, Event enum + 34-byte LE encoder, EventKind codes 1-8.
   1.2 slab.rs: Vec<Slot{id u64, qty u32, px i32, prev u32, next u32}> (24 B, static_assert via size_of test) + u32 free list; FxHashMap<u64,u32> id->slot.
   1.3 levels.rs: per-side Vec<Level{head u32, tail u32, qty u32, count u32}> (16 B) indexed by (px - base)/tick; two-level bitmap (u64 words + u64 summary; n <= 4096) with best via msb/ctz; overflow BTreeMap<i32, Level> for off-grid/out-of-range; counters {overflow_hits, max_abs_offset}.
   1.4 book.rs: Book::new(base_px, n_levels, tick); add / cancel(partial, in place) / delete / execute(by id) / replace(delete + tail add) -> Result<Event, Unknown{kind,id}>; l1(), l2(depth), queue_ahead(id), state_hash(), check() (I1-I8).
   1.5 reference.rs: RefBook = BTreeMap<i32, VecDeque<(u64,u32)>> per side + HashMap<u64,(Side,i32)>; identical API; also the tree comparator for benches.
   Tests: unit oracles from oracles_out.json (FIFO [1,2,3] -> X(1,50) qty 550 -> U(1->9) [2,3,9]; unknown cancel(777) no-op; bitmap {5,130,1000}->words{0,2,15}, summary 0b1000000000000101; delete 1000 -> best 130); proptest I1-I12 vs RefBook (event suffix every op, snapshot every 64 ops), in-range and overflow-heavy strategies, PROPTEST_CASES=64 in CI, 1000 locally; alloc-counter test.

2. crates/lob-feed
   2.1 itch/frame.rs: 2-byte BE length framing over a Read (streaming gz via flate2 zlib-rs, 4 MB ring) and over &[u8]; truncated tail discarded + counted.
   2.2 itch/msg.rs: borrowed views for S R H A F E C X D U P Q B (offsets per spec; size table for all 23 types); u48 timestamp; unknown type skipped by length.
   2.3 itch/apply.rs: locate -> Book; A/F add; E and C reduce by id (Printable ignored for state); X in-place; D delete; U delete + tail add carrying side from the original; P/Q/B no-op; S event codes and H state per locate; symbol->locate map from R; watchlist gets ArrayBook, others RefBook.
   2.4 mbo/record.rs: 56-byte LE MboMsg decoder (unaligned reads), DBN header (magic "DBN", version 3, u32 metadata len) - format test only.
   2.5 stats.rs: the replay-statistics contract (type x hour histogram, msgs/s with and without gunzip, live HWM, overflow hits, crossed before/after Q, unknown-id = 0, negative-qty = 0, E/C-at-head fraction, close hashes).
   Tests: hand-decoded bytes for each type (A hex; first 40 bytes of the real head = 000c 'S' 'O' ts 10,953,404,452,051); length-table test on the synthetic day (0 mismatches); dbn stub round-trip (XNAS.ITCH NVDA 4 x 'A', GLBX 2 x 'C'); #[ignore] real-head test: md5 8bd91e6f..., 128,450,560 B, 4,330,679 msgs, unknown-id 0, negative-qty 0, histogram pinned after first run.

3. crates/lob-synth
   synth_itch(seed, n, SynthConfig) -> SynthDay{bytes, truth}; rand_chacha; internal RefBook truth; mix parameterised (default from pre-open histogram until a regular-session slice is measured); placeholder adds 1%, unknown-ref knob, sub-penny knob, crossed pre-open then 'Q'.
   Tests: determinism (seed 7, 100k msgs -> pinned sha256); parser round-trip field-exact; lob-feed replay == truth after every message for seeds 1-3; committed fixture tests/fixtures/synth_s7_100k.itch (~3 MB) with sidecar hash.

4. crates/lob-features
   wmid (integer num/den, f64 only at the boundary), imb1, imb5, signed imbalance, spread, depth5, CKS OFI e_n / windowed sum.
   Tests: 3-snapshot fixture (wmid 10000.75 / 10000.8333 / 10001.1111; e_1 +200, e_2 +150, OFI 350); L5 fixture I5 0.538462; identity wmid = mid + (I - 1/2) spread.

5. crates/lob-bench (bin `lobcore`)
   `lobcore replay --stats FILE [--symbol ...] [--array-window N]`; `lobcore synth --seed --n --out`; `lobcore bench-quick` (criterion-free: msgs/s on the in-memory synthetic day and on the inflated real head, bounded vs RefBook on the same stream, preflight block printed). criterion + hdrhistogram harnesses are v0.3.

6. python/lobcore (maturin, pyo3 0.29.2 abi3-py311, numpy 0.29.0) - read-only surface only: Book (add/cancel/delete/execute/replace/l1/l2/queue_ahead/state_hash), replay_itch(..) -> dict of numpy arrays under Python::detach, replay_stats, synth_itch; .pyi stubs; python/tests: hash parity with the pure-Python reference on the committed fixture; l2 equality on 3 snapshots.

7. Repo hygiene: README per CONVENTIONS (badges ci / python 3.11|3.12 / MIT; 'Run it'; 'Design rules' Tested vs By construction; 'What is where'; 'Roadmap' v0.2 matcher+sim+PyO3 iterator, v0.3 benches+LATENCY.md+quotesim adapter; 'Data and privacy'; 'Companion repos'); CHANGELOG; NOTICE (dbn stubs); scripts/fetch_itch.sh; .gitignore data/; ci.yml as specified under missing.

8. v0.1 numbers (all with basis labels, no single-run quotes): (a) parse+apply msgs/s on the in-memory synthetic day (CI-reproducible, machine named); (b) parse+apply msgs/s on the inflated 128,450,560-byte real head (local, md5-pinned) and wall time including gunzip labelled gzip-bound; (c) bounded-array vs RefBook ratio on the same stream; (d) overflow-hit share and max offset per watchlist locate; (e) unknown-id = 0, negative-qty = 0, crossed-count pre-open. Full streamed 8.25 GB day = optional appendix row.

docs/DESIGN.md sections (fixed):
 0. Decision this serves (purpose box) + abstract (what, the one design choice, the number with basis, what it is not)
 1. Why another order book: positioning table (nanolob 33 ns Intel Core Ultra; limitbook; order-book-rs M3; OrderBook-rs M4 Max 917 ns p50; rymnc; liquibook; itchy-rust 20 M/s) with machine and method columns
 2. Data structures and memory layout: 24-B slot, 16-B level, two-level bitmap, overflow BTreeMap, slab + FxHashMap; Figure 1 byte/cache-line diagram drawn for 64 B and 128 B lines; why none of the three reference repos do this; the 9.3 GB failure mode of a 65,536-tick full-market window
 3. Message path: ITCH framing and offsets, U = delete + tail add (spec 1.4.5), X in place, E and C identical for state, P/Q/B ignored; MBO record layout and the v0.2 apply rules; what 'no per-message allocation' means and how it is tested
 4. Threading model: single writer, and the SPSC pipeline I did not build
 5. Correctness: reference model, proptest invariants I1-I12, differential pattern (nanolob), crossed pre-open as data not bug, event-log and book-state hash contracts (34-byte record, type codes, sha256, version byte)
 6. Measurement methodology (summary; full protocol in LATENCY.md): 41.667 ns tick, K-batch granularity, no affinity on Apple Silicon (KERN_NOT_SUPPORTED), QoS, load gate, 5 interleaved runs, overhead row, gzip-bound caveat
 7. Results: Table 1 replay stats block; Table 2 bounded vs tree on the same stream; Figure 2 latency histogram (v0.3); Figure 3 throughput vs window width / overflow share; profile (samply) in v0.3
 8. What I left out: kernel bypass (Onload/ef_vi/DPDK), FPGA parsing, A/B feed arbitration and gap recovery, isolcpus/IRQ pinning/busy-poll, hugepages, SPSC rings, hardware timestamps/PTP, cache warming, colocation, exchange conformance, re-centring, MBO replay, matcher (v0.2)
 9. Measurement pitfalls, logged verbatim from build sessions (wrong number -> corrected number), never composed
 10. Reproduce: exact commands, machine block, fixture hashes, fetch script, what is CI vs local
 11. References (specs, papers, repos with commit hashes)

## Later
- v0.2: lob-match (price-time, limit/market/IOC/FOK, STP cancel_newest + cancel_oldest with the fixture
  hashes above), lob-sim (entry / response / feed latency constant + jitter; queue-position models
  risk-adverse / proportional / power-k with the research oracles), MBO apply rules, PyO3 Matcher/Sim and
  the per-event iterator, re-centring decision from the v0.1 overflow numbers.
- v0.3: criterion + hdrhistogram harness, LATENCY.md with the protocol above, samply profile, the rtrb
  SPSC row, quotesim adapter, the write-up "A bounded-array order book in Rust: design, measurements,
  and what I left out" (repo + personal site, gated on Linux CI green and regenerated numbers).

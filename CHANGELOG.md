# Changelog

## 0.1.0 — 2026-09-21

First release: the read-only book, replay, synthetic day, features, CLI and Python surface.

- `lob-core`: `ArrayBook` (flat 16-B levels indexed by tick offset, two-level u64 bitmap best
  price, 24-B slab slots with intrusive FIFO links, `FxHashMap` id map, `BTreeMap` overflow for
  off-grid / out-of-window prices), `RefBook` (`BTreeMap` + `VecDeque`) with the identical
  `OrderBook` trait and error precedence, the 34-byte event record and sha256 `EventLog`, the
  book-state hash, the `ops.bin` op-sequence format with a seed-7 fixture pinned by a
  stdlib-only Python reference. Differential proptest I1-I12 over three strategies, oracle
  tests, and a counting-allocator test (0 allocations across 100,000 in-range ops).
- `lob-feed`: ITCH 5.0 framing over `Read` (gzip via flate2/zlib-rs, 4 MB compacting buffer)
  and over slices, borrowed views for S R H A F E C X D U P Q B with the 23-type size table,
  `Session` (one book per locate, array books for the watchlist, spec semantics: X in place,
  U = delete + tail add with the side carried, E/C by id, P/Q/B no-ops), the replay-statistics
  block with `--write-readme` / `--check-readme`, the DBN v3 header + 56-byte MBO record
  decoder on the two vendored Apache-2.0 stubs, and an `#[ignore]` md5-pinned real-prefix
  test (4,330,712 messages, unknown-id 0, negative-qty 0).
- `lob-synth`: seeded synthetic ITCH day with a per-message truth sidecar (best, L5, live,
  book hash); the committed 100,000-message seed-7 fixture (sha256 `20cc1acc...`) and its
  truth sha256; placeholder, stale-reference, sub-penny and crossed-pre-open knobs.
- `lob-features`: weighted mid as an exact rational, L1 / L5 imbalance, spread, depth5, CKS
  OFI step and window, with the 3-snapshot and L5 fixtures and the wmid identity.
- `lob-bench` (`lobcore` binary): `replay --stats`, `synth`, `bench-quick` (5 fresh
  interleaved processes, median with min-max, preflight with the M1 timer tick / cache line /
  load average, load gate on `--out`).
- `python/lobcore` (PyO3 0.29.2, abi3-py311, numpy 0.29): `Book`, `RefBook`, `replay_itch`
  (numpy batches under `Python::detach`), `replay_stats`, `synth_itch` (bytes, optional truth
  arrays), `event_encode`, `event_log_hash`, `sha256`, `UnknownId` / `BookReject` exceptions
  carrying the Rust text and the `.event` record; hand-written `lobcore.pyi` kept in sync by a
  test.
- Docs: README with the CI-checked replay-statistics block, the local real-prefix block and
  the bench-quick table; docs/DESIGN.md (12 sections, including the measurement pitfalls hit
  in this build); NOTICE for the DBN stubs and the dependency licences.

Not in 0.1.0: matcher, simulator, MBO apply rules, per-event Python iterator, window
re-centring (v0.2); criterion / hdrhistogram harness, LATENCY.md, samply profile, rtrb SPSC
row, quotesim adapter (v0.3); the cargo-deny licence check the plan listed (NOTICE records the
dependency licences by hand).

### Found by the pre-release verification pass

Each fix landed with a test that failed before the change:

- `lobcore.synth_itch(..., truth=True)` returned `book_hash` as a numpy `S32` array, and numpy
  strips trailing NUL bytes from `S` elements: 420 of the fixture's 100,000 digests (every one
  ending in 0x00) came back 31 bytes long and unequal to the true hash. Now an `(n, 32)` uint8
  array (`row.tobytes()` is the digest); the truth dict also carries `l5_bid` / `l5_ask`
  (`(n, 5)` uint32) as the Rust `Truth` and the CLI CSV do. python/tests/test_synth.py checks
  every row against `Book` / `RefBook`.
- Every `synth_itch` keyword is validated (`SynthConfig::validate` / `check_args` in lob-synth):
  an all-zero, negative or NaN `mix`, a rate outside `[0, 1]`, `open_ns >= close_ns`, a
  `close_ns` past 48 bits, `locates = 0` or `n > 2^32` raise `ValueError` instead of reaching a
  Rust panic, which under the wheel's `panic = "abort"` killed the interpreter (SIGABRT). The
  CLI maps the same check to `lobcore: --... ` / exit 1 (`--locates 0` used to exit 134).
- `Session::apply_msg` returns whether the book changed; `replay_itch` emits rows and advances
  `every_n` / `every_ns` on book-changing messages only, as its docstring said (a rejected
  message used to emit a duplicate row and count toward the cadence).
- The E/C-at-head fraction counts executions the book applied (a rejected over-execute no
  longer inflates the denominator).
- A watched locate's array window is centred after the first *accepted* non-placeholder add
  or replace; a rejected message (duplicate id, unknown reference, bad qty) used to centre it
  and could move the published max |offset|. Hashes and the fixture / real-prefix blocks are
  unchanged (they have no rejected messages).
- `DbnError::Record { at }` names the offending record's offset (it reported the end of the
  record area).
- gzip is detected by the `1f 8b` magic, not the extension; every member is decoded; bytes
  after the last member that are not another member end the input and are counted in the
  "inflated bytes consumed" row (they used to fail the whole file, losing every message of an
  intact member); an empty `.gz` is an empty input, not a cut stream.
- Stats block: new row "adds at placeholder prices" (fixture 413 of 41,029 = 1.01 %, real
  prefix 15,232 of 1,768,698 = 0.86 %, replacing the untraceable 15,844 in DESIGN.md); the
  stdout timing rows name their denominators (`parse+apply X ms, read Y ms` / `wall Z ms`).
  `replay_stats` gains `placeholder_adds` and `trailing_bytes`.
- `bench-quick` prints the min / median / max of the per-run paired array / reference ratio.
- Tests: the ignored real-prefix test asserts every literal of the README block (histogram,
  8,906 locates, live HWM / close, crossed 21, E/C 17,789 / 17,789, event-log and all-locates
  hashes, the four watchlist rows); python/tests/test_stub.py compares parameter names and
  defaults with the extension's `__text_signature__`; crates/lob-feed/tests/session_semantics.rs
  covers rejected messages; the counting-allocator test also covers `RefBook::l1`.
- Docs: README (three badges, single-run rows moved out of the table, the ratio-robustness
  claim withdrawn, `preflight` is a module not a subcommand, crossed = `bid >= ask`);
  DESIGN.md (gzip share of wall is 15 %, not "gzip-bound"; `RefBook::l1` does not allocate and
  the tree baseline's real cost is the O(depth) `find`; add path touches the old tail slot;
  "never reallocates" not "never rehashes"; tick rule; section 8 says what each omitted
  technique buys; pitfalls 2 / 5 / 6 marked as session notes; CI description; criterion /
  hdrhistogram reserved, not pinned); CI runs `ruff check python` (repo-root `ruff.toml`) and
  no longer runs the vacuous `cargo bench --no-run`; `lobcore.pyi` names `EventKind`.

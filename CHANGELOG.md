# Changelog

## 0.1.0 — 2026-09-15

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
row, quotesim adapter (v0.3).

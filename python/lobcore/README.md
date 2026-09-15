# lobcore (Python bindings)

The read-only v0.1 surface of [lobcore](https://github.com/charlieyanhx/lobcore): `Book` /
`RefBook` (the bounded-array and reference L3 books with the identical API), `replay_itch`
(sampled L1 snapshots plus book features as numpy arrays, computed with the GIL released),
`replay_stats` (the replay-statistics contract as a dict), `synth_itch` (the seeded synthetic
ITCH 5.0 day as bytes) and the event-log hash helpers. Build from the repository root with
`cd python/lobcore && maturin develop --release`; the tests are in `python/tests`.

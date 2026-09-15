"""`lobcore.replay_itch` on the committed fixture: array shapes and dtypes, monotone timestamps,
the first snapshot equals the values a Book built from the same messages gives, the feature
identities, and the `every_n` / `every_ns` cadences against a Python count.
"""

from __future__ import annotations

from collections import defaultdict

import lobcore
import numpy as np
import pytest
from conftest import BOOK_TYPES, FIXTURE, apply_msg

COLUMNS = {
    "ts": np.uint64, "locate": np.uint16, "bid_px": np.int32, "bid_qty": np.uint32, "ask_px": np.int32,
    "ask_qty": np.uint32, "depth5_bid": np.uint64, "depth5_ask": np.uint64, "wmid": np.float64,
    "imb1": np.float64, "imb5": np.float64, "spread": np.int32, "ofi": np.int64,
}  # fmt: skip


@pytest.fixture(scope="module")
def rows():
    return lobcore.replay_itch(FIXTURE)


def test_columns_dtypes_lengths_and_monotone_ts(rows, fixture_messages):
    assert set(rows) == set(COLUMNS)
    n = len(rows["ts"])
    for k, dt in COLUMNS.items():
        assert rows[k].dtype == dt, k
        assert len(rows[k]) == n, k
    n_book = sum(1 for m in fixture_messages if m["type"] in BOOK_TYPES)
    assert n == n_book  # default cadence: one row per book-changing message
    assert np.all(np.diff(rows["ts"].astype(np.int64)) >= 0)
    assert set(np.unique(rows["locate"]).tolist()) == set(range(1, 9))


def test_first_snapshot_of_each_locate_equals_a_book_built_from_the_messages(rows, fixture_messages):
    seen: dict[int, int] = {}
    books: dict[int, lobcore.RefBook] = {}
    checked = 0
    for m in fixture_messages:
        if m["type"] not in BOOK_TYPES:
            continue
        loc = m["locate"]
        b = books.setdefault(loc, lobcore.RefBook())
        apply_msg(b, m)
        i = seen.get(loc, 0)
        if i < 3:  # compare the first three rows of every locate, row by row
            idx = np.flatnonzero(rows["locate"] == loc)[i]
            bid, ask = b.l1()
            assert rows["ts"][idx] == m["ts"]
            assert (rows["bid_px"][idx], rows["bid_qty"][idx]) == ((bid[0], bid[1]) if bid else (0, 0))
            assert (rows["ask_px"][idx], rows["ask_qty"][idx]) == ((ask[0], ask[1]) if ask else (0, 0))
            l2 = b.l2(5)
            assert rows["depth5_bid"][idx] == l2["bid"]["qty"].sum()
            assert rows["depth5_ask"][idx] == l2["ask"]["qty"].sum()
            checked += 1
        seen[loc] = i + 1
    assert checked == 24


def test_feature_identities(rows):
    both = (rows["bid_px"] > 0) & (rows["ask_px"] > 0)
    bp, bq = rows["bid_px"][both].astype(float), rows["bid_qty"][both].astype(float)
    ap, aq = rows["ask_px"][both].astype(float), rows["ask_qty"][both].astype(float)
    mid = (bp + ap) / 2
    spread = ap - bp
    imb1 = bq / (bq + aq)
    assert np.allclose(rows["spread"][both], spread)
    assert np.allclose(rows["imb1"][both], imb1)
    assert np.allclose(rows["wmid"][both], mid + (imb1 - 0.5) * spread, atol=1e-6)
    assert np.all(np.isnan(rows["wmid"][~both])) and np.all(rows["spread"][~both] == 0)
    d = rows["depth5_bid"] + rows["depth5_ask"]
    ok = d > 0
    assert np.allclose(rows["imb5"][ok], rows["depth5_bid"][ok] / d[ok])
    # OFI is the CKS step between consecutive rows of the same locate (0 when a side was missing)
    for loc in np.unique(rows["locate"]):
        r = np.flatnonzero(rows["locate"] == loc)
        assert rows["ofi"][r[0]] == 0
        for prev, cur in zip(r[:-1], r[1:], strict=True):
            if not (both[prev] and both[cur]):
                assert rows["ofi"][cur] == 0
                continue
            e = 0
            pb, pa = int(rows["bid_px"][prev]), int(rows["ask_px"][prev])
            cb, ca = int(rows["bid_px"][cur]), int(rows["ask_px"][cur])
            if cb >= pb:
                e += int(rows["bid_qty"][cur])
            if cb <= pb:
                e -= int(rows["bid_qty"][prev])
            if ca <= pa:
                e -= int(rows["ask_qty"][cur])
            if ca >= pa:
                e += int(rows["ask_qty"][prev])
            assert rows["ofi"][cur] == e
            if cur > r[0] + 2_000:  # enough of the locate checked
                break


def test_every_n_counts_book_messages_per_locate(fixture_messages):
    per_locate = defaultdict(int)
    for m in fixture_messages:
        if m["type"] in BOOK_TYPES:
            per_locate[m["locate"]] += 1
    r = lobcore.replay_itch(FIXTURE, every_n=100)
    for loc, n in per_locate.items():
        assert int((r["locate"] == loc).sum()) == n // 100, loc
    r1 = lobcore.replay_itch(FIXTURE, every_n=1)
    assert len(r1["ts"]) == sum(per_locate.values())


def test_every_ns_spaces_rows_per_locate(fixture_messages):
    gap = 60_000_000_000  # one minute
    r = lobcore.replay_itch(FIXTURE, every_ns=gap)
    for loc in np.unique(r["locate"]):
        ts = r["ts"][r["locate"] == loc].astype(np.int64)
        assert np.all(np.diff(ts) >= gap)
    # Python replica of the rule: emit the first book message, then whenever ts - last >= gap
    expected = 0
    last: dict[int, int] = {}
    for m in fixture_messages:
        if m["type"] not in BOOK_TYPES:
            continue
        loc = m["locate"]
        if loc not in last or m["ts"] - last[loc] >= gap:
            last[loc] = m["ts"]
            expected += 1
    assert len(r["ts"]) == expected
    assert 8 <= expected < len(fixture_messages) // 10


def test_symbol_and_locate_filters_use_the_array_book():
    r = lobcore.replay_itch(FIXTURE, symbols=["SYN0003"], every_n=10)
    assert set(np.unique(r["locate"]).tolist()) == {3}
    r2 = lobcore.replay_itch(FIXTURE, locates=[3], every_n=10)
    assert np.array_equal(r["ts"], r2["ts"]) and np.array_equal(r["wmid"], r2["wmid"], equal_nan=True)
    r3 = lobcore.replay_itch(FIXTURE, symbols=["SYN0003"], locates=[5], every_n=10, array_window=256)
    assert set(np.unique(r3["locate"]).tolist()) == {3, 5}
    full = lobcore.replay_itch(FIXTURE, every_n=10)
    # the array book and the reference book give the same snapshots for the same locate
    sel = full["locate"] == 3
    assert np.array_equal(full["ts"][sel], r["ts"]) and np.array_equal(full["bid_px"][sel], r["bid_px"])


def test_features_false_returns_only_the_l1_columns():
    r = lobcore.replay_itch(FIXTURE, features=False, every_n=50)
    assert set(r) == {"ts", "locate", "bid_px", "bid_qty", "ask_px", "ask_qty"}


def test_argument_errors(tmp_path):
    with pytest.raises(ValueError):
        lobcore.replay_itch(FIXTURE, every_n=10, every_ns=10)
    with pytest.raises(ValueError):
        lobcore.replay_itch(FIXTURE, every_n=0)
    with pytest.raises(ValueError):
        lobcore.replay_itch(FIXTURE, array_window=0)
    with pytest.raises(OSError):
        lobcore.replay_itch(tmp_path / "missing.itch")
    bad = tmp_path / "bad.itch"
    bad.write_bytes(b"\x00\x05AXXXX")  # an 'A' with the wrong length is a layout error
    with pytest.raises(OSError):
        lobcore.replay_itch(bad)


def test_gzip_input_gives_identical_rows(tmp_path, fixture_bytes):
    import gzip

    gz = tmp_path / "fixture.itch.gz"
    gz.write_bytes(gzip.compress(fixture_bytes, compresslevel=1))
    a = lobcore.replay_itch(FIXTURE, every_n=25)
    b = lobcore.replay_itch(gz, every_n=25)
    for k in a:
        assert np.array_equal(a[k], b[k], equal_nan=True), k

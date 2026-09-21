"""`lobcore.synth_itch`: the bytes equal the committed fixture, the truth sidecar is checked at
EVERY row (not only the last row per locate) against `lobcore.Book` / `lobcore.RefBook` driven
from the pure-Python framer, the hash column is a real (n, 32) uint8 array (a 32-byte digest
ends in 0x00 on 1 row in 256, which a numpy `S32` array would silently shorten), and every
invalid keyword raises ``ValueError`` instead of aborting the interpreter (the extension is
built with ``panic = "abort"``, so any Rust panic behind the boundary would be a SIGABRT).
"""

from __future__ import annotations

import hashlib
import math
import re

import lobcore
import numpy as np
import pytest
from conftest import BOOK_TYPES, apply_msg, decode, frames
from test_hash_parity import MigratingBook

EMPTY_HASH = hashlib.sha256(b"").digest()


@pytest.fixture(scope="module")
def day():
    return lobcore.synth_itch(7, 100_000, truth=True)


def test_bytes_equal_the_committed_fixture_and_the_bytes_only_call(day, fixture_bytes):
    bytes_, truth = day
    assert bytes_ == fixture_bytes
    assert lobcore.synth_itch(7, 100_000) == fixture_bytes
    assert set(truth) == {"ts", "locate", "bid_px", "bid_qty", "ask_px", "ask_qty", "l5_bid", "l5_ask", "live_orders", "book_hash"}  # fmt: skip
    assert truth["book_hash"].dtype == np.uint8 and truth["book_hash"].shape == (100_000, 32)
    assert truth["l5_bid"].dtype == np.uint32 and truth["l5_bid"].shape == (100_000, 5)
    assert truth["l5_ask"].dtype == np.uint32 and truth["l5_ask"].shape == (100_000, 5)
    for k, dt in [("ts", np.uint64), ("locate", np.uint16), ("bid_px", np.int32), ("bid_qty", np.uint32),
                  ("ask_px", np.int32), ("ask_qty", np.uint32), ("live_orders", np.uint32)]:  # fmt: skip
        assert truth[k].dtype == dt and truth[k].shape == (100_000,), k


def test_truth_matches_lobcore_books_at_every_row(day, fixture_bytes):
    _, truth = day
    hashes = truth["book_hash"]
    # every digest is 32 bytes; 1 in 256 ends in 0x00 and the fixture has hundreds of those
    assert all(len(hashes[i].tobytes()) == 32 for i in range(len(hashes)))
    ends_in_nul = int((hashes[:, 31] == 0).sum())
    assert 300 <= ends_in_nul <= 550, ends_in_nul
    books: dict[int, MigratingBook] = {}
    refs: dict[int, lobcore.RefBook] = {}
    checked = 0
    for i, payload in enumerate(frames(fixture_bytes)):
        m = decode(payload)
        loc = m["locate"]
        digest = hashes[i].tobytes()
        if m["type"] not in BOOK_TYPES:
            # system / directory / action / L / P rows: the book of that locate, or the empty book
            if loc in books:
                assert digest == books[loc].state_hash(), i
            else:
                assert digest == EMPTY_HASH, i
                assert truth["live_orders"][i] == 0 and truth["bid_px"][i] == 0 and truth["ask_px"][i] == 0
            continue
        b = books.setdefault(loc, MigratingBook())
        r = refs.setdefault(loc, lobcore.RefBook())
        assert apply_msg(b, m) == apply_msg(r, m)
        assert digest == b.state_hash() == r.state_hash(), i
        bid, ask = b.l1()
        assert (truth["bid_px"][i], truth["bid_qty"][i]) == ((bid[0], bid[1]) if bid else (0, 0)), i
        assert (truth["ask_px"][i], truth["ask_qty"][i]) == ((ask[0], ask[1]) if ask else (0, 0)), i
        assert int(truth["live_orders"][i]) == b.live_orders, i
        l2 = b.l2(5)
        for side, key in (("bid", "l5_bid"), ("ask", "l5_ask")):
            got = np.zeros(5, dtype=np.uint32)
            q = l2[side]["qty"]
            got[: len(q)] = q
            assert np.array_equal(truth[key][i], got), (i, key)
        checked += 1
    assert checked == 94_895  # A + F + E + X + D + U of the fixture
    assert set(books) == set(range(1, 9))


@pytest.mark.parametrize(
    "kw, text",
    [
        ({"locates": 0}, "locates must be at least 1"),
        ({"mix": [0.0] * 9}, "mix weights must not all be zero"),
        ({"mix": [-1.0] + [0.5] * 8}, "mix weights must be finite and >= 0"),
        ({"mix": [math.nan] * 9}, "mix weights must be finite and >= 0"),
        ({"mix": [0.5] * 8}, "mix must have 9 weights"),
        ({"placeholder_rate": 2.0}, "placeholder_rate must be in [0, 1], got 2"),
        ({"placeholder_rate": math.nan}, "placeholder_rate must be in [0, 1], got NaN"),
        ({"unknown_ref_rate": 1.5}, "unknown_ref_rate must be in [0, 1], got 1.5"),
        ({"unknown_ref_rate": -0.1}, "unknown_ref_rate must be in [0, 1], got -0.1"),
        ({"open_ns": 10, "close_ns": 10}, "open_ns (10) must precede close_ns (10)"),
        ({"close_ns": 1 << 48}, "must fit 48 bits"),
    ],
)
def test_invalid_config_raises_value_error(kw, text):
    with pytest.raises(ValueError, match=re.escape(text)):
        lobcore.synth_itch(1, 100, **kw)


def test_n_bounds_raise_value_error():
    with pytest.raises(ValueError, match=r"n_msgs must be at least 2 \* locates \+ 6 = 22, got 21"):
        lobcore.synth_itch(1, 21)
    with pytest.raises(ValueError, match="n_msgs must be at most 4294967296"):
        lobcore.synth_itch(1, 1 << 50)
    with pytest.raises(TypeError, match='unexpected keyword argument "seed_"'):
        lobcore.synth_itch(1, 100, seed_=1)
    # the minimum stream is the six system events, one R and one H per locate
    _, t = lobcore.synth_itch(1, 22, truth=True)
    assert len(t["ts"]) == 22 and int(t["live_orders"].sum()) == 0

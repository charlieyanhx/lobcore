"""The lob-core research oracles through the Python surface (report_core, ORACLES section):
FIFO priority under X and U, unknown-id no-op, over-execute reject, empty-side best, the
64-level overflow window, the L2 numpy layout, and the 3-snapshot L2 equality between the
bounded-array book and the reference book.
"""

from __future__ import annotations

import hashlib

import lobcore
import numpy as np
import pytest

L2_DTYPE = np.dtype([("px", "<i4"), ("qty", "<u4"), ("count", "<u4")])


def fifo_book(cls=lobcore.Book):
    """Level 10050 FIFO [1, 2, 3] with 300/100/200 shares, a 100-share bid at 10049 and an ask
    at 10051 (the research fixture)."""
    b = cls(10_000, 128, 1) if cls is lobcore.Book else cls()
    b.add(1, "B", 10_050, 300)
    b.add(2, "B", 10_050, 100)
    b.add(3, "B", 10_050, 200)
    b.add(4, "B", 10_049, 100)
    b.add(5, "S", 10_051, 50)
    return b


def fifo_ids(b, side, px):
    return [oid for s, lpx, fifo in b.snapshot() if s == side and lpx == px for oid, _ in fifo]


@pytest.mark.parametrize("cls", [lobcore.Book, lobcore.RefBook])
def test_partial_cancel_keeps_priority_and_replace_loses_it(cls):
    b = fifo_book(cls)
    assert b.cancel(1, 50) == ("cancel", 1, 250, 10_050, 50, 0)
    assert fifo_ids(b, 0, 10_050) == [1, 2, 3]
    assert b.l1()[0] == (10_050, 550, 3)
    assert b.replace(1, 9, 10_050, 250) == ("replace", 1, 9, 10_050, 250, 0)
    assert fifo_ids(b, 0, 10_050) == [2, 3, 9]
    assert b.queue_ahead(9) == 300 and b.queue_ahead(2) == 0 and b.queue_ahead(3) == 100
    assert b.queue_ahead(1) is None


@pytest.mark.parametrize("cls", [lobcore.Book, lobcore.RefBook])
def test_unknown_id_is_a_lookup_error_with_no_state_change(cls):
    b = fifo_book(cls)
    before = (b.state_hash(), b.live_orders, b.event_count)
    for call, kind_code in [(lambda: b.cancel(777, 5), 2), (lambda: b.delete(777), 4), (lambda: b.execute(777, 5), 3)]:
        with pytest.raises(LookupError) as ei:
            call()
        assert isinstance(ei.value, lobcore.UnknownId)
        assert "unknown order id 777" in str(ei.value)
        assert ei.value.event == ("unknown", 777, kind_code, 0, 0, 0)
    with pytest.raises(lobcore.UnknownId) as ei:
        b.replace(777, 8, 10_050, 1)
    assert ei.value.event == ("unknown", 777, 5, 0, 0, 0)
    assert b.l1()[0] == (10_050, 600, 3)
    assert b.l2(2)["bid"]["qty"].tolist() == [600, 100]
    assert b.live_orders == 5
    assert b.state_hash() == before[0]
    assert b.event_count == before[2] + 4  # rejections are logged, the book is unchanged


@pytest.mark.parametrize("cls", [lobcore.Book, lobcore.RefBook])
def test_over_execute_and_duplicate_id_reject_with_no_change(cls):
    b = fifo_book(cls)
    h = b.state_hash()
    with pytest.raises(ValueError) as ei:
        b.execute(3, 999)
    assert isinstance(ei.value, lobcore.BookReject)
    assert "have 200, want 999" in str(ei.value)
    assert ei.value.event == ("reject", 3, 3, 0, 999, 0)
    with pytest.raises(lobcore.BookReject) as ei:
        b.add(1, "S", 10_051, 1)
    assert ei.value.event == ("reject", 1, 1, 0, 0, 0)
    with pytest.raises(lobcore.BookReject):
        b.add(99, "B", 10_050, 0)
    with pytest.raises(lobcore.BookReject):
        b.add(99, "B", 0, 10)
    assert b.state_hash() == h and b.l1()[0] == (10_050, 600, 3)


@pytest.mark.parametrize("cls", [lobcore.Book, lobcore.RefBook])
def test_execute_reduces_by_id_and_removes_at_zero(cls):
    b = fifo_book(cls)
    assert b.execute(2, 100) == ("exec", 2, 0, 10_050, 100, 0)
    assert fifo_ids(b, 0, 10_050) == [1, 3]
    assert b.execute(1, 120) == ("exec", 1, 180, 10_050, 120, 0)
    assert b.l1()[0] == (10_050, 380, 2)
    assert b.cancel(3, 500) == ("cancel", 3, 0, 10_050, 200, 0)  # qty >= remaining removes
    assert b.delete(1) == ("delete", 1, 0, 10_050, 180, 0)
    assert b.live_orders == 2


@pytest.mark.parametrize("cls", [lobcore.Book, lobcore.RefBook])
def test_empty_bid_side_has_no_best(cls):
    b = fifo_book(cls)
    for oid in (1, 2, 3, 4):
        b.delete(oid)
    assert b.l1() == (None, (10_051, 50, 1))
    assert len(b.l2(5)["bid"]) == 0 and b.l2(5)["ask"]["px"].tolist() == [10_051]


def test_overflow_window_serves_best_from_the_map():
    b = lobcore.Book(10_000, 64, 1)
    b.add(1, "B", 10_030, 10)
    b.add(2, "S", 10_031, 10)
    b.add(3, "S", 10_999, 10)  # out of range -> overflow
    b.add(4, "B", 9_000, 10)  # below base -> overflow
    b.add(5, "B", 10_029, 10)
    assert b.l1() == ((10_030, 10, 1), (10_031, 10, 1))
    assert b.overflow_hits == 2 and b.max_abs_offset == 1_000
    b.check()
    for oid in (1, 2, 5):
        b.delete(oid)
    assert b.l1() == ((9_000, 10, 1), (10_999, 10, 1))
    assert b.queue_ahead(3) == 0
    b.check()


def test_constructor_validates_the_window():
    with pytest.raises(ValueError):
        lobcore.Book(10_000, 0)
    with pytest.raises(ValueError):
        lobcore.Book(10_000, lobcore.MAX_LEVELS + 1)
    with pytest.raises(ValueError):
        lobcore.Book(10_000, 16, 0)
    with pytest.raises(ValueError):
        lobcore.Book(0, 16)
    with pytest.raises(ValueError):
        lobcore.Book(2_000_000_000, 4096, 1_000_000)
    assert lobcore.Book(10_000, 16).config == (10_000, 16, 100)


def test_side_spellings_and_bad_side():
    b = lobcore.RefBook()
    assert b.add(1, "B", 100, 1)[5] == 0
    assert b.add(2, "bid", 100, 1)[5] == 0
    assert b.add(3, 0, 100, 1)[5] == 0
    assert b.add(4, "S", 101, 1)[5] == 1
    assert b.add(5, "ask", 101, 1)[5] == 1
    assert b.add(6, 1, 101, 1)[5] == 1
    with pytest.raises(ValueError):
        b.add(7, "X", 101, 1)
    with pytest.raises(ValueError):
        b.add(7, 2, 101, 1)


def test_level_total_beyond_u32_is_rejected_on_both_books():
    for b in (lobcore.Book(10_000, 16, 1), lobcore.RefBook()):
        b.add(1, "B", 10_005, 3_000_000_000)
        with pytest.raises(lobcore.BookReject):
            b.add(2, "B", 10_005, 3_000_000_000)
        assert b.l1()[0] == (10_005, 3_000_000_000, 1)
    with pytest.raises(OverflowError):
        lobcore.RefBook().add(1, "B", 10, 2**32)


def test_l2_is_a_structured_numpy_array_best_first():
    b = fifo_book()
    l2 = b.l2(3)
    assert l2["bid"].dtype == L2_DTYPE and l2["ask"].dtype == L2_DTYPE
    assert l2["bid"].tolist() == [(10_050, 600, 3), (10_049, 100, 1)]
    assert l2["ask"].tolist() == [(10_051, 50, 1)]
    assert len(b.l2(1)["bid"]) == 1 and len(b.l2(0)["bid"]) == 0


def test_l2_equal_on_the_three_feature_snapshots():
    """The lob-features 3-snapshot fixture (prices in cents as 1e-4 units): (10000,300 | 10001,100)
    -> (10000,500 | 10001,100) -> (10001,50 | 10002,400); both books agree level by level."""
    steps = [
        [(1, "B", 1_000_000, 300), (2, "S", 1_000_100, 100)],
        [(3, "B", 1_000_000, 200)],
        [(4, "B", 1_000_100, 50), (5, "S", 1_000_200, 400)],
    ]
    expected = [
        ([(1_000_000, 300, 1)], [(1_000_100, 100, 1)]),
        ([(1_000_000, 500, 2)], [(1_000_100, 100, 1)]),
        ([(1_000_100, 50, 1), (1_000_000, 500, 2)], [(1_000_100, 100, 1), (1_000_200, 400, 1)]),
    ]
    arr, ref = lobcore.Book(990_000, 2048, 100), lobcore.RefBook()
    for adds, (bid, ask) in zip(steps, expected, strict=True):
        for a in adds:
            assert arr.add(*a) == ref.add(*a)
        la, lr = arr.l2(5), ref.l2(5)
        assert la["bid"].tolist() == lr["bid"].tolist() == bid
        assert la["ask"].tolist() == lr["ask"].tolist() == ask
        assert arr.state_hash() == ref.state_hash()
        assert arr.snapshot() == ref.snapshot()
    # after the third snapshot the fixture is crossed (bid 10001 >= ask 10001): allowed here, a statistic in replay
    assert arr.l1()[0][0] == arr.l1()[1][0]


def test_hashes_are_32_byte_bytes_and_empty_book_is_sha256_of_nothing():
    b = lobcore.Book(10_000, 16)
    assert b.state_hash() == hashlib.sha256(b"").digest()
    assert b.add(1, "B", 10_000, 5) == ("add", 1, 0, 10_000, 5, 0)
    assert isinstance(b.state_hash(), bytes) and len(b.state_hash()) == 32
    assert isinstance(b.event_log_hash(), bytes) and len(b.event_log_hash()) == 32
    assert b.event_log_hash() == lobcore.event_log_hash([("add", 1, 0, 10_000, 5, 0)])

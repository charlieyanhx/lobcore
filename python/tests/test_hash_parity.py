"""Cross-implementation hash parity on the committed synthetic fixture.

Three implementations replay tests/fixtures/synth_s7_100k.itch:
1. `lobcore.replay_stats` (the Rust session, every locate on the reference book, then again
   with every locate on the bounded-array book);
2. `lobcore.Book` / `lobcore.RefBook` driven message by message from the pure-Python framer in
   conftest.py, migrating each locate to a 1,024-level array window at its first real add
   exactly as the Rust session does;
3. `PyRefBook` below: ~100 lines of dict + deque, no lobcore, with the event-record conventions
   of lob_core::types and the book-state hash of docs/DESIGN.md section 5 written with hashlib.

The close book-state hash per locate, the all-locates hash and the event-log hash must agree
across all three and with the generator's truth sidecar (`lobcore.synth_itch(..., truth=True)`).
"""

from __future__ import annotations

import hashlib
import struct
from collections import deque

import lobcore
from conftest import BOOK_TYPES, FIXTURE, apply_msg

KIND_CODE = {
    "add": 1, "cancel": 2, "exec": 3, "delete": 4, "replace": 5, "modify": 6,
    "unknown": 7, "reject": 8, "trade": 9, "stp_cancel": 10, "ioc_cancel": 11, "fok_reject": 12,
}  # fmt: skip
ARRAY_WINDOW = 1024


def encode_event(e: tuple) -> bytes:
    kind, a, b, px, qty, side = e
    return struct.pack("<BQQqQB", KIND_CODE[kind], a, b, px, qty, side)


def py_event_log_hash(events) -> bytes:
    h = hashlib.sha256(b"\x01")
    for e in events:
        h.update(encode_event(e))
    return h.digest()


class PyRefBook:
    """dict + deque L3 book with the lob-core event conventions; the Python truth."""

    def __init__(self):
        self.levels = ({}, {})  # side -> {px: deque([[id, qty], ...])}
        self.orders = {}  # id -> [side, px]

    def add(self, oid, side, px, qty):
        s = 0 if side == "B" else 1
        if oid in self.orders:
            raise LookupError(f"duplicate order id {oid}")
        self.levels[s].setdefault(px, deque()).append([oid, qty])
        self.orders[oid] = [s, px]
        return ("add", oid, 0, px, qty, s)

    def _reduce(self, kind, oid, qty):
        if oid not in self.orders:
            raise LookupError(f"unknown order id {oid}")
        s, px = self.orders[oid]
        fifo = self.levels[s][px]
        node = next(n for n in fifo if n[0] == oid)
        if kind == "exec" and qty > node[1]:
            raise ValueError("over-execute")
        removed = min(qty, node[1])
        node[1] -= removed
        if node[1] == 0:
            fifo.remove(node)
            if not fifo:
                del self.levels[s][px]
            del self.orders[oid]
        return (kind, oid, node[1], px, removed, s)

    def cancel(self, oid, qty):
        return self._reduce("cancel", oid, qty)

    def execute(self, oid, qty):
        return self._reduce("exec", oid, qty)

    def delete(self, oid):
        if oid not in self.orders:
            raise LookupError(f"unknown order id {oid}")
        s, px = self.orders[oid]
        fifo = self.levels[s][px]
        node = next(n for n in fifo if n[0] == oid)
        fifo.remove(node)
        if not fifo:
            del self.levels[s][px]
        del self.orders[oid]
        return ("delete", oid, 0, px, node[1], s)

    def replace(self, old, new, px, qty):
        if old not in self.orders:
            raise LookupError(f"unknown order id {old}")
        s = self.orders[old][0]
        self.delete(old)
        self.levels[s].setdefault(px, deque()).append([new, qty])
        self.orders[new] = [s, px]
        return ("replace", old, new, px, qty, s)

    def state_hash(self) -> bytes:
        h = hashlib.sha256()
        for s in (0, 1):
            for px in sorted(self.levels[s]):
                fifo = self.levels[s][px]
                h.update(struct.pack("<qI", px, len(fifo)))
                for oid, qty in fifo:
                    h.update(struct.pack("<QI", oid, qty))
        return h.digest()


def is_placeholder(px: int) -> bool:
    return px <= 100 or px >= 1_999_000_000


def tick_for(px: int) -> int:
    return 100 if px >= 10_000 or px % 100 == 0 else 1


class MigratingBook:
    """lobcore.RefBook until the first non-placeholder add, then a lobcore.Book window centred
    there with the resting orders re-added in FIFO order (the Rust session's rule)."""

    def __init__(self):
        self.book = lobcore.RefBook()
        self.array = False

    def _ensure_array(self, px: int):
        if self.array or is_placeholder(px):
            return
        tick = tick_for(px)
        k = min(ARRAY_WINDOW // 2, (px - 1) // tick)
        arr = lobcore.Book(px - k * tick, ARRAY_WINDOW, tick)
        for side, lpx, fifo in self.book.snapshot():
            for oid, qty in fifo:
                arr.add(oid, side, lpx, qty)
        self.book, self.array = arr, True

    def add(self, oid, side, px, qty):
        self._ensure_array(px)
        return self.book.add(oid, side, px, qty)

    def replace(self, old, new, px, qty):
        self._ensure_array(px)
        return self.book.replace(old, new, px, qty)

    def __getattr__(self, name):
        return getattr(self.book, name)


def test_close_hashes_and_event_log_agree_across_three_implementations(fixture_messages):
    rust = lobcore.replay_stats(FIXTURE)
    rust_array = lobcore.replay_stats(FIXTURE, locates=list(range(1, 9)), array_window=ARRAY_WINDOW)
    _, truth = lobcore.synth_itch(7, 100_000, truth=True)

    py_books: dict[int, PyRefBook] = {}
    lob_books: dict[int, MigratingBook] = {}
    events = []
    for m in fixture_messages:
        if m["type"] not in BOOK_TYPES:
            continue
        loc = m["locate"]
        pb = py_books.setdefault(loc, PyRefBook())
        lb = lob_books.setdefault(loc, MigratingBook())
        e_py = apply_msg(pb, m)
        e_lob = apply_msg(lb, m)
        assert e_py == e_lob
        events.append(e_py)

    assert rust["unknown_id"] == 0 and rust["negative_qty"] == 0 and rust["duplicate_id"] == 0
    assert rust["event_count"] == len(events)
    assert rust["event_log_hash"] == rust_array["event_log_hash"]
    assert rust["event_log_hash"] == lobcore.event_log_hash(events)
    assert rust["event_log_hash"] == py_event_log_hash(events)

    by_locate = {row["locate"]: row for row in rust["locates"]}
    by_locate_array = {row["locate"]: row for row in rust_array["locates"]}
    assert set(by_locate) == set(py_books) == set(range(1, 9))
    all_h = hashlib.sha256()
    for loc in sorted(py_books):
        py_hash = py_books[loc].state_hash()
        assert py_hash == lob_books[loc].state_hash()
        assert py_hash == by_locate[loc]["close_hash"]
        assert py_hash == by_locate_array[loc]["close_hash"]
        last_truth = max(i for i, tl in enumerate(truth["locate"]) if tl == loc)
        assert py_hash == truth["book_hash"][last_truth]
        assert len(py_books[loc].orders) == lob_books[loc].live_orders == int(truth["live_orders"][last_truth])
        assert by_locate_array[loc]["array"] and not by_locate[loc]["array"]
        all_h.update(struct.pack("<H", loc) + py_hash)
    assert rust["all_locates_hash"] == all_h.digest()
    assert rust["live"] == sum(len(b.orders) for b in py_books.values())


def test_event_encode_matches_hashlib_reference():
    e = ("trade", 5, 4, 10001, 30, 0)
    assert lobcore.event_encode(e) == encode_event(e)
    assert lobcore.event_encode(e).hex() == "090500000000000000040000000000000011270000000000001e0000000000000000"
    assert lobcore.event_log_hash([]) == hashlib.sha256(b"\x01").digest()
    assert lobcore.event_log_hash([e]) == py_event_log_hash([e])


def test_book_state_hash_matches_hashlib_reference():
    ref = PyRefBook()
    book = lobcore.Book(10_000, 64, 1)
    assert book.state_hash() == hashlib.sha256(b"").digest() == ref.state_hash()
    for args in [(1, "B", 10_030, 300), (2, "B", 10_030, 100), (3, "S", 10_031, 200), (4, "S", 10_999, 5)]:
        assert ref.add(*args) == book.add(*args)
    assert ref.state_hash() == book.state_hash()
    assert ref.cancel(1, 50) == book.cancel(1, 50)
    assert ref.execute(3, 200) == book.execute(3, 200)
    assert ref.replace(2, 9, 10_029, 7) == book.replace(2, 9, 10_029, 7)
    assert ref.state_hash() == book.state_hash()

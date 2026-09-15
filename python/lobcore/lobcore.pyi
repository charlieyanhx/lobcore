"""Type stubs for the `lobcore` extension module (hand-written; keep in sync with src/*.rs).

Prices are ``int`` in 1e-4 units (ITCH Price(4)), quantities are shares, timestamps are ns
since midnight. An event is ``(kind, a, b, px, qty, side)`` with ``kind`` one of the names in
``EVENT_KINDS`` and ``side`` 0 = bid / 1 = ask (the 34-byte record fields of the event-log
hash contract).
"""

from __future__ import annotations

from os import PathLike
from typing import Any, Literal, TypeAlias

import numpy as np
from numpy.typing import NDArray

__version__: str
EVENT_LOG_VERSION: int
EVENT_RECORD_LEN: int
MAX_LEVELS: int

EventKind: TypeAlias = Literal[
    "add", "cancel", "exec", "delete", "replace", "modify",
    "unknown", "reject", "trade", "stp_cancel", "ioc_cancel", "fok_reject",
]  # fmt: skip
Event: TypeAlias = tuple[EventKind, int, int, int, int, int]
Side: TypeAlias = str | int
"""``"B"`` / ``"S"``, ``"bid"`` / ``"ask"`` (any case) or ``0`` / ``1``."""
Level: TypeAlias = tuple[int, int, int]
"""``(px, qty, count)``."""
L2: TypeAlias = dict[str, NDArray[np.void]]
"""``{"bid": array, "ask": array}``, dtype ``[("px", "<i4"), ("qty", "<u4"), ("count", "<u4")]``."""
Snapshot: TypeAlias = list[tuple[int, int, list[tuple[int, int]]]]
"""``[(side, px, [(id, qty), ...]), ...]`` best first per side, bids then asks."""

class UnknownId(LookupError):
    """A cancel / delete / execute / replace named an order id that is not live."""

    event: Event

class BookReject(ValueError):
    """Over-execute, duplicate id, zero quantity / level overflow, or a non-positive price."""

    event: Event

class Book:
    """The bounded-array L3 book: ``n_levels`` prices ``base_px + i * tick`` per side in a flat
    array; other positive prices go to the overflow map (correct, slower, counted)."""

    def __init__(self, base_px: int, n_levels: int, tick: int = 100) -> None: ...
    def add(self, id: int, side: Side, px: int, qty: int) -> Event: ...
    def cancel(self, id: int, qty: int) -> Event: ...
    def delete(self, id: int) -> Event: ...
    def execute(self, id: int, qty: int) -> Event: ...
    def replace(self, old: int, new: int, px: int, qty: int) -> Event: ...
    def l1(self) -> tuple[Level | None, Level | None]: ...
    def l2(self, depth: int = 5) -> L2: ...
    def queue_ahead(self, id: int) -> int | None: ...
    def state_hash(self) -> bytes: ...
    def snapshot(self) -> Snapshot: ...
    def event_log_hash(self) -> bytes: ...
    def check(self) -> None: ...
    @property
    def live_orders(self) -> int: ...
    @property
    def event_count(self) -> int: ...
    @property
    def config(self) -> tuple[int, int, int]: ...
    @property
    def overflow_hits(self) -> int: ...
    @property
    def max_abs_offset(self) -> int: ...

class RefBook:
    """The BTreeMap reference book with the identical surface and semantics; no window."""

    def __init__(self) -> None: ...
    def add(self, id: int, side: Side, px: int, qty: int) -> Event: ...
    def cancel(self, id: int, qty: int) -> Event: ...
    def delete(self, id: int) -> Event: ...
    def execute(self, id: int, qty: int) -> Event: ...
    def replace(self, old: int, new: int, px: int, qty: int) -> Event: ...
    def l1(self) -> tuple[Level | None, Level | None]: ...
    def l2(self, depth: int = 5) -> L2: ...
    def queue_ahead(self, id: int) -> int | None: ...
    def state_hash(self) -> bytes: ...
    def snapshot(self) -> Snapshot: ...
    def event_log_hash(self) -> bytes: ...
    @property
    def live_orders(self) -> int: ...
    @property
    def event_count(self) -> int: ...

def event_encode(event: Event) -> bytes: ...
def event_log_hash(events: Any) -> bytes: ...
def sha256(data: bytes) -> bytes: ...
def replay_itch(
    path: str | PathLike[str],
    symbols: list[str] | None = None,
    locates: list[int] | None = None,
    every_n: int | None = None,
    every_ns: int | None = None,
    features: bool = True,
    array_window: int = 2048,
) -> dict[str, NDArray[Any]]:
    """Sampled L1 snapshots (+ features) of a plain or ``.gz`` ITCH 5.0 file as numpy arrays:
    ``ts u64, locate u16, bid_px i32, bid_qty u32, ask_px i32, ask_qty u32`` and, with
    ``features``, ``depth5_bid u64, depth5_ask u64, wmid f64, imb1 f64, imb5 f64, spread i32,
    ofi i64``. Computed with the GIL released."""

def replay_stats(
    path: str | PathLike[str],
    symbols: list[str] | None = None,
    locates: list[int] | None = None,
    array_window: int = 2048,
) -> dict[str, Any]:
    """The replay-statistics contract as a dict (counts, ``bytes`` hashes, ``locates`` rows,
    timing, and the rendered markdown block under ``"render"``)."""

def synth_itch(
    seed: int,
    n: int,
    truth: bool = False,
    *,
    locates: int = ...,
    mix: list[float] = ...,
    placeholder_rate: float = ...,
    unknown_ref_rate: float = ...,
    subpenny: bool = ...,
    crossed_preopen: bool = ...,
    open_ns: int = ...,
    close_ns: int = ...,
) -> bytes | tuple[bytes, dict[str, NDArray[Any]]]:
    """Exactly ``n`` framed ITCH 5.0 messages for ``(seed, cfg)``; with ``truth=True`` also the
    per-message truth sidecar (``ts, locate, bid_px, bid_qty, ask_px, ask_qty, live_orders,
    book_hash (S32)``)."""

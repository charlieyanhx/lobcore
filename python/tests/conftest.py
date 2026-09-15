"""Shared fixtures: repository paths and a tiny pure-Python ITCH 5.0 framer.

The framer reads the emi.nasdaq.com BinaryFILE framing (2-byte big-endian payload length before
every message) and decodes only the fields the book needs, with the spec offsets: every message
carries locate (u16 @1) and timestamp (u48 @5); A/F ref u64 @11, side @19, shares u32 @20, price
u32 @32; E/C ref @11, executed u32 @19; X ref @11, cancelled u32 @19; D ref @11; U original @11,
new @19, shares u32 @27, price u32 @31. R carries the symbol @11 (8 bytes, space padded).
"""

from __future__ import annotations

import struct
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
FIXTURE = REPO / "tests" / "fixtures" / "synth_s7_100k.itch"
README = REPO / "README.md"

BOOK_TYPES = frozenset({b"A", b"F", b"E", b"C", b"X", b"D", b"U"})


def u48(b: bytes, at: int) -> int:
    return int.from_bytes(b[at : at + 6], "big")


def frames(data: bytes):
    """Yield every complete framed payload; a truncated tail is dropped (and counted by lobcore)."""
    i, n = 0, len(data)
    while i + 2 <= n:
        ln = (data[i] << 8) | data[i + 1]
        i += 2
        if i + ln > n:
            return
        yield data[i : i + ln]
        i += ln


def decode(p: bytes) -> dict:
    """Decode one payload into a dict with `type`, `locate`, `ts` and the book fields."""
    t = p[0:1]
    m = {"type": t, "locate": struct.unpack_from(">H", p, 1)[0], "ts": u48(p, 5)}
    if t in (b"A", b"F"):
        m.update(ref=struct.unpack_from(">Q", p, 11)[0], side=p[19:20], qty=struct.unpack_from(">I", p, 20)[0])
        m["px"] = struct.unpack_from(">I", p, 32)[0]
    elif t in (b"E", b"C"):
        m.update(ref=struct.unpack_from(">Q", p, 11)[0], qty=struct.unpack_from(">I", p, 19)[0])
    elif t == b"X":
        m.update(ref=struct.unpack_from(">Q", p, 11)[0], qty=struct.unpack_from(">I", p, 19)[0])
    elif t == b"D":
        m.update(ref=struct.unpack_from(">Q", p, 11)[0])
    elif t == b"U":
        m.update(
            ref=struct.unpack_from(">Q", p, 11)[0],
            new=struct.unpack_from(">Q", p, 19)[0],
            qty=struct.unpack_from(">I", p, 27)[0],
            px=struct.unpack_from(">I", p, 31)[0],
        )
    elif t == b"R":
        m["symbol"] = p[11:19].decode("ascii").rstrip()
    return m


def apply_msg(book, m: dict):
    """Apply one decoded book message to any object with the Book surface; returns the event."""
    t = m["type"]
    if t in (b"A", b"F"):
        return book.add(m["ref"], "B" if m["side"] == b"B" else "S", m["px"], m["qty"])
    if t in (b"E", b"C"):
        return book.execute(m["ref"], m["qty"])
    if t == b"X":
        return book.cancel(m["ref"], m["qty"])
    if t == b"D":
        return book.delete(m["ref"])
    if t == b"U":
        return book.replace(m["ref"], m["new"], m["px"], m["qty"])
    return None


@pytest.fixture(scope="session")
def fixture_bytes() -> bytes:
    assert FIXTURE.exists(), f"missing committed fixture {FIXTURE}"
    return FIXTURE.read_bytes()


@pytest.fixture(scope="session")
def fixture_messages(fixture_bytes) -> list[dict]:
    return [decode(p) for p in frames(fixture_bytes)]

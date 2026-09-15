"""Pure-Python reference book (dict + deque, no tree) that replays an ops.bin stream with the
lob-core event conventions (crates/lob-core/src/types.rs) and prints the event-log digest,
the close book-state hash, the error count and the live-order count.

    python ops_pyref.py ops_seed7.bin

It reproduced the four pins in tests/ops_seed7.rs independently of the Rust code; the PyO3
crate's python/tests/test_hash_parity.py can reuse it. Standard library only.
"""
import hashlib, struct, sys
from collections import deque

REC = struct.Struct("<BQQiI")
EV = struct.Struct("<BQQqQB")
ADD, CANCEL, EXEC, DELETE, REPLACE, UNKNOWN, REJECT = 1, 2, 3, 4, 5, 7, 8

class Book:
    def __init__(self):
        self.bids = {}; self.asks = {}; self.ids = {}
    def side(self, s): return self.bids if s == 0 else self.asks
    def level(self, s, px): return self.side(s).setdefault(px, deque())
    def find(self, oid):
        s, px = self.ids[oid]
        lvl = self.side(s)[px]
        for i, (o, q) in enumerate(lvl):
            if o == oid: return s, px, lvl, i, q
    def take(self, oid, by):
        s, px, lvl, i, q = self.find(oid)
        rem = q - by
        if rem == 0:
            del lvl[i]
            if not lvl: del self.side(s)[px]
            del self.ids[oid]
        else:
            lvl[i] = (oid, rem)
        return s, px, rem
    def add(self, oid, s, px, qty):
        if qty == 0: return (REJECT, 0, 0, 0, 0, 0)
        if px <= 0: return (REJECT, 0, 0, 0, 0, 0)
        if oid in self.ids: return (REJECT, oid, ADD, 0, 0, 0)
        if sum(q for _, q in self.side(s).get(px, ())) + qty > 2**32 - 1: return (REJECT, 0, 0, 0, 0, 0)
        self.level(s, px).append((oid, qty)); self.ids[oid] = (s, px)
        return (ADD, oid, 0, px, qty, s)
    def cancel(self, oid, qty):
        if oid not in self.ids: return (UNKNOWN, oid, CANCEL, 0, 0, 0)
        if qty == 0: return (REJECT, 0, 0, 0, 0, 0)
        have = self.find(oid)[4]; removed = min(qty, have)
        s, px, rem = self.take(oid, removed)
        return (CANCEL, oid, rem, px, removed, s)
    def execute(self, oid, qty):
        if oid not in self.ids: return (UNKNOWN, oid, EXEC, 0, 0, 0)
        if qty == 0: return (REJECT, 0, 0, 0, 0, 0)
        have = self.find(oid)[4]
        if qty > have: return (REJECT, oid, EXEC, 0, qty, 0)
        s, px, rem = self.take(oid, qty)
        return (EXEC, oid, rem, px, qty, s)
    def delete(self, oid):
        if oid not in self.ids: return (UNKNOWN, oid, DELETE, 0, 0, 0)
        have = self.find(oid)[4]
        s, px, _ = self.take(oid, have)
        return (DELETE, oid, 0, px, have, s)
    def replace(self, old, new, px, qty):
        if old not in self.ids: return (UNKNOWN, old, REPLACE, 0, 0, 0)
        if new in self.ids: return (REJECT, new, ADD, 0, 0, 0)
        if qty == 0: return (REJECT, 0, 0, 0, 0, 0)
        if px <= 0: return (REJECT, 0, 0, 0, 0, 0)
        s, old_px, lvl, i, have = self.find(old)
        existing = sum(q for _, q in self.side(s).get(px, ())) - (have if px == old_px else 0)
        if existing + qty > 2**32 - 1: return (REJECT, 0, 0, 0, 0, 0)
        self.take(old, have)
        self.level(s, px).append((new, qty)); self.ids[new] = (s, px)
        return (REPLACE, old, new, px, qty, s)
    def state_hash(self):
        h = hashlib.sha256()
        for side in (self.bids, self.asks):
            for px in sorted(side):
                lvl = side[px]
                h.update(struct.pack("<qI", px, len(lvl)))
                for o, q in lvl: h.update(struct.pack("<QI", o, q))
        return h.hexdigest()

data = open(sys.argv[1], "rb").read()
b = Book(); log = hashlib.sha256(b"\x01"); errs = 0
for (kind, a, a2, px, qty) in REC.iter_unpack(data):
    if kind == 1: ev = b.add(a, a2, px, qty)
    elif kind == 2: ev = b.cancel(a, qty)
    elif kind == 3: ev = b.execute(a, qty)
    elif kind == 4: ev = b.delete(a)
    elif kind == 5: ev = b.replace(a, a2, px, qty)
    else: raise SystemExit(f"bad kind {kind}")
    if ev[0] in (UNKNOWN, REJECT): errs += 1
    log.update(EV.pack(*ev))
print("event_log", log.hexdigest())
print("state_hash", b.state_hash())
print("errors", errs, "live", len(b.ids))

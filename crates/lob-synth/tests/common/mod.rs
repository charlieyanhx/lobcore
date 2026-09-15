//! An independent ITCH 5.0 reader and L3 book for the round-trip tests. Shares no code with the
//! generator: frames are split by the 2-byte length, fields are read at the spec offsets, the book is
//! a fresh dict + deque per locate, and the book-state hash is re-derived from the contract text.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use lob_synth::{SynthConfig, SynthDay, Truth};

use sha2::{Digest, Sha256};

/// Spec payload sizes (the same table lob-feed dispatches on).
pub const SIZES: &[(u8, usize)] = &[
    (b'S', 12),
    (b'R', 39),
    (b'H', 25),
    (b'Y', 20),
    (b'L', 26),
    (b'V', 35),
    (b'W', 12),
    (b'K', 28),
    (b'J', 35),
    (b'h', 21),
    (b'A', 36),
    (b'F', 40),
    (b'E', 31),
    (b'C', 36),
    (b'X', 23),
    (b'D', 19),
    (b'U', 35),
    (b'P', 44),
    (b'Q', 40),
    (b'B', 19),
    (b'I', 50),
    (b'N', 20),
];

pub fn spec_size(t: u8) -> Option<usize> {
    SIZES.iter().find(|(x, _)| *x == t).map(|(_, n)| *n)
}

/// One framed message: type, locate, timestamp, payload (including the type byte).
#[derive(Clone, Copy, Debug)]
pub struct Msg<'a> {
    pub ty: u8,
    pub locate: u16,
    pub ts: u64,
    pub body: &'a [u8],
}

pub fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}
pub fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}
pub fn be48(b: &[u8]) -> u64 {
    let mut x = [0u8; 8];
    x[2..8].copy_from_slice(&b[..6]);
    u64::from_be_bytes(x)
}
pub fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b[..8].try_into().unwrap())
}

/// Splits the framed stream. Panics on a frame whose length is not the spec size for its type.
pub fn frames(bytes: &[u8]) -> Vec<Msg<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let len = be16(&bytes[i..]) as usize;
        let body = &bytes[i + 2..i + 2 + len];
        let ty = body[0];
        assert_eq!(
            Some(len),
            spec_size(ty),
            "frame {} type '{}' has length {len}",
            out.len(),
            ty as char
        );
        out.push(Msg {
            ty,
            locate: be16(&body[1..]),
            ts: be48(&body[5..]),
            body,
        });
        i += 2 + len;
    }
    assert_eq!(i, bytes.len(), "trailing partial frame");
    out
}

impl Msg<'_> {
    pub fn order_ref(&self) -> u64 {
        be64(&self.body[11..])
    }
    pub fn new_ref(&self) -> u64 {
        be64(&self.body[19..])
    }
    pub fn match_no(&self) -> u64 {
        match self.ty {
            b'E' | b'C' => be64(&self.body[23..]),
            b'P' => be64(&self.body[36..]),
            _ => unreachable!(),
        }
    }
    pub fn add_side(&self) -> u8 {
        self.body[19]
    }
    pub fn add_qty(&self) -> u32 {
        be32(&self.body[20..])
    }
    pub fn add_price(&self) -> u32 {
        be32(&self.body[32..])
    }
    pub fn event_code(&self) -> u8 {
        self.body[11]
    }
    pub fn stock(&self) -> &[u8] {
        &self.body[11..19]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Bid,
    Ask,
}

/// The independent per-locate L3 book.
#[derive(Default)]
pub struct Book {
    pub bids: BTreeMap<i64, VecDeque<(u64, u32)>>,
    pub asks: BTreeMap<i64, VecDeque<(u64, u32)>>,
    pub orders: HashMap<u64, (Side, i64)>,
    pub unknown: u64,
}

impl Book {
    fn side(&mut self, s: Side) -> &mut BTreeMap<i64, VecDeque<(u64, u32)>> {
        match s {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    pub fn add(&mut self, id: u64, side: u8, px: i64, qty: u32) {
        let s = if side == b'B' { Side::Bid } else { Side::Ask };
        assert!(
            self.orders.insert(id, (s, px)).is_none(),
            "duplicate ref {id}"
        );
        self.side(s).entry(px).or_default().push_back((id, qty));
    }

    fn remove(&mut self, id: u64) -> Option<(Side, i64)> {
        let (s, px) = self.orders.remove(&id)?;
        let lvl = self.side(s).get_mut(&px).unwrap();
        let pos = lvl.iter().position(|(i, _)| *i == id).unwrap();
        lvl.remove(pos);
        if lvl.is_empty() {
            self.side(s).remove(&px);
        }
        Some((s, px))
    }

    /// Returns false for an unknown ref (counted, no change).
    pub fn delete(&mut self, id: u64) -> bool {
        if self.remove(id).is_none() {
            self.unknown += 1;
            return false;
        }
        true
    }

    pub fn reduce(&mut self, id: u64, qty: u32) -> bool {
        let Some((s, px)) = self.orders.get(&id).copied() else {
            self.unknown += 1;
            return false;
        };
        let lvl = self.side(s).get_mut(&px).unwrap();
        let slot = lvl.iter_mut().find(|(i, _)| *i == id).unwrap();
        assert!(qty <= slot.1, "over-execute of {id}");
        slot.1 -= qty;
        if slot.1 == 0 {
            self.remove(id);
        }
        true
    }

    pub fn replace(&mut self, old: u64, new: u64, px: i64, qty: u32) -> bool {
        let Some((s, _)) = self.remove(old) else {
            self.unknown += 1;
            return false;
        };
        let side = if s == Side::Bid { b'B' } else { b'S' };
        self.add(new, side, px, qty);
        true
    }

    pub fn best_bid(&self) -> Option<(i32, u32)> {
        self.bids
            .iter()
            .next_back()
            .map(|(p, l)| (*p as i32, l.iter().map(|(_, q)| *q).sum()))
    }

    pub fn best_ask(&self) -> Option<(i32, u32)> {
        self.asks
            .iter()
            .next()
            .map(|(p, l)| (*p as i32, l.iter().map(|(_, q)| *q).sum()))
    }

    pub fn l5(&self, s: Side) -> [u32; 5] {
        let mut out = [0; 5];
        let levels: Vec<u32> = match s {
            Side::Bid => self
                .bids
                .values()
                .rev()
                .take(5)
                .map(|l| l.iter().map(|(_, q)| *q).sum())
                .collect(),
            Side::Ask => self
                .asks
                .values()
                .take(5)
                .map(|l| l.iter().map(|(_, q)| *q).sum())
                .collect(),
        };
        out[..levels.len()].copy_from_slice(&levels);
        out
    }

    /// Contract: sha256 over bid then ask, ascending price, i64 px LE | u32 count LE | (u64 id, u32 qty)*.
    pub fn hash(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        for side in [&self.bids, &self.asks] {
            for (px, lvl) in side {
                h.update(px.to_le_bytes());
                h.update((lvl.len() as u32).to_le_bytes());
                for (id, q) in lvl {
                    h.update(id.to_le_bytes());
                    h.update(q.to_le_bytes());
                }
            }
        }
        h.finalize().into()
    }

    /// Applies one message; returns whether it changed the book (unknown refs and non-book types
    /// return false).
    pub fn apply(&mut self, m: &Msg<'_>) -> bool {
        match m.ty {
            b'A' | b'F' => {
                self.add(
                    m.order_ref(),
                    m.add_side(),
                    i64::from(m.add_price()),
                    m.add_qty(),
                );
                true
            }
            b'E' | b'C' => self.reduce(m.order_ref(), be32(&m.body[19..])),
            b'X' => self.reduce(m.order_ref(), be32(&m.body[19..])),
            b'D' => self.delete(m.order_ref()),
            b'U' => self.replace(
                m.order_ref(),
                m.new_ref(),
                i64::from(be32(&m.body[31..])),
                be32(&m.body[27..]),
            ),
            _ => false,
        }
    }
}

/// Book state per locate for a whole stream; index 0 is a permanently empty book for system events.
pub struct Market {
    pub books: Vec<Book>,
}

impl Market {
    pub fn new(locates: usize) -> Self {
        Self {
            books: (0..=locates).map(|_| Book::default()).collect(),
        }
    }

    pub fn book(&mut self, locate: u16) -> &mut Book {
        &mut self.books[locate as usize]
    }
}

/// Replays `day.bytes` through the independent book and checks every truth row (best, L5, live
/// count, hash) and the structural guarantees. Returns the number of unknown-ref messages seen.
pub fn check_day(day: &SynthDay, cfg: &SynthConfig, n_msgs: u64) -> u64 {
    let msgs = frames(&day.bytes);
    assert_eq!(msgs.len() as u64, n_msgs, "frame count");
    assert_eq!(day.truth.len() as u64, n_msgs, "truth rows");
    let locates = cfg.locates as usize;

    // header and trailer shape
    assert_eq!((msgs[0].ty, msgs[0].event_code()), (b'S', b'O'));
    assert_eq!((msgs[1].ty, msgs[1].event_code()), (b'S', b'S'));
    for (i, m) in msgs[2..2 + locates].iter().enumerate() {
        assert_eq!(m.ty, b'R');
        assert_eq!(m.locate as usize, i + 1);
        assert_eq!(m.stock(), format!("SYN{:04} ", i + 1).as_bytes());
    }
    for (i, m) in msgs[2 + locates..2 + 2 * locates].iter().enumerate() {
        assert_eq!((m.ty, m.locate as usize, m.body[19]), (b'H', i + 1, b'T'));
    }
    let n = msgs.len();
    let tail: Vec<(u8, u8)> = msgs[n - 3..]
        .iter()
        .map(|m| (m.ty, m.event_code()))
        .collect();
    assert_eq!(tail, [(b'S', b'M'), (b'S', b'E'), (b'S', b'C')]);
    let q_pos = msgs
        .iter()
        .position(|m| m.ty == b'S' && m.event_code() == b'Q')
        .expect("S 'Q' present");
    assert!(q_pos >= 2 + 2 * locates);
    assert_eq!(msgs[q_pos].ts, cfg.open_ns);
    assert!(
        msgs[..q_pos]
            .iter()
            .all(|m| m.ts < cfg.open_ns || m.ty == b'E')
    );
    assert!(msgs[..q_pos].iter().all(|m| m.ts <= cfg.open_ns));
    assert!(msgs[q_pos..n - 3].iter().all(|m| m.ts < cfg.close_ns));
    assert!(msgs[n - 3..].iter().all(|m| m.ts == cfg.close_ns));

    // timestamps monotone, refs strictly increasing, match numbers strictly increasing
    let mut last_ts = 0;
    let mut last_ref = 0;
    let mut last_match = 0;
    let mut market = Market::new(locates);
    let mut unknown = 0;
    let mut placeholders = 0;
    let mut off_grid_locates = HashSet::new();
    let mut crossed_after_q = 0;
    for (i, m) in msgs.iter().enumerate() {
        assert!(m.ts >= last_ts, "ts not monotone at {i}");
        assert!(m.ts < (1u64 << 48));
        last_ts = m.ts;
        assert!(m.locate as usize <= locates);
        match m.ty {
            b'A' | b'F' => {
                let r = m.order_ref();
                assert!(r > last_ref, "ref not increasing at {i}");
                last_ref = r;
                let px = m.add_price();
                assert!((1..=2_000_000_000).contains(&px));
                if px == 100 || px == 1_999_999_900 {
                    placeholders += 1;
                }
                if px % 100 != 0 {
                    assert!(cfg.subpenny, "off-grid price without subpenny at {i}");
                    assert!(px < 10_000, "sub-penny price at or above $1 at {i}");
                    off_grid_locates.insert(m.locate);
                }
            }
            b'U' => {
                let r = m.new_ref();
                assert!(r > last_ref, "new ref not increasing at {i}");
                last_ref = r;
            }
            b'E' | b'C' | b'P' => {
                let mn = m.match_no();
                assert!(mn > last_match, "match number not increasing at {i}");
                last_match = mn;
            }
            _ => {}
        }
        let book = market.book(m.locate);
        let before = book.unknown;
        if matches!(m.ty, b'E' | b'C') && book.orders.contains_key(&m.order_ref()) {
            // E/C consume from the head of the best level on their side
            let (side, px) = book.orders[&m.order_ref()];
            let head = match side {
                Side::Bid => book.bids.iter().next_back(),
                Side::Ask => book.asks.iter().next(),
            }
            .map(|(p, l)| (*p, l.front().unwrap().0))
            .unwrap();
            assert_eq!(
                head,
                (px, m.order_ref()),
                "E/C not at the level head at {i}"
            );
        }
        if m.ty == b'X' {
            let rem = book
                .orders
                .get(&m.order_ref())
                .and_then(|(s, p)| match s {
                    Side::Bid => book.bids.get(p),
                    Side::Ask => book.asks.get(p),
                })
                .and_then(|l| l.iter().find(|(id, _)| *id == m.order_ref()))
                .map(|(_, q)| *q);
            if let Some(rem) = rem {
                assert!(be32(&m.body[19..]) < rem, "X is not partial at {i}");
            }
        }
        book.apply(m);
        if book.unknown > before {
            unknown += 1;
            assert!(matches!(m.ty, b'X' | b'D' | b'E' | b'C' | b'U'));
        }
        let t = &day.truth[i];
        assert_eq!((t.ts, t.locate), (m.ts, m.locate), "truth row {i}");
        let expect = Truth {
            ts: m.ts,
            locate: m.locate,
            best_bid: book.best_bid(),
            best_ask: book.best_ask(),
            l5_bid: book.l5(Side::Bid),
            l5_ask: book.l5(Side::Ask),
            live_orders: book.orders.len() as u32,
            book_hash: book.hash(),
        };
        assert_eq!(
            *t, expect,
            "truth mismatch after message {i} type {}",
            m.ty as char
        );
        if i > q_pos
            && let (Some((bb, _)), Some((ba, _))) = (t.best_bid, t.best_ask)
            && bb >= ba
        {
            crossed_after_q += 1;
        }
    }
    if cfg.unknown_ref_rate == 0.0 {
        assert_eq!(unknown, 0, "a message targeted a non-live ref");
    }
    if cfg.placeholder_rate > 0.0 && n_msgs >= 10_000 {
        assert!(placeholders > 0, "no placeholder adds");
    }
    if !cfg.subpenny {
        assert!(off_grid_locates.is_empty());
    }
    // the open uncrosses every locate; nothing crosses afterwards
    let budget_left = n_msgs > 2 * cfg.locates as u64 + 6 + 64;
    if budget_left {
        assert_eq!(crossed_after_q, 0, "crossed book after S 'Q'");
    }
    unknown
}

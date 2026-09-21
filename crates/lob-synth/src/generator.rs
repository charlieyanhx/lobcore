//! The seeded generator: per-locate price walks, the message mix, and the truth sidecar.
//!
//! Every random draw is an integer or a bit-pattern float from `ChaCha8Rng` (no `ln`/`pow`/`exp`),
//! so the same `(seed, cfg)` yields identical bytes on every platform.

use std::collections::HashMap;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::itch::{self, Header, MAX_PRICE};
use crate::truth::{Side, TruthBook};
use crate::{SynthConfig, SynthDay, Truth};

/// Index into `SynthConfig::mix`.
const A: usize = 0;
const F: usize = 1;
const D: usize = 2;
const X: usize = 3;
const U: usize = 4;
const E: usize = 5;
const C: usize = 6;
const P: usize = 7;
const L: usize = 8;

const MPID: &[u8; 4] = b"SYNM";
const PLACEHOLDER_BID: i32 = 100; // $0.01
const PLACEHOLDER_ASK: i32 = 1_999_999_900; // $199,999.99
const MAX_OFFSET_TICKS: i32 = 15;
/// Upper bound on the bytes reserved up front for the output and the truth (1 GiB each).
const RESERVE_CAP: usize = 1 << 30;
const CROSS_PROB: f64 = 0.3;
const MID_STEP_PROB: f64 = 0.15;
const ROUND_LOT_PROB: f64 = 0.8;

struct Locate {
    stock: [u8; 8],
    tick: i32,
    mid: i32,
    book: TruthBook,
    live: Vec<u64>,
    live_idx: HashMap<u64, usize>,
    dead: Vec<u64>,
}

impl Locate {
    fn track_add(&mut self, id: u64) {
        self.live_idx.insert(id, self.live.len());
        self.live.push(id);
    }

    fn track_remove(&mut self, id: u64, keep_dead: bool) {
        let i = self.live_idx.remove(&id).expect("removing a tracked ref");
        let last = self.live.pop().expect("live list non-empty");
        if i < self.live.len() {
            self.live[i] = last;
            self.live_idx.insert(last, i);
        }
        if keep_dead {
            self.dead.push(id);
        }
    }
}

pub(crate) struct Generator<'a> {
    rng: ChaCha8Rng,
    cfg: &'a SynthConfig,
    out: Vec<u8>,
    truth: Option<Vec<Truth>>,
    locs: Vec<Locate>,
    cum: [f64; 9],
    next_ref: u64,
    next_match: u64,
    tracking: u16,
    ts: u64,
    keep_dead: bool,
    empty_hash: [u8; 32],
}

impl<'a> Generator<'a> {
    pub(crate) fn new(seed: u64, cfg: &'a SynthConfig, want_truth: bool, n_msgs: u64) -> Self {
        if let Err(e) = crate::check_args(n_msgs, cfg) {
            panic!("lob-synth: {e}");
        }
        let total: f64 = cfg.mix.iter().map(|w| f64::from(*w)).sum();
        let mut cum = [0f64; 9];
        let mut acc = 0.0;
        for (c, w) in cum.iter_mut().zip(cfg.mix.iter()) {
            acc += f64::from(*w) / total;
            *c = acc;
        }
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let locs = (1..=cfg.locates)
            .map(|i| new_locate(&mut rng, cfg, i))
            .collect();
        // reserve ~32 B/message up front, capped so a huge `n` grows instead of failing at once
        let cap = usize::try_from(n_msgs)
            .unwrap_or(0)
            .saturating_mul(32)
            .min(RESERVE_CAP);
        Self {
            rng,
            cfg,
            out: Vec::with_capacity(cap),
            truth: want_truth.then(|| {
                Vec::with_capacity(
                    usize::try_from(n_msgs)
                        .unwrap_or(0)
                        .min(RESERVE_CAP / std::mem::size_of::<Truth>()),
                )
            }),
            locs,
            cum,
            next_ref: 1,
            next_match: 1,
            tracking: 0,
            ts: 0,
            keep_dead: cfg.unknown_ref_rate > 0.0,
            empty_hash: TruthBook::new().hash(),
        }
    }

    /// Writes the whole day: header, mix (with the open uncross and the S 'Q'), trailer.
    pub(crate) fn run(mut self, n_msgs: u64) -> SynthDay {
        let locates = u64::from(self.cfg.locates);
        let fixed = 2 + 2 * locates + 1 + 3;
        let mix_budget = n_msgs - fixed; // `new` checked n_msgs >= fixed
        let span = self.cfg.close_ns - self.cfg.open_ns;
        let t0 = self.cfg.open_ns.saturating_sub(span / 8);
        self.ts = t0;
        self.header();
        let gap_max = (self.cfg.close_ns - t0)
            .checked_div(mix_budget)
            .map_or(0, |g| 2 * g);
        let mut emitted = 0;
        let mut q_done = false;
        while emitted < mix_budget {
            self.advance_clock(gap_max);
            if !q_done && self.ts >= self.cfg.open_ns {
                let resume = self.ts;
                self.ts = self.cfg.open_ns;
                emitted += self.open_market(mix_budget - emitted);
                self.ts = resume;
                q_done = true;
                if emitted >= mix_budget {
                    break;
                }
            }
            self.step();
            emitted += 1;
        }
        if !q_done {
            self.ts = self.ts.max(self.cfg.open_ns);
            self.system_event(b'Q');
        }
        self.ts = self.cfg.close_ns;
        for code in *b"MEC" {
            self.system_event(code);
        }
        if let Some(t) = &self.truth {
            debug_assert_eq!(t.len() as u64, n_msgs);
        }
        SynthDay {
            bytes: self.out,
            truth: self.truth.unwrap_or_default(),
        }
    }

    fn advance_clock(&mut self, gap_max: u64) {
        let gap = self.rng.random_range(0..=gap_max);
        self.ts = (self.ts + gap).min(self.cfg.close_ns - 1);
    }

    fn header(&mut self) {
        self.system_event(b'O');
        self.system_event(b'S');
        for li in 0..self.locs.len() {
            let h = self.hdr(li);
            let stock = self.locs[li].stock;
            itch::stock_directory(&mut self.out, &h, &stock);
            self.record(li);
        }
        for li in 0..self.locs.len() {
            let h = self.hdr(li);
            let stock = self.locs[li].stock;
            itch::trading_action(&mut self.out, &h, &stock, b'T');
            self.record(li);
        }
    }

    fn hdr(&mut self, li: usize) -> Header {
        let tracking = self.tracking;
        self.tracking = self.tracking.wrapping_add(1);
        Header {
            locate: u16::try_from(li + 1).expect("locate fits u16"),
            tracking,
            ts: self.ts,
        }
    }

    fn system_event(&mut self, code: u8) {
        let tracking = self.tracking;
        self.tracking = self.tracking.wrapping_add(1);
        let h = Header {
            locate: 0,
            tracking,
            ts: self.ts,
        };
        itch::system_event(&mut self.out, &h, code);
        if let Some(t) = self.truth.as_mut() {
            t.push(Truth {
                ts: self.ts,
                locate: 0,
                best_bid: None,
                best_ask: None,
                l5_bid: [0; 5],
                l5_ask: [0; 5],
                live_orders: 0,
                book_hash: self.empty_hash,
            });
        }
    }

    fn record(&mut self, li: usize) {
        let Some(t) = self.truth.as_mut() else {
            return;
        };
        let b = &self.locs[li].book;
        t.push(Truth {
            ts: self.ts,
            locate: u16::try_from(li + 1).expect("locate fits u16"),
            best_bid: b.best_bid(),
            best_ask: b.best_ask(),
            l5_bid: b.l5(Side::Bid),
            l5_ask: b.l5(Side::Ask),
            live_orders: u32::try_from(b.live_orders()).expect("live orders fit u32"),
            book_hash: b.hash(),
        });
    }

    /// Opening, at exactly `open_ns`: execute crossed resting orders from the level heads (E, one
    /// match number each), then the S 'Q' event. Returns the number of E messages written (bounded by
    /// `budget`; a stream too short to finish leaves that locate crossed).
    fn open_market(&mut self, budget: u64) -> u64 {
        let mut n = 0;
        'locates: for li in 0..self.locs.len() {
            loop {
                let b = &self.locs[li].book;
                let (Some((bb, _)), Some((ba, _))) = (b.best_bid(), b.best_ask()) else {
                    break;
                };
                if bb < ba {
                    break;
                }
                let (hb, qb) = b.head_of_best(Side::Bid).expect("non-empty bid level");
                let (ha, qa) = b.head_of_best(Side::Ask).expect("non-empty ask level");
                let q = qb.min(qa);
                for id in [hb, ha] {
                    if n >= budget {
                        break 'locates;
                    }
                    self.execute_known(li, id, q, false);
                    n += 1;
                }
            }
        }
        self.system_event(b'Q');
        n
    }

    fn draw_kind(&mut self) -> usize {
        let u: f64 = self.rng.random();
        self.cum.iter().position(|c| u < *c).unwrap_or(L)
    }

    fn step(&mut self) {
        let li = self.rng.random_range(0..self.locs.len());
        let kind = self.draw_kind();
        match kind {
            A | F => self.add(li, kind == F),
            D | X | U | E | C => {
                if self.maybe_unknown(li, kind) {
                    return;
                }
                match kind {
                    D => self.delete(li),
                    X => self.cancel(li),
                    U => self.replace(li),
                    _ => self.execute(li, kind == C),
                }
            }
            P => self.trade(li),
            _ => self.mpid(li),
        }
    }

    fn take_ref(&mut self) -> u64 {
        let r = self.next_ref;
        self.next_ref += 1;
        r
    }

    fn take_match(&mut self) -> u64 {
        let m = self.next_match;
        self.next_match += 1;
        m
    }

    fn qty(&mut self) -> u32 {
        if self.rng.random_bool(ROUND_LOT_PROB) {
            100 * self.rng.random_range(1..=10u32)
        } else {
            self.rng.random_range(1..=499u32)
        }
    }

    fn offset_ticks(&mut self) -> i32 {
        let mut k = 0;
        while k < MAX_OFFSET_TICKS && self.rng.random_bool(0.5) {
            k += 1;
        }
        k
    }

    fn crossing_allowed(&self) -> bool {
        self.cfg.crossed_preopen && self.ts < self.cfg.open_ns
    }

    /// Price for a new resting order: anchored on the locate's mid, capped so it never crosses the
    /// opposite best — except pre-open with `crossed_preopen`, where it may cross by 1..3 ticks.
    fn price_for(&mut self, li: usize, side: Side) -> i32 {
        let crossed_ok = self.crossing_allowed();
        let offset = self.offset_ticks();
        let (tick, mid, best_bid, best_ask) = {
            let l = &self.locs[li];
            (l.tick, l.mid, l.book.best_bid(), l.book.best_ask())
        };
        let cross = if crossed_ok && self.rng.random_bool(CROSS_PROB) {
            tick * self.rng.random_range(1..=3)
        } else {
            0
        };
        match side {
            Side::Bid => {
                let mut anchor = mid + cross;
                if !crossed_ok && let Some((ba, _)) = best_ask {
                    anchor = anchor.min(ba - tick);
                }
                (anchor - offset * tick).max(tick)
            }
            Side::Ask => {
                let mut anchor = mid + tick - cross;
                if !crossed_ok && let Some((bb, _)) = best_bid {
                    anchor = anchor.max(bb + tick);
                }
                (anchor + offset * tick).min(MAX_PRICE as i32)
            }
        }
    }

    fn walk_mid(&mut self, li: usize) {
        let up = self.rng.random_bool(MID_STEP_PROB);
        let down = self.rng.random_bool(MID_STEP_PROB);
        let l = &mut self.locs[li];
        let step = i32::from(up) - i32::from(down);
        l.mid = (l.mid + step * l.tick).clamp(2 * l.tick, MAX_PRICE as i32 / 2);
    }

    fn add(&mut self, li: usize, with_mpid: bool) {
        let side = if self.rng.random_bool(0.5) {
            Side::Bid
        } else {
            Side::Ask
        };
        let placeholder = self.rng.random_bool(f64::from(self.cfg.placeholder_rate));
        let px = if placeholder {
            match side {
                Side::Bid => PLACEHOLDER_BID,
                Side::Ask => PLACEHOLDER_ASK,
            }
        } else {
            self.walk_mid(li);
            self.price_for(li, side)
        };
        let qty = self.qty();
        let id = self.take_ref();
        let h = self.hdr(li);
        let stock = self.locs[li].stock;
        let side_byte = side_byte(side);
        let px_u = u32::try_from(px).expect("price positive");
        if with_mpid {
            itch::add_order_mpid(&mut self.out, &h, id, side_byte, qty, &stock, px_u, MPID);
        } else {
            itch::add_order(&mut self.out, &h, id, side_byte, qty, &stock, px_u);
        }
        let l = &mut self.locs[li];
        l.book.add(id, side, px, qty);
        l.track_add(id);
        self.record(li);
    }

    fn pick_live(&mut self, li: usize) -> Option<u64> {
        let l = &self.locs[li];
        if l.live.is_empty() {
            return None;
        }
        let i = self.rng.random_range(0..l.live.len());
        Some(l.live[i])
    }

    /// With probability `unknown_ref_rate`, targets a dead ref instead of a live one (truth unchanged).
    fn maybe_unknown(&mut self, li: usize, kind: usize) -> bool {
        if !self.keep_dead || self.locs[li].dead.is_empty() {
            return false;
        }
        if !self.rng.random_bool(f64::from(self.cfg.unknown_ref_rate)) {
            return false;
        }
        let d = &self.locs[li].dead;
        let id = d[self.rng.random_range(0..d.len())];
        let qty = self.qty();
        let h = self.hdr(li);
        match kind {
            D => itch::order_delete(&mut self.out, &h, id),
            X => itch::order_cancel(&mut self.out, &h, id, qty),
            U => {
                let new = self.take_ref();
                let px = self.locs[li].mid;
                let px_u = u32::try_from(px).expect("price positive");
                itch::order_replace(&mut self.out, &h, id, new, qty, px_u);
            }
            E => {
                let m = self.take_match();
                itch::order_executed(&mut self.out, &h, id, qty, m);
            }
            _ => {
                let m = self.take_match();
                let px_u = u32::try_from(self.locs[li].mid).expect("price positive");
                itch::order_executed_with_price(&mut self.out, &h, id, qty, m, b'Y', px_u);
            }
        }
        self.record(li);
        true
    }

    fn delete(&mut self, li: usize) {
        let Some(id) = self.pick_live(li) else {
            return self.add(li, false);
        };
        let h = self.hdr(li);
        itch::order_delete(&mut self.out, &h, id);
        self.remove_live(li, id);
        self.record(li);
    }

    fn remove_live(&mut self, li: usize, id: u64) {
        let l = &mut self.locs[li];
        assert!(l.book.delete(id), "deleting a live ref");
        l.track_remove(id, self.keep_dead);
    }

    /// X: partial cancel of 1..qty-1 shares in place; an order of one share is deleted instead.
    fn cancel(&mut self, li: usize) {
        let Some(id) = self.pick_live(li) else {
            return self.add(li, false);
        };
        let rem = self.locs[li].book.qty(id).expect("live ref has qty");
        if rem <= 1 {
            let h = self.hdr(li);
            itch::order_delete(&mut self.out, &h, id);
            self.remove_live(li, id);
        } else {
            let k = self.rng.random_range(1..rem);
            let h = self.hdr(li);
            itch::order_cancel(&mut self.out, &h, id, k);
            assert!(self.locs[li].book.reduce(id, k), "cancel on a live ref");
        }
        self.record(li);
    }

    /// U: the old ref leaves, the new ref joins the tail of its (possibly new) level.
    fn replace(&mut self, li: usize) {
        let Some(old) = self.pick_live(li) else {
            return self.add(li, false);
        };
        let (side, _) = self.locs[li].book.lookup(old).expect("live ref");
        let px = self.price_for(li, side);
        let qty = self.qty();
        let new = self.take_ref();
        let h = self.hdr(li);
        itch::order_replace(
            &mut self.out,
            &h,
            old,
            new,
            qty,
            u32::try_from(px).expect("price positive"),
        );
        let l = &mut self.locs[li];
        assert!(l.book.replace(old, new, px, qty), "replace on a live ref");
        l.track_remove(old, self.keep_dead);
        l.track_add(new);
        self.record(li);
    }

    /// E / C: consume 1..qty shares from the head order of the best level on a random side.
    fn execute(&mut self, li: usize, with_price: bool) {
        let first = if self.rng.random_bool(0.5) {
            Side::Bid
        } else {
            Side::Ask
        };
        let b = &self.locs[li].book;
        let head = b
            .head_of_best(first)
            .or_else(|| b.head_of_best(opposite(first)));
        let Some((id, rem)) = head else {
            return self.add(li, false);
        };
        let q = self.rng.random_range(1..=rem);
        self.execute_known(li, id, q, with_price);
    }

    fn execute_known(&mut self, li: usize, id: u64, q: u32, with_price: bool) {
        let m = self.take_match();
        let h = self.hdr(li);
        if with_price {
            let (_, px) = self.locs[li].book.lookup(id).expect("live ref");
            let printable = if self.rng.random_bool(0.5) {
                b'Y'
            } else {
                b'N'
            };
            let px_u = u32::try_from(px).expect("price positive");
            itch::order_executed_with_price(&mut self.out, &h, id, q, m, printable, px_u);
        } else {
            itch::order_executed(&mut self.out, &h, id, q, m);
        }
        let l = &mut self.locs[li];
        assert!(l.book.reduce(id, q), "execute on a live ref");
        if l.book.lookup(id).is_none() {
            l.track_remove(id, self.keep_dead);
        }
        self.record(li);
    }

    fn trade(&mut self, li: usize) {
        let qty = self.qty();
        let m = self.take_match();
        let h = self.hdr(li);
        let l = &self.locs[li];
        let px = l.book.best_bid().map_or(l.mid, |(p, _)| p);
        itch::trade(
            &mut self.out,
            &h,
            qty,
            &l.stock,
            u32::try_from(px).expect("price positive"),
            m,
        );
        self.record(li);
    }

    fn mpid(&mut self, li: usize) {
        let h = self.hdr(li);
        let stock = self.locs[li].stock;
        itch::mpid_position(&mut self.out, &h, MPID, &stock);
        self.record(li);
    }
}

fn side_byte(side: Side) -> u8 {
    match side {
        Side::Bid => b'B',
        Side::Ask => b'S',
    }
}

fn opposite(side: Side) -> Side {
    match side {
        Side::Bid => Side::Ask,
        Side::Ask => Side::Bid,
    }
}

/// Base price bucket by locate (stratified so every price regime appears with >= 4 locates):
/// 0: $0.25-$1 (sub-penny grid when enabled), 1: $1-$10, 2: $10-$100, 3: $100-$400.
fn new_locate(rng: &mut ChaCha8Rng, cfg: &SynthConfig, locate: u16) -> Locate {
    let (lo, hi) = match (locate - 1) % 4 {
        0 => (2_500, 10_000),
        1 => (10_000, 100_000),
        2 => (100_000, 1_000_000),
        _ => (1_000_000, 4_000_000),
    };
    let base: i32 = rng.random_range(lo..hi);
    let tick = if base < 10_000 && cfg.subpenny {
        1
    } else {
        100
    };
    let mid = (base / tick * tick).max(2 * tick);
    Locate {
        stock: itch::symbol8(&format!("SYN{locate:04}")),
        tick,
        mid,
        book: TruthBook::new(),
        live: Vec::new(),
        live_idx: HashMap::new(),
        dead: Vec::new(),
    }
}

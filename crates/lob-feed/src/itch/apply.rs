//! [`Session`]: one L3 book per stock locate, fed by ITCH messages.
//!
//! Semantics (spec 5.0, read from the document, not assumed): `A` / `F` rest a new order at
//! the tail of its level; `E` and `C` reduce the order **by id** at its display price
//! (`Printable` and the execution price never touch book state); `X` reduces in place and keeps
//! priority; `D` removes; `U` removes the original reference and rests the new one at the tail
//! of its (possibly same) level with the side carried from the original (spec 1.4.5); `P`, `Q`
//! and `B` are counted and never touch a book. `S` codes are recorded (`Q` starts the
//! after-open regime for the crossed-snapshot statistic); `H` states are kept per locate.
//!
//! Books: every locate starts as a `RefBook` when its `R` arrives (or on demand for an order
//! message without a directory entry, which is counted). A locate on the watchlist is migrated
//! to an `ArrayBook` at its first non-placeholder add: the window of `array_window` levels is
//! centred on that price (tick 0.01 at or above $1, 0.0001 below), the few resting placeholder
//! orders are re-added in FIFO order, and from then on every operation is allocation-free
//! while it stays inside the window (the overflow map is the documented allocating path and
//! its hits are published per locate). Anything not on the watchlist runs the reference book;
//! a full-market array window does not fit in 8 GB.
//!
//! Every book call's result is logged (`Event` or `BookError::event`) into one `EventLog`, so
//! the event-log hash covers rejections too. Live-order counts are tracked from the returned
//! events (add +1, delete -1, cancel / execute reaching zero -1), never by re-counting.

use lob_core::ops::logged_event;
use lob_core::{
    ArrayBook, BookConfig, BookError, Event, EventKind, EventLog, OrderBook, OrderId, Px, Qty,
    RefBook, Side,
};
use sha2::{Digest, Sha256};

use crate::itch::frame::FrameSource;
use crate::itch::msg::{Msg, ParseError, parse};
use crate::stats::{LocateStats, ReplayError, Stats, Timing};

/// Prices at or below $0.01 and at or above $199,900 are treated as placeholder quotes and do
/// not centre an array window (they still rest in the book, via the overflow map).
#[inline]
pub const fn is_placeholder(px: u32) -> bool {
    px <= 100 || px >= 1_999_000_000
}

/// Tick of the array grid for a locate whose first real price is `px`: 1c at or above $1 (Reg
/// NMS rule 612) and for a sub-dollar price that sits on the 1c grid; 0.0001 for a sub-dollar
/// price that does not. A wrong guess costs overflow-map hits (published), never correctness.
#[inline]
pub const fn tick_for(px: u32) -> Px {
    if px >= 10_000 || px.is_multiple_of(100) {
        100
    } else {
        1
    }
}

/// Which locates get the bounded array book.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Watchlist {
    /// Symbols (matched against the `R` message, space padded to 8).
    pub symbols: Vec<[u8; 8]>,
    /// Locate numbers.
    pub locates: Vec<u16>,
    /// Every locate (only sensible for small synthetic universes).
    pub all: bool,
}

impl Watchlist {
    /// No array books at all.
    pub fn none() -> Watchlist {
        Watchlist::default()
    }

    /// Every locate.
    pub fn all() -> Watchlist {
        Watchlist {
            all: true,
            ..Default::default()
        }
    }

    /// From symbol strings (at most 8 bytes each; longer ones are truncated).
    pub fn symbols<S: AsRef<str>>(syms: impl IntoIterator<Item = S>) -> Watchlist {
        Watchlist {
            symbols: syms.into_iter().map(|s| pad8(s.as_ref())).collect(),
            ..Default::default()
        }
    }

    /// Add locate numbers.
    pub fn with_locates(mut self, locates: impl IntoIterator<Item = u16>) -> Watchlist {
        self.locates.extend(locates);
        self
    }

    fn matches(&self, locate: u16, symbol: Option<&[u8; 8]>) -> bool {
        self.all
            || self.locates.contains(&locate)
            || symbol.is_some_and(|s| self.symbols.contains(s))
    }

    /// One-line description for the statistics block.
    pub fn describe(&self) -> String {
        if self.all {
            return "all locates".into();
        }
        let mut parts: Vec<String> = self
            .symbols
            .iter()
            .map(|s| String::from_utf8_lossy(s).trim_end().to_string())
            .collect();
        parts.extend(self.locates.iter().map(|l| format!("#{l}")));
        if parts.is_empty() {
            "none".into()
        } else {
            parts.join(" ")
        }
    }
}

/// Symbol string as the 8-byte space-padded ITCH field.
pub fn pad8(s: &str) -> [u8; 8] {
    let mut a = [b' '; 8];
    let b = s.as_bytes();
    let n = b.len().min(8);
    a[..n].copy_from_slice(&b[..n]);
    a
}

/// Session parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    /// Locates that get an `ArrayBook`.
    pub watchlist: Watchlist,
    /// Levels per side of the array window (1..=4096). Default 2048 (+-$10.24 at a 1c tick).
    pub array_window: u32,
    /// Slab / id-map pre-size of each array book (orders). Default 8,192.
    pub array_reserve: usize,
    /// Name recorded in the statistics block.
    pub source: String,
    /// Hash every book event into the event log (sha256 of 34-byte records). Default true;
    /// `replay --stats` needs it, `bench-quick` measures with and without it because the
    /// incremental sha256 costs more per message than the book operation itself.
    pub log_events: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            watchlist: Watchlist::none(),
            array_window: 2048,
            array_reserve: 8_192,
            source: String::new(),
            log_events: true,
        }
    }
}

/// The two book kinds. An enum (not `Box<dyn OrderBook>`) so the array book's overflow
/// counters and window stay reachable for the statistics; every call still goes through the
/// shared `OrderBook` trait.
#[derive(Debug)]
pub enum LocateBook {
    /// Watchlist locate after its first real add.
    Array(ArrayBook),
    /// Everyone else (and a watchlist locate before its first real add).
    Ref(RefBook),
}

impl LocateBook {
    /// The book behind the trait.
    #[inline]
    pub fn book(&mut self) -> &mut dyn OrderBook {
        match self {
            LocateBook::Array(b) => b,
            LocateBook::Ref(b) => b,
        }
    }

    /// Shared view.
    #[inline]
    pub fn book_ref(&self) -> &dyn OrderBook {
        match self {
            LocateBook::Array(b) => b,
            LocateBook::Ref(b) => b,
        }
    }
}

/// One replay: books per locate, session state, the event log and the statistics.
#[derive(Debug)]
pub struct Session {
    cfg: SessionConfig,
    books: Vec<Option<Box<LocateBook>>>,
    watched: Vec<bool>,
    s_state: u8,
    after_q: bool,
    log: EventLog,
    stats: Stats,
    finished: bool,
}

impl Session {
    /// New session; nothing is allocated per locate until the first `R`.
    pub fn new(cfg: SessionConfig) -> Session {
        let stats = Stats {
            source: cfg.source.clone(),
            array_window: cfg.array_window,
            watchlist: cfg.watchlist.describe(),
            ..Default::default()
        };
        Session {
            cfg,
            books: Vec::new(),
            watched: Vec::new(),
            s_state: 0,
            after_q: false,
            log: EventLog::new(),
            stats,
            finished: false,
        }
    }

    /// Configuration.
    pub fn config(&self) -> &SessionConfig {
        &self.cfg
    }

    /// Last system event code (0 before the first `S`).
    pub fn system_state(&self) -> u8 {
        self.s_state
    }

    /// The book of a locate, if one exists.
    pub fn book(&self, locate: u16) -> Option<&dyn OrderBook> {
        self.books
            .get(locate as usize)
            .and_then(|b| b.as_deref())
            .map(LocateBook::book_ref)
    }

    /// The book kind of a locate, if one exists.
    pub fn locate_book(&self, locate: u16) -> Option<&LocateBook> {
        self.books.get(locate as usize).and_then(|b| b.as_deref())
    }

    /// Locate of a symbol from the directory, if seen.
    pub fn locate_of(&self, symbol: &str) -> Option<u16> {
        let s = pad8(symbol);
        self.stats
            .locates
            .iter()
            .position(|l| l.symbol == s)
            .map(|i| i as u16)
    }

    /// The statistics so far (close hashes are filled by [`Session::finish`]).
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// The event log.
    pub fn event_log(&self) -> &EventLog {
        &self.log
    }

    /// Drive a frame source to its end, then [`Session::finish`]. Wall time and the source's
    /// read time are recorded in `stats.timing`.
    pub fn replay<F: FrameSource>(&mut self, src: &mut F) -> Result<(), ReplayError> {
        let t0 = std::time::Instant::now();
        let mut at = 0u64;
        while let Some(payload) = src.next_frame()? {
            self.apply_payload(payload)
                .map_err(|err| ReplayError::Parse { at, err })?;
            at += 1;
        }
        let wall_ns = t0.elapsed().as_nanos() as u64;
        debug_assert_eq!(self.stats.messages, src.frames());
        self.stats.truncated = src.truncated();
        self.stats.source_cut = src.source_cut();
        self.stats.bytes_in = src.bytes_in();
        self.stats.timing = Some(Timing {
            wall_ns,
            io_ns: src.io_ns(),
        });
        self.finish();
        Ok(())
    }

    /// Parse and apply one payload (the bytes after the 2-byte frame).
    #[inline]
    pub fn apply_payload(&mut self, payload: &[u8]) -> Result<(), ParseError> {
        let m = parse(payload)?;
        self.apply_msg(m);
        Ok(())
    }

    /// Apply one parsed message.
    pub fn apply_msg(&mut self, m: Msg<'_>) {
        let ty = m.ty();
        self.stats.messages += 1;
        self.stats.by_type[ty as usize] += 1;
        if let Some(ts) = m.ts() {
            self.stats.by_type_hour[ty as usize][Stats::hour(ts)] += 1;
            if self.stats.first_ts.is_none() {
                self.stats.first_ts = Some(ts);
            }
            self.stats.last_ts = Some(ts);
        }
        match m {
            Msg::System(s) => {
                self.s_state = s.event_code();
                if s.event_code() == b'Q' {
                    self.after_q = true;
                }
                self.stats.s_events.push((s.event_code(), s.ts()));
            }
            Msg::Directory(r) => self.directory(r.locate(), r.stock()),
            Msg::Action(h) => {
                let l = h.locate();
                self.ensure_locate(l);
                self.stats.locates[l as usize].h_state = h.trading_state();
                self.stats.locates[l as usize].msgs += 1;
            }
            Msg::Add(a) => self.add(a.locate(), a.order_ref(), a.side(), a.price(), a.shares()),
            Msg::AddMpid(f) => self.add(f.locate(), f.order_ref(), f.side(), f.price(), f.shares()),
            Msg::Exec(e) => self.execute(e.locate(), e.order_ref(), e.executed()),
            Msg::ExecPx(c) => self.execute(c.locate(), c.order_ref(), c.executed()),
            Msg::Cancel(x) => self.cancel(x.locate(), x.order_ref(), x.cancelled()),
            Msg::Delete(d) => self.delete(d.locate(), d.order_ref()),
            Msg::Replace(u) => self.replace(
                u.locate(),
                u.original_ref(),
                u.new_ref(),
                u.price(),
                u.shares(),
            ),
            Msg::Trade(_) | Msg::Cross(_) | Msg::Broken(_) | Msg::Other(_) => {
                if let Some(l) = m.locate()
                    && (l as usize) < self.stats.locates.len()
                {
                    self.stats.locates[l as usize].msgs += 1;
                }
            }
            Msg::Unknown(_) => self.stats.unknown_type += 1,
        }
    }

    /// Fill close hashes, per-locate close state and the event-log digest. Idempotent.
    pub fn finish(&mut self) {
        let mut all = Sha256::new();
        for (i, slot) in self.books.iter().enumerate() {
            let Some(b) = slot else { continue };
            let ls = &mut self.stats.locates[i];
            let book = b.book_ref();
            ls.close_hash = book.state_hash();
            ls.live = book.live_orders() as u32;
            if let LocateBook::Array(ab) = b.as_ref() {
                ls.array = true;
                ls.overflow_hits = ab.overflow_hits();
                ls.max_abs_offset = ab.max_abs_offset();
                let c = ab.config();
                ls.window = Some((c.base_px, c.n_levels, c.tick));
            }
            all.update((i as u16).to_le_bytes());
            all.update(ls.close_hash);
        }
        self.stats.all_locates_hash = all.finalize().into();
        self.stats.event_logged = self.cfg.log_events;
        self.stats.event_count = self.log.len();
        self.stats.event_log_hash = self.log.digest();
        self.finished = true;
    }

    /// Consume the session and return the statistics (finishing first if needed).
    pub fn into_stats(mut self) -> Stats {
        if !self.finished {
            self.finish();
        }
        self.stats
    }

    fn ensure_locate(&mut self, locate: u16) {
        let need = locate as usize + 1;
        if self.books.len() < need {
            self.books.resize_with(need, || None);
            self.watched.resize(need, false);
            self.stats.locates.resize_with(need, LocateStats::default);
        }
    }

    fn directory(&mut self, locate: u16, stock: [u8; 8]) {
        self.ensure_locate(locate);
        let i = locate as usize;
        self.stats.locates[i].symbol = stock;
        self.stats.locates[i].msgs += 1;
        if self.cfg.watchlist.matches(locate, Some(&stock)) {
            self.watched[i] = true;
            self.stats.locates[i].watched = true;
        }
        if self.books[i].is_none() {
            self.books[i] = Some(Box::new(LocateBook::Ref(RefBook::new())));
            self.stats.locates[i].has_book = true;
        }
    }

    /// Make sure a book exists for an order message (created on demand, and counted, when no
    /// `R` preceded it) and count the message against the locate.
    #[inline]
    fn ensure_book(&mut self, locate: u16) {
        let i = locate as usize;
        if i >= self.books.len() || self.books[i].is_none() {
            self.ensure_locate(locate);
            self.stats.no_directory += 1;
            if self.cfg.watchlist.matches(locate, None) {
                self.watched[i] = true;
                self.stats.locates[i].watched = true;
            }
            self.books[i] = Some(Box::new(LocateBook::Ref(RefBook::new())));
            self.stats.locates[i].has_book = true;
        }
        self.stats.locates[i].msgs += 1;
    }

    /// The book of a locate that `ensure_book` has seen.
    #[inline]
    fn book_mut(&mut self, locate: u16) -> &mut dyn OrderBook {
        self.books[locate as usize]
            .as_deref_mut()
            .expect("ensure_book ran")
            .book()
    }

    /// For a watched locate: migrate from the reference book to an array window centred on
    /// `px` at the first real add, then track the largest real |tick offset|.
    #[inline]
    fn ensure_array(&mut self, locate: u16, px: Px) {
        let i = locate as usize;
        if !self.watched[i] {
            return;
        }
        let slot = self.books[i].as_deref_mut().expect("book exists");
        let LocateBook::Ref(rb) = slot else {
            if let LocateBook::Array(ab) = slot {
                let c = ab.config();
                let off = ((px as i64 - c.base_px as i64) / c.tick as i64).abs();
                let ls = &mut self.stats.locates[i];
                ls.max_abs_offset_real = ls.max_abs_offset_real.max(off);
            }
            return;
        };
        let n = self.cfg.array_window;
        let tick = tick_for(px as u32);
        let k = ((n / 2) as i64).min(((px - 1) / tick) as i64) as i32;
        let base = px - k * tick;
        let mut ab =
            ArrayBook::with_capacity(BookConfig::new(base, n, tick), self.cfg.array_reserve);
        for (side, lvl_px, fifo) in rb.snapshot() {
            for (id, qty) in fifo {
                ab.add(id, side, lvl_px, qty)
                    .expect("re-adding a resting order into an empty array book");
            }
        }
        *slot = LocateBook::Array(ab);
        self.stats.locates[i].max_abs_offset_real = k as i64;
    }

    /// Log the result, count errors, track live orders; `Some(event)` when the book changed.
    #[inline]
    fn record(&mut self, locate: u16, r: Result<Event, BookError>) -> Option<Event> {
        if self.cfg.log_events {
            self.log.push(logged_event(&r));
        }
        match r {
            Ok(ev) => {
                let delta: i64 = match ev.kind {
                    EventKind::Add => 1,
                    EventKind::Delete => -1,
                    EventKind::Cancel | EventKind::Exec if ev.b == 0 => -1,
                    _ => 0,
                };
                if delta != 0 {
                    let ls = &mut self.stats.locates[locate as usize];
                    ls.live = (ls.live as i64 + delta) as u32;
                    ls.live_hwm = ls.live_hwm.max(ls.live);
                    self.stats.live = (self.stats.live as i64 + delta) as u64;
                    self.stats.live_hwm = self.stats.live_hwm.max(self.stats.live);
                }
                Some(ev)
            }
            Err(e) => {
                match e {
                    BookError::UnknownId { .. } => self.stats.unknown_id += 1,
                    BookError::OverExecute { .. } => self.stats.over_execute += 1,
                    BookError::DuplicateId(_) => self.stats.duplicate_id += 1,
                    BookError::BadQty => self.stats.bad_qty += 1,
                    BookError::BadPrice => self.stats.bad_price += 1,
                }
                None
            }
        }
    }

    /// Crossed-snapshot statistic after a book change.
    #[inline]
    fn check_crossed(&mut self, locate: u16) {
        let i = locate as usize;
        let (bid, ask) = self.books[i]
            .as_deref()
            .expect("book exists")
            .book_ref()
            .l1();
        if let (Some(b), Some(a)) = (bid, ask)
            && b.0 >= a.0
        {
            if self.after_q {
                self.stats.crossed_after_q += 1;
                if self.stats.locates[i].h_state == b'T' {
                    self.stats.crossed_after_q_trading += 1;
                }
            } else {
                self.stats.crossed_before_q += 1;
            }
        }
    }

    fn add(&mut self, locate: u16, id: OrderId, side: Option<Side>, px: u32, qty: Qty) {
        self.ensure_book(locate);
        let Some(side) = side else {
            self.stats.bad_side += 1;
            return;
        };
        if !is_placeholder(px) {
            self.ensure_array(locate, px as Px);
        }
        let r = self.book_mut(locate).add(id, side, px as Px, qty);
        if self.record(locate, r).is_some() {
            self.check_crossed(locate);
        }
    }

    fn execute(&mut self, locate: u16, id: OrderId, qty: Qty) {
        self.ensure_book(locate);
        let book = self.book_mut(locate);
        let ahead = book.queue_ahead(id);
        let r = book.execute(id, qty);
        if let Some(ahead) = ahead {
            self.stats.ec_total += 1;
            if ahead == 0 {
                self.stats.ec_at_head += 1;
            }
        }
        if self.record(locate, r).is_some() {
            self.check_crossed(locate);
        }
    }

    fn cancel(&mut self, locate: u16, id: OrderId, qty: Qty) {
        self.ensure_book(locate);
        let r = self.book_mut(locate).cancel(id, qty);
        if let Some(ev) = self.record(locate, r) {
            if ev.qty < qty as u64 {
                self.stats.over_cancel += 1;
            }
            self.check_crossed(locate);
        }
    }

    fn delete(&mut self, locate: u16, id: OrderId) {
        self.ensure_book(locate);
        let r = self.book_mut(locate).delete(id);
        if self.record(locate, r).is_some() {
            self.check_crossed(locate);
        }
    }

    fn replace(&mut self, locate: u16, old: OrderId, new: OrderId, px: u32, qty: Qty) {
        self.ensure_book(locate);
        if !is_placeholder(px) {
            self.ensure_array(locate, px as Px);
        }
        let r = self.book_mut(locate).replace(old, new, px as Px, qty);
        if self.record(locate, r).is_some() {
            self.check_crossed(locate);
        }
    }
}

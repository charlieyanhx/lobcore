//! The frozen `OrderBook` trait, its error type, the window configuration and the snapshot
//! type shared by `ArrayBook` and `RefBook`.
//!
//! Error contract: on `Err` the book state is unchanged (state hash before == after) and
//! `err.event()` is the `unknown` / `reject` event to log. The error carries everything the
//! event needs, so neither book stores a "last error" field.

use crate::types::{Event, EventKind, OrderId, Px, Qty, Side};

/// Window configuration of an `ArrayBook`: prices `base_px + i * tick`, `0 <= i < n_levels`,
/// live in the array; every other positive price goes to the overflow map. `n_levels <= 4096`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookConfig {
    pub base_px: Px,
    pub n_levels: u32,
    pub tick: Px,
}

impl BookConfig {
    pub const fn new(base_px: Px, n_levels: u32, tick: Px) -> BookConfig {
        BookConfig {
            base_px,
            n_levels,
            tick,
        }
    }
}

/// Every non-empty level, best first per side (bids descending, then asks ascending), with its
/// FIFO as `(id, qty)` pairs. The differential comparator.
pub type Snapshot = Vec<(Side, Px, Vec<(OrderId, Qty)>)>;

/// `(px, qty, count)` of one level.
pub type LevelView = (Px, Qty, u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BookError {
    /// The id named by a cancel / delete / execute / replace is not live.
    UnknownId { kind: EventKind, id: OrderId },
    /// Execute asked for more than the order's remaining quantity.
    OverExecute { id: OrderId, have: Qty, want: Qty },
    /// Add or replace named an id that is already live.
    DuplicateId(OrderId),
    /// Zero quantity, or a level total that would exceed `u32::MAX`.
    BadQty,
    /// Non-positive price.
    BadPrice,
}

impl BookError {
    /// The event to log for this error (`unknown` for `UnknownId`, `reject` otherwise). Derived
    /// from the error alone so both book implementations agree by construction.
    pub const fn event(&self) -> Event {
        match *self {
            BookError::UnknownId { kind, id } => Event::unknown(kind, id),
            BookError::OverExecute { id, want, .. } => {
                Event::reject(EventKind::Exec, id, want as u64)
            }
            BookError::DuplicateId(id) => Event::reject(EventKind::Add, id, 0),
            BookError::BadQty | BookError::BadPrice => Event::new(EventKind::Reject, 0, 0, 0, 0, 0),
        }
    }
}

impl std::fmt::Display for BookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BookError::UnknownId { kind, id } => write!(f, "unknown order id {id} in {kind:?}"),
            BookError::OverExecute { id, have, want } => {
                write!(f, "over-execute of order {id}: have {have}, want {want}")
            }
            BookError::DuplicateId(id) => write!(f, "duplicate order id {id}"),
            BookError::BadQty => write!(f, "bad quantity (zero or level overflow)"),
            BookError::BadPrice => write!(f, "bad price (non-positive)"),
        }
    }
}

impl std::error::Error for BookError {}

/// The L3 book interface. Every mutating call returns exactly one event on success and one
/// error (with `BookError::event`) on failure, so a log built from the returned values is the
/// canonical event log.
///
/// Error precedence (identical in both implementations): `add` checks `BadQty` (zero), then
/// `BadPrice`, then level overflow (`BadQty`), then `DuplicateId`; `cancel` / `delete` /
/// `execute` check `UnknownId` first, then `BadQty` (zero), then `OverExecute`; `replace`
/// checks `UnknownId(old)`, `DuplicateId(new)`, `BadQty`, `BadPrice`, then level overflow.
pub trait OrderBook {
    /// Rest a new order at the tail of its level. `id` must be unknown, `px > 0`, `qty > 0`.
    fn add(&mut self, id: OrderId, side: Side, px: Px, qty: Qty) -> Result<Event, BookError>;
    /// Partial cancel in place (priority kept); `qty >= remaining` removes the order. `qty > 0`.
    fn cancel(&mut self, id: OrderId, qty: Qty) -> Result<Event, BookError>;
    /// Remove the order.
    fn delete(&mut self, id: OrderId) -> Result<Event, BookError>;
    /// Execute `qty` from the order by id (ITCH E/C); `qty > remaining` rejects with no change.
    fn execute(&mut self, id: OrderId, qty: Qty) -> Result<Event, BookError>;
    /// ITCH U: delete `old`, add `new` at the tail of the (possibly same) level; side carried
    /// from the old order; `new` must be unknown.
    fn replace(&mut self, old: OrderId, new: OrderId, px: Px, qty: Qty)
    -> Result<Event, BookError>;
    /// Best bid and best ask as `(px, qty, count)`.
    fn l1(&self) -> (Option<LevelView>, Option<LevelView>);
    /// Up to `depth` levels per side, best first.
    fn l2(&self, depth: usize) -> (Vec<LevelView>, Vec<LevelView>);
    /// Exact quantity resting ahead of `id` in its level's FIFO; `None` if unknown.
    fn queue_ahead(&self, id: OrderId) -> Option<Qty>;
    /// Number of live orders.
    fn live_orders(&self) -> usize;
    /// Book-state hash: sha256 over bid side then ask side, ascending price, each level as
    /// `i64 px, u32 count` then `(u64 id, u32 qty)` in FIFO order, all little-endian.
    fn state_hash(&self) -> [u8; 32];
    /// The differential comparator.
    fn snapshot(&self) -> Snapshot;
}

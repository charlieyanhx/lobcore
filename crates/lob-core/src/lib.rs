//! lob-core: a bounded-array L3 order book (`ArrayBook`), the BTreeMap reference book
//! (`RefBook`) with the identical `OrderBook` API, the 34-byte event record and sha256 event
//! log, the book-state hash, and the `ops.bin` op-sequence format.
//!
//! Units: prices are `i32` in 1e-4 units (ITCH Price(4)), positive in the API; quantities are
//! `u32`. Semantics follow ITCH 5.0: `cancel` reduces in place (priority kept, X), `execute`
//! reduces by id (E/C), `replace` is delete + add at the tail with the side carried from the
//! original (U, spec 1.4.5). `bid < ask` is not an invariant here (books are legitimately
//! crossed pre-open); it is a replay statistic and a matcher invariant.
//!
//! Every mutating call returns one `Event` or one `BookError` whose `event()` is the record to
//! log; on `Err` the state is unchanged. Hot-path operations at in-range prices allocate
//! nothing once the book is `reserve`d (see `tests/alloc_count.rs`).

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod api;
pub mod book;
pub mod hash;
pub mod levels;
pub mod ops;
pub mod reference;
pub mod slab;
pub mod types;

pub use api::{BookConfig, BookError, LevelView, OrderBook, Snapshot};
pub use book::ArrayBook;
pub use levels::{Bitmap, Level, MAX_LEVELS};
pub use ops::{Op, OpsError, decode_ops, encode_ops, replay};
pub use reference::RefBook;
pub use slab::Slot;
pub use types::{
    EVENT_LOG_VERSION, EVENT_RECORD_LEN, Event, EventKind, EventLog, OrderId, Px, Qty, Side, hex,
};

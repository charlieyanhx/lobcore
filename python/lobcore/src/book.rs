//! `lobcore.Book` (the bounded-array book) and `lobcore.RefBook` (the BTreeMap reference book)
//! with the identical surface. Both hold their own event log so a Python driver gets the
//! Rust event-log digest of everything it applied, rejections included.

use lob_core::{
    ArrayBook, BookConfig, BookError, Event, EventLog, MAX_LEVELS, OrderBook, Px,
    RefBook as CoreRefBook,
};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyTuple};

use crate::{book_error, event_tuple, side_from_py};

/// numpy dtype of an L2 side: `[('px', '<i4'), ('qty', '<u4'), ('count', '<u4')]`.
const L2_DTYPE: [(&str, &str); 3] = [("px", "<i4"), ("qty", "<u4"), ("count", "<u4")];

/// Log the result and convert it for Python.
fn finish<'py>(
    py: Python<'py>,
    log: &mut EventLog,
    r: Result<Event, BookError>,
) -> PyResult<Bound<'py, PyTuple>> {
    log.push(lob_core::ops::logged_event(&r));
    match r {
        Ok(e) => event_tuple(py, &e),
        Err(e) => Err(book_error(py, e)),
    }
}

fn level_tuple<'py>(py: Python<'py>, l: Option<(Px, u32, u32)>) -> PyResult<Bound<'py, PyAny>> {
    match l {
        None => Ok(py.None().into_bound(py)),
        Some((px, qty, count)) => {
            Ok(PyTuple::new(py, [px as i64, qty as i64, count as i64])?.into_any())
        }
    }
}

/// One side as a numpy structured array.
fn l2_array<'py>(py: Python<'py>, levels: &[(Px, u32, u32)]) -> PyResult<Bound<'py, PyAny>> {
    let np = py.import("numpy")?;
    let dtype = np.getattr("dtype")?.call1((L2_DTYPE.to_vec(),))?;
    let arr = np.getattr("empty")?.call1((levels.len(), dtype))?;
    let px: Vec<i32> = levels.iter().map(|l| l.0).collect();
    let qty: Vec<u32> = levels.iter().map(|l| l.1).collect();
    let count: Vec<u32> = levels.iter().map(|l| l.2).collect();
    arr.set_item("px", numpy::PyArray1::from_vec(py, px))?;
    arr.set_item("qty", numpy::PyArray1::from_vec(py, qty))?;
    arr.set_item("count", numpy::PyArray1::from_vec(py, count))?;
    Ok(arr)
}

fn snapshot_list<'py>(py: Python<'py>, book: &dyn OrderBook) -> PyResult<Bound<'py, PyAny>> {
    let snap = book.snapshot();
    let out = pyo3::types::PyList::empty(py);
    for (side, px, fifo) in snap {
        let fifo: Vec<(u64, u32)> = fifo;
        out.append((side.code(), px, fifo))?;
    }
    Ok(out.into_any())
}

/// The shared `#[pymethods]` body of both book classes (one block per class: pyo3 without
/// the `multiple-pymethods` feature allows exactly one), plus the class-specific items.
macro_rules! book_methods {
    ($name:ident, { $($extra:tt)* }) => {
        #[pymethods]
        impl $name {
            $($extra)*

            /// Rest a new order at the tail of its level.
            #[pyo3(signature = (id, side, px, qty))]
            fn add<'py>(&mut self, py: Python<'py>, id: u64, side: &Bound<'py, PyAny>, px: i32, qty: u32) -> PyResult<Bound<'py, PyTuple>> {
                let side = side_from_py(side)?;
                let r = self.book.add(id, side, px, qty);
                finish(py, &mut self.log, r)
            }

            /// Partial cancel in place (ITCH X); `qty >= remaining` removes the order.
            fn cancel<'py>(&mut self, py: Python<'py>, id: u64, qty: u32) -> PyResult<Bound<'py, PyTuple>> {
                let r = self.book.cancel(id, qty);
                finish(py, &mut self.log, r)
            }

            /// Remove the order (ITCH D).
            fn delete<'py>(&mut self, py: Python<'py>, id: u64) -> PyResult<Bound<'py, PyTuple>> {
                let r = self.book.delete(id);
                finish(py, &mut self.log, r)
            }

            /// Execute `qty` by id (ITCH E/C); more than the remaining quantity rejects.
            fn execute<'py>(&mut self, py: Python<'py>, id: u64, qty: u32) -> PyResult<Bound<'py, PyTuple>> {
                let r = self.book.execute(id, qty);
                finish(py, &mut self.log, r)
            }

            /// ITCH U: delete `old`, rest `new` at the tail of its level with the side carried.
            fn replace<'py>(&mut self, py: Python<'py>, old: u64, new: u64, px: i32, qty: u32) -> PyResult<Bound<'py, PyTuple>> {
                let r = self.book.replace(old, new, px, qty);
                finish(py, &mut self.log, r)
            }

            /// `((bid_px, bid_qty, bid_count) | None, (ask_px, ask_qty, ask_count) | None)`.
            fn l1<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
                let (b, a) = self.book.l1();
                PyTuple::new(py, [level_tuple(py, b)?, level_tuple(py, a)?])
            }

            /// `{"bid": array, "ask": array}` of up to `depth` levels per side, best first, each a
            /// numpy structured array with fields `px` (int32), `qty` (uint32), `count` (uint32).
            #[pyo3(signature = (depth = 5))]
            fn l2<'py>(&self, py: Python<'py>, depth: usize) -> PyResult<Bound<'py, PyDict>> {
                let (b, a) = self.book.l2(depth);
                let d = PyDict::new(py);
                d.set_item("bid", l2_array(py, &b)?)?;
                d.set_item("ask", l2_array(py, &a)?)?;
                Ok(d)
            }

            /// Exact quantity resting ahead of `id` in its level's FIFO, or `None` if unknown.
            fn queue_ahead(&self, id: u64) -> Option<u32> {
                self.book.queue_ahead(id)
            }

            /// Book-state hash (32 bytes): sha256 over bid side then ask side, ascending price,
            /// `i64 px, u32 count` then `(u64 id, u32 qty)` in FIFO order, little-endian.
            fn state_hash<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
                PyBytes::new(py, &self.book.state_hash())
            }

            /// Number of live orders.
            #[getter]
            fn live_orders(&self) -> usize {
                self.book.live_orders()
            }

            /// Every non-empty level, best first per side: `[(side, px, [(id, qty), ...]), ...]`.
            fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
                snapshot_list(py, &self.book)
            }

            /// sha256 of the events this object returned or raised, in order (version byte first).
            fn event_log_hash<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
                PyBytes::new(py, &self.log.digest())
            }

            /// Records in this object's event log.
            #[getter]
            fn event_count(&self) -> u64 {
                self.log.len()
            }
        }
    };
}

/// The bounded-array L3 book: `n_levels` prices `base_px + i * tick` per side live in a flat
/// array; every other positive price goes to the overflow map (correct, slower, counted).
#[pyclass(module = "lobcore")]
pub struct Book {
    book: ArrayBook,
    log: EventLog,
}

book_methods!(Book, {
    /// `Book(base_px, n_levels, tick=100)`; prices in 1e-4 units, `1 <= n_levels <= 4096`.
    #[new]
    #[pyo3(signature = (base_px, n_levels, tick = 100))]
    fn new(base_px: i32, n_levels: u32, tick: i32) -> PyResult<Book> {
        if n_levels == 0 || n_levels > MAX_LEVELS {
            return Err(PyValueError::new_err(format!(
                "n_levels must be in 1..={MAX_LEVELS}, got {n_levels}"
            )));
        }
        if tick <= 0 {
            return Err(PyValueError::new_err(format!(
                "tick must be positive, got {tick}"
            )));
        }
        if base_px <= 0 {
            return Err(PyValueError::new_err(format!(
                "base_px must be positive, got {base_px}"
            )));
        }
        let top = base_px as i64 + (n_levels as i64 - 1) * tick as i64;
        if top > i32::MAX as i64 {
            return Err(PyValueError::new_err(format!(
                "window top {top} exceeds the price range"
            )));
        }
        Ok(Book {
            book: ArrayBook::with_capacity(BookConfig::new(base_px, n_levels, tick), 1024),
            log: EventLog::new(),
        })
    }

    /// `(base_px, n_levels, tick)`.
    #[getter]
    fn config(&self) -> (i32, u32, i32) {
        let c = self.book.config();
        (c.base_px, c.n_levels, c.tick)
    }

    /// Adds / replaces routed to the overflow map so far.
    #[getter]
    fn overflow_hits(&self) -> u64 {
        self.book.overflow_hits()
    }

    /// Largest `|(px - base_px) / tick|` seen by an add.
    #[getter]
    fn max_abs_offset(&self) -> i64 {
        self.book.max_abs_offset()
    }

    /// Run the I1-I8 invariant checker; raises `AssertionError` with the first violation.
    fn check(&self) -> PyResult<()> {
        self.book
            .check()
            .map_err(pyo3::exceptions::PyAssertionError::new_err)
    }
});

/// The BTreeMap reference book with the identical surface and semantics; no window.
#[pyclass(module = "lobcore")]
pub struct RefBook {
    book: CoreRefBook,
    log: EventLog,
}

book_methods!(RefBook, {
    #[new]
    fn new() -> RefBook {
        RefBook {
            book: CoreRefBook::new(),
            log: EventLog::new(),
        }
    }
});

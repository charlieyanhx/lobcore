//! `lobcore` Python module: the read-only v0.1 surface over lob-core, lob-feed, lob-features
//! and lob-synth.
//!
//! Units and conventions are the Rust ones: prices are integers in 1e-4 units (ITCH Price(4)),
//! quantities are shares, timestamps are nanoseconds since midnight. Every book event is the
//! tuple `(kind, a, b, px, qty, side)` with `kind` one of the event-log type names
//! (`add cancel exec delete replace modify unknown reject trade stp_cancel ioc_cancel
//! fok_reject`), `side` 0 = bid / 1 = ask, and exactly the field conventions of
//! `lob_core::types` so `event_encode` / `event_log_hash` reproduce the Rust digests.
//!
//! Errors: an unknown order id raises `lobcore.UnknownId` (a `LookupError`), every other
//! `BookError` raises `lobcore.BookReject` (a `ValueError`); both carry the Rust error text and
//! an `.event` attribute holding the `unknown` / `reject` record the Rust event log would hash.
//!
//! The batch entry points (`replay_itch`, `replay_stats`, `synth_itch`) run their Rust loop
//! with the GIL released (`Python::detach`) and hand back numpy arrays or `bytes`.

#![deny(unsafe_code)]

mod book;
mod replay;
mod synth;

use lob_core::{
    BookError, EVENT_LOG_VERSION, EVENT_RECORD_LEN, Event, EventKind, MAX_LEVELS, Side,
};
use pyo3::create_exception;
use pyo3::exceptions::{PyLookupError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyTuple};

create_exception!(
    lobcore,
    UnknownId,
    PyLookupError,
    "A cancel / delete / execute / replace named an order id that is not live."
);
create_exception!(
    lobcore,
    BookReject,
    PyValueError,
    "The book rejected the operation (over-execute, duplicate id, bad quantity or price)."
);

/// Event type names in code order (index = code).
const KIND_NAMES: [&str; 13] = [
    "",
    "add",
    "cancel",
    "exec",
    "delete",
    "replace",
    "modify",
    "unknown",
    "reject",
    "trade",
    "stp_cancel",
    "ioc_cancel",
    "fok_reject",
];

/// The name of an event kind.
pub(crate) fn kind_name(k: EventKind) -> &'static str {
    KIND_NAMES[k.code() as usize]
}

/// The kind of an event name.
pub(crate) fn kind_of(name: &str) -> Option<EventKind> {
    KIND_NAMES
        .iter()
        .position(|&n| !n.is_empty() && n == name)
        .and_then(|i| EventKind::from_code(i as u8))
}

/// `(kind, a, b, px, qty, side)`.
pub(crate) fn event_tuple<'py>(py: Python<'py>, e: &Event) -> PyResult<Bound<'py, PyTuple>> {
    PyTuple::new(
        py,
        [
            kind_name(e.kind).into_pyobject(py)?.into_any(),
            e.a.into_pyobject(py)?.into_any(),
            e.b.into_pyobject(py)?.into_any(),
            e.px.into_pyobject(py)?.into_any(),
            e.qty.into_pyobject(py)?.into_any(),
            e.side.into_pyobject(py)?.into_any(),
        ],
    )
}

/// Parse `(kind, a, b, px, qty, side)`.
pub(crate) fn event_from_tuple(obj: &Bound<'_, PyAny>) -> PyResult<Event> {
    let t: (String, u64, u64, i64, u64, u8) = obj.extract().map_err(|_| {
        PyTypeError::new_err(
            "event must be a (kind: str, a: int, b: int, px: int, qty: int, side: int) tuple",
        )
    })?;
    let kind = kind_of(&t.0)
        .ok_or_else(|| PyValueError::new_err(format!("unknown event kind {:?}", t.0)))?;
    Ok(Event::new(kind, t.1, t.2, t.3, t.4, t.5))
}

/// Map a `BookError` to the Python exception carrying the Rust text and the `.event` record.
pub(crate) fn book_error(py: Python<'_>, e: BookError) -> PyErr {
    let msg = e.to_string();
    let ty = match e {
        BookError::UnknownId { .. } => py.get_type::<UnknownId>(),
        _ => py.get_type::<BookReject>(),
    };
    let build = || -> PyResult<PyErr> {
        let exc = ty.call1((msg.clone(),))?;
        exc.setattr("event", event_tuple(py, &e.event())?)?;
        Ok(PyErr::from_value(exc))
    };
    build().unwrap_or_else(|err| err)
}

/// Accept `"B"` / `"S"`, `"bid"` / `"ask"` (any case) or `0` / `1`.
pub(crate) fn side_from_py(obj: &Bound<'_, PyAny>) -> PyResult<Side> {
    if let Ok(s) = obj.extract::<String>() {
        return match s.to_ascii_lowercase().as_str() {
            "b" | "bid" | "buy" => Ok(Side::Bid),
            "s" | "a" | "ask" | "sell" => Ok(Side::Ask),
            _ => Err(PyValueError::new_err(format!(
                "side must be 'B'/'S', 'bid'/'ask' or 0/1, got {s:?}"
            ))),
        };
    }
    let code: u8 = obj
        .extract()
        .map_err(|_| PyTypeError::new_err("side must be 'B'/'S', 'bid'/'ask' or 0/1"))?;
    Side::from_code(code).ok_or_else(|| {
        PyValueError::new_err(format!("side must be 0 (bid) or 1 (ask), got {code}"))
    })
}

/// The 34-byte little-endian record of one event.
#[pyfunction]
fn event_encode<'py>(py: Python<'py>, event: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    let e = event_from_tuple(event)?;
    Ok(PyBytes::new(py, &e.encode()))
}

/// sha256 over the format-version byte then every event's 34-byte record, in order: the
/// event-log hash of `docs/DESIGN.md` section 5.
#[pyfunction]
fn event_log_hash<'py>(
    py: Python<'py>,
    events: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyBytes>> {
    let mut log = lob_core::EventLog::new();
    for item in events.try_iter()? {
        log.push(event_from_tuple(&item?)?);
    }
    Ok(PyBytes::new(py, &log.digest()))
}

/// sha256 of arbitrary bytes (the fixture pin helper).
#[pyfunction]
fn sha256<'py>(py: Python<'py>, data: &[u8]) -> Bound<'py, PyBytes> {
    PyBytes::new(py, &lob_synth::sha256(data))
}

#[pymodule]
fn lobcore(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("EVENT_LOG_VERSION", EVENT_LOG_VERSION)?;
    m.add("EVENT_RECORD_LEN", EVENT_RECORD_LEN)?;
    m.add("MAX_LEVELS", MAX_LEVELS)?;
    m.add("UnknownId", py.get_type::<UnknownId>())?;
    m.add("BookReject", py.get_type::<BookReject>())?;
    m.add_class::<book::Book>()?;
    m.add_class::<book::RefBook>()?;
    m.add_function(wrap_pyfunction!(event_encode, m)?)?;
    m.add_function(wrap_pyfunction!(event_log_hash, m)?)?;
    m.add_function(wrap_pyfunction!(sha256, m)?)?;
    m.add_function(wrap_pyfunction!(replay::replay_itch, m)?)?;
    m.add_function(wrap_pyfunction!(replay::replay_stats, m)?)?;
    m.add_function(wrap_pyfunction!(synth::synth_itch, m)?)?;
    Ok(())
}

//! `lobcore.synth_itch`: the seeded synthetic ITCH day as `bytes`, optionally with its truth
//! sidecar as numpy arrays.

use lob_synth::{SynthConfig, SynthDay, synth_itch as synth, synth_itch_bytes};
use numpy::PyArray1;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

/// Build a `SynthConfig` from keyword arguments; unknown keys are a `TypeError`.
fn config(kw: Option<&Bound<'_, PyDict>>) -> PyResult<SynthConfig> {
    let mut cfg = SynthConfig::default();
    let Some(kw) = kw else { return Ok(cfg) };
    for (k, v) in kw.iter() {
        let key: String = k.extract()?;
        match key.as_str() {
            "locates" => cfg.locates = v.extract()?,
            "mix" => {
                let m: Vec<f32> = v.extract()?;
                if m.len() != 9 {
                    return Err(PyValueError::new_err(
                        "mix must have 9 weights over A F D X U E C P L",
                    ));
                }
                cfg.mix.copy_from_slice(&m);
            }
            "placeholder_rate" => cfg.placeholder_rate = v.extract()?,
            "unknown_ref_rate" => cfg.unknown_ref_rate = v.extract()?,
            "subpenny" => cfg.subpenny = v.extract()?,
            "crossed_preopen" => cfg.crossed_preopen = v.extract()?,
            "open_ns" => cfg.open_ns = v.extract()?,
            "close_ns" => cfg.close_ns = v.extract()?,
            other => {
                return Err(PyTypeError::new_err(format!(
                    "synth_itch() got an unexpected keyword argument {other:?}"
                )));
            }
        }
    }
    if cfg.locates == 0 {
        return Err(PyValueError::new_err("locates must be at least 1"));
    }
    Ok(cfg)
}

fn truth_dict<'py>(py: Python<'py>, day: &SynthDay) -> PyResult<Bound<'py, PyDict>> {
    let n = day.truth.len();
    let mut ts = Vec::with_capacity(n);
    let mut locate = Vec::with_capacity(n);
    let mut bid_px = Vec::with_capacity(n);
    let mut bid_qty = Vec::with_capacity(n);
    let mut ask_px = Vec::with_capacity(n);
    let mut ask_qty = Vec::with_capacity(n);
    let mut live = Vec::with_capacity(n);
    let mut hashes = Vec::with_capacity(n * 32);
    for t in &day.truth {
        ts.push(t.ts);
        locate.push(t.locate);
        let (bp, bq) = t.best_bid.unwrap_or((0, 0));
        let (ap, aq) = t.best_ask.unwrap_or((0, 0));
        bid_px.push(bp);
        bid_qty.push(bq);
        ask_px.push(ap);
        ask_qty.push(aq);
        live.push(t.live_orders);
        hashes.extend_from_slice(&t.book_hash);
    }
    let d = PyDict::new(py);
    d.set_item("ts", PyArray1::from_vec(py, ts))?;
    d.set_item("locate", PyArray1::from_vec(py, locate))?;
    d.set_item("bid_px", PyArray1::from_vec(py, bid_px))?;
    d.set_item("bid_qty", PyArray1::from_vec(py, bid_qty))?;
    d.set_item("ask_px", PyArray1::from_vec(py, ask_px))?;
    d.set_item("ask_qty", PyArray1::from_vec(py, ask_qty))?;
    d.set_item("live_orders", PyArray1::from_vec(py, live))?;
    // One 32-byte digest per message as a `S32` array: sliceable, comparable to `bytes`.
    let np = py.import("numpy")?;
    let raw = PyArray1::from_vec(py, hashes);
    let hash_arr = np.getattr("frombuffer")?.call1((raw, "S32"))?;
    d.set_item("book_hash", hash_arr)?;
    Ok(d)
}

/// `synth_itch(seed, n, truth=False, **cfg) -> bytes | (bytes, dict)`: exactly `n` framed
/// ITCH 5.0 messages (emi 2-byte big-endian length framing) for `(seed, cfg)`, byte-identical
/// to `lobcore synth --seed SEED --n N`. With `truth=True` the per-message truth sidecar is
/// returned too (`ts, locate, bid_px, bid_qty, ask_px, ask_qty, live_orders, book_hash`).
/// Config keys: `locates, mix, placeholder_rate, unknown_ref_rate, subpenny, crossed_preopen,
/// open_ns, close_ns`.
#[pyfunction]
#[pyo3(signature = (seed, n, truth = false, **cfg))]
pub fn synth_itch<'py>(
    py: Python<'py>,
    seed: u64,
    n: u64,
    truth: bool,
    cfg: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    let cfg = config(cfg)?;
    let min = 2 * cfg.locates as u64 + 6;
    if n < min {
        return Err(PyValueError::new_err(format!(
            "n must be at least 2 * locates + 6 = {min}, got {n}"
        )));
    }
    if truth {
        let day = py.detach(|| synth(seed, n, &cfg));
        let bytes = PyBytes::new(py, &day.bytes);
        let t = truth_dict(py, &day)?;
        Ok((bytes, t).into_pyobject(py)?.into_any())
    } else {
        let bytes = py.detach(|| synth_itch_bytes(seed, n, &cfg));
        Ok(PyBytes::new(py, &bytes).into_any())
    }
}

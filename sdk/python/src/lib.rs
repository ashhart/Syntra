//! Python bindings for [`syntra::client::LocalDecider`].
//!
//! Decisions run in-process on the Rust decision core; the Python package
//! (`python/syntra`) adds the pure-Python HTTP client and type hints.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use serde_json::{Map, Number, Value};
use syntra::client::{Background, ClientError, DecideOptions, FlushReport, LocalDecider};
use syntra::decision::ActionSpec;

pyo3::create_exception!(syntra._native, SyntraError, PyRuntimeError, "An error from Syntra.");

fn to_py_err(e: ClientError) -> PyErr {
    SyntraError::new_err(e.0)
}

/// Deepest nesting accepted in a context or detail object.
const MAX_DEPTH: usize = 64;

/// Convert a Python value (dict, list, tuple, str, int, float, bool, None)
/// to JSON.
fn to_json(obj: &Bound<'_, PyAny>, depth: usize) -> PyResult<Value> {
    if depth > MAX_DEPTH {
        return Err(PyValueError::new_err("value is nested too deeply"));
    }
    if obj.is_none() {
        return Ok(Value::Null);
    }
    // bool before int: Python's bool is an int subclass.
    if let Ok(b) = obj.cast::<PyBool>() {
        return Ok(Value::Bool(b.is_true()));
    }
    if obj.cast::<PyInt>().is_ok() {
        if let Ok(v) = obj.extract::<i64>() {
            return Ok(Value::Number(v.into()));
        }
        if let Ok(v) = obj.extract::<u64>() {
            return Ok(Value::Number(v.into()));
        }
        return Err(PyValueError::new_err("integer does not fit in 64 bits"));
    }
    if let Ok(f) = obj.cast::<PyFloat>() {
        let v = f.value();
        return Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| PyValueError::new_err("NaN and infinity are not valid JSON numbers"));
    }
    if let Ok(s) = obj.cast::<PyString>() {
        return Ok(Value::String(s.to_str()?.to_string()));
    }
    if let Ok(d) = obj.cast::<PyDict>() {
        let mut map = Map::with_capacity(d.len());
        for (k, v) in d.iter() {
            let key = k
                .cast::<PyString>()
                .map_err(|_| PyTypeError::new_err("object keys must be strings"))?
                .to_str()?
                .to_string();
            map.insert(key, to_json(&v, depth + 1)?);
        }
        return Ok(Value::Object(map));
    }
    if let Ok(l) = obj.cast::<PyList>() {
        return l.iter().map(|v| to_json(&v, depth + 1)).collect::<PyResult<Vec<_>>>().map(Value::Array);
    }
    if let Ok(t) = obj.cast::<PyTuple>() {
        return t.iter().map(|v| to_json(&v, depth + 1)).collect::<PyResult<Vec<_>>>().map(Value::Array);
    }
    Err(PyTypeError::new_err(format!(
        "cannot convert {} to JSON",
        obj.get_type().name()?
    )))
}

/// A decision made in-process.
#[pyclass(frozen, get_all, module = "syntra._native")]
struct Decision {
    /// Pass this to `reward`.
    decision_id: String,
    action: String,
    action_index: usize,
    /// Probability with which `action` was chosen (its propensity).
    probability: f64,
    /// Eligible actions and their probabilities, most probable first.
    ranking: Vec<(String, f64)>,
    model_version: u64,
}

#[pymethods]
impl Decision {
    /// Alias of `decision_id`.
    #[getter]
    fn id(&self) -> &str {
        &self.decision_id
    }

    fn __repr__(&self) -> String {
        format!(
            "Decision(action={:?}, probability={:.4}, decision_id={:?}, model_version={})",
            self.action, self.probability, self.decision_id, self.model_version
        )
    }
}

fn report_dict<'py>(py: Python<'py>, r: &FlushReport) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("decisions_accepted", r.decisions_accepted)?;
    d.set_item("decisions_rejected", r.decisions_rejected)?;
    d.set_item("rewards_applied", r.rewards_applied)?;
    d.set_item("rewards_failed", r.rewards_failed)?;
    d.set_item("requeued", r.requeued)?;
    d.set_item("errors", r.errors.clone())?;
    Ok(d)
}

/// Decides in-process against a capsule's published model; decisions and
/// rewards upload to the server, which verifies and learns from them.
#[pyclass(frozen, module = "syntra._native")]
struct NativeDecider {
    inner: Arc<LocalDecider>,
    background: Mutex<Option<Background>>,
}

#[pymethods]
impl NativeDecider {
    #[new]
    #[pyo3(signature = (url, token, tenant, job, capsule, max_queue = 100_000))]
    fn new(
        py: Python<'_>,
        url: &str,
        token: &str,
        tenant: &str,
        job: &str,
        capsule: &str,
        max_queue: usize,
    ) -> PyResult<Self> {
        let decider = py
            .detach(|| LocalDecider::connect(url, token, tenant, job, capsule))
            .map_err(to_py_err)?
            .with_max_queue(max_queue);
        Ok(NativeDecider {
            inner: Arc::new(decider),
            background: Mutex::new(None),
        })
    }

    #[pyo3(signature = (context = None, actions = None, exclude = None, baseline = None))]
    fn decide(
        &self,
        context: Option<&Bound<'_, PyAny>>,
        actions: Option<&Bound<'_, PyAny>>,
        exclude: Option<Vec<String>>,
        baseline: Option<String>,
    ) -> PyResult<Decision> {
        let context = match context {
            Some(c) => to_json(c, 0)?,
            None => Value::Null,
        };
        let actions = match actions {
            Some(a) if !a.is_none() => {
                let v = to_json(a, 0)?;
                Some(
                    serde_json::from_value::<Vec<ActionSpec>>(v)
                        .map_err(|e| PyValueError::new_err(format!("invalid actions: {e}")))?,
                )
            }
            _ => None,
        };
        let options = DecideOptions {
            actions,
            excluded_actions: exclude.unwrap_or_default(),
            baseline_action: baseline,
        };
        let d = self
            .inner
            .decide_with(context, options)
            .map_err(to_py_err)?;
        Ok(Decision {
            decision_id: d.decision_id,
            action: d.action,
            action_index: d.action_index,
            probability: d.probability,
            ranking: d.ranking,
            model_version: d.model_version,
        })
    }

    #[pyo3(signature = (decision_id, reward, idempotency_key = None, detail = None))]
    fn reward(
        &self,
        decision_id: &str,
        reward: f64,
        idempotency_key: Option<&str>,
        detail: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let detail = match detail {
            Some(d) if !d.is_none() => Some(to_json(d, 0)?),
            _ => None,
        };
        self.inner
            .reward_with(decision_id, reward, idempotency_key, detail)
            .map_err(to_py_err)
    }

    fn flush<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let inner = self.inner.clone();
        let report = py.detach(move || inner.flush()).map_err(to_py_err)?;
        report_dict(py, &report)
    }

    fn sync(&self, py: Python<'_>) -> PyResult<bool> {
        let inner = self.inner.clone();
        py.detach(move || inner.sync()).map_err(to_py_err)
    }

    /// Flush and sync every `interval` seconds on a background thread.
    fn start_background(&self, interval: f64) -> PyResult<()> {
        if !(interval.is_finite() && interval > 0.0) {
            return Err(PyValueError::new_err("interval must be a positive number of seconds"));
        }
        let mut bg = self.background.lock().unwrap();
        if bg.is_none() {
            *bg = Some(self.inner.start_background(Duration::from_secs_f64(interval)));
        }
        Ok(())
    }

    /// Stop the background thread (after its final flush) and flush.
    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let bg = self.background.lock().unwrap().take();
        let inner = self.inner.clone();
        let report = py
            .detach(move || {
                drop(bg);
                inner.flush()
            })
            .map_err(to_py_err)?;
        report_dict(py, &report)
    }

    #[getter]
    fn model_version(&self) -> u64 {
        self.inner.model_version()
    }

    #[getter]
    fn model_tag(&self) -> String {
        self.inner.model_tag()
    }

    #[getter]
    fn pending(&self) -> usize {
        self.inner.pending()
    }
}

impl Drop for NativeDecider {
    fn drop(&mut self) {
        // Never block a finalizer on the network: the thread makes its last
        // flush on its own. Call close() to wait for it.
        if let Some(bg) = self.background.get_mut().ok().and_then(Option::take) {
            bg.detach();
        }
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<NativeDecider>()?;
    m.add_class::<Decision>()?;
    m.add("SyntraError", m.py().get_type::<SyntraError>())?;
    Ok(())
}

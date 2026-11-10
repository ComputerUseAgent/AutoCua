//! MemoryTracker: how full the MAIN agent's working memory is right now, for
//! its memory bar.
//!
//! The bar answers "how full is the MAIN agent's working memory right now?",
//! NOT "how many tokens have we used in total". Every turn the agent rebuilds
//! and re-sends its whole (trimmed) context, and the provider reports the
//! true size of that exact prompt. The bar shows that latest size against a
//! FIXED budget, MEMORY_CAP (300k), so it rises as the history grows and
//! DROPS when the runtime memory optimization (stripping old images and
//! element trees) or a compression kicks in.
//!
//! The cap is fixed on purpose and is NOT the model's context window: memory
//! compression keeps the agent's real context under this budget, so 300k is
//! the headroom gauge for it.
//!
//! `context_tokens` is the full prompt size INCLUDING cached tokens: a token
//! read from the cache still takes up memory, so it counts either way (see
//! LLMManager._normalize_usage). Only the main agent is tracked; the CLI,
//! coder and minion sub-agents run on their own LLMManager and never feed
//! this bar. On reopen the caller seeds `initial_context` from the chat's
//! last saved value.
//!
//! The Rust copy of memory_tracker.py, imported by Python under its old
//! dotted name (AutoCua.memory_compression.memory_tracker). Its numbers stay
//! Python objects, and int(), truthiness, division and min() are Python's,
//! so a caller sees the same values and types as before.

use std::sync::{Mutex, MutexGuard, PoisonError};

use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyFloat, PyInt};

use crate::memory_compression::global;

/// Fixed memory budget the bar fills toward (headroom for compression).
pub const MEMORY_CAP: i64 = 300_000;

/// This module, as registered (its MEMORY_CAP is read from it at run time).
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || {
            py.import("AutoCua.agent_native.memory_compression.memory_tracker")
                .map(Bound::unbind)
        })
        .map(|m| m.bind(py).clone())
}

/// `int(value)`, Python's builtin.
fn int<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    value.py().get_type::<PyInt>().call1((value,))
}

/// `int(usage.get(key, 0) or 0)`.
fn count<'py>(usage: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    let value = usage.call_method1("get", (key, 0))?;
    if value.is_truthy()? {
        int(&value)
    } else {
        int(&0i64.into_pyobject(usage.py())?.into_any())
    }
}

/// An argument as the call gave it, or Missing when the call left it out
/// and Python's default (bound when the def ran) applies.
enum Given<'py> {
    Value(Bound<'py, PyAny>),
    Missing,
}

impl<'py> FromPyObject<'_, 'py> for Given<'py> {
    type Error = PyErr;

    fn extract(obj: Borrowed<'_, 'py, PyAny>) -> PyResult<Self> {
        Ok(Given::Value(obj.to_owned()))
    }
}

struct TrackerState {
    cap: Py<PyAny>,
    current: Py<PyAny>,
    last_input: Py<PyAny>,
    last_output: Py<PyAny>,
}

/// Holds the latest context size for a single chat (overwrite, never sum).
///
/// PURELY COSMETIC. A display gauge ONLY: it never gates the agent. The agent
/// does not stop at the cap (or at 1M); `current` keeps the REAL, unclamped
/// token count, and only the bar's `pct` stops at 100%, so a full bar reads
/// "memory full" while the agent keeps running. Do NOT wire any agent stop or
/// limit to this class.
#[pyclass(frozen, dict, weakref, subclass, module = "AutoCua.memory_compression.memory_tracker")]
pub struct MemoryTracker {
    state: Mutex<TrackerState>,
}

impl MemoryTracker {
    fn lock(&self) -> MutexGuard<'_, TrackerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read<'py>(&self, py: Python<'py>, field: fn(&TrackerState) -> &Py<PyAny>) -> Bound<'py, PyAny> {
        let value = field(&self.lock()).clone_ref(py);
        value.into_bound(py)
    }

    fn write(&self, field: fn(&mut TrackerState) -> &mut Py<PyAny>, value: Bound<'_, PyAny>) {
        let old = std::mem::replace(field(&mut self.lock()), value.unbind());
        // Dropped with the lock released: it can run Python code (a __del__).
        drop(old);
    }
}

#[pymethods]
impl MemoryTracker {
    #[new]
    #[pyo3(
        signature = (cap = Given::Missing, initial_context = None),
        text_signature = "(cap=300000, initial_context=0)"
    )]
    fn new(py: Python<'_>, cap: Given<'_>, initial_context: Option<Bound<'_, PyAny>>) -> PyResult<Self> {
        // self.cap = int(cap or MEMORY_CAP): the default is the 300_000 the
        // def bound; only a falsy cap reads the module's MEMORY_CAP now.
        let cap = match cap {
            Given::Value(v) => v,
            Given::Missing => MEMORY_CAP.into_pyobject(py)?.into_any(),
        };
        let cap = if cap.is_truthy()? { cap } else { global(&module(py)?, "MEMORY_CAP")? };
        let cap = int(&cap)?;
        // self.current = int(initial_context or 0)
        let zero = 0i64.into_pyobject(py)?.into_any();
        let current = match initial_context {
            Some(v) if v.is_truthy()? => int(&v)?,
            _ => int(&zero)?,
        };
        Ok(MemoryTracker {
            state: Mutex::new(TrackerState {
                cap: cap.unbind(),
                current: current.unbind(),
                last_input: zero.clone().unbind(),
                last_output: zero.unbind(),
            }),
        })
    }

    #[getter]
    fn cap<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.cap)
    }

    #[setter]
    fn set_cap(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.cap, value);
    }

    #[getter]
    fn current<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.current)
    }

    #[setter]
    fn set_current(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.current, value);
    }

    #[getter]
    fn last_input<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.last_input)
    }

    #[setter]
    fn set_last_input(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.last_input, value);
    }

    #[getter]
    fn last_output<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.last_output)
    }

    #[setter]
    fn set_last_output(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.last_output, value);
    }

    /// Sets the bar to this LLM call's full prompt size. `usage` is the
    /// normalized {input_tokens, output_tokens, context_tokens, ...} from
    /// llm_manager. A turn that reports 0 context (a failed call left
    /// last_usage stale, say) is ignored, so the bar holds its last good
    /// value instead of dropping to empty. Returns the current payload.
    fn record<'py>(slf: &Bound<'py, Self>, usage: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.get();
        let usage = if usage.is_truthy()? { usage } else { PyDict::new(py).into_any() };
        let ctx = count(&usage, "context_tokens")?;
        this.write(|s| &mut s.last_input, count(&usage, "input_tokens")?);
        this.write(|s| &mut s.last_output, count(&usage, "output_tokens")?);
        if ctx.gt(0)? {
            this.write(|s| &mut s.current, ctx);
        }
        slf.call_method0("payload")
    }

    /// The snapshot the frontend gets: the current size, the cap, and the %.
    /// `used` is the real count (it may be above `cap`); `pct` stops at 100
    /// for the bar's height only, and is not a limit the agent obeys.
    fn payload<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        // pct = min(100.0, (self.current / self.cap) * 100) if self.cap else 0.0
        let pct = if self.read(py, |s| &s.cap).is_truthy()? {
            let ratio = self.read(py, |s| &s.current).div(self.read(py, |s| &s.cap))?.mul(100)?;
            global(&module(py)?, "min")?.call1((PyFloat::new(py, 100.0), ratio))?
        } else {
            PyFloat::new(py, 0.0).into_any()
        };
        let payload = PyDict::new(py);
        payload.set_item("used", self.read(py, |s| &s.current))?;
        payload.set_item("cap", self.read(py, |s| &s.cap))?;
        payload.set_item("pct", pct)?;
        payload.set_item("last_input", self.read(py, |s| &s.last_input))?;
        payload.set_item("last_output", self.read(py, |s| &s.last_output))?;
        Ok(payload)
    }

    /// The instance's attributes, as Python's default __getstate__ gave
    /// them, so copy.copy, copy.deepcopy and pickle work as before.
    fn __getstate__<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        let py = slf.py();
        let this = slf.get();
        let state = PyDict::new(py);
        state.set_item("cap", this.read(py, |s| &s.cap))?;
        state.set_item("current", this.read(py, |s| &s.current))?;
        state.set_item("last_input", this.read(py, |s| &s.last_input))?;
        state.set_item("last_output", this.read(py, |s| &s.last_output))?;
        if let Ok(extra) = slf.getattr("__dict__") {
            state.call_method1("update", (extra,))?;
        }
        Ok(state)
    }

    fn __setstate__(slf: &Bound<'_, Self>, state: Bound<'_, PyDict>) -> PyResult<()> {
        for (key, value) in state.iter() {
            slf.setattr(key.cast_into::<pyo3::types::PyString>()?, value)?;
        }
        Ok(())
    }
}

/// Fills AutoCua.agent_native.memory_compression.memory_tracker.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("MEMORY_CAP", MEMORY_CAP)?;
    m.add_class::<MemoryTracker>()?;
    let _ = MODULE.set(m.py(), m.clone().unbind());
    Ok(())
}

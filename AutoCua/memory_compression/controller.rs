//! CompressionController: the runtime side of the handoff compression agent
//! (agent/service.rs beside it builds the dump and makes the LLM call).
//!
//! The compression agent is its OWN agent: WHEN it fires, HOW the worker
//! runs and HOW the result is spliced back all live here, in ONE place
//! shared by every platform agent (mac, windows, linux and ios in Python;
//! web, android and the coder in Rust). An agent wires it with four hooks,
//! all called from its main loop:
//!
//!     reset()          at the top of process_request (makes stale workers harmless)
//!     maybe_trigger()  right after each main-agent LLM call (last_usage is fresh)
//!     apply_pending()  at the top of each loop iteration (splices IN PLACE)
//!     finish_run()     after the loop exits (turns off a dangling indicator)
//!
//! An agent can pass two constructor callables, dump_builder and
//! synthetic_entry, that swap the default history format (the legacy 4-block
//! JSON the defaults read) for its own native one, as the coder first did and
//! every agent does now; the trigger, the threading and the splice stay
//! shared.
//!
//! Threading contract: the dump is snapshotted on the CALLER's thread; the
//! worker is a Python daemon thread (threading.Thread, as before, so the
//! interpreter's exit treats it the same) that makes one plain-text LLM call
//! and only deposits {gen, k, text}; every list change happens inside
//! apply_pending, on the main loop's thread. A generation counter makes a
//! stale result (from an earlier run on the same instance) unappliable.
//!
//! The state sits behind a lock that is never held while Python code runs
//! (a callback, the LLM call, stop_event.is_set(), a list operation), so the
//! worker and the main loop can never deadlock on it. Its values stay
//! Python objects, and the comparisons, arithmetic, int() and truthiness on
//! them are Python's, as is every print() line.
//!
//! The Rust copy of controller.py, imported by Python and by the Rust agents
//! under its old dotted name, AutoCua.memory_compression.controller.

use std::sync::{Mutex, MutexGuard, PoisonError};

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyInt, PyList, PySlice, PyString, PyTuple};

use crate::memory_compression::{format_value, global, is_exception, join_str, print_line};

/// What a fresh controller's threshold is when AutoCua_COMPRESS_THRESHOLD
/// does not say otherwise: the agent's live context size (cached tokens
/// included) that starts a compression. memory_tracker's MEMORY_CAP is the
/// bar's display budget; this is where compression starts keeping the
/// context down.
const DEFAULT_THRESHOLD: i64 = 110_000;

/// This module, as registered: COMPRESSION, COMPRESS_THRESHOLD and the names
/// it imported are read from it at run time, as Python read its globals.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || {
            py.import("AutoCua.agent_native.memory_compression.controller")
                .map(Bound::unbind)
        })
        .map(|m| m.bind(py).clone())
}

/// COMPRESS_THRESHOLD as Python worked it out once, at import:
/// int(os.getenv("AutoCua_COMPRESS_THRESHOLD", "") or 110_000), and 110_000
/// when int() refuses the value (a TypeError or ValueError). A test knob:
/// child agents (the coder) inherit the environment, so one setting covers
/// every agent.
fn compress_threshold(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    let raw = py
        .import("os")?
        .call_method1("getenv", ("AutoCua_COMPRESS_THRESHOLD", ""))?;
    let value = if raw.is_truthy()? { raw } else { DEFAULT_THRESHOLD.into_pyobject(py)?.into_any() };
    match py.get_type::<PyInt>().call1((value,)) {
        Ok(threshold) => Ok(threshold),
        Err(e) if e.is_instance_of::<PyTypeError>(py) || e.is_instance_of::<PyValueError>(py) => {
            Ok(DEFAULT_THRESHOLD.into_pyobject(py)?.into_any())
        }
        Err(e) => Err(e),
    }
}

/// `int((manager.last_usage or {}).get("context_tokens", 0) or 0)`.
fn context_tokens<'py>(manager: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = manager.py();
    let usage = manager.getattr("last_usage")?;
    let usage = if usage.is_truthy()? { usage } else { PyDict::new(py).into_any() };
    let tokens = usage.call_method1("get", ("context_tokens", 0))?;
    let tokens = if tokens.is_truthy()? { tokens } else { 0i64.into_pyobject(py)?.into_any() };
    py.get_type::<PyInt>().call1((tokens,))
}

/// `slice(None, stop)`, what `seq[:stop]` indexes with.
fn slice_to<'py>(stop: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = stop.py();
    py.get_type::<PySlice>().call1((py.None(), stop))
}

/// The type name CPython puts in "cannot unpack non-iterable X object": a
/// class's own name, or for a built-in type its dotted C name ("int",
/// "datetime.date").
fn unpack_type_name(value: &Bound<'_, PyAny>) -> PyResult<String> {
    let kind = value.get_type();
    let name: String = kind.getattr("__name__")?.extract()?;
    // SAFETY: a valid type object.
    let flags = unsafe { ffi::PyType_GetFlags(kind.as_type_ptr()) };
    if flags & ffi::Py_TPFLAGS_HEAPTYPE != 0 {
        return Ok(name);
    }
    let module: String = kind.getattr("__module__")?.extract()?;
    Ok(if module == "builtins" { name } else { format!("{module}.{name}") })
}

/// `a, b = value`: Python's unpacking of exactly two items, with the errors
/// the running Python gives (3.14 added "got N" for a list, tuple or dict
/// with too many).
fn unpack2<'py>(value: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let py = value.py();
    let not_enough = |got: usize| PyValueError::new_err(format!("not enough values to unpack (expected 2, got {got})"));
    let mut items = match value.try_iter() {
        Ok(items) => items,
        Err(e) => {
            let kind = value.get_type();
            // SAFETY: a valid type object and a valid object.
            let has_iter = unsafe { !ffi::PyType_GetSlot(kind.as_type_ptr(), ffi::Py_tp_iter).is_null() };
            let is_sequence = unsafe { ffi::PySequence_Check(value.as_ptr()) } == 1;
            if e.is_instance_of::<PyTypeError>(py) && !has_iter && !is_sequence {
                return Err(PyTypeError::new_err(format!(
                    "cannot unpack non-iterable {} object",
                    unpack_type_name(value)?
                )));
            }
            return Err(e);
        }
    };
    let first = items.next().ok_or_else(|| not_enough(0))??;
    let second = items.next().ok_or_else(|| not_enough(1))??;
    match items.next() {
        None => Ok((first, second)),
        Some(Err(e)) => Err(e),
        Some(Ok(_)) => {
            let exact = value.is_exact_instance_of::<PyList>()
                || value.is_exact_instance_of::<PyTuple>()
                || value.is_exact_instance_of::<PyDict>();
            let message = if exact && py.version_info() >= (3, 14) {
                format!("too many values to unpack (expected 2, got {})", value.len()?)
            } else {
                "too many values to unpack (expected 2)".to_string()
            };
            Err(PyValueError::new_err(message))
        }
    }
}

struct State {
    /// The owning agent's manager: only its last_usage is read.
    llm_manager: Py<PyAny>,
    /// The agent's LLMManager class, for the compressor's own second manager.
    llm_manager_cls: Py<PyAny>,
    /// The frontend token pipe; the indicator rides on it.
    token_callback: Py<PyAny>,
    stop_event: Py<PyAny>,
    /// The agent's flavor hooks (None: the defaults). dump_builder renders
    /// steps [0..k] into the compressor's <input> text; synthetic_entry maps
    /// (step_k_entry, handoff_text) to (entry, tool_slot) for the splice.
    dump_builder: Py<PyAny>,
    synthetic_entry: Py<PyAny>,
    threshold: Py<PyAny>,
    /// One compression in flight, ever.
    inflight: Py<PyAny>,
    /// {"gen", "k", "text"}, deposited by the worker.
    result: Py<PyAny>,
    /// The generation guard: a stale result never applies.
    gen: Py<PyAny>,
    /// No new trigger until this many entries exist.
    rearm_len: Py<PyAny>,
    /// The MemoryCompressionAgent, made on first use (its own second manager).
    compressor: Py<PyAny>,
}

/// The runtime orchestration of the handoff compression agent: the trigger,
/// the background worker, the indicator and the splice, shared by every
/// platform agent. See the module notes for the four hooks and the
/// threading contract.
#[pyclass(frozen, dict, weakref, subclass, module = "AutoCua.memory_compression.controller")]
pub struct CompressionController {
    state: Mutex<State>,
}

impl CompressionController {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One attribute, read under the lock (nothing in Python runs while it is held).
    fn read<'py>(&self, py: Python<'py>, field: fn(&State) -> &Py<PyAny>) -> Bound<'py, PyAny> {
        let value = field(&self.lock()).clone_ref(py);
        value.into_bound(py)
    }

    /// One attribute, set under the lock.
    fn write(&self, field: fn(&mut State) -> &mut Py<PyAny>, value: Bound<'_, PyAny>) {
        let old = std::mem::replace(field(&mut self.lock()), value.unbind());
        // Let go of the old value only now, with the lock released: dropping
        // it can run Python code (a __del__).
        drop(old);
    }

    /// `self.stop_event and self.stop_event.is_set()`, as a truth value.
    fn stop_set(&self, py: Python<'_>) -> PyResult<bool> {
        if !self.read(py, |s| &s.stop_event).is_truthy()? {
            return Ok(false);
        }
        self.read(py, |s| &s.stop_event).call_method0("is_set")?.is_truthy()
    }
}

#[pymethods]
impl CompressionController {
    #[new]
    #[pyo3(signature = (llm_manager, llm_manager_cls, token_callback, stop_event, dump_builder=None, synthetic_entry=None))]
    fn new(
        py: Python<'_>,
        llm_manager: Py<PyAny>,
        llm_manager_cls: Py<PyAny>,
        token_callback: Py<PyAny>,
        stop_event: Py<PyAny>,
        dump_builder: Option<Py<PyAny>>,
        synthetic_entry: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        let threshold = global(&module(py)?, "COMPRESS_THRESHOLD")?.unbind();
        let zero = 0i64.into_pyobject(py)?.into_any().unbind();
        Ok(CompressionController {
            state: Mutex::new(State {
                llm_manager,
                llm_manager_cls,
                token_callback,
                stop_event,
                dump_builder: dump_builder.unwrap_or_else(|| py.None()),
                synthetic_entry: synthetic_entry.unwrap_or_else(|| py.None()),
                threshold,
                inflight: PyBool::new(py, false).to_owned().into_any().unbind(),
                result: py.None(),
                gen: zero.clone_ref(py),
                rearm_len: zero,
                compressor: py.None(),
            }),
        })
    }

    /// New process_request: a worker still running from an earlier run can
    /// no longer deposit anything that applies.
    fn reset(&self, py: Python<'_>) -> PyResult<()> {
        // self._gen += 1
        let gen = self.read(py, |s| &s.gen);
        let one = 1i64.into_pyobject(py)?;
        // SAFETY: two valid objects; a new reference or NULL with the error set.
        let next = unsafe { Bound::from_owned_ptr_or_err(py, ffi::PyNumber_InPlaceAdd(gen.as_ptr(), one.as_ptr())) }?;
        self.write(|s| &mut s.gen, next);
        self.write(|s| &mut s.inflight, PyBool::new(py, false).to_owned().into_any());
        self.write(|s| &mut s.result, py.None().into_bound(py));
        self.write(|s| &mut s.rearm_len, 0i64.into_pyobject(py)?.into_any());
        Ok(())
    }

    /// Fire and forget: start ONE background handoff compression when the
    /// context crosses the threshold. The dump is snapshotted HERE, on the
    /// main thread; the worker never reads the live lists.
    fn maybe_trigger(
        slf: &Bound<'_, Self>,
        assistant_messages: Bound<'_, PyAny>,
        tool_responses: Bound<'_, PyAny>,
        task: Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        let module = module(py)?;
        let ctx = match context_tokens(&this.read(py, |s| &s.llm_manager)) {
            Ok(ctx) => ctx,
            Err(e) if is_exception(py, &e) => return Ok(()),
            Err(e) => return Err(e),
        };
        if ctx.lt(this.read(py, |s| &s.threshold))? {
            return Ok(());
        }
        // The on/off flag. False: never compress (the token tracking and the
        // memory bar keep working as usual).
        if !global(&module, "COMPRESSION")?.is_truthy()? {
            return Ok(());
        }
        if this.read(py, |s| &s.inflight).is_truthy()? || !this.read(py, |s| &s.result).is_none() {
            return Ok(());
        }
        // Nothing worth compressing.
        if assistant_messages.len()? < 2 {
            return Ok(());
        }
        // The anti-thrash guard.
        if assistant_messages.len()?.into_pyobject(py)?.lt(this.read(py, |s| &s.rearm_len))? {
            return Ok(());
        }
        if this.read(py, |s| &s.compressor).is_none() {
            // A SECOND manager: the same provider, model and key, in plain-text
            // mode. Never the main one: its last_usage would race the loop.
            let made = (|| -> PyResult<()> {
                let main = this.read(py, |s| &s.llm_manager);
                let cls = this.read(py, |s| &s.llm_manager_cls);
                let provider = main.getattr("provider")?;
                let model = main.getattr("model_short_name")?;
                let kwargs = PyDict::new(py);
                kwargs.set_item("api_key", main.getattr("runtime_api_key")?)?;
                kwargs.set_item("mode", "text")?;
                let second = cls.call((provider, model), Some(&kwargs))?;
                let compressor = global(&module, "MemoryCompressionAgent")?.call1((second,))?;
                this.write(|s| &mut s.compressor, compressor);
                Ok(())
            })();
            if let Err(e) = made {
                if !is_exception(py, &e) {
                    return Err(e);
                }
                let line = join_str(
                    py,
                    &[
                        PyString::new(py, "\u{1F9E0} [memory] Compression unavailable: ").as_any(),
                        format_value(e.value(py).as_any(), "")?.as_any(),
                    ],
                )?;
                print_line(&module, &line)?;
                let rearm = (assistant_messages.len()? + 5).into_pyobject(py)?.into_any();
                this.write(|s| &mut s.rearm_len, rearm);
                return Ok(());
            }
        }
        // The last completed step.
        let k = assistant_messages.len()? as i64 - 1;
        let k_obj = k.into_pyobject(py)?.into_any();
        let builder = this.read(py, |s| &s.dump_builder);
        let build = if builder.is_truthy()? {
            builder
        } else {
            this.read(py, |s| &s.compressor).getattr("build_dump")?
        };
        let dump = build.call1((&assistant_messages, &tool_responses, &k_obj, &task))?;
        this.write(|s| &mut s.inflight, PyBool::new(py, true).to_owned().into_any());
        // The indicator on.
        slf.call_method1("_notify", ("start",))?;
        // "\u{2014}" is the em dash the Python line printed.
        let line = join_str(
            py,
            &[
                PyString::new(py, "\u{1F9E0} [memory] Context at ").as_any(),
                format_value(&ctx, ",")?.as_any(),
                PyString::new(py, &format!(" tokens \u{2014} compressing steps 1..{} in background", k + 1)).as_any(),
            ],
        )?;
        print_line(&module, &line)?;
        // threading.Thread(target=self._run, args=(dump, k, self._gen), daemon=True).start()
        let thread_cls = global(&module, "threading")?.getattr("Thread")?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("target", slf.getattr("_run")?)?;
        kwargs.set_item("args", PyTuple::new(py, [dump, k_obj, this.read(py, |s| &s.gen)])?)?;
        kwargs.set_item("daemon", true)?;
        thread_cls.call((), Some(&kwargs))?.call_method0("start")?;
        Ok(())
    }

    /// The worker thread: one plain-text LLM call, and a deposit only. It
    /// NEVER touches the conversation lists; apply_pending splices them on
    /// the main loop.
    #[pyo3(name = "_run")]
    fn run(slf: &Bound<'_, Self>, dump: Bound<'_, PyAny>, k: Bound<'_, PyAny>, gen: Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        let module = module(py)?;
        let attempt = (|| -> PyResult<()> {
            // The run is ending: skip the call.
            if this.stop_set(py)? {
                return Ok(());
            }
            let text = this.read(py, |s| &s.compressor).call_method1("compress", (&dump,))?;
            if text.is_truthy()?
                && text.call_method0("strip")?.is_truthy()?
                && gen.eq(this.read(py, |s| &s.gen))?
                && !this.stop_set(py)?
            {
                let result = PyDict::new(py);
                result.set_item("gen", &gen)?;
                result.set_item("k", &k)?;
                result.set_item("text", text.call_method0("strip")?)?;
                this.write(|s| &mut s.result, result.into_any());
            }
            Ok(())
        })();
        let outcome = match attempt {
            Err(e) if is_exception(py, &e) => (|| -> PyResult<()> {
                let line = join_str(
                    py,
                    &[
                        PyString::new(py, "\u{1F9E0} [memory] Compression failed, dropping (will re-trigger): ").as_any(),
                        format_value(e.value(py).as_any(), "")?.as_any(),
                    ],
                )?;
                print_line(&module, &line)?;
                // Back off about 3 steps.
                this.write(|s| &mut s.rearm_len, k.add(4)?);
                Ok(())
            })(),
            other => other,
        };
        // finally: the indicator goes off the moment the LLM call resolves,
        // whichever way; the splice is certain at the caller's next loop top.
        this.write(|s| &mut s.inflight, PyBool::new(py, false).to_owned().into_any());
        slf.call_method1("_notify", ("end",))?;
        outcome
    }

    /// MAIN LOOP ONLY: splices a deposited handoff into the live lists.
    ///
    /// Replaces entries [0..k] with ONE synthetic entry (step k's JSON with
    /// only its memory field turned into the <handoff> document) and keeps
    /// step k's tool slot; both lists change IN PLACE. Returns
    /// web_memory_index shifted past the splice (None if the slot was inside
    /// the compressed span). A stale or absent result changes nothing and
    /// returns web_memory_index as it came.
    fn apply_pending<'py>(
        slf: &Bound<'py, Self>,
        assistant_messages: Bound<'py, PyAny>,
        tool_responses: Bound<'py, PyAny>,
        web_memory_index: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.get();
        let res = this.read(py, |s| &s.result);
        if res.is_none() {
            return Ok(web_memory_index);
        }
        this.write(|s| &mut s.result, py.None().into_bound(py));
        if res.call_method1("get", ("gen",))?.ne(this.read(py, |s| &s.gen))?
            || !res.call_method1("get", ("text",))?.is_truthy()?
        {
            return Ok(web_memory_index);
        }
        let k = res.get_item("k")?;
        // if not (0 <= k < len(assistant_messages))
        if !(0i64.into_pyobject(py)?.le(&k)? && k.lt(assistant_messages.len()?)?) {
            return Ok(web_memory_index);
        }
        let synthetic = this.read(py, |s| &s.synthetic_entry);
        let (entry, slot) = if !synthetic.is_none() {
            let made = synthetic.call1((assistant_messages.get_item(&k)?, res.get_item("text")?))?;
            unpack2(&made)?
        } else {
            let make = global(&module(py)?, "make_synthetic_entry")?;
            let entry = make.call1((assistant_messages.get_item(&k)?, res.get_item("text")?))?;
            let slot = if k.lt(tool_responses.len()?)? {
                tool_responses.get_item(&k)?
            } else {
                py.None().into_bound(py)
            };
            (entry, slot)
        };
        // assistant_messages[:k + 1] = [entry]; tool_responses[:k + 1] = [slot]
        assistant_messages.set_item(slice_to(&k.add(1)?)?, PyList::new(py, [entry])?)?;
        tool_responses.set_item(slice_to(&k.add(1)?)?, PyList::new(py, [slot])?)?;
        let web_memory_index = if web_memory_index.is_none() {
            web_memory_index
        } else if web_memory_index.le(&k)? {
            py.None().into_bound(py)
        } else {
            web_memory_index.sub(&k)?
        };
        // Anti-thrash: no new trigger until 3 fresh steps exist.
        let rearm = (assistant_messages.len()? + 3).into_pyobject(py)?.into_any();
        this.write(|s| &mut s.rearm_len, rearm);
        let line = join_str(
            py,
            &[
                PyString::new(py, "\u{1F9E0} [memory] Compressed steps 1..").as_any(),
                format_value(&k.add(1)?, "")?.as_any(),
                PyString::new(py, " into a handoff (").as_any(),
                format_value(assistant_messages.len()?.into_pyobject(py)?.as_any(), "")?.as_any(),
                PyString::new(py, " entries remain)").as_any(),
            ],
        )?;
        print_line(&module(py)?, &line)?;
        Ok(web_memory_index)
    }

    /// The run is over with a compression still pending (in flight, or
    /// deposited but never applied): the result dies at the next reset(), so
    /// make sure the indicator isn't left on.
    fn finish_run(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        if this.read(py, |s| &s.inflight).is_truthy()? || !this.read(py, |s| &s.result).is_none() {
            slf.call_method1("_notify", ("end",))?;
        }
        Ok(())
    }

    /// The frontend indicator: {"memory_compression": "start"|"end"} rides
    /// the existing token pipe, with no new callback wiring. A callback that
    /// raises is ignored.
    #[pyo3(name = "_notify")]
    fn notify(&self, py: Python<'_>, state: Bound<'_, PyAny>) -> PyResult<()> {
        let callback = self.read(py, |s| &s.token_callback);
        if callback.is_truthy()? {
            let event = PyDict::new(py);
            event.set_item("memory_compression", state)?;
            match callback.call1((event,)) {
                Ok(_) => {}
                Err(e) if is_exception(py, &e) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    // The attributes, readable and writable as on the Python class.

    #[getter]
    fn llm_manager<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.llm_manager)
    }

    #[setter]
    fn set_llm_manager(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.llm_manager, value);
    }

    #[getter]
    fn llm_manager_cls<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.llm_manager_cls)
    }

    #[setter]
    fn set_llm_manager_cls(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.llm_manager_cls, value);
    }

    #[getter]
    fn token_callback<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.token_callback)
    }

    #[setter]
    fn set_token_callback(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.token_callback, value);
    }

    #[getter]
    fn stop_event<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.stop_event)
    }

    #[setter]
    fn set_stop_event(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.stop_event, value);
    }

    #[getter(_dump_builder)]
    fn dump_builder<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.dump_builder)
    }

    #[setter(_dump_builder)]
    fn set_dump_builder(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.dump_builder, value);
    }

    #[getter(_synthetic_entry)]
    fn synthetic_entry<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.synthetic_entry)
    }

    #[setter(_synthetic_entry)]
    fn set_synthetic_entry(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.synthetic_entry, value);
    }

    #[getter(_threshold)]
    fn threshold<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.threshold)
    }

    #[setter(_threshold)]
    fn set_threshold(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.threshold, value);
    }

    #[getter(_inflight)]
    fn inflight<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.inflight)
    }

    #[setter(_inflight)]
    fn set_inflight(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.inflight, value);
    }

    #[getter(_result)]
    fn result<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.result)
    }

    #[setter(_result)]
    fn set_result(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.result, value);
    }

    #[getter(_gen)]
    fn gen<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.gen)
    }

    #[setter(_gen)]
    fn set_gen(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.gen, value);
    }

    #[getter(_rearm_len)]
    fn rearm_len<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.rearm_len)
    }

    #[setter(_rearm_len)]
    fn set_rearm_len(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.rearm_len, value);
    }

    #[getter(_compressor)]
    fn compressor<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.compressor)
    }

    #[setter(_compressor)]
    fn set_compressor(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.compressor, value);
    }
}

/// Fills AutoCua.agent_native.memory_compression.controller: the names
/// controller.py had, its imports (os, threading, and MemoryCompressionAgent
/// and make_synthetic_entry from the service module) included.
pub fn register(m: &Bound<'_, PyModule>, service: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("os", py.import("os")?)?;
    m.add("threading", py.import("threading")?)?;
    m.add("MemoryCompressionAgent", service.getattr("MemoryCompressionAgent")?)?;
    m.add("make_synthetic_entry", service.getattr("make_synthetic_entry")?)?;
    // The on/off flag. True: compress when the context crosses
    // COMPRESS_THRESHOLD. False: never compress (the token tracking and the
    // memory bar keep working as usual).
    m.add("COMPRESSION", true)?;
    m.add("COMPRESS_THRESHOLD", compress_threshold(py)?)?;
    m.add_class::<CompressionController>()?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}

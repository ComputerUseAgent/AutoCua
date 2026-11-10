//! The handoff compression agent: it turns the main agent's in-run step
//! history into ONE plain-text handoff document. What the model is told to
//! do with it is system_prompt.md beside this file, read at run time.
//!
//! A pure module: no threads and no list changes. The caller (the platform
//! agent's main loop, through ../controller.rs) snapshots the history,
//! starts the background thread and splices the result back into its memory
//! lists.
//!
//! The Rust copy of agent/service.py. Python imports it under the old dotted
//! name, AutoCua.memory_compression.agent.service (memory_compression's
//! __init__.py points that name here), and gets the same names, arguments
//! and results. Where Python's own rules decide a result they are called,
//! not imitated: json.loads and json.dumps, str(), format(), truthiness,
//! slicing and a text-mode open(). The text stays Python text from end to
//! end, so even a lone surrogate (a JSON escape can put one in a Python
//! string, and Rust strings cannot hold it) comes back exactly as it went in.

use std::borrow::Cow;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use pyo3::exceptions::PyTypeError;
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBytes, PyDict, PyList, PySlice, PyString, PyTuple};
use regex::bytes::Regex;

use crate::memory_compression::{format_value, global, is_exception, join_str};

/// The synthetic entry's memory field wraps the handoff document in this tag,
/// so the NEXT compression (and a resumed session) can find it and give it to
/// the compressor as === PREVIOUS HANDOFF ===.
pub const HANDOFF_OPEN: &str = "<handoff>";
pub const HANDOFF_CLOSE: &str = "</handoff>";

/// A tool slot that starts with this (after leading whitespace) is a
/// run-boundary request marker: a user turn of its own in the dump.
const REQUEST_MARKER_PREFIX: &str = "<updated_user_request";

/// The two patterns as the Python module spelled them; kept as compiled
/// Python patterns on the module (_STEP_PREFIX_RE, _HANDOFF_RE) beside the
/// Rust ones below, which do the matching.
const STEP_PREFIX_PATTERN: &str = r"(<Step_no=\d+ />\n)(.*)";
const HANDOFF_PATTERN: &str = r"<handoff>\n?(.*?)\n?</handoff>";

/// Python's name for this module, which its functions and its class report
/// as their __module__.
pub const PY_MODULE: &str = "AutoCua.memory_compression.agent.service";

/// The `<Step_no=N />\n` prefix at the start of an entry (Python's
/// _STEP_PREFIX_RE.match: everything after it is the entry's JSON). \d is
/// any Unicode decimal digit, as in Python. Matched on the text's UTF-8
/// bytes (see to_bytes).
fn step_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\A<Step_no=\d+ />\n").unwrap())
}

/// Python's _HANDOFF_RE with re.DOTALL: the body of the first <handoff>
/// block, without one newline just inside each tag. `(?-u:.)` takes any
/// byte, so a lone surrogate's bytes are matched like any other character's;
/// the tags are ASCII, so the body always starts and ends on a character.
fn handoff_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<handoff>\n?((?-u:.)*?)\n?</handoff>").unwrap())
}

/// This module, as registered (its globals: json, os, open, ...).
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || {
            py.import("AutoCua.agent_native.memory_compression.agent.service")
                .map(Bound::unbind)
        })
        .map(|m| m.bind(py).clone())
}

// ---------------------------------------------------------------------------
// Python text, losslessly
// ---------------------------------------------------------------------------

/// A Python str's text as UTF-8 bytes, for the Rust patterns. A lone
/// surrogate has no UTF-8 form, so a str holding one is encoded the way
/// Python's "surrogatepass" does it, which from_bytes turns back into the
/// same str.
fn to_bytes<'a>(text: &'a Bound<'_, PyString>) -> PyResult<Cow<'a, [u8]>> {
    if let Ok(s) = text.to_str() {
        return Ok(Cow::Borrowed(s.as_bytes()));
    }
    let py = text.py();
    // SAFETY: a valid str object and two NUL-terminated C strings; the
    // result is a new reference or NULL with the error set.
    let encoded = unsafe {
        Bound::from_owned_ptr_or_err(
            py,
            ffi::PyUnicode_AsEncodedString(text.as_ptr(), c"utf-8".as_ptr(), c"surrogatepass".as_ptr()),
        )
    }?;
    Ok(Cow::Owned(encoded.cast_into::<PyBytes>()?.as_bytes().to_vec()))
}

/// The Python str whose to_bytes these bytes are.
fn from_bytes<'py>(py: Python<'py>, bytes: &[u8]) -> PyResult<Bound<'py, PyString>> {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Ok(PyString::new(py, s));
    }
    // A slice is never longer than isize::MAX bytes, so the length fits.
    let len = bytes.len() as ffi::Py_ssize_t;
    // SAFETY: the pointer and length describe `bytes`; the result is a new
    // reference or NULL with the error set.
    let text = unsafe {
        Bound::from_owned_ptr_or_err(
            py,
            ffi::PyUnicode_DecodeUTF8(bytes.as_ptr().cast(), len, c"surrogatepass".as_ptr()),
        )
    }?;
    Ok(text.cast_into::<PyString>()?)
}

/// `value or ""`.
fn or_empty<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    if value.is_truthy()? {
        Ok(value.clone())
    } else {
        Ok(PyString::new(value.py(), "").into_any())
    }
}

/// `slice(None, stop)`, what `seq[:stop]` indexes with.
fn slice_to<'py>(stop: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = stop.py();
    py.get_type::<PySlice>().call1((py.None(), stop))
}

/// json.loads(text), through this module's `json` global.
fn json_loads<'py>(text: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = text.py();
    global(&module(py)?, "json")?.getattr("loads")?.call1((text,))
}

/// json.dumps(value, indent=2, ensure_ascii=False), through this module's
/// `json` global. Python's own encoder, so floats, big ints, NaN and lone
/// surrogates come out exactly as before.
fn json_dumps_pretty<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = value.py();
    let kwargs = PyDict::new(py);
    kwargs.set_item("indent", 2)?;
    kwargs.set_item("ensure_ascii", false)?;
    global(&module(py)?, "json")?.getattr("dumps")?.call((value,), Some(&kwargs))
}

/// The TypeError Python's re.match gives a str pattern used on something
/// that is not a str. Its wording changed between Python versions, so the
/// running Python's re raises it.
fn not_text_error(subject: &Bound<'_, PyAny>) -> PyErr {
    let py = subject.py();
    let raised = (|| -> PyResult<PyErr> {
        let re = global(&module(py)?, "re")?;
        let pattern = re.call_method1("compile", (STEP_PREFIX_PATTERN, re.getattr("DOTALL")?))?;
        pattern.call_method1("match", (subject,))?;
        Ok(PyTypeError::new_err("expected string or bytes-like object"))
    })();
    match raised {
        Ok(e) | Err(e) => e,
    }
}

// ---------------------------------------------------------------------------
// The functions
// ---------------------------------------------------------------------------

/// system_prompt.md, the compressor's instructions, read from the folder
/// agent/service.py was in (this file's folder) with Python's text-mode
/// open(path, "r", encoding="utf-8"): universal newlines, the same errors,
/// and the embedded copy a packaged app's patched open() serves.
pub fn load_system_prompt(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    let module = module(py)?;
    let os_path = global(&module, "os")?.getattr("path")?;
    // os.path.join(os.path.dirname(os.path.abspath(__file__)), "system_prompt.md")
    // with __file__ the old service.py, beside the extension's folder.
    let native = crate::native_dir(py)?.to_string_lossy().into_owned();
    let file = os_path.call_method1("join", (native, "memory_compression", "agent", "service.py"))?;
    let folder = os_path.call_method1("dirname", (os_path.call_method1("abspath", (file,))?,))?;
    let path = os_path.call_method1("join", (folder, "system_prompt.md"))?;

    let kwargs = PyDict::new(py);
    kwargs.set_item("encoding", "utf-8")?;
    let manager = global(&module, "open")?.call((path, "r"), Some(&kwargs))?;
    // with open(...) as fh: return fh.read()
    let kind = manager.get_type();
    let enter = kind.getattr("__enter__")?;
    let exit = kind.getattr("__exit__")?;
    let fh = enter.call1((&manager,))?;
    match fh.call_method0("read") {
        Ok(text) => {
            exit.call1((&manager, py.None(), py.None(), py.None()))?;
            Ok(text)
        }
        Err(e) => {
            let traceback = e.traceback(py).map(|t| t.into_any()).unwrap_or_else(|| py.None().into_bound(py));
            let swallowed = exit.call1((&manager, e.get_type(py), e.value(py), traceback))?;
            if swallowed.is_truthy()? {
                Ok(py.None().into_bound(py))
            } else {
                Err(e)
            }
        }
    }
}

/// `<handoff>\n{text}\n</handoff>`; anything but a str is formatted as an
/// f-string would format it.
pub fn wrap_handoff<'py>(text: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyString>> {
    let py = text.py();
    let open = PyString::new(py, "<handoff>\n");
    let close = PyString::new(py, "\n</handoff>");
    join_str(py, &[open.as_any(), format_value(text, "")?.as_any(), close.as_any()])
}

/// The handoff body if memory_text carries a <handoff> block, else None
/// (always None for anything that is not a str).
pub fn extract_handoff<'py>(memory_text: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyString>>> {
    let Ok(text) = memory_text.cast::<PyString>() else {
        return Ok(None);
    };
    let bytes = to_bytes(text)?;
    match handoff_re().captures(&bytes).and_then(|c| c.get(1)) {
        Some(body) => Ok(Some(from_bytes(text.py(), body.as_bytes())?)),
        None => Ok(None),
    }
}

/// Splits '<Step_no=N />\n{json}' into (prefix, json_str). A bridge entry
/// or any other bare-JSON entry gives ('', entry), and a falsy entry ('', '').
pub fn entry_parts<'py>(entry: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyString>, Bound<'py, PyAny>)> {
    let py = entry.py();
    let subject = or_empty(entry)?;
    let Ok(text) = subject.cast::<PyString>() else {
        return Err(not_text_error(&subject));
    };
    let bytes = to_bytes(text)?;
    if let Some(m) = step_prefix_re().find(&bytes) {
        let prefix = from_bytes(py, &bytes[..m.end()])?;
        let rest = from_bytes(py, &bytes[m.end()..])?;
        return Ok((prefix, rest.into_any()));
    }
    Ok((PyString::new(py, ""), or_empty(entry)?))
}

/// Older dump steps keep decision/memory/next_goal only (the main agent's
/// sliding-window trim; the latest step stays full). An entry whose JSON
/// does not parse into a dict comes back unchanged.
pub fn trim_for_dump<'py>(entry: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = entry.py();
    let (prefix, json_part) = entry_parts(entry)?;
    let trimmed = (|| -> PyResult<Bound<'py, PyAny>> {
        let data = json_loads(&json_part)?;
        for field in ["thinking", "eval", "action"] {
            data.call_method1("pop", (field, py.None()))?;
        }
        prefix.add(json_dumps_pretty(&data)?)
    })();
    match trimmed {
        Ok(text) => Ok(text),
        Err(e) if is_exception(py, &e) => Ok(entry.clone()),
        Err(e) => Err(e),
    }
}

/// Step K's original JSON with ONLY its 'memory' field replaced by the
/// wrapped handoff; every other field stays as it was. A step K that does
/// not parse falls back to a fresh minimal entry (any <Step_no=K /> prefix
/// kept).
pub fn make_synthetic_entry<'py>(
    step_k_entry: &Bound<'py, PyAny>,
    handoff_text: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = step_k_entry.py();
    let (prefix, json_part) = entry_parts(step_k_entry)?;
    let wrapped = wrap_handoff(handoff_text)?;
    let made = (|| -> PyResult<Bound<'py, PyAny>> {
        let data = json_loads(&json_part)?;
        data.set_item("memory", &wrapped)?;
        prefix.add(json_dumps_pretty(&data)?)
    })();
    match made {
        Ok(entry) => Ok(entry),
        Err(e) if is_exception(py, &e) => {
            let fresh = PyDict::new(py);
            fresh.set_item("memory", &wrapped)?;
            fresh.set_item(
                "next_goal",
                "Memory compressed to a handoff document; continue the task using it.",
            )?;
            prefix.add(json_dumps_pretty(fresh.as_any())?)
        }
        Err(e) => Err(e),
    }
}

/// Renders steps [0..k] in the compressor prompt's <input> format.
///
/// MAIN THREAD ONLY: it snapshots the lists' content into one string, so the
/// worker thread never reads the live lists. The main agent's system prompt
/// and the live user message are never in these lists, so never in the dump.
pub fn build_dump<'py>(
    assistant_messages: &Bound<'py, PyAny>,
    tool_responses: &Bound<'py, PyAny>,
    k: &Bound<'py, PyAny>,
    task: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = k.py();
    let text = |s: &str| PyString::new(py, s).into_any();

    // entries = [str(e) for e in assistant_messages[:k + 1]]
    let mut entries: Vec<Bound<'py, PyAny>> = Vec::new();
    for entry in assistant_messages.get_item(slice_to(&k.add(1)?)?)?.try_iter()? {
        entries.push(entry?.str()?.into_any());
    }
    // tools = list(tool_responses[:k + 1]), padded with None to len(entries)
    let tools_list = py.get_type::<PyList>().call1((tool_responses.get_item(slice_to(&k.add(1)?)?)?,))?;
    let mut tools: Vec<Bound<'py, PyAny>> = tools_list.try_iter()?.collect::<PyResult<_>>()?;
    while tools.len() < entries.len() {
        tools.push(py.None().into_bound(py));
    }

    let mut out: Vec<Bound<'py, PyAny>> = vec![
        text("Session: live (in-run rolling compression)"),
        join_str(py, &[&text("Task: "), format_value(task, "")?.as_any()])?.into_any(),
        text("Trigger: rolling"),
    ];

    // PREVIOUS HANDOFF: entry 0 is a prior synthetic entry when its memory
    // carries <handoff>. The document moves out into its own section and the
    // inline copy keeps a pointer to it, so it isn't summarized twice.
    if let Some(first) = entries.first().cloned() {
        let (prefix0, json0) = entry_parts(&first)?;
        let lifted = (|| -> PyResult<Option<(Bound<'py, PyAny>, Bound<'py, PyString>)>> {
            let data0 = json_loads(&json0)?;
            let Some(prev) = extract_handoff(&data0.call_method1("get", ("memory",))?)? else {
                return Ok(None);
            };
            data0.set_item("memory", "(carried in PREVIOUS HANDOFF above)")?;
            Ok(Some((prefix0.add(json_dumps_pretty(&data0)?)?, prev)))
        })();
        match lifted {
            Ok(Some((entry0, prev))) => {
                entries[0] = entry0;
                out.extend([text(""), text("=== PREVIOUS HANDOFF ==="), prev.into_any()]);
            }
            Ok(None) => {}
            Err(e) if is_exception(py, &e) => {}
            Err(e) => return Err(e),
        }
    }

    // The first task's marker, as the live payload prepends it at step 1 (no="1").
    let first_request = join_str(
        py,
        &[
            &text("<updated_user_request no=\"1\">\n"),
            format_value(task, "")?.as_any(),
            &text("\n</updated_user_request no=\"1\">"),
        ],
    )?;
    out.extend([text(""), text("--- USER ---"), first_request.into_any()]);

    let last = entries.len().checked_sub(1);
    for (i, step) in entries.iter().enumerate() {
        let shown = if Some(i) == last { step.clone() } else { trim_for_dump(step)? };
        out.extend([text(""), text("--- ASSISTANT ---"), shown]);
        let tr = &tools[i];
        if !tr.is_truthy()? {
            continue;
        }
        let is_marker = tr.is_instance_of::<PyString>()
            && tr
                .call_method0("lstrip")?
                .call_method1("startswith", (REQUEST_MARKER_PREFIX,))?
                .is_truthy()?;
        if is_marker {
            // A run-boundary request marker: a user turn in its own right.
            out.extend([text(""), text("--- USER ---"), tr.str()?.into_any()]);
        } else {
            out.extend([
                text(""),
                text("--- USER ---"),
                text("<tool_response>"),
                tr.str()?.into_any(),
                text("</tool_response>"),
            ]);
        }
    }
    // "\n".join(out) + "\n"
    PyString::new(py, "\n")
        .call_method1("join", (PyList::new(py, out)?,))?
        .add("\n")
}

// ---------------------------------------------------------------------------
// For Python: the functions under their old names
// ---------------------------------------------------------------------------

/// The compressor's instructions, system_prompt.md, as one string.
#[pyfunction]
#[pyo3(name = "load_system_prompt")]
fn load_system_prompt_py(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    load_system_prompt(py)
}

/// `<handoff>\n{text}\n</handoff>`.
#[pyfunction]
#[pyo3(name = "wrap_handoff")]
fn wrap_handoff_py<'py>(text: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyString>> {
    wrap_handoff(&text)
}

/// The handoff body if memory_text carries a <handoff> block, else None.
#[pyfunction]
#[pyo3(name = "extract_handoff")]
fn extract_handoff_py<'py>(memory_text: Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyString>>> {
    extract_handoff(&memory_text)
}

/// Split '<Step_no=N />\n{json}' into (prefix, json_str). Bridge entries and
/// other bare-JSON entries give ('', entry).
#[pyfunction]
#[pyo3(name = "_entry_parts")]
fn entry_parts_py<'py>(entry: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let (prefix, json_part) = entry_parts(&entry)?;
    PyTuple::new(entry.py(), [prefix.into_any(), json_part])
}

/// Older dump steps keep decision/memory/next_goal only; the latest step
/// stays full.
#[pyfunction]
#[pyo3(name = "_trim_for_dump")]
fn trim_for_dump_py<'py>(entry: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    trim_for_dump(&entry)
}

/// Step K's original JSON with ONLY its 'memory' field replaced by the
/// wrapped handoff. An unparseable step K falls back to a fresh minimal
/// entry (any <Step_no=K /> prefix kept).
#[pyfunction]
#[pyo3(name = "make_synthetic_entry")]
fn make_synthetic_entry_py<'py>(
    step_k_entry: Bound<'py, PyAny>,
    handoff_text: Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    make_synthetic_entry(&step_k_entry, &handoff_text)
}

/// Render steps [0..k] in the compressor prompt's <input> format. MAIN
/// THREAD ONLY: it snapshots the lists into one string.
#[pyfunction]
#[pyo3(name = "build_dump")]
fn build_dump_py<'py>(
    assistant_messages: Bound<'py, PyAny>,
    tool_responses: Bound<'py, PyAny>,
    k: Bound<'py, PyAny>,
    task: Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    build_dump(&assistant_messages, &tool_responses, &k, &task)
}

// ---------------------------------------------------------------------------
// MemoryCompressionAgent
// ---------------------------------------------------------------------------

struct AgentState {
    llm_manager: Py<PyAny>,
    system_prompt: Py<PyAny>,
}

/// Plain-text handoff compressor. It owns its OWN LLMManager instance, never
/// the main agent's (that one's last_usage would race with the loop's
/// trigger reads).
///
/// Its two attributes live behind a lock that is never held while Python
/// runs, so the worker thread's compress() and the main thread can use the
/// same instance at once, as they could the Python one.
#[pyclass(frozen, dict, weakref, subclass, module = "AutoCua.memory_compression.agent.service")]
pub struct MemoryCompressionAgent {
    state: Mutex<AgentState>,
}

impl MemoryCompressionAgent {
    fn lock(&self) -> MutexGuard<'_, AgentState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read<'py>(&self, py: Python<'py>, field: fn(&AgentState) -> &Py<PyAny>) -> Bound<'py, PyAny> {
        let value = field(&self.lock()).clone_ref(py);
        value.into_bound(py)
    }

    fn write(&self, field: fn(&mut AgentState) -> &mut Py<PyAny>, value: Bound<'_, PyAny>) {
        let old = std::mem::replace(field(&mut self.lock()), value.unbind());
        // Let go of the old value only now, with the lock released: dropping
        // it can run Python code (a __del__).
        drop(old);
    }
}

#[pymethods]
impl MemoryCompressionAgent {
    #[new]
    fn new(llm_manager: Bound<'_, PyAny>) -> PyResult<Self> {
        let system_prompt = load_system_prompt(llm_manager.py())?;
        Ok(MemoryCompressionAgent {
            state: Mutex::new(AgentState {
                llm_manager: llm_manager.unbind(),
                system_prompt: system_prompt.unbind(),
            }),
        })
    }

    #[getter]
    fn llm_manager<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.llm_manager)
    }

    #[setter]
    fn set_llm_manager(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.llm_manager, value);
    }

    #[getter]
    fn system_prompt<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.read(py, |s| &s.system_prompt)
    }

    #[setter]
    fn set_system_prompt(&self, value: Bound<'_, PyAny>) {
        self.write(|s| &mut s.system_prompt, value);
    }

    /// The module's build_dump: steps [0..k] in the compressor's <input> format.
    fn build_dump<'py>(
        &self,
        assistant_messages: Bound<'py, PyAny>,
        tool_responses: Bound<'py, PyAny>,
        k: Bound<'py, PyAny>,
        task: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        build_dump(&assistant_messages, &tool_responses, &k, &task)
    }

    /// One blocking plain-text LLM call (no image, no schema): the system
    /// prompt and the dump, through llm_manager.send_request. Raises on
    /// failure; the caller decides the drop and retry policy.
    fn compress<'py>(&self, dump_text: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = dump_text.py();
        let system = PyDict::new(py);
        system.set_item("role", "system")?;
        system.set_item("content", self.read(py, |s| &s.system_prompt))?;
        let user = PyDict::new(py);
        user.set_item("role", "user")?;
        user.set_item("content", &dump_text)?;
        let messages = PyList::new(py, [system, user])?;
        self.read(py, |s| &s.llm_manager).call_method1("send_request", (messages,))
    }
}

/// Fills AutoCua.agent_native.memory_compression.agent.service: the names
/// agent/service.py had, its imports (json, os, re) and its two patterns
/// included.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("json", py.import("json")?)?;
    m.add("os", py.import("os")?)?;
    let re = py.import("re")?;
    m.add("re", &re)?;
    m.add("HANDOFF_OPEN", HANDOFF_OPEN)?;
    m.add("HANDOFF_CLOSE", HANDOFF_CLOSE)?;
    m.add("_REQUEST_MARKER_PREFIX", REQUEST_MARKER_PREFIX)?;
    let dotall = re.getattr("DOTALL")?;
    m.add("_STEP_PREFIX_RE", re.call_method1("compile", (STEP_PREFIX_PATTERN, &dotall))?)?;
    m.add("_HANDOFF_RE", re.call_method1("compile", (HANDOFF_PATTERN, &dotall))?)?;

    let functions = [
        pyo3::wrap_pyfunction!(load_system_prompt_py, m)?,
        pyo3::wrap_pyfunction!(wrap_handoff_py, m)?,
        pyo3::wrap_pyfunction!(extract_handoff_py, m)?,
        pyo3::wrap_pyfunction!(entry_parts_py, m)?,
        pyo3::wrap_pyfunction!(trim_for_dump_py, m)?,
        pyo3::wrap_pyfunction!(make_synthetic_entry_py, m)?,
        pyo3::wrap_pyfunction!(build_dump_py, m)?,
    ];
    for function in functions {
        function.setattr("__module__", PY_MODULE)?;
        m.add_function(function)?;
    }
    m.add_class::<MemoryCompressionAgent>()?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}

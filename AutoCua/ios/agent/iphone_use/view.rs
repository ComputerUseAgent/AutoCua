//! The iPhone agent's transcript codec, its side of the handoff
//! compression, and the terminal display of a step.
//!
//! NATIVE TRANSCRIPT CODEC. A step is saved as two strings that go
//! together: the assistant turn (its text and the tool calls it made) and
//! the results of those calls. Both are JSON, so the turn is rebuilt exactly
//! on the next request. That is the point of native tool calling: the model
//! sees itself having called tools in the shape every provider was trained
//! on, not a text that says it did. The notes ride on a call of their own:
//! `reasoning` {thinking, memory, next_goal} in quality mode, `memory`
//! {next_goal} in fast mode. Older saves carry them as arguments of the
//! action calls, and the renderers read both. Anything that is not our JSON
//! (a bridge note, a legacy "<Step_no=N />\n{...}" step, a request marker)
//! is replayed as plain text, so resumed chats keep working. A legacy tool
//! slot is wrapped in <tool_response> again, so saves from the schema days
//! replay as the old builder replayed them.
//!
//! HANDOFF COMPRESSION. compression_dump and compression_entry are the two
//! callables CompressionController takes in place of memory_compression's
//! defaults, which cannot read native entries. They handle mixed histories:
//! a legacy step renders as the old default builder rendered it, a native
//! step through snapshot_turn, so the compressor's prompt needs no change.
//!
//! The Rust copy of view.py, and the computer-use agent's view
//! (computer/view.rs) but for two things: snapshot_turn leaves
//! out a note field the step's mode never carried, and extract_tools keeps
//! video_player's sub-command. Python imports it under its old dotted name,
//! AutoCua.ios.agent.iphone_use.view (iphone_use/__init__.py points that
//! name here), and gets the same names, arguments and results.
//! Python's own rules decide every result, so they are called, not
//! imitated: json.loads and json.dumps, str(), format(), truthiness,
//! slicing, comparison and the compiled pattern. The values stay Python
//! objects from end to end, so even a lone surrogate (a JSON escape can put
//! one in a Python string, and Rust strings cannot hold it) comes back
//! exactly as it went in.
//!
//! A name view.py read as a global (json, wrap_handoff, decode_step, ...)
//! is read from this module each time it is used. So replacing it on the
//! module changes what happens here, as it did there.

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyList, PySlice, PyString, PyTuple, PyType};
use pyo3::PyTypeInfo;

use crate::ios::NATIVE;
use crate::computer::{as_python_class, unpack2, Handling};
use crate::memory_compression::{format_value, global, is_exception, join_str};

/// Slot strings that must replay BARE, not wrapped again: the request
/// markers at a run's start, results that are wrapped already, and the
/// compression note.
const BARE_SLOT_PREFIXES: [&str; 3] = ["<updated_user_request", "<tool_response", "<history_compressed"];

/// What the snapshot shows for `thinking` on a step that had none. The model
/// never writes this word: a route step leaves the field out (or passes ""),
/// and the renderer fills it in, so a reader can tell "nothing thought, on
/// purpose" from "nothing recorded".
const SKIPPED_THINKING: &str = "skipped";

/// The `reasoning` call's result on the wire. Every provider needs a result
/// for every call, and this is the smallest one. The snapshot and the
/// compression dump hide it.
const REASONING_RESULT: &str = "ok";

/// The calls whose whole content is the notes: `reasoning`, Claude's
/// `agent_memory`, fast mode's `memory`, and `notes`, its old name, still in
/// older saves.
const NOTE_CALLS: [&str; 4] = ["reasoning", "agent_memory", "memory", "notes"];

/// A legacy entry's '<Step_no=N />' line and the JSON after it, as the
/// Python module spelled it. It is kept as a compiled Python pattern on the
/// module (_STEP_PREFIX_RE), and that pattern does the matching.
const STEP_PREFIX_PATTERN: &str = r"(<Step_no=\d+ />\n)(.*)";

/// The tool slot that goes with the synthetic handoff entry: a plain string
/// that replays as a bare user turn (see BARE_SLOT_PREFIXES), never as a
/// role "tool" result with no call, never as two assistant turns in a row.
const COMPRESSION_NOTE: &str = "<history_compressed>\nEarlier steps of this session were \
compressed into the handoff document above. Trust it as \
accurate memory and continue from the state it describes.\n\
</history_compressed>";

/// (field, label) in the class's order, the order of format_response's
/// lines.
const FIELD_EMOJIS: [(&str, &str); 4] = [
    ("thinking", "🧠 Thinking"),
    ("next_goal", "🎯 Next Goal"),
    ("memory", "💾 Memory"),
    ("action", "⚡ Action"),
];

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || {
            py.import(format!("{NATIVE}.agent.iphone_use.view")).map(Bound::unbind)
        })
        .map(|m| m.bind(py).clone())
}

/// One of this module's globals (or a builtin), read now.
fn name<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    global(&module(py)?, name)
}

// ---------------------------------------------------------------------------
// Python's rules
// ---------------------------------------------------------------------------

/// `isinstance(value, T)`.
fn is_a<T: PyTypeInfo>(value: &Bound<'_, PyAny>) -> PyResult<bool> {
    value.is_instance(&value.py().get_type::<T>())
}

/// `value or ""`.
fn or_text<'py>(value: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    if value.is_truthy()? {
        Ok(value)
    } else {
        Ok(PyString::new(value.py(), "").into_any())
    }
}

/// `value or []`.
fn or_list<'py>(value: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    if value.is_truthy()? {
        Ok(value)
    } else {
        Ok(PyList::empty(value.py()).into_any())
    }
}

/// `value or {}`.
fn or_dict<'py>(value: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    if value.is_truthy()? {
        Ok(value)
    } else {
        Ok(PyDict::new(value.py()).into_any())
    }
}

/// `slice(None, stop)`, what `seq[:stop]` indexes with.
fn slice_to<'py>(stop: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = stop.py();
    py.get_type::<PySlice>().call1((py.None(), stop))
}

/// `json.loads`, through this module's `json`.
fn json_loads(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    name(py, "json")?.getattr("loads")
}

/// `json.dumps`, through this module's `json`. Python's own encoder, so
/// floats, big ints, NaN and lone surrogates come out as before.
fn json_dumps(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    name(py, "json")?.getattr("dumps")
}

/// The keywords of `json.dumps(value, ensure_ascii=False)`.
fn compact(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("ensure_ascii", false)?;
    Ok(kwargs)
}

/// The keywords of `json.dumps(value, indent=2, ensure_ascii=False)`.
fn pretty(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("indent", 2)?;
    kwargs.set_item("ensure_ascii", false)?;
    Ok(kwargs)
}

/// `text.lstrip().startswith(prefix)`, as a truth value.
fn starts_with(text: &Bound<'_, PyAny>, prefix: &Bound<'_, PyAny>) -> PyResult<bool> {
    text.call_method0("lstrip")?.getattr("startswith")?.call1((prefix,))?.is_truthy()
}

/// `f"<tool_response>\n{text}\n</tool_response>"`.
fn tool_response<'py>(text: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = text.py();
    let open = PyString::new(py, "<tool_response>\n");
    let close = PyString::new(py, "\n</tool_response>");
    Ok(join_str(py, &[open.as_any(), format_value(text, "")?.as_any(), close.as_any()])?.into_any())
}

/// An argument as the call gave it, or Missing when the call left it out
/// and the default Python's def bound applies.
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

// ---------------------------------------------------------------------------
// The codec
// ---------------------------------------------------------------------------

/// Persisted assistant turn -> {"content": str, "tool_calls": list, "meta":
/// dict}. `meta` is the provider's own per-turn metadata (see encode_step).
/// It is {} for turns saved before it existed and for providers that give
/// none, so old history replays exactly as before.
#[pyfunction]
fn decode_step<'py>(raw: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = raw.py();
    // What anything that is not our JSON replays as: its own text, no calls.
    let prose = || -> PyResult<Bound<'py, PyDict>> {
        let content = or_text(raw.clone())?.str()?;
        let step = PyDict::new(py);
        step.set_item("content", content)?;
        step.set_item("tool_calls", PyList::empty(py))?;
        step.set_item("meta", PyDict::new(py))?;
        Ok(step)
    };
    let parsed = (|| json_loads(py)?.call1((raw.str()?,)))();
    let data = match parsed {
        Ok(data) => data,
        Err(e) if is_exception(py, &e) => {
            let _handling = Handling::new(py, &e);
            return prose();
        }
        Err(e) => return Err(e),
    };
    if !is_a::<PyDict>(&data)? {
        return prose();
    }
    let calls = data.call_method1("get", ("tool_calls",))?;
    let meta = data.call_method1("get", ("provider_meta",))?;
    if is_a::<PyList>(&calls)? && (data.contains("content")? || calls.is_truthy()?) {
        let content = or_text(data.call_method1("get", ("content",))?)?.str()?;
        let meta = if is_a::<PyDict>(&meta)? { meta } else { PyDict::new(py).into_any() };
        let step = PyDict::new(py);
        step.set_item("content", content)?;
        step.set_item("tool_calls", calls)?;
        step.set_item("meta", meta)?;
        return Ok(step);
    }
    // A legacy 4-block schema step, or a bridge note: its prose replays.
    prose()
}

/// Assistant turn -> persisted string. `calls` are OpenAI-shaped: {"id",
/// "type": "function", "function": {"name", "arguments": json}}.
///
/// `meta` is the PROVIDER's own per-turn metadata, carried so the turn can
/// be sent back exactly as the model made it: Gemini 3 thought signatures,
/// OpenRouter reasoning blocks. Tool calling on a reasoning model breaks
/// without it. It is written only when it holds something, and each
/// provider tags and checks its own name, so a resumed chat can never hand
/// one provider another's blob.
#[pyfunction]
#[pyo3(signature = (text, calls, meta=None))]
fn encode_step<'py>(
    text: Bound<'py, PyAny>,
    calls: Bound<'py, PyAny>,
    meta: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = text.py();
    let content = or_text(text)?;
    let calls = or_list(calls)?;
    let data = PyDict::new(py);
    data.set_item("content", content)?;
    data.set_item("tool_calls", calls)?;
    if let Some(meta) = meta {
        if meta.is_truthy()? {
            data.set_item("provider_meta", meta)?;
        }
    }
    json_dumps(py)?.call((data,), Some(&compact(py)?))
}

/// Persisted results -> the `role: "tool"` messages of one step. A legacy
/// plain string replays as a USER turn. Unless it is a request marker (or
/// wrapped already), it is wrapped in <tool_response> again, exactly how
/// the old builder replayed the tool slots of the schema days.
#[pyfunction]
fn decode_results<'py>(raw: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
    let py = raw.py();
    if !raw.is_truthy()? {
        return Ok(PyList::empty(py));
    }
    let parsed = (|| json_loads(py)?.call1((raw.str()?,)))();
    let data = match parsed {
        Ok(data) => data,
        Err(e) if is_exception(py, &e) => py.None().into_bound(py),
        Err(e) => return Err(e),
    };
    if is_a::<PyList>(&data)? {
        let mut native = true;
        for d in data.try_iter()? {
            let d = d?;
            if !(is_a::<PyDict>(&d)? && d.contains("tool_call_id")?) {
                native = false;
                break;
            }
        }
        if native {
            let results = PyList::empty(py);
            for d in data.try_iter()? {
                let d = d?;
                let id = d.get_item("tool_call_id")?;
                let content = or_text(d.call_method1("get", ("content",))?)?.str()?;
                let result = PyDict::new(py);
                result.set_item("role", "tool")?;
                result.set_item("tool_call_id", id)?;
                result.set_item("content", content)?;
                results.append(result)?;
            }
            return Ok(results);
        }
    }
    let mut text = raw.str()?.into_any();
    if !starts_with(&text, &name(py, "_BARE_SLOT_PREFIXES")?)? {
        text = tool_response(&text)?;
    }
    let turn = PyDict::new(py);
    turn.set_item("role", "user")?;
    turn.set_item("content", text)?;
    PyList::new(py, [turn])
}

/// [{"tool_call_id", "content"}] -> persisted string.
#[pyfunction]
fn encode_results<'py>(results: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = results.py();
    let dumps = json_dumps(py)?;
    dumps.call((or_list(results)?,), Some(&compact(py)?))
}

/// Normalized calls -> the OpenAI-shaped `tool_calls` the assistant turn
/// carries on the wire. The arguments are written out again EXACTLY as the
/// model sent them (the tracking parameters included), so the transcript
/// sent back never goes against the schema that asked for them.
#[pyfunction]
fn wire_calls_from<'py>(calls: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
    let py = calls.py();
    let wire = PyList::empty(py);
    for call in or_list(calls)?.try_iter()? {
        let call = call?;
        let id = call.get_item("id")?;
        let function_name = call.get_item("name")?;
        let dumps = json_dumps(py)?;
        let arguments = or_dict(call.call_method1("get", ("arguments",))?)?;
        let arguments = dumps.call((arguments,), Some(&compact(py)?))?;
        let function = PyDict::new(py);
        function.set_item("name", function_name)?;
        function.set_item("arguments", arguments)?;
        let item = PyDict::new(py);
        item.set_item("id", id)?;
        item.set_item("type", "function")?;
        item.set_item("function", function)?;
        wire.append(item)?;
    }
    Ok(wire)
}

/// DISPLAY ONLY: one native assistant turn in the familiar block shape
/// {"thinking", "memory", "next_goal", "action"}. The notes are taken OUT of
/// the call arguments (the `reasoning` or `agent_memory` call in quality
/// mode, the `memory` call in fast mode, the first action call in older
/// saves) and shown as fields of their own. The notes call itself is NOT
/// listed under action, and a `thinking` the `reasoning` call left out
/// renders as "skipped". The API payload is untouched: the model keeps
/// getting the native transcript.
///
/// A field the step's MODE never carried is left out, not rendered blank:
/// fast mode has no `thinking`, and an empty one would read as a dropped
/// block. A turn with no calls at all (a compression handoff, the repair
/// turn after no tool was called) keeps all three, so its prose still has
/// somewhere to land.
#[pyfunction]
fn snapshot_turn<'py>(text: Bound<'py, PyAny>, calls: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = text.py();
    let empty = || PyString::new(py, "").into_any();
    let actions = PyList::empty(py);
    // values and present, both in the order thinking, memory, next_goal
    let mut values = [empty(), empty(), empty()];
    let mut present = [false; 3];
    for tc in or_list(calls.clone())?.try_iter()? {
        // fn = (tc or {}).get("function") or {}
        let function = or_dict(or_dict(tc?)?.call_method1("get", ("function",))?)?;
        let mut args = function.call_method1("get", ("arguments",))?;
        if is_a::<PyString>(&args)? {
            let parsed = (|| json_loads(py)?.call1((&args,)))();
            args = match parsed {
                Ok(parsed) => parsed,
                // except (ValueError, TypeError)
                Err(e) if e.is_instance_of::<PyValueError>(py) || e.is_instance_of::<PyTypeError>(py) => {
                    let kept = PyDict::new(py);
                    kept.set_item("raw_arguments", &args)?;
                    kept.into_any()
                }
                Err(e) => return Err(e),
            };
        }
        if !is_a::<PyDict>(&args)? {
            args = PyDict::new(py).into_any();
        }
        // args = dict(args)
        let args = py.get_type::<PyDict>().call1((args,))?.cast_into::<PyDict>()?;
        // name = fn.get("name") or ""
        let called = or_text(function.call_method1("get", ("name",))?)?;
        let is_note = PyTuple::new(py, NOTE_CALLS)?.contains(&called)?;
        if PyTuple::new(py, ["reasoning", "agent_memory"])?.contains(&called)? {
            // The `reasoning` call carries all three by contract: a
            // `thinking` it left out is a route step and must still render
            // (as "skipped"). Claude's `agent_memory` renders the same way:
            // its thinking slot carries the model's own prose when there is
            // any, else "skipped". The fast `memory` call carries
            // `next_goal` alone, which the loop below picks up, so fast
            // turns keep leaving `thinking` out.
            present = [true; 3];
        }
        // Popped UNCONDITIONALLY, then the first value that holds something
        // is kept: later calls carry "" and must not render as action fields.
        for (i, field) in ["thinking", "memory", "next_goal"].into_iter().enumerate() {
            if args.contains(field)? {
                present[i] = true;
            }
            let step_value = or_text(args.call_method1("pop", (field, ""))?)?.str()?.call_method0("strip")?;
            if !values[i].is_truthy()? {
                values[i] = step_value;
            }
        }
        if is_note {
            // Its whole content is the note fields above.
            continue;
        }
        // {"type": fn.get("name") or "", **args}
        let action = PyDict::new(py);
        action.set_item("type", or_text(function.call_method1("get", ("name",))?)?)?;
        action.update(args.as_mapping())?;
        actions.append(action)?;
    }
    if !calls.is_truthy()? {
        present = [true; 3];
    }
    let [thinking, memory, next_goal] = values;
    let rendered = PyDict::new(py);
    if present[0] {
        // values["thinking"] or (text or "").strip() or SKIPPED_THINKING
        let thinking = if thinking.is_truthy()? {
            thinking
        } else {
            let spoken = or_text(text)?.call_method0("strip")?;
            if spoken.is_truthy()? {
                spoken
            } else {
                name(py, "SKIPPED_THINKING")?
            }
        };
        rendered.set_item("thinking", thinking)?;
    }
    if present[1] {
        rendered.set_item("memory", memory)?;
    }
    if present[2] {
        rendered.set_item("next_goal", next_goal)?;
    }
    rendered.set_item("action", actions)?;
    json_dumps(py)?.call((rendered,), Some(&pretty(py)?))
}

// ---------------------------------------------------------------------------
// Handoff compression
// ---------------------------------------------------------------------------

// str(value), cut to `limit` characters with a note of how many were cut.
// A plain comment, as on _looks_native below: the Python function had no
// docstring, and a doc comment here would give it one.
#[pyfunction]
#[pyo3(name = "_clip", signature = (value, limit = Given::Missing), text_signature = "(value, limit=300)")]
fn clip<'py>(value: Bound<'py, PyAny>, limit: Given<'py>) -> PyResult<Bound<'py, PyAny>> {
    let py = value.py();
    let limit = match limit {
        Given::Value(limit) => limit,
        Given::Missing => 300i64.into_pyobject(py)?.into_any(),
    };
    let s = value.str()?;
    // s if len(s) <= limit else s[:limit] + f"... [clipped {len(s) - limit} chars]"
    if s.len()?.into_pyobject(py)?.le(&limit)? {
        return Ok(s.into_any());
    }
    let head = s.get_item(slice_to(&limit)?)?;
    let cut = s.len()?.into_pyobject(py)?.sub(&limit)?;
    let open = PyString::new(py, "... [clipped ");
    let close = PyString::new(py, " chars]");
    let tail = join_str(py, &[open.as_any(), format_value(&cut, "")?.as_any(), close.as_any()])?;
    head.add(tail)
}

// True for a native turn: JSON with "content" and a list of "tool_calls".
#[pyfunction]
#[pyo3(name = "_looks_native")]
fn looks_native(raw: Bound<'_, PyAny>) -> PyResult<bool> {
    let py = raw.py();
    let parsed = (|| json_loads(py)?.call1((raw.str()?,)))();
    let d = match parsed {
        Ok(d) => d,
        Err(e) if is_exception(py, &e) => return Ok(false),
        Err(e) => return Err(e),
    };
    Ok(is_a::<PyDict>(&d)? && d.contains("content")? && is_a::<PyList>(&d.call_method1("get", ("tool_calls",))?)?)
}

/// The JSON body of an entry from the schema days, with any '<Step_no=N />'
/// line before it taken off (the dump stamps its own, with new numbers).
#[pyfunction]
#[pyo3(name = "_legacy_dump_body")]
fn legacy_dump_body<'py>(raw_entry: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = raw_entry.py();
    let matcher = name(py, "_STEP_PREFIX_RE")?.getattr("match")?;
    let m = matcher.call1((raw_entry.str()?,))?;
    if m.is_truthy()? {
        m.call_method1("group", (2,))
    } else {
        Ok(raw_entry.str()?.into_any())
    }
}

/// One persisted assistant entry -> text the compressor can read. A native
/// turn renders through snapshot_turn ({thinking, memory, next_goal,
/// action}). A legacy turn renders as the old default builder rendered it:
/// older steps keep only {memory, next_goal}, the last step stays full.
/// Bridge notes and other prose that is not JSON replay as they are.
#[pyfunction]
#[pyo3(name = "_dump_step")]
fn dump_step<'py>(raw_entry: Bound<'py, PyAny>, full: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = raw_entry.py();
    let step = name(py, "decode_step")?.call1((&raw_entry,))?;
    if step.get_item("tool_calls")?.is_truthy()? || name(py, "_looks_native")?.call1((&raw_entry,))?.is_truthy()? {
        let loads = json_loads(py)?;
        let snapshot = name(py, "snapshot_turn")?;
        let rendered = loads.call1((snapshot.call1((step.get_item("content")?, step.get_item("tool_calls")?))?,))?;
        if !full.is_truthy()? {
            // rendered["action"] = [{key: _clip(val) for key, val in action.items()}
            //                       for action in rendered.get("action", [])]
            let clipped = PyList::empty(py);
            for action in rendered.call_method1("get", ("action", PyList::empty(py)))?.try_iter()? {
                let short = PyDict::new(py);
                for pair in action?.call_method0("items")?.try_iter()? {
                    let (key, val) = unpack2(&pair?)?;
                    short.set_item(key, name(py, "_clip")?.call1((val,))?)?;
                }
                clipped.append(short)?;
            }
            rendered.set_item("action", clipped)?;
        }
        return json_dumps(py)?.call((rendered,), Some(&pretty(py)?));
    }
    let body = name(py, "_legacy_dump_body")?.call1((&raw_entry,))?;
    if full.is_truthy()? {
        return Ok(body);
    }
    let parsed = (|| json_loads(py)?.call1((&body,)))();
    let data = match parsed {
        Ok(data) => data,
        Err(e) if is_exception(py, &e) => return Ok(body),
        Err(e) => return Err(e),
    };
    if !is_a::<PyDict>(&data)? {
        return Ok(body);
    }
    for key in ["thinking", "eval", "action"] {
        data.call_method1("pop", (key, py.None()))?;
    }
    json_dumps(py)?.call((data,), Some(&pretty(py)?))
}

/// The handoff document of an earlier compression, whichever generation
/// wrote it: a native synthetic entry carries it in its CONTENT, a legacy
/// one carried it in the step's `memory` field.
#[pyfunction]
#[pyo3(name = "_prior_handoff")]
fn prior_handoff<'py>(entry0: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = entry0.py();
    let step = name(py, "decode_step")?.call1((&entry0,))?;
    if step.get_item("tool_calls")?.is_truthy()? || name(py, "_looks_native")?.call1((&entry0,))?.is_truthy()? {
        return name(py, "extract_handoff")?.call1((step.get_item("content")?,));
    }
    let parsed = (|| {
        let loads = json_loads(py)?;
        loads.call1((name(py, "_legacy_dump_body")?.call1((&entry0,))?,))
    })();
    let data = match parsed {
        Ok(data) => data,
        Err(e) if is_exception(py, &e) => return Ok(py.None().into_bound(py)),
        Err(e) => return Err(e),
    };
    if is_a::<PyDict>(&data)? {
        let extract = name(py, "extract_handoff")?;
        return extract.call1((or_text(data.call_method1("get", ("memory",))?)?.str()?,));
    }
    Ok(py.None().into_bound(py))
}

/// The iPhone agent's flavor of memory_compression's build_dump: steps [0..k]
/// in the compressor prompt's <input> format, for histories of mixed
/// generations.
///
/// MAIN THREAD ONLY: it snapshots the lists' content into one string, so
/// the worker thread never reads the live lists (the same contract as the
/// default's).
#[pyfunction]
fn compression_dump<'py>(
    assistant_messages: Bound<'py, PyAny>,
    tool_responses: Bound<'py, PyAny>,
    k: Bound<'py, PyAny>,
    task: Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = k.py();
    let text = |s: &str| PyString::new(py, s).into_any();

    // entries = [str(e) for e in assistant_messages[:k + 1]]
    let mut entries: Vec<Bound<'py, PyAny>> = Vec::new();
    for entry in assistant_messages.get_item(slice_to(&k.add(1)?)?)?.try_iter()? {
        entries.push(entry?.str()?.into_any());
    }
    // tools = list(tool_responses[:k + 1]), padded with None to len(entries)
    let tools = py
        .get_type::<PyList>()
        .call1((tool_responses.get_item(slice_to(&k.add(1)?)?)?,))?
        .cast_into::<PyList>()?;
    for _ in tools.len()..entries.len() {
        tools.append(py.None())?;
    }

    let mut out: Vec<Bound<'py, PyAny>> = vec![
        text("Session: live (in-run rolling compression)"),
        join_str(py, &[&text("Task: "), format_value(&task, "")?.as_any()])?.into_any(),
        text("Trigger: rolling"),
    ];

    // PREVIOUS HANDOFF: entry 0 is an earlier synthetic entry when it
    // carries a <handoff>. The document moves out into its own section and
    // the entry is skipped (its note slot with it), so it isn't summarized
    // twice.
    let mut start = 0;
    if let Some(first) = entries.first() {
        let prev = name(py, "_prior_handoff")?.call1((first,))?;
        if !prev.is_none() {
            out.extend([text(""), text("=== PREVIOUS HANDOFF ==="), prev]);
            start = 1;
        }
    }

    let first_request = join_str(
        py,
        &[
            &text("<updated_user_request no=\"1\">\n"),
            format_value(&task, "")?.as_any(),
            &text("\n</updated_user_request no=\"1\">"),
        ],
    )?;
    out.extend([text(""), text("--- USER ---"), first_request.into_any()]);

    let mut step_no = 0u64;
    let last = entries.len().checked_sub(1);
    for (i, entry) in entries.iter().enumerate().skip(start) {
        step_no += 1;
        let stamp = text(&format!("<Step_no={step_no} />"));
        // _dump_step(entries[i], full=(i == last))
        let full = PyDict::new(py);
        full.set_item("full", Some(i) == last)?;
        let shown = name(py, "_dump_step")?.call((entry,), Some(&full))?;
        out.extend([text(""), text("--- ASSISTANT ---"), stamp, shown]);
        let tr = tools.get_item(i)?;
        if !tr.is_truthy()? {
            continue;
        }
        let results = name(py, "decode_results")?.call1((&tr,))?;
        if results.is_truthy()? && results.get_item(0)?.call_method1("get", ("role",))?.eq("tool")? {
            // The `reasoning` call's bare `ok` is wire plumbing, not a result.
            let contents = PyList::empty(py);
            for r in results.try_iter()? {
                let r = r?;
                let content = or_text(r.call_method1("get", ("content",))?)?.str()?.into_any();
                if content.ne(name(py, "REASONING_RESULT")?)? {
                    contents.append(or_text(r.call_method1("get", ("content",))?)?.str()?)?;
                }
            }
            if contents.is_empty() {
                continue;
            }
            let block = text("<tool_response>\n")
                .add(text("\n").call_method1("join", (contents,))?)?
                .add("\n</tool_response>")?;
            out.extend([text(""), text("--- USER ---"), block]);
        } else {
            let raw = tr.str()?.into_any();
            if starts_with(&raw, &text("<updated_user_request"))? {
                // The step numbers start again after every new task.
                step_no = 0;
                out.extend([text(""), text("--- USER ---"), raw]);
            } else if starts_with(&raw, &text("<tool_response"))? {
                out.extend([text(""), text("--- USER ---"), raw]);
            } else {
                out.extend([text(""), text("--- USER ---"), tool_response(&raw)?]);
            }
        }
    }
    // "\n".join(out) + "\n"
    text("\n").call_method1("join", (PyList::new(py, out)?,))?.add("\n")
}

/// (entry, tool_slot) that replace entries [0..k] once a handoff lands: ONE
/// native assistant turn with content only, the wrapped handoff, paired with
/// the note slot, a plain string. step_k_entry is not used: the arguments
/// are the ones the controller's default takes.
#[pyfunction]
fn compression_entry<'py>(
    step_k_entry: Bound<'py, PyAny>,
    handoff_text: Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyTuple>> {
    let py = handoff_text.py();
    let _ = step_k_entry;
    let encode = name(py, "encode_step")?;
    let wrapped = name(py, "wrap_handoff")?.call1((handoff_text,))?;
    let entry = encode.call1((wrapped, PyList::empty(py)))?;
    PyTuple::new(py, [entry, name(py, "_COMPRESSION_NOTE")?])
}

// ---------------------------------------------------------------------------
// AgentResponseFormatter
// ---------------------------------------------------------------------------

/// Formats agent JSON responses for terminal display
#[pyclass(frozen, dict, weakref, subclass)]
pub struct AgentResponseFormatter;

#[pymethods]
impl AgentResponseFormatter {
    /// The class has no state. A call that gives arguments is turned down
    /// with Python's own words, unless a subclass has an __init__ to take
    /// them.
    #[new]
    #[classmethod]
    #[pyo3(signature = (*args, **kwargs), text_signature = "()")]
    fn new(cls: &Bound<'_, PyType>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<Self> {
        let py = cls.py();
        let given = !args.is_empty() || kwargs.is_some_and(|k| !k.is_empty());
        if given {
            let own_init = cls.getattr("__init__")?;
            let object_init = py.get_type::<PyAny>().getattr("__init__")?;
            if own_init.is(&object_init) {
                let class_name: String = cls.getattr("__name__")?.extract()?;
                return Err(PyTypeError::new_err(format!("{class_name}() takes no arguments")));
            }
        }
        Ok(AgentResponseFormatter)
    }

    /// The label each field is shown under, in the order of the lines.
    #[classattr]
    #[pyo3(name = "FIELD_EMOJIS")]
    fn field_emojis(py: Python<'_>) -> PyResult<Py<PyDict>> {
        let labels = PyDict::new(py);
        for (field, label) in FIELD_EMOJIS {
            labels.set_item(field, label)?;
        }
        Ok(labels.unbind())
    }

    /// This turn's tools from the parsed action block, in the order they
    /// run, for the frontend's tool-flow chain. e.g.
    ///     [{"name": "left_click", "clicks": 2}, {"name": "input"}, {"name": "web"}]
    #[staticmethod]
    fn extract_tools<'py>(normalized_response: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
        let py = normalized_response.py();
        let tools = PyList::empty(py);
        let read = (|| -> PyResult<()> {
            let data = json_loads(py)?.call1((&normalized_response,))?;
            let mut actions = data.call_method1("get", ("action", PyList::empty(py)))?;
            if is_a::<PyDict>(&actions)? {
                actions = PyList::new(py, [actions])?.into_any();
            }
            for item in actions.try_iter()? {
                let item = item?;
                if !is_a::<PyDict>(&item)? {
                    continue;
                }
                let tool_name = item.call_method1("get", ("type",))?;
                if !tool_name.is_truthy()? {
                    continue;
                }
                let tool = PyDict::new(py);
                tool.set_item("name", &tool_name)?;
                for field in ["clicks", "direction"] {
                    if item.contains(field)? {
                        tool.set_item(field, item.call_method1("get", (field,))?)?;
                    }
                }
                // video_player's sub-command (play, pause, close, streaming)
                // picks the media sign in the frontend's tool chain.
                if tool_name.eq("video_player")? && item.contains("value")? {
                    tool.set_item("value", item.call_method1("get", ("value",))?)?;
                }
                tools.append(tool)?;
            }
            Ok(())
        })();
        match read {
            Err(e) if !is_exception(py, &e) => Err(e),
            // What was found before an error stays.
            _ => Ok(tools),
        }
    }

    /// The normalized JSON response as readable lines for the terminal, one
    /// per field, each under its label. include_action: true keeps the
    /// action block (the terminal), false leaves it out (the frontend's
    /// stream). Anything that cannot be read comes back as it was given.
    #[staticmethod]
    #[pyo3(
        signature = (normalized_response, include_action = Given::Missing),
        text_signature = "(normalized_response, include_action=False)"
    )]
    fn format_response<'py>(
        normalized_response: Bound<'py, PyAny>,
        include_action: Given<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = normalized_response.py();
        let include_action = match include_action {
            Given::Value(flag) => flag,
            Given::Missing => false.into_pyobject(py)?.to_owned().into_any(),
        };
        let text = |s: &str| PyString::new(py, s).into_any();
        let formatted = (|| -> PyResult<Bound<'py, PyAny>> {
            // The clean JSON string, parsed as it is (no markdown around it).
            let data = json_loads(py)?.call1((&normalized_response,))?;
            let lines = PyList::empty(py);
            let fields = name(py, "AgentResponseFormatter")?.getattr("FIELD_EMOJIS")?.call_method0("items")?;
            for pair in fields.try_iter()? {
                let (field, emoji_label) = unpack2(&pair?)?;
                if !data.contains(&field)? {
                    continue;
                }
                let mut value = data.get_item(&field)?;
                // The action field is left out unless include_action is set:
                // the frontend must not stream it.
                if field.eq("action")? && !include_action.is_truthy()? {
                    continue;
                }
                // A dict or a list is shown as JSON.
                let containers = PyTuple::new(py, [py.get_type::<PyDict>(), py.get_type::<PyList>()])?;
                if value.is_instance(&containers)? {
                    let indent = PyDict::new(py);
                    indent.set_item("indent", 2)?;
                    value = json_dumps(py)?.call((value,), Some(&indent))?;
                }
                // f"- {emoji_label}: {value}"
                let line = join_str(
                    py,
                    &[
                        &text("- "),
                        format_value(&emoji_label, "")?.as_any(),
                        &text(": "),
                        format_value(&value, "")?.as_any(),
                    ],
                )?;
                lines.append(line)?;
            }
            text("\n").call_method1("join", (lines,))
        })();
        match formatted {
            Ok(shown) => Ok(shown),
            // On any error, the response as it was given.
            Err(e) if is_exception(py, &e) => Ok(normalized_response),
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes view.py's place: the names view.py had, in
/// its order, the stdlib names it imported included (json, re, os, time,
/// Path). wrap_handoff and extract_handoff, which it imported from
/// memory_compression, are bound by the loader when the package is
/// imported, the moment view.py's import line ran.
///
/// `m` carries Python's name for the module (lib.rs makes it so), which the
/// functions and the class then report as their __module__.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("json", py.import("json")?)?;
    let re = py.import("re")?;
    m.add("re", &re)?;
    m.add("os", py.import("os")?)?;
    m.add("time", py.import("time")?)?;
    m.add("Path", py.import("pathlib")?.getattr("Path")?)?;

    m.add_function(pyo3::wrap_pyfunction!(decode_step, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(encode_step, m)?)?;
    m.add("_BARE_SLOT_PREFIXES", PyTuple::new(py, BARE_SLOT_PREFIXES)?)?;
    m.add_function(pyo3::wrap_pyfunction!(decode_results, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(encode_results, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(wire_calls_from, m)?)?;
    m.add("SKIPPED_THINKING", SKIPPED_THINKING)?;
    m.add("REASONING_RESULT", REASONING_RESULT)?;
    m.add_function(pyo3::wrap_pyfunction!(snapshot_turn, m)?)?;
    m.add("_STEP_PREFIX_RE", re.call_method1("compile", (STEP_PREFIX_PATTERN, re.getattr("DOTALL")?))?)?;
    m.add("_COMPRESSION_NOTE", COMPRESSION_NOTE)?;
    m.add_function(pyo3::wrap_pyfunction!(clip, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(looks_native, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(legacy_dump_body, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(dump_step, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(prior_handoff, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(compression_dump, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(compression_entry, m)?)?;
    m.add_class::<AgentResponseFormatter>()?;
    let python_name = m.name()?;
    let class = py.get_type::<AgentResponseFormatter>();
    as_python_class(&class, &python_name)?;
    for method in ["extract_tools", "format_response"] {
        class.getattr(method)?.setattr("__module__", &python_name)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}

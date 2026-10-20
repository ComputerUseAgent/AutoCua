"""Computer-use training data, recorded from the agent's own runs.

One file for every platform: each driver hands it plain values (its screen,
its clean frame, its element mapping), so nothing here knows which OS it runs
on. Wired into the mac driver today.

With ENABLED on, every run records into AutoCua_data/dataset/. Runs only ever
add to it, so the dataset grows with each run and nothing already recorded is
touched:

    dataset/
        index.jsonl                 one line per finished run (an episode with
                                    several requests repeats; its LAST line is current)
        episodes/<episode>/
            episode.json            task, model, screen, outcome, and one entry per request
            steps.jsonl             one line per step, written as the run goes
            frames/step_NNN.png     the CLEAN screenshot the step was decided on

An episode is one continuous trajectory. A main.py run is one episode. A
desktop-app chat is ONE episode too: every task asked in that chat APPENDS to
the same folder (steps and frames keep counting, each step carries its request
number, the header lists every request with its task text and first step), so
the record has the same shape as the history the model sees at run time - the
earlier steps, then a new request, then the steps that answer it.

A step keeps both readings of one decision: what the teacher saw (the element
tree and its ids) and where each action really landed (the controller's own
click point, in screen points and on the 0-1000 grid of the frame). So one step
can train a vision-only model (frame -> coordinate) and a tree model (tree -> id).
"""

import io
import json
import os
import secrets
from datetime import datetime

from . import data_root

# Record every run into the dataset. False: runs behave exactly as before.
ENABLED = False

# The desktop app's chat this run belongs to (its chat_<id>), set by the app
# around each computer-use run; None for main.py runs. The chat IS the episode:
# its folder is episodes/<chat_id>, and every later task in the chat reopens
# it and appends (see Episode).
chat_id = None

# Tools the agent does WITHOUT while recording. A recorded run is a lesson in
# GUI work, so a tool whose work leaves no trace on screen is not sent at all
# rather than sent and ignored - the model only ever sees tools it may use:
#   open_app   an app opened by name teaches nothing; let it be opened the way
#              a person does it, on screen.
#   sub_agent / agent_wait   a sub-agent (coder, browser, iPhone) works off
#              this screen, so no frame explains what happened. With these
#              gone no sub-agent can start, and <sub_agents> says offline.
#   web        an answer fetched by another model, nothing on screen.
#   applescript  drives an app from the outside; the same work must be
#              done on screen instead. (mac only; harmless elsewhere.)
# shell is kept on purpose (owner decision, 2026-10-11).
# Recording off: every tool is sent, exactly as before.
OFF_TOOLS = frozenset(("open_app", "sub_agent", "agent_wait", "web", "applescript"))


def live_tools(tools):
    """The main driver's registry minus OFF_TOOLS while recording.

    Each platform's tool_registry calls this once, where its MAIN_TOOLS are
    built, so the tools the model is GIVEN and the names a call is CHECKED
    against (MAIN_TOOL_NAMES*, derived from the same lists) can never
    disagree: a withheld tool is simply not a tool this run."""
    return tools if not ENABLED else [t for t in tools if t.get("name") not in OFF_TOOLS]


# Tools that act at one element's point. drag_drop acts at two ("8 to 15").
_POINTER_TOOLS = ("left_click", "right_click", "input", "scroll")


def begin(task, platform, provider, model, speed, screen, original_task=None, resumed=False):
    """Open this run's episode, or None while recording is off.

    screen: {x, y, width, height, scale} of the captured display, in the same
    units as the element rects and click points. resumed: this run continues
    an earlier chat, so `task` is the follow-up and `original_task` the goal
    the whole chat started from."""
    if not ENABLED:
        return None
    try:
        return Episode(task, platform, provider, model, speed, screen, original_task, resumed)
    except Exception as e:
        print(f"Dataset: not recording this run ({e})")
        return None


class Episode:
    """One trajectory: a header, then its steps as they happen.

    A chat's episode is reopened by every later request of that chat: the
    step and frame numbers carry on from what is already on disk (counted
    from the files, so a run that crashed before finish() cannot make the next
    one overwrite anything), and the header's `requests` list gets one more
    entry. A chat whose folder does not exist yet (recording switched on
    mid-chat, or the dataset cleared) starts a fresh episode with
    `resumed: true`, so the converter can see its history is cut off."""

    def __init__(self, task, platform, provider, model, speed, screen,
                 original_task=None, resumed=False):
        self.root = data_root() / "dataset"
        self.id = chat_id or datetime.now().strftime("%Y-%m-%d_%H-%M-%S_") + secrets.token_hex(3)
        self.dir = self.root / "episodes" / self.id
        header_path = self.dir / "episode.json"
        self.header = _read_json(header_path) if chat_id and header_path.exists() else None
        if self.header is None:
            (self.dir / "frames").mkdir(parents=True, exist_ok=True)
            self.header = {
                "episode": self.id, "task": task,
                "original_task": original_task or task, "resumed": bool(resumed), "chat": chat_id,
                "platform": platform, "provider": provider, "model": model, "speed": speed,
                "screen": screen, "started_at": _now(), "requests": [],
            }
        steps_path = self.dir / "steps.jsonl"
        self.steps = sum(1 for _ in steps_path.open("rb")) if steps_path.exists() else 0
        self.frames = sum(1 for p in (self.dir / "frames").iterdir() if p.suffix == ".png")
        self._base = self.steps          # this request's steps count on from here
        self.request = len(self.header["requests"]) + 1
        self.header["requests"].append({
            "no": self.request, "task": task, "resumed": bool(resumed),
            "first_step": self.steps + 1, "provider": provider, "model": model,
            "speed": speed, "started_at": _now(),
        })
        _write_json(header_path, self.header)
        print(f"Dataset: recording episode {self.id}"
              + (f", request {self.request} (from step {self.steps + 1})" if self.request > 1 else ""))

    def add_step(self, step, screen, frame_image, app, element_tree, elements, clicks,
                 reasoning, text, tool_calls, actions, result, context=None):
        """Append one step (a thinking-only step has no actions). Recording
        must never break the run, so a failure here costs the step's record
        and nothing else.

        frame_image: the clean PIL screenshot this step was decided on, or None
                     when the step had no screen of its own (report, digest,
                     scan-off steps would otherwise pair with an older screen).
        elements:    {id: {name, type, visibility, rect, visible_rect}} of that
                     screen; clicks: [{index, point, via}] in the order they ran.
        context:     the other text the model saw this step, raw, by name
                     (e.g. todo, scratchpad, skills), as it was BEFORE the
                     step's actions ran."""
        try:
            # The driver counts steps from 1 in every run; the episode counts
            # on from the requests before this one.
            step = self._base + int(step)
            frame = None
            if frame_image is not None:
                frame = f"frames/step_{step:03d}.png"
                buf = io.BytesIO()
                frame_image.convert("RGB").save(buf, "PNG", compress_level=3)
                (self.dir / frame).write_bytes(buf.getvalue())
                self.frames += 1
            _append(self.dir / "steps.jsonl", {
                "step": step,
                "request": self.request,
                "time": _now(),
                "frame": frame,
                "screen": screen,
                "app": app if frame else None,
                "element_tree": element_tree if frame else "",
                "elements": [_element(i, e) for i, e in elements.items()] if frame else [],
                "context": dict(context or {}),
                "reasoning": dict(reasoning),
                "text": text,
                "tool_calls": tool_calls,
                "actions": _actions(actions, clicks, elements, screen),
                "result": result,
            })
            self.steps += 1
        except Exception as e:
            print(f"Dataset: step {step} not recorded ({e})")

    def finish(self, status, message):
        """Close this request: outcome on its entry and on the episode, totals
        refreshed, one more index line (the last line of an episode is current)."""
        try:
            now = _now()
            self.header["requests"][-1].update({"finished_at": now, "status": status,
                                                "message": message, "steps": self.steps - self._base})
            self.header.update({"finished_at": now, "status": status, "message": message,
                                "steps": self.steps, "frames": self.frames})
            _write_json(self.dir / "episode.json", self.header)
            line = {k: self.header[k] for k in (
                "episode", "task", "original_task", "resumed", "chat", "platform", "model",
                "status", "steps", "frames", "started_at", "finished_at")}
            line["requests"] = len(self.header["requests"])
            _append(self.root / "index.jsonl", line)
            print(f"Dataset: episode {self.id} saved ({self.steps} steps, {self.frames} frames, "
                  f"{len(self.header['requests'])} request(s))")
        except Exception as e:
            print(f"Dataset: episode {self.id} not closed ({e})")


def _actions(actions, clicks, elements, screen):
    """The step's actions, each element id resolved to the element and to the
    point the controller really used. `clicks` come in the order they ran, so
    each action takes the first unused one with its id. An action that never
    ran (a batch stops at its first error) keeps point None."""
    unused = list(clicks)
    out = []
    for action in actions:
        record = dict(action)
        kind = action.get("type")
        if kind in _POINTER_TOOLS:
            ids = [str(action.get("id"))]
        elif kind == "drag_drop":
            ids = [p.strip() for p in str(action.get("value", "")).split(" to ")]
        else:
            ids = []
        targets = []
        for index in ids:
            hit = next((c for c in unused if c["index"] == index), None)
            if hit:
                unused.remove(hit)
            info = elements.get(index)
            targets.append({
                "element": _element(index, info) if info else None,
                "point": hit["point"] if hit else None,
                "point_norm": _norm(hit["point"], screen) if hit else None,
                "via": hit["via"] if hit else None,
            })
        if kind == "drag_drop" and len(targets) == 2:
            record["from"], record["to"] = targets
        elif len(targets) == 1:
            record.update(targets[0])
        out.append(record)
    return out


def _element(index, info):
    """One element as the teacher's tree knew it, boxes in screen points."""
    out = {"id": index, "name": info.get("name"), "type": info.get("type"),
           "visibility": info.get("visibility"), "box": _box(info.get("rect"))}
    if info.get("visible_rect") and info.get("visible_rect") != info.get("rect"):
        out["visible_box"] = _box(info["visible_rect"])
    return out


def _box(rect):
    """[left, top, right, bottom] from a Rect namedtuple (or any 4-sequence)."""
    return [int(v) for v in list(rect)[:4]] if rect else None


def _norm(point, screen):
    """Screen points -> [x, y] on the frame's 0-1000 grid (Qwen3-VL's scale)."""
    x = (point[0] - screen["x"]) / screen["width"] * 1000
    y = (point[1] - screen["y"]) / screen["height"] * 1000
    return [min(1000, max(0, round(x))), min(1000, max(0, round(y)))]


def _now():
    return datetime.now().isoformat(timespec="seconds")


def _read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def _write_json(path, obj):
    # Path writes, not open(): compiled builds patch builtins.open (see
    # AutoCua/__init__.py). Written aside and swapped in, never half-written.
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(obj, indent=2, ensure_ascii=False, default=str), encoding="utf-8")
    os.replace(tmp, path)


def _append(path, obj):
    # One line per call, O_APPEND: parallel runs sharing index.jsonl never
    # interleave inside a line, and nothing written earlier is rewritten.
    line = (json.dumps(obj, ensure_ascii=False, default=str) + "\n").encode("utf-8")
    fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    try:
        os.write(fd, line)
    finally:
        os.close(fd)

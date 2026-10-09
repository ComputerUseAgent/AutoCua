"""Computer-use training data, recorded from the agent's own runs.

One file for every platform: each driver hands it plain values (its screen,
its clean frame, its element mapping), so nothing here knows which OS it runs
on. Wired into the mac driver today.

With ENABLED on, every run appends one episode to AutoCua_data/dataset/. Runs
only ever add to it, so the dataset grows with each run and nothing already
recorded is touched:

    dataset/
        index.jsonl                 one line per finished run
        episodes/<episode>/
            episode.json            task, model, screen, outcome
            steps.jsonl             one line per step, written as the run goes
            frames/step_NNN.png     the CLEAN screenshot the step was decided on

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
    """One run of the agent: a header, then its steps as they happen."""

    def __init__(self, task, platform, provider, model, speed, screen,
                 original_task=None, resumed=False):
        self.id = datetime.now().strftime("%Y-%m-%d_%H-%M-%S_") + secrets.token_hex(3)
        self.root = data_root() / "dataset"
        self.dir = self.root / "episodes" / self.id
        (self.dir / "frames").mkdir(parents=True, exist_ok=True)
        self.steps = 0
        self.frames = 0
        self.header = {
            "episode": self.id, "task": task,
            "original_task": original_task or task, "resumed": bool(resumed),
            "platform": platform, "provider": provider, "model": model, "speed": speed,
            "screen": screen, "started_at": _now(),
        }
        _write_json(self.dir / "episode.json", self.header)
        print(f"Dataset: recording episode {self.id}")

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
            frame = None
            if frame_image is not None:
                frame = f"frames/step_{step:03d}.png"
                buf = io.BytesIO()
                frame_image.convert("RGB").save(buf, "PNG", compress_level=3)
                (self.dir / frame).write_bytes(buf.getvalue())
                self.frames += 1
            _append(self.dir / "steps.jsonl", {
                "step": step,
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
        """Close the episode and add it to the dataset index."""
        try:
            self.header.update({"finished_at": _now(), "status": status, "message": message,
                                "steps": self.steps, "frames": self.frames})
            _write_json(self.dir / "episode.json", self.header)
            _append(self.root / "index.jsonl", {k: self.header[k] for k in (
                "episode", "task", "original_task", "resumed", "platform", "model",
                "status", "steps", "frames", "started_at", "finished_at")})
            print(f"Dataset: episode {self.id} saved ({self.steps} steps, {self.frames} frames)")
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

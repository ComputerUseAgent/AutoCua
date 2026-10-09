"""The Responses endpoint, shared by the providers that carry a model whose
table entry says `async_tools` (GPT-6 Astra on OpenRouter and on OpenAI
direct). Every platform uses it, the web agent included.

Under the ordinary tool protocol Astra issues ONE tool call per turn, so the
notes-first batch never happens. On the Responses endpoint a tool declared
`async: true` lets it keep working after the call, and a synchronous call
ends its turn. So every tool the model must batch past is async, the notes
tool included, and only the terminal tools (`done`, `exit`) stay synchronous:
they end the turn, which is exactly the "alone in its turn" rule. The
transcript is the same canonical one every provider sees; it is translated
on the way out and the reply is folded back into the same
{content, tool_calls, provider_meta} envelope the loop consumes.
"""

import json
from typing import Optional

import requests

from . import LLM_HTTP_TIMEOUT, screenshots

# Tools that end the model's turn: the driver's `done` and the coder /
# minion's `exit`. Everything else is declared async.
SYNC_TOOLS = ("done", "exit")

# A visible turn is a few hundred tokens (26 to 426 measured on the web
# agent, FULL thinking included). After its async calls Astra sometimes
# idles, emitting empty reasoning items until this ceiling cuts it (about
# 28 s per 1,500 tokens of budget, billed as almost nothing); the calls are
# complete by then and _post_responses keeps them, so the ceiling only sets
# how long that idling can last. 2,000 bounds it to about 40 s while leaving
# room for the longest legitimate turn.
MAX_OUTPUT_TOKENS = 2000


def async_route(info: dict) -> bool:
    """True when the resolved table entry routes this model here."""
    return bool(isinstance(info, dict) and info.get("async_tools"))


def responses_tools(tools: list) -> list:
    """The registry flattened to the Responses shape, `async: true` on every
    tool but SYNC_TOOLS. Name, description and parameters pass through
    untouched. Accepts the chat-completions form ({"type": "function",
    "function": {...}}, openrouter/openai) and the already-flat form
    ({"type": "function", "name": ...}, perplexity)."""
    out = []
    for t in tools or []:
        f = t.get("function") if isinstance(t.get("function"), dict) else t
        name = f.get("name") or ""
        tool = {
            "type": "function",
            "name": name,
            "description": f.get("description") or "",
            "parameters": f.get("parameters") or {},
        }
        if name not in SYNC_TOOLS:
            tool["async"] = True
        out.append(tool)
    return out


def _text(content) -> str:
    """Text of a message content field (string, or list of blocks with
    cache_control): the shared _extract_text shape, first text block wins."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        for item in content:
            if isinstance(item, dict) and item.get("type") == "text":
                return item.get("text", "") or ""
        return json.dumps(content, ensure_ascii=False)
    if content is None:
        return ""
    return str(content)


def _responses_input(messages: list, shot: Optional[str], cli_agent: bool):
    """The canonical transcript in the Responses dialect: the system prompt
    as `instructions`, user text as input_text (the screenshot as an
    input_image on the LAST user message only), a previous turn from this
    path replayed verbatim from `provider_meta.responses_output` (its
    function_call and reasoning items), any other assistant turn
    reconstructed from its text and tool_calls, and every tool result as a
    function_call_output. `provider_meta` never leaves the process."""
    instructions = None
    items = []
    for msg in messages:
        role = msg.get("role") or ""
        content = msg.get("content")
        if role == "system":
            instructions = _text(content)
        elif role == "tool":
            items.append({
                "type": "function_call_output",
                "call_id": msg.get("tool_call_id") or "",
                "output": _text(content),
            })
        elif role == "assistant":
            meta = msg.get("provider_meta")
            replay = meta.get("responses_output") if isinstance(meta, dict) else None
            if isinstance(replay, list):
                items.extend(replay)
                continue
            text = _text(content)
            if text.strip():
                items.append({"role": "assistant",
                              "content": [{"type": "output_text", "text": text}]})
            for tc in msg.get("tool_calls") or []:
                fn = (tc or {}).get("function") or {}
                name = fn.get("name") or ""
                raw = fn.get("arguments")
                item = {
                    "type": "function_call",
                    "call_id": (tc or {}).get("id") or "",
                    "name": name,
                    "arguments": raw if isinstance(raw, str) else json.dumps(raw or {}, ensure_ascii=False),
                }
                if name not in SYNC_TOOLS:
                    item["async"] = True
                items.append(item)
        elif role == "user":
            items.append({"role": "user",
                          "content": [{"type": "input_text", "text": _text(content)}]})
    if shot and not cli_agent and items and items[-1].get("role") == "user":
        for one in screenshots(shot):
            items[-1]["content"].append({
                "type": "input_image",
                "image_url": f"data:image/jpeg;base64,{one}",
            })
    return instructions, items


def send(session: requests.Session, url: str, api_key: str, error_prefix: str,
         model: str, messages: list, shot: Optional[str], cli_agent: bool,
         tools: list, reasoning: dict, extra: dict,
         provider_tag: Optional[str]) -> dict:
    """Build, POST (with one degrade retry) and normalize one Responses
    request, on the calling provider's Session (one kept connection per
    provider object). `reasoning` is {"reasoning": {"effort": ...}} or {};
    `extra` holds provider-specific top-level fields (routing, store); under a
    `provider_tag` the reply's output array is kept as
    provider_meta.responses_output so the next request replays it verbatim
    (None for a provider whose meta is stripped before every send, see
    _META_PROVIDERS in llm_manager.py)."""
    instructions, items = _responses_input(messages, shot, cli_agent)
    data = {"model": model}
    if instructions:
        data["instructions"] = instructions
    data["input"] = items
    data["max_output_tokens"] = MAX_OUTPUT_TOKENS
    data.update(reasoning or {})
    data.update(extra or {})
    data["tools"] = responses_tools(tools)
    # Canonical rationale in openrouter/service.py: every turn must call a
    # tool.
    data["tool_choice"] = "required"
    headers = {
        "Authorization": f"Bearer {api_key}",
        "Content-Type": "application/json",
    }
    try:
        result = _post_responses(session, url, headers, data, error_prefix)
    except Exception as first:
        # A replayed `reasoning` item is bound to the endpoint that wrote it,
        # so a router pins every retry to that one endpoint; when it is the
        # one rate-limited or failing, the three tries of the manager all hit
        # the same wall. One degrade here: the same request with the
        # reasoning items dropped (function_call, message and
        # function_call_output items stay), which the model still batches on.
        if not any(i.get("type") == "reasoning" for i in items):
            raise
        first_line = (str(first).splitlines() or [""])[0]
        print(f"   Responses request failed ({first_line}); retrying once without the replayed reasoning items")
        degraded = dict(data)
        degraded["input"] = [i for i in items if i.get("type") != "reasoning"]
        result = _post_responses(session, url, headers, degraded, error_prefix)
    return _normalize_responses(result, provider_tag)


def _post_json(session: requests.Session, url: str, headers: dict, data: dict):
    """One JSON POST: (status, body). Transport failures raise."""
    resp = session.post(url, json=data, headers=headers, timeout=LLM_HTTP_TIMEOUT)
    return resp.status_code, resp.text


def _post_responses(session: requests.Session, url: str, headers: dict, data: dict,
                    prefix: str) -> dict:
    """POST one Responses request. Besides the HTTP status, the body's own
    `error` and `status` are checked: a router answers 200 to an upstream
    failure mid-generation with status "failed" / "incomplete" and a cut
    function_call, which must reach the manager's retry ladder as an error,
    not the loop as a half call."""
    try:
        status, body = _post_json(session, url, headers, data)
    except requests.exceptions.RequestException as e:
        raise Exception(f"{prefix}: {e}")
    if status >= 400:
        raise Exception(f"{prefix}: HTTP {status}\nResponse Body: {body}")
    try:
        result = json.loads(body)
    except ValueError as e:
        raise Exception(f"{prefix}: {e}")
    if result.get("error") is not None:
        raise Exception(f"{prefix}: Responses error: {json.dumps(result['error'], ensure_ascii=False)}")
    state = result.get("status") or "completed"
    if state == "completed":
        return result
    # "incomplete" on max_output_tokens with the calls already complete is
    # the idle tail described at MAX_OUTPUT_TOKENS, not a failure: keep the
    # output up to the last completed function_call and drop the rest (empty
    # reasoning items, a cut item). Any other incomplete reason, or a cut
    # with no complete call, is an error for the manager to retry.
    reason = (result.get("incomplete_details") or {}).get("reason") or ""
    output = result.get("output") or []
    last = None
    for i, item in enumerate(output):
        if (item.get("type") == "function_call"
                and (item.get("status") or "completed") == "completed"):
            last = i
    if reason == "max_output_tokens" and last is not None:
        dropped = len(output) - (last + 1)
        print(f"   Responses status incomplete after {last + 1} complete call(s): "
              f"accepted, {dropped} trailing item(s) dropped")
        kept = dict(result)
        kept["output"] = output[:last + 1]
        kept["status"] = "completed"
        return kept
    raise Exception(f"{prefix}: Responses status {state}: "
                    f"{json.dumps(result.get('incomplete_details'), ensure_ascii=False)}")


def _normalize_responses(result: dict, provider_tag: Optional[str]) -> dict:
    """A Responses reply folded into the chat-path envelope: function_call
    items become tool_calls keyed by call_id, the first output_text becomes
    the content, and (under a provider tag) the output array rides along as
    `provider_meta.responses_output` so the next request replays it
    verbatim, reasoning items included."""
    output = result.get("output") or []
    text = ""
    calls = []
    for item in output:
        kind = item.get("type")
        if kind == "message":
            if not text:
                for block in item.get("content") or []:
                    if block.get("type") == "output_text":
                        text = block.get("text") or ""
                        break
        elif kind == "function_call":
            if (item.get("status") or "completed") != "completed":
                continue
            raw = item.get("arguments")
            if isinstance(raw, dict):
                args = raw
            else:
                try:
                    args = json.loads(raw or "{}")
                except Exception:
                    args = {}
            if not isinstance(args, dict):
                args = {}
            calls.append({
                "id": str(item.get("call_id") or item.get("id") or "") or f"call_{len(calls)}",
                "name": item.get("name") or "",
                "arguments": args,
            })
    # The tell that the route stripped the async flags (a router narrows an
    # async request to the endpoints that support it; one that does not gets
    # plain tools and the model falls back to one call per turn).
    fc = [i for i in output if i.get("type") == "function_call"]
    if (any((i.get("name") or "") not in SYNC_TOOLS for i in fc)
            and not any(i.get("async") is True for i in fc)):
        # Plain ASCII: a web agent child on Windows may write to a cp1252 log
        # file, where an emoji would raise here and cost the reply.
        print("Warning: Responses route returned no async function_call: the async tool flags "
              "were dropped upstream, expect one call per turn")
    message = {"content": text, "tool_calls": calls}
    if provider_tag:
        # Replay only what carries something: completed calls, messages, and
        # reasoning items that hold encrypted content. An empty reasoning
        # item (no content, no summary) is the idle tail and would only bloat
        # every later request.
        replay = []
        for item in output:
            kind = item.get("type")
            if kind == "function_call":
                if (item.get("status") or "completed") == "completed":
                    replay.append(item)
            elif kind == "reasoning":
                if item.get("encrypted_content"):
                    replay.append(item)
            else:
                replay.append(item)
        if replay:
            message["provider_meta"] = {"provider": provider_tag, "responses_output": replay}
    # Usage arrives as input_tokens / output_tokens, which _normalize_usage
    # reads as is; the cached count is mirrored where the TTY line looks.
    usage = dict(result.get("usage") or {})
    cached = (usage.get("input_tokens_details") or {}).get("cached_tokens")
    if cached is not None:
        usage["prompt_tokens_details"] = {"cached_tokens": cached}
    return {"choices": [{"message": message}], "usage": usage}

import json
import os
import re
from typing import Dict, Any, Optional
from urllib.parse import quote

import requests

from .view import DEFAULT_MAX_TOKENS, DEFAULT_REGION, find_entry, get_request_fields
from .. import LLM_HTTP_TIMEOUT, screenshots

# One cache checkpoint: Bedrock caches everything in the request before it.
_CACHE_POINT = {"cachePoint": {"type": "default"}}
# Claude takes at most 4 checkpoints per request, and the system prompt's is one.
_MAX_MESSAGE_CACHE_POINTS = 3


def _as_text(content) -> str:
    """Flatten a message content field (string, or list of content blocks) to
    plain text — used when translating the canonical OpenAI-shaped transcript
    into Converse content blocks."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = [b.get("text", "") for b in content
                 if isinstance(b, dict) and b.get("type") == "text"]
        return "\n".join(p for p in parts if p)
    return "" if content is None else str(content)


def _cache_marked(content) -> bool:
    """True when the agent loop marked this turn as its cache breakpoint: a
    parts array carrying cache_control (see the agents' prompt-caching step)."""
    return isinstance(content, list) and any(
        isinstance(p, dict) and p.get("cache_control") for p in content)


def _args_dict(raw) -> dict:
    if isinstance(raw, dict):
        return raw
    try:
        parsed = json.loads(raw or "{}")
    except Exception:
        return {}
    return parsed if isinstance(parsed, dict) else {}


def _bedrock_token(raw, empty: str) -> str:
    """A tool call id or name Bedrock accepts. Bedrock checks every name in the
    history against [a-zA-Z0-9_-]+ on every model (a replayed rejected call
    named "(unnamed)" is a 400 for the whole request); Claude checks ids the
    same way (a Kimi-style `functions.click:0` is a 400 there), and ids are
    capped at 64 characters. A call and its result go through the same
    mapping, so the pair stays matched."""
    return re.sub(r"[^a-zA-Z0-9_-]", "_", str(raw or ""))[:64] or empty


def _tool_use_id(raw) -> str:
    return _bedrock_token(raw, "call")


def _pair_tool_results(api_messages: list) -> None:
    """Send a tool result that answers no call of the turn before it as plain
    text. Bedrock rejects the whole request over one ("The number of
    toolResult blocks ... exceeds the number of toolUse blocks of previous
    turn"), and the transcript keeps the step, so every later request of the
    run would fail too. The minion's leaked-envelope salvage can write one."""
    open_ids = set()
    for msg in api_messages:
        if msg["role"] == "assistant":
            open_ids = {b["toolUse"]["toolUseId"] for b in msg["content"] if "toolUse" in b}
            continue
        paired, orphans, rest = [], [], []
        for block in msg["content"]:
            result = block.get("toolResult")
            if result is None:
                rest.append(block)
            elif result["toolUseId"] in open_ids:
                open_ids.discard(result["toolUseId"])
                paired.append(block)
            else:
                orphans.append({"text": f"Result of tool call {result['toolUseId']}: "
                                        f"{result['content'][0]['text']}"})
        if orphans:
            # Results first, as Bedrock requires; everything else after them.
            msg["content"] = paired + orphans + rest
        open_ids = set()


def _tool_spec(tool: dict) -> dict:
    """One chat-completions function tool as a Converse toolSpec."""
    fn = tool.get("function") or tool
    spec = {
        "name": fn.get("name") or "",
        "inputSchema": {"json": fn.get("parameters") or {"type": "object", "properties": {}}},
    }
    if fn.get("description"):
        spec["description"] = fn["description"]
    return {"toolSpec": spec}


def _usage(u: dict) -> dict:
    """Bedrock usage in the Anthropic key style the manager reads.

    inputTokens leaves the cached tokens out, as Anthropic's input_tokens does
    (measured: a 12777-token cache read came back as inputTokens 25), so the
    manager's context size adds the two cache classes back."""
    u = u or {}
    return {
        "input_tokens": u.get("inputTokens", 0) or 0,
        "output_tokens": u.get("outputTokens", 0) or 0,
        "total_tokens": u.get("totalTokens", 0) or 0,
        "cache_read_input_tokens": u.get("cacheReadInputTokens", 0) or 0,
        "cache_creation_input_tokens": u.get("cacheWriteInputTokens", 0) or 0,
    }


class AWSProvider:
    """Amazon Bedrock provider (Converse API) for LLM interactions"""

    def __init__(self, api_key: str, cli_agent: bool = False, model_info: dict = None,
                 tools: list = None, max_tokens: int = None):
        self.api_key = api_key
        self.cli_agent = cli_agent
        self.model_info = model_info or {}
        self.max_tokens = max_tokens
        # The regional endpoint every call goes through (see view.py).
        self.region = (os.getenv("AWS_BEDROCK_REGION") or "").strip().lower() or DEFAULT_REGION
        self.api_base = f"https://bedrock-runtime.{self.region}.amazonaws.com"
        self.session = requests.Session()
        # Native tool calling: chat-completions function tools — the format
        # the manager hands every provider without a dialect of its own, and
        # the one the web agent's Rust manager sends. They become Converse
        # toolSpecs per request, because show_screen_tools swaps this list
        # between calls. None only for mode="text" (plain prose).
        self.tools = tools or None

    def send_request(self, messages: list, model: str, annotated_screenshot_base64: Optional[str] = None) -> Dict[str, Any]:
        """Send request to the Bedrock Converse API"""

        # The table entry for the model on the wire, looked up by api_name, so
        # the settings follow the model actually called (the manager's one-call
        # fallback swaps it) and also apply when the model was given by its
        # Bedrock id rather than its short name. An unregistered ID gets none.
        info = find_entry(model) or (self.model_info if self.model_info.get("api_name") == model else {})
        cache = bool(info.get("cache"))

        # NATIVE TRANSCRIPT translation. The agents speak the canonical OpenAI
        # shape (assistant messages carrying `tool_calls`, plus `role: "tool"`
        # results keyed by tool_call_id). Converse carries the same as content
        # blocks: toolUse on the assistant turn, toolResult on a following
        # user turn. A blank text block is a 400 on every model, so empty text
        # is left out and a turn left with nothing gets a placeholder.
        system = []
        api_messages = []
        marked = set()  # api_messages indexes of the turns the loop marked for caching
        for msg in messages:
            role = msg.get("role")
            content = msg.get("content")
            if role == "system":
                text = _as_text(content)
                if text.strip():
                    system.append({"text": text})
                continue

            if role == "tool":
                result = {
                    "toolUseId": _tool_use_id(msg.get("tool_call_id")),
                    "content": [{"text": _as_text(content) or "(no output)"}],
                }
                if msg.get("is_error"):
                    result["status"] = "error"
                # Results for a batch of calls must ride on ONE user turn.
                prev = api_messages[-1] if api_messages else None
                if prev and prev["role"] == "user" and "toolResult" in prev["content"][0]:
                    prev["content"].append({"toolResult": result})
                else:
                    api_messages.append({"role": "user", "content": [{"toolResult": result}]})
            elif role == "assistant":
                blocks = []
                text = _as_text(content)
                if text.strip():
                    blocks.append({"text": text})
                for tc in msg.get("tool_calls") or []:
                    fn = (tc or {}).get("function") or {}
                    blocks.append({"toolUse": {
                        "toolUseId": _tool_use_id((tc or {}).get("id")),
                        "name": _bedrock_token(fn.get("name"), "unnamed"),
                        "input": _args_dict(fn.get("arguments")),
                    }})
                api_messages.append({"role": "assistant", "content": blocks or [{"text": "(no content)"}]})
            else:
                text = _as_text(content)
                api_messages.append({"role": "user", "content": [{"text": text if text.strip() else "(no content)"}]})

            if role in ("tool", "user") and _cache_marked(content):
                marked.add(len(api_messages) - 1)
        _pair_tool_results(api_messages)

        # Prompt caching (Claude): a checkpoint after the system prompt, which
        # also covers the tools ahead of it, and one at the end of each turn the
        # loop marked — the newest ones only, within Claude's limit of 4.
        if cache:
            for i in sorted(marked)[-_MAX_MESSAGE_CACHE_POINTS:]:
                api_messages[i]["content"].append(dict(_CACHE_POINT))
            if system:
                system.append(dict(_CACHE_POINT))

        # If screenshot is provided and NOT cli_agent, add it to the live user
        # message — after any checkpoint: it changes every step, so caching it
        # would write a prefix that is never read back. Skipped when the
        # transcript ends on tool results; the driver always ends on its live
        # user message anyway.
        if annotated_screenshot_base64 and not self.cli_agent and api_messages:
            last = api_messages[-1]
            if last["role"] == "user" and "toolResult" not in last["content"][0]:
                for shot in screenshots(annotated_screenshot_base64):
                    last["content"].append({
                        "image": {
                            "format": "jpeg",  # image/jpeg: LLM_IMAGE_FORMAT in tree/element.py
                            "source": {"bytes": shot},
                        }
                    })

        body = {
            "messages": api_messages,
            # Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
            # everything else keeps the model's cap from view.py.
            "inferenceConfig": {"maxTokens": self.max_tokens or info.get("max_tokens") or DEFAULT_MAX_TOKENS},
        }
        if system:
            body["system"] = system

        # Claude's effort or Kimi/Grok's reasoning level, per view.py.
        fields = get_request_fields(info)
        if fields:
            body["additionalModelRequestFields"] = fields

        # No tools (mode="text") -> plain text: omit the tool config entirely.
        if self.tools:
            # {"any": {}} - every turn must call at least one tool; a text-only
            # turn is never a valid step (even termination is the `exit`
            # tool). Canonical rationale: see openrouter/service.py. The
            # manager may override it per line (the scrape optimizer runs on
            # "auto": its only tool is `more` and its answer is the text reply).
            choice = "auto" if self.model_info.get("tool_choice") == "auto" else "any"
            body["toolConfig"] = {
                "tools": [_tool_spec(t) for t in self.tools],
                "toolChoice": {choice: {}},
            }

        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json"
        }
        url = f"{self.api_base}/model/{quote(model, safe='')}/converse"

        try:
            response = self.session.post(url, json=body, headers=headers, timeout=LLM_HTTP_TIMEOUT)
            response.raise_for_status()
            result = response.json()
        except requests.exceptions.RequestException as e:
            error_msg = f"AWS Bedrock API request failed: {str(e)}"
            if hasattr(e, 'response') and e.response is not None:
                error_msg += f"\nResponse Body: {e.response.text}"
            raise Exception(error_msg)

        # Normalize to the OpenAI-style shape the manager reads
        # (choices[0].message). Kimi K3 and Grok 4.7 also return their
        # reasoning as reasoningContent blocks; those are neither text nor a
        # call and are dropped, as the other providers drop their thinking.
        blocks = ((result.get("output") or {}).get("message") or {}).get("content") or []
        message = {
            "content": "\n".join(b["text"] for b in blocks if isinstance(b, dict) and b.get("text")),
        }
        if self.tools:
            calls = []
            for i, block in enumerate(blocks):
                use = block.get("toolUse") if isinstance(block, dict) else None
                if use:
                    calls.append({
                        "id": str(use.get("toolUseId") or "") or f"call_{i}",
                        "name": use.get("name") or "",
                        "arguments": _args_dict(use.get("input")),
                    })
            message["tool_calls"] = calls
        return {
            "choices": [{"message": message}],
            "usage": _usage(result.get("usage")),
        }

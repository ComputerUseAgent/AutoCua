import json
import requests
import base64
from typing import Dict, Any, Optional

from .view import get_reasoning_params
from ..responses import async_route, send as send_responses
from .. import LLM_HTTP_TIMEOUT, screenshots

def _is_gemini_route(model: str) -> bool:
    """True when OpenRouter will hand this request to Google's backend, where a
    tool result stops being a string and becomes a protobuf Struct."""
    name = str(model or "").lower()
    return name.startswith("google/") or "gemini" in name


def _wrap_for_gemini(content) -> str:
    """Carry one tool result to a Gemini route as a SINGLE JSON object.

    Gemini's functionResponse body is a Struct, so the text we send is coerced
    into JSON on the way in. A plain envelope survives that trip only while it
    contains nothing JSON-shaped: measured against
    google/gemini-3.1-pro-preview, `<Tool_response>` + a JSON head + an
    <output> block came back `MALFORMED_RESPONSE` with an EMPTY turn, and an
    envelope whose output body merely CONTAINED braces (a viewed .json file,
    `view`'s own [N] line numbers, any code) reached the model as scraped
    fragments — `[{},[2],[3],{"react":"18.0.0"},[4],[5]]` in place of the file.
    That is the bug behind a coder that reads every command as "no output" and
    then invents paths it never saw.

    Wrapping the whole envelope as one JSON object makes the coercion lossless:
    the Struct holds a single string field, and the model receives the envelope
    byte-for-byte (verified: plain output, a JSON file with [N] numbers, and a
    multi-result batch all echoed back verbatim). Only Google routes are
    wrapped — every other model on OpenRouter keeps the raw envelope."""
    text = content if isinstance(content, str) else str(content or "")
    return json.dumps({"tool_response": text}, ensure_ascii=False)


class OpenRouterProvider:
    """OpenRouter API provider for LLM interactions"""

    def __init__(self, api_key: str, cli_agent: bool = False, model_info: dict = None, tools: list = None, max_tokens: int = None):
        self.api_key = api_key
        self.max_tokens = max_tokens
        self.api_url = "https://openrouter.ai/api/v1/chat/completions"
        self.session = requests.Session()
        self.cli_agent = cli_agent
        self.model_info = model_info or {}
        # Native tool calling: OpenAI-format function tools — the model's
        # output contract. None only for mode="text" (plain prose).
        self.tools = tools or None
        
    def send_request(self, messages: list, model: str, annotated_screenshot_base64: Optional[str] = None) -> Dict[str, Any]:
        """Send request to OpenRouter API"""

        # GPT-6 Astra (table flag `async_tools`): the Responses endpoint with
        # async tool definitions is the one request shape in which it emits
        # the notes call AND the actions in a single turn; on chat/completions
        # it emits one tool call per turn. mode="text" (no tools) stays on
        # chat/completions like every other model.
        if self.tools and async_route(self.model_info):
            return send_responses(
                session=self.session,
                url="https://openrouter.ai/api/v1/responses",
                api_key=self.api_key,
                error_prefix="OpenRouter API request failed",
                model=model, messages=messages, shot=annotated_screenshot_base64,
                cli_agent=self.cli_agent, tools=self.tools,
                reasoning=get_reasoning_params(model, self.model_info),
                # Same routing preference as the chat path. No temperature
                # (Astra rejects it) and no route:fallback (unverified on this
                # endpoint).
                extra={"provider": {"sort": "throughput"}},
                provider_tag="openrouter",
            )

        # A `role: "tool"` message must carry a PLAIN STRING here. The agent
        # loop marks its newest persistent turn with a parts-array
        # cache_control breakpoint, and in a native transcript that turn is a
        # tool result — but OpenRouter forwards messages verbatim to whatever
        # backend it routes to, and a tool message whose content is an array
        # carrying the Anthropic-only `cache_control` key is not something a
        # non-Anthropic backend (Gemini, GPT, …) is obliged to read. The
        # direct Anthropic provider flattens this itself (see _as_text there)
        # and google/perplexity flatten via _extract_text; OpenRouter was the
        # one passthrough path, so the newest tool result — the exact one the
        # model has to judge each step — was the one at risk of arriving
        # unreadable. Flatten it back to text; the breakpoint is dropped for
        # tool turns on this provider, which costs some cache reuse and buys
        # the model actually seeing its tool output.
        # Rebuilt, not mutated in place: the caller's message dicts are shared
        # with the agent loop's own history.
        #
        # The same pass translates the transcript's private `provider_meta`
        # into this dialect: a reasoning model's own blocks go back on the
        # assistant turn as `reasoning_details`. Gemini 3 (and every other
        # reasoning model routed here) binds a function RESULT to the call
        # through the encrypted signature inside those blocks — strip them and
        # the model either rejects the turn or reads its tool output as empty.
        # The key itself is ALWAYS removed, tagged for this provider or not —
        # it is ours, not OpenRouter's, and an unknown message key is a 400
        # risk. Tool results bound for a Gemini route are wrapped as one JSON
        # object (see _wrap_for_gemini).
        gemini_route = _is_gemini_route(model)
        prepared = []
        for msg in messages:
            if msg.get("role") == "tool":
                if isinstance(msg.get("content"), list):
                    msg = {**msg, "content": "\n".join(
                        part.get("text", "") for part in msg["content"]
                        if isinstance(part, dict) and part.get("type") == "text")}
                if gemini_route:
                    msg = {**msg, "content": _wrap_for_gemini(msg.get("content"))}
            meta = msg.get("provider_meta")
            if meta is not None:
                msg = {k: v for k, v in msg.items() if k != "provider_meta"}
                if (msg.get("role") == "assistant" and isinstance(meta, dict)
                        and meta.get("provider") == "openrouter"
                        and meta.get("reasoning_details")):
                    msg["reasoning_details"] = meta["reasoning_details"]
            prepared.append(msg)
        messages = prepared

        # If screenshot is provided and NOT cli_agent, modify the user message to include the annotated image
        if annotated_screenshot_base64 and not self.cli_agent and len(messages) > 1:
            user_message = messages[-1]["content"]
            messages[-1]["content"] = [
                {
                    "type": "text",
                    "text": user_message
                },
            ] + [
                {
                    "type": "image_url",
                    "image_url": {
                        "url": f"data:image/jpeg;base64,{shot}"
                    }
                }
                for shot in screenshots(annotated_screenshot_base64)
            ]
        

        data = {
            "model": model,
            "messages": messages,
            # 0.2, not 0.5: this loop wants the confident, repeatable choice
            # every step, and its output is tool-call JSON rather than prose.
            # Models that no longer accept the knob at all (the Anthropic
            # entries) are unaffected — OpenRouter drops parameters a route
            # does not support rather than erroring, verified against
            # claude-sonnet-5.
            "temperature": 0.2,
            # Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
            # everything else keeps this default.
            "max_tokens": self.max_tokens or 10000,
            "route": "fallback",
            # Without this, OpenRouter load-balances every call across backends
            # by price — so identical steps land on different upstreams and the
            # same call has been measured at 20s, 38s and 63s. Sorting turns
            # load balancing OFF and pins the route by throughput, which also
            # keeps the prefix cache warm (caching is per-backend: a rebalanced
            # step pays a cold prefill on top of a slow backend).
            "provider": {"sort": "throughput"},
        }
        # How hard the model thinks, per view.py's per-model table. Carried in
        # OpenRouter's unified `reasoning` field, which it translates to each
        # backend's own control. Left off for a model with no level set and
        # for any hand-typed name, which then keeps its published default.
        data.update(get_reasoning_params(model, self.model_info))

        # No tools (mode="text") -> plain text: omit the tool params entirely.
        if self.tools:
            data["tools"] = self.tools
            # "required" - every turn MUST call at least one tool. A text-only
            # turn is never a valid step in this loop: every step routes an
            # action and even termination is a tool (`exit` carries the final
            # report in its value), so there is no "final answer on the text
            # channel" ending. Forcing makes the text-only failure mode
            # unrepresentable instead of salvageable; prose still rides the
            # text channel alongside the calls. OpenRouter passes tool_choice
            # through to the backend and upstream support is uneven, so the
            # no-tool-called nudge + leaked-envelope salvage stay as backstop.
            # A table entry may override the value (Qwen: "auto", the one
            # value its backend accepts with thinking on; see view.py).
            data["tool_choice"] = self.model_info.get("tool_choice") or "required"

        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json"
        }
        
        try:
            response = self.session.post(self.api_url, json=data, headers=headers, timeout=LLM_HTTP_TIMEOUT)
            response.raise_for_status()
            result = response.json()
            if self.tools:
                return _normalize_tool_response(result)
            return result
        except requests.exceptions.RequestException as e:
            error_msg = f"OpenRouter API request failed: {str(e)}"
            if hasattr(e, 'response') and e.response is not None:
                error_msg += f"\nResponse Body: {e.response.text}"
            raise Exception(error_msg)


def _normalize_tool_response(result: dict) -> dict:
    """Normalize an OpenAI-format tool-call response: keep the usual
    choices/message shape but replace tool_calls with
    [{"id", "name", "arguments": dict}] (arguments JSON-decoded). The `id` is
    load-bearing — the loop echoes it back so each tool result is matched to
    the call that produced it.

    `reasoning_details` is preserved the same way, as `provider_meta`: it
    carries the reasoning blocks (including Gemini 3's encrypted thought
    signature, keyed to the first tool call's id) that this turn must be
    replayed with. Dropping it here is what left the model unable to bind its
    tool results to the calls that asked for them."""
    message = ((result.get("choices") or [{}])[0].get("message")) or {}
    calls = []
    for i, tc in enumerate(message.get("tool_calls") or []):
        fn = (tc or {}).get("function") or {}
        raw_args = fn.get("arguments")
        if isinstance(raw_args, dict):
            args = raw_args
        else:
            try:
                args = json.loads(raw_args or "{}")
            except Exception:
                args = {}
        if not isinstance(args, dict):
            args = {}
        calls.append({
            "id": str((tc or {}).get("id") or "") or f"call_{i}",
            "name": fn.get("name") or "",
            "arguments": args,
        })
    out = {"content": message.get("content") or "", "tool_calls": calls}
    details = message.get("reasoning_details")
    if isinstance(details, list) and details:
        out["provider_meta"] = {"provider": "openrouter", "reasoning_details": details}
    return {
        "choices": [{"message": out}],
        "usage": result.get("usage", {}),
    }
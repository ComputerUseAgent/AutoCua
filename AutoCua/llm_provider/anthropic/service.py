import json
import requests
import copy
from typing import Dict, Any, Optional

from .view import get_model_info, get_sampling_params, get_thinking_params, get_tool_choice
from .. import LLM_HTTP_TIMEOUT, screenshots


def _as_text(content) -> str:
    """Flatten a message content field (string, or list of content blocks) to
    plain text — used when translating the canonical OpenAI-shaped transcript
    into Anthropic's block format."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = [b.get("text", "") for b in content
                 if isinstance(b, dict) and b.get("type") == "text"]
        return "\n".join(p for p in parts if p)
    return "" if content is None else str(content)


def _args_dict(raw) -> dict:
    if isinstance(raw, dict):
        return raw
    try:
        parsed = json.loads(raw or "{}")
    except Exception:
        return {}
    return parsed if isinstance(parsed, dict) else {}

class AnthropicProvider:
    """Anthropic API provider for LLM interactions"""
    
    def __init__(self, api_key: str, cli_agent: bool = False, tools: list = None, max_tokens: int = None, model_info: dict = None):
        self.api_key = api_key
        self.max_tokens = max_tokens
        self.model_info = model_info or {}
        self.api_url = "https://api.anthropic.com/v1/messages"
        self.session = requests.Session()
        self.cli_agent = cli_agent
        # Native tool calling: Messages-format tools ({name, input_schema,
        # description}) — the model's output contract. None only for
        # mode="text" (plain prose).
        # Deepcopy before stripping: the dialect converter aliases the shared
        # CODER_TOOLS parameter dicts — stripping in place would mutate the
        # registry for every other provider.
        self.tools = None
        if tools:
            self.tools = copy.deepcopy(tools)
            for tool in self.tools:
                self._strip_unsupported_keywords(tool.get("input_schema", {}))
        
    @staticmethod
    def _strip_unsupported_keywords(obj):
        """Recursively strip JSON Schema keywords not supported by Anthropic structured outputs"""
        unsupported = {"maxItems", "minItems", "strict"}
        if isinstance(obj, dict):
            for key in list(obj.keys()):
                if key in unsupported:
                    del obj[key]
                else:
                    AnthropicProvider._strip_unsupported_keywords(obj[key])
        elif isinstance(obj, list):
            for item in obj:
                AnthropicProvider._strip_unsupported_keywords(item)
    
    def send_request(self, messages: list, model: str, annotated_screenshot_base64: Optional[str] = None) -> Dict[str, Any]:
        """Send request to Anthropic API"""
        
        # Extract system prompt from messages (Anthropic uses top-level 'system' field)
        system_content = None
        api_messages = []
        
        for msg in messages:
            role = msg.get("role")
            if role == "system":
                system_content = msg["content"]
                continue

            # NATIVE TRANSCRIPT translation. The coder speaks the canonical
            # OpenAI shape (assistant messages carrying `tool_calls`, plus
            # `role: "tool"` results keyed by tool_call_id). Anthropic carries
            # the same information as content BLOCKS: tool_use on the
            # assistant turn, tool_result on a following user turn.
            if role == "tool":
                block = {
                    "type": "tool_result",
                    "tool_use_id": msg.get("tool_call_id") or "",
                    "content": _as_text(msg.get("content")),
                }
                if msg.get("is_error"):
                    block["is_error"] = True
                # Cache breakpoint: the loop marks its newest persistent turn
                # (a tool message) with a parts-array cache_control, which
                # _as_text would silently drop — lift it onto the tool_result
                # block instead (Anthropic supports cache_control there).
                raw = msg.get("content")
                if isinstance(raw, list) and any(
                        isinstance(p, dict) and p.get("cache_control") for p in raw):
                    block["cache_control"] = {"type": "ephemeral"}
                # Results for a batch of calls must ride on ONE user turn.
                prev = api_messages[-1] if api_messages else None
                if (prev and prev.get("role") == "user"
                        and isinstance(prev.get("content"), list) and prev["content"]
                        and isinstance(prev["content"][0], dict)
                        and prev["content"][0].get("type") == "tool_result"):
                    prev["content"].append(block)
                else:
                    api_messages.append({"role": "user", "content": [block]})
                continue

            if role == "assistant" and msg.get("tool_calls"):
                blocks = []
                text = _as_text(msg.get("content"))
                if text.strip():
                    blocks.append({"type": "text", "text": text})
                for tc in msg["tool_calls"]:
                    fn = (tc or {}).get("function") or {}
                    blocks.append({
                        "type": "tool_use",
                        "id": (tc or {}).get("id") or "",
                        "name": fn.get("name") or "",
                        "input": _args_dict(fn.get("arguments")),
                    })
                api_messages.append({"role": "assistant", "content": blocks})
                continue

            api_messages.append({"role": role, "content": msg["content"]})
        
        # If screenshot is provided and NOT cli_agent, modify the last user message to include the annotated image
        if annotated_screenshot_base64 and not self.cli_agent and len(api_messages) > 0:
            last_msg = api_messages[-1]
            # A native transcript can end on a user turn whose content is a list
            # of tool_result blocks. The splice below REBUILDS content as
            # [text, image], which would destroy those results (and leave the
            # model's tool_use calls dangling), so skip it in that case - the
            # driver never ends a request on tool results anyway, since the live
            # user message always comes last.
            content = last_msg.get("content")
            is_tool_result = (isinstance(content, list) and content
                              and isinstance(content[0], dict)
                              and content[0].get("type") == "tool_result")
            if last_msg.get("role") == "user" and not is_tool_result:
                user_text = content

                # Handle case where content is already a list
                if isinstance(user_text, list):
                    text_content = ""
                    for item in user_text:
                        if isinstance(item, dict) and item.get("type") == "text":
                            text_content = item.get("text", "")
                            break
                    user_text = text_content
                
                # Anthropic uses source-based image format (not image_url)
                api_messages[-1]["content"] = [
                    {
                        "type": "text",
                        "text": user_text
                    },
                ] + [
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/jpeg",  # LLM_IMAGE_FORMAT in tree/element.py
                            "data": shot
                        }
                    }
                    for shot in screenshots(annotated_screenshot_base64)
                ]
        
        # Build system as list with cache_control for prompt caching
        system_param = None
        if system_content:
            system_param = [
                {
                    "type": "text",
                    "text": system_content,
                    "cache_control": {"type": "ephemeral"}
                }
            ]
        
        headers = {
            "x-api-key": self.api_key,
            "anthropic-version": "2023-06-01",
            "content-type": "application/json"
        }
        
        data = {
            "model": model,
            "messages": api_messages,
            # Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
            # everything else keeps this default.
            "max_tokens": self.max_tokens or 4000
        }

        # top_p / top_k per model — never temperature. Opus 5 and Sonnet 5
        # removed all three knobs, so their registry entries opt out and get
        # none of them; sending any one is a 400 that fails every attempt of
        # every step.
        data.update(get_sampling_params(model))

        # Adaptive thinking + effort, per model. The inverse of the line
        # above: the 5-series takes these and not the sampling knobs, Haiku
        # takes the knobs and not these. Left off entirely for Haiku, which
        # 400s on both fields.
        data.update(get_thinking_params(model))


        if system_param:
            data["system"] = system_param
        
        # No tools (mode="text") -> plain text: omit the tool params entirely.
        if self.tools:
            data["tools"] = self.tools
            # {"type": "any"} - every turn must call at least one tool; a
            # text-only turn is never a valid step (even termination is the
            # `exit` tool). Note: under "any" Anthropic usually suppresses
            # plain text blocks - fine here, thinking/next_goal ride as tool
            # params. Canonical rationale: see openrouter/service.py.
            # A table entry may override it (Sonnet 5.5: "auto", since it
            # rejects "any"; see view.py).
            # The manager may override per line (the scrape optimizer runs on
            # "auto": its only tool is `more` and its answer is the text reply);
            # otherwise the table decides.
            choice = self.model_info.get("tool_choice")
            data["tool_choice"] = {"type": choice} if choice else get_tool_choice(model)
        
        try:
            response = self.session.post(self.api_url, json=data, headers=headers, timeout=LLM_HTTP_TIMEOUT)
            response.raise_for_status()
            result = response.json()
            
            # Normalize response to match OpenAI-style format (choices[0].message.content)
            # Anthropic returns: content: [{type: "thinking", ...}, {type: "text", text: "..."},
            # {type: "tool_use", name, input}, ...]
            if self.tools:
                # Tools mode: collect ALL text blocks (don't stop at the first —
                # prose can interleave with tool_use) and every tool_use block
                # as {"name", "arguments": dict}.
                texts = []
                calls = []
                for i, block in enumerate(result.get("content", [])):
                    if block.get("type") == "text":
                        texts.append(block.get("text", ""))
                    elif block.get("type") == "tool_use":
                        args = block.get("input")
                        calls.append({
                            "id": str(block.get("id") or "") or f"call_{i}",
                            "name": block.get("name") or "",
                            "arguments": args if isinstance(args, dict) else {},
                        })
                return {
                    "choices": [{
                        "message": {
                            "content": "\n".join(t for t in texts if t),
                            "tool_calls": calls,
                        }
                    }],
                    "usage": result.get("usage", {}),
                }

            text_content = ""
            for block in result.get("content", []):
                if block.get("type") == "text":
                    text_content = block.get("text", "")
                    break

            return {
                "choices": [{
                    "message": {
                        "content": text_content
                    }
                }],
                "usage": result.get("usage", {}),
            }
            
        except requests.exceptions.RequestException as e:
            error_msg = f"Anthropic API request failed: {str(e)}"
            if hasattr(e, 'response') and e.response is not None:
                error_msg += f"\nResponse Body: {e.response.text}"
            raise Exception(error_msg)
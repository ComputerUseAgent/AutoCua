import json
import requests
from typing import Dict, Any, Optional
from .. import LLM_HTTP_TIMEOUT, screenshots

class CerebrasProvider:
    """Cerebras API provider for LLM interactions (OpenAI-compatible chat completions)"""

    def __init__(self, api_key: str, cli_agent: bool = False, model_info: dict = None, tools: list = None, max_tokens: int = None):
        self.api_key = api_key
        self.max_tokens = max_tokens
        self.api_url = "https://api.cerebras.ai/v1/chat/completions"
        self.session = requests.Session()
        self.cli_agent = cli_agent
        self.model_info = model_info or {}
        # Native tool calling: OpenAI-format function tools — the model's
        # output contract. None only for mode="text" (plain prose).
        self.tools = tools or None

    def send_request(self, messages: list, model: str, annotated_screenshot_base64: Optional[str] = None) -> Dict[str, Any]:
        """Send request to Cerebras API"""

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

        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json"
        }

        data = {
            "model": model,
            "messages": messages,
            "temperature": 1,
            "top_p": 0.95,
            "seed": 42,
            # Thinking bills as completion tokens, so leave room for it AND
            # the tool call that has to follow.
            # Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
            # everything else keeps this default.
            "max_completion_tokens": self.max_tokens or 32768,
            # The coder and its minions (cli_agent) work through code and
            # think hard; computer use, web and the memory handoff answer
            # every step and stay quick.
            "reasoning_effort": "high" if self.cli_agent else "low",
        }

        # No tools (mode="text") -> plain text: omit the tool params entirely.
        if self.tools:
            data["tools"] = self.tools
            # "required" - every turn must call at least one tool; a text-only
            # turn is never a valid step. Canonical rationale: see
            # openrouter/service.py.
            data["tool_choice"] = self.model_info.get("tool_choice") or "required"

        try:
            response = self.session.post(self.api_url, json=data, headers=headers, timeout=LLM_HTTP_TIMEOUT)
            response.raise_for_status()
            result = response.json()
            if self.tools:
                return _normalize_tool_response(result)
            return result
        except requests.exceptions.RequestException as e:
            error_msg = f"Cerebras API request failed: {str(e)}"
            if hasattr(e, 'response') and e.response is not None:
                error_msg += f"\nResponse: {e.response.text}"
            raise Exception(error_msg)


def _normalize_tool_response(result: dict) -> dict:
    """Normalize an OpenAI-format tool-call response: keep the usual
    choices/message shape but replace tool_calls with
    [{"id", "name", "arguments": dict}] (arguments JSON-decoded). The `id` is
    echoed back by the loop so each tool result matches its call."""
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
    return {
        "choices": [{
            "message": {
                "content": message.get("content") or "",
                "tool_calls": calls,
            }
        }],
        "usage": result.get("usage", {}),
    }

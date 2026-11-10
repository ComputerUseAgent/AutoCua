from typing import Dict, Any, Optional
from .. import entry_value, http_timeout, out_cap, screenshots
from ..requesty.service import _normalize_tool_response

class LiteLLMProvider:
    """LiteLLM SDK provider for LLM interactions (OpenAI-compatible chat completions).

    The model id names the upstream (`anthropic/claude-sonnet-5`,
    `gemini/gemini-3.6-flash`, `bedrock/...`, `vertex_ai/...`, `azure/...`,
    or `litellm_proxy/<alias>` behind a LiteLLM proxy), and LiteLLM turns the
    OpenAI-format request (tools, tool_choice, image parts) into that
    provider's own API. Credentials are LiteLLM's usual per-provider
    environment variables; `api_key` / `api_base` are only sent when set."""

    def __init__(self, api_key: Optional[str] = None, cli_agent: bool = False, model_info: dict = None,
                 tools: list = None, max_tokens: int = None, api_base: Optional[str] = None):
        try:
            import litellm
        except ImportError:
            raise ValueError("LiteLLM is not installed: pip install 'litellm>=1.97.0,<1.106.0'") from None
        litellm.suppress_debug_info = True
        self._completion = litellm.completion
        self.api_key = api_key or None
        self.api_base = (api_base or "").rstrip("/") or None
        self.max_tokens = max_tokens
        self.cli_agent = cli_agent
        self.model_info = model_info or {}
        # Native tool calling: OpenAI-format function tools, the model's
        # output contract. None only for mode="text" (plain prose).
        self.tools = tools or None

    def send_request(self, messages: list, model: str, annotated_screenshot_base64: Optional[str] = None) -> Dict[str, Any]:
        """Send request through LiteLLM"""

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
            "temperature": 0.2,
            # Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
            # everything else keeps this default.
            "max_tokens": self.max_tokens or out_cap(self.cli_agent, 10000, self.model_info),
            # The read timeout of the (connect, read) pair the other providers use.
            "timeout": http_timeout(self.cli_agent)[1],
            # Drop the fields a backend does not take (temperature on a model
            # that only accepts its default, reasoning_effort on one that
            # does not reason) instead of failing the step with a 400.
            "drop_params": True,
        }
        if self.api_key:
            data["api_key"] = self.api_key
        if self.api_base:
            data["api_base"] = self.api_base
        # How hard the model thinks, from view.py's table. LiteLLM maps the
        # level onto each backend (an effort for OpenAI, a thinking budget for
        # Anthropic and Gemini). An entry with no level keeps its default.
        if self.model_info.get("effort"):
            data["reasoning_effort"] = entry_value(self.model_info, "effort", self.cli_agent)

        # No tools (mode="text") -> plain text: omit the tool params entirely.
        if self.tools:
            data["tools"] = self.tools
            # "required" - every turn must call at least one tool; a text-only
            # turn is never a valid step. Canonical rationale: see
            # openrouter/service.py. LiteLLM sends it as Anthropic's "any" and
            # Gemini's mode ANY.
            data["tool_choice"] = entry_value(self.model_info, "tool_choice", self.cli_agent) or "required"

        try:
            result = self._completion(**data).model_dump()
        except Exception as e:
            raise Exception(f"LiteLLM request failed ({type(e).__name__}): {getattr(e, 'message', None) or e}")
        if self.tools:
            return _normalize_tool_response(result)
        message = ((result.get("choices") or [{}])[0].get("message")) or {}
        return {
            "choices": [{"message": {"content": message.get("content") or ""}}],
            "usage": result.get("usage") or {},
        }

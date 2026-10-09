import json
import requests
from typing import Dict, Any, Optional

from openai import OpenAI, Timeout

from .view import get_reasoning_effort
from ..responses import async_route, send as send_responses
from .. import LLM_HTTP_TIMEOUT, screenshots

class OpenAIProvider:
    """OpenAI API provider for LLM interactions"""
    
    def __init__(self, api_key: str, cli_agent: bool = False, tools: list = None, max_tokens: int = None, model_info: dict = None):
        # LLM_HTTP_TIMEOUT in the SDK's own Timeout class (httpx in openai
        # 2.x, httpx2 in 3.x): connect 15 s, each read and write 180 s, in
        # place of the SDK's connect 5 s, read 600 s. max_retries stays at
        # the SDK default of 2, so a server that stays silent costs 3 x 180 s
        # before the manager's own retry.
        connect, read = LLM_HTTP_TIMEOUT
        self.client = OpenAI(api_key=api_key, timeout=Timeout(read, connect=connect))
        # Kept for the Responses path, which posts without the SDK.
        self.api_key = api_key
        self.session = requests.Session()
        # The resolved table entry; the manager re-points it on a fallback swap.
        self.model_info = model_info or {}
        self.max_tokens = max_tokens
        self.cli_agent = cli_agent
        # Native tool calling: OpenAI-format function tools — the model's
        # output contract. None only for mode="text" (plain prose).
        self.tools = tools or None
        
    def send_request(self, messages: list, model: str, annotated_screenshot_base64: Optional[str] = None) -> Dict[str, Any]:
        """Send request to OpenAI API"""

        # GPT-6 Astra (table flag `async_tools`): /v1/chat/completions refuses
        # function tools on this model with any reasoning_effort, and `none`
        # is not a level it accepts, so with tools it can only be driven on
        # /v1/responses, where async tool definitions also give the notes
        # call plus the batch in one turn (see ..responses). mode="text" (no
        # tools) stays on chat/completions.
        if self.tools and async_route(self.model_info):
            effort = get_reasoning_effort(model, self.model_info).get("reasoning_effort")
            return send_responses(
                session=self.session,
                url="https://api.openai.com/v1/responses",
                api_key=self.api_key,
                error_prefix="OpenAI API request failed",
                model=model, messages=messages, shot=annotated_screenshot_base64,
                cli_agent=self.cli_agent, tools=self.tools,
                # The table's `reasoning_effort` level in the Responses spelling.
                reasoning={"reasoning": {"effort": effort}} if effort else {},
                # Nothing stored server-side: the transcript is replayed from
                # our side (function calls and their outputs), and the model
                # batches on that replay.
                extra={"store": False},
                # provider_meta is stripped before every send on this provider
                # (_META_PROVIDERS): the replay reconstructs the calls.
                provider_tag=None,
            )
        
        # If screenshot is provided and NOT cli_agent, modify the user message to include the annotated image
        if annotated_screenshot_base64 and not self.cli_agent and len(messages) > 1:
            user_message = messages[-1]["content"]
            
            # Handle case where content might already be a list
            if isinstance(user_message, list):
                # Extract text from existing list structure
                text_content = ""
                for item in user_message:
                    if isinstance(item, dict) and item.get("type") == "text":
                        text_content = item.get("text", "")
                        break
                user_message = text_content
            
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
        
        # Prepare API call parameters
        params = {
            "model": model,
            "messages": messages,
            # Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
            # everything else keeps this default.
            "max_completion_tokens": self.max_tokens or 4000,
            "verbosity": "medium"  # Set verbosity to medium
        }
        # How hard the model thinks, per view.py's table. Kept out of the dict
        # above because it is per-model, not a constant: an unregistered model
        # contributes nothing and keeps OpenAI's default. No sampling params
        # are ever added here — GPT-6 Astra rejects temperature/top_p/seed.
        params.update(get_reasoning_effort(model, self.model_info))


        # No tools (mode="text") -> plain text: omit the tool params entirely.
        if self.tools:
            params["tools"] = self.tools
            # "required" - every turn must call at least one tool; a text-only
            # turn is never a valid step (even termination is the `exit`
            # tool). Canonical rationale: see openrouter/service.py.
            params["tool_choice"] = self.model_info.get("tool_choice") or "required"

        try:
            response = self.client.chat.completions.create(**params)

            # Return in the same format as other providers
            msg = response.choices[0].message
            message: Dict[str, Any] = {"content": msg.content or ""}
            if self.tools:
                # SDK tool-call objects -> [{"id", "name", "arguments": dict}]
                # (arguments arrive as a JSON string; malformed -> {}). The id
                # is echoed back by the loop to match each result to its call.
                calls = []
                for i, tc in enumerate(msg.tool_calls or []):
                    fn = tc.function
                    try:
                        args = json.loads(fn.arguments or "{}")
                    except Exception:
                        args = {}
                    if not isinstance(args, dict):
                        args = {}
                    calls.append({
                        "id": str(getattr(tc, "id", "") or "") or f"call_{i}",
                        "name": fn.name or "",
                        "arguments": args,
                    })
                message["tool_calls"] = calls
            usage = getattr(response, "usage", None)
            return {
                "choices": [{
                    "message": message
                }],
                "usage": {
                    "input_tokens": getattr(usage, "prompt_tokens", 0) or 0,
                    "output_tokens": getattr(usage, "completion_tokens", 0) or 0,
                    "total_tokens": getattr(usage, "total_tokens", 0) or 0,
                } if usage else {},
            }
        except Exception as e:
            error_msg = f"OpenAI API request failed: {str(e)}"
            raise Exception(error_msg)
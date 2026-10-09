import os
import base64
import json
from typing import Dict, Any, Optional

from google import genai
from google.genai import types
from dotenv import load_dotenv

from .view import get_model_info, get_thinking_level, MODEL_MAPPINGS
from .. import LLM_HTTP_TIMEOUT, screenshots

load_dotenv()


def _args_dict(raw) -> dict:
    """Tool-call arguments as a dict (they ride as a JSON string in the
    canonical transcript; malformed -> {})."""
    if isinstance(raw, dict):
        return raw
    try:
        parsed = json.loads(raw or "{}")
    except Exception:
        return {}
    return parsed if isinstance(parsed, dict) else {}


def _sig_to_text(sig) -> str:
    """A thought signature as a JSON-safe string (the SDK hands over bytes)."""
    if isinstance(sig, str):
        return sig
    try:
        return base64.b64encode(sig).decode("ascii")
    except Exception:
        return ""


def _function_call_part(name: str, args: dict, signature: str = ""):
    """One functionCall part, carrying back the thought signature Gemini 3
    minted for it. That signature is how the model re-attaches its own call —
    and the functionResponse that follows — to the reasoning that produced it;
    rebuilding the call without it makes Gemini 3 reject the turn or read the
    tool result as if it were empty. Best-effort by design: an SDK build
    without the field sends exactly the part it sent before."""
    part = types.Part.from_function_call(name=name, args=args)
    if signature:
        try:
            part.thought_signature = base64.b64decode(signature)
        except Exception:
            pass
    return part


def _function_response_part(name: str, output: str, call_id: str = ""):
    """One functionResponse part. Gemini 3 mints an `id` on every functionCall
    and maps the result back BY that id, so echo it whenever the model gave us
    one — name-only matching (the pre-Gemini-3 contract) is ambiguous the
    moment one step calls the same tool twice, which this agent does routinely
    (parallel minions, several scratchpad writes). Falls back to the name-only
    part for ids we synthesized ourselves and for older SDKs."""
    if call_id:
        try:
            return types.Part(function_response=types.FunctionResponse(
                id=call_id, name=name, response={"output": output}))
        except Exception:
            pass
    return types.Part.from_function_response(name=name, response={"output": output})


def _http_options():
    """LLM_HTTP_TIMEOUT for the Gemini SDK, which takes ONE timeout, in
    milliseconds. httpx applies it to the connect and to every read and
    write (so the connect bound here is 180 s, not 15 s), and the SDK also
    sends it to Google as the X-Server-Timeout deadline. Without it a server
    that goes silent is waited on forever. The read half is the one to use:
    the call is not streamed, so nothing arrives until the answer is
    complete, and the read bound is the cap on that wait, as for every other
    provider. A fresh object per client: genai.Client can write a base URL
    into the options it is handed."""
    return types.HttpOptions(timeout=LLM_HTTP_TIMEOUT[1] * 1000)


def _clean_schema_for_google(schema):
    """Recursively remove 'additionalProperties' which Gemini API doesn't support."""
    if not isinstance(schema, dict):
        return schema
    cleaned = {k: _clean_schema_for_google(v) for k, v in schema.items() if k != "additionalProperties"}
    for k, v in cleaned.items():
        if isinstance(v, list):
            cleaned[k] = [_clean_schema_for_google(item) for item in v]
    return cleaned


class GoogleProvider:
    """Google Gemini API provider for LLM interactions"""
    
    def __init__(self, api_key: str = None, cli_agent: bool = False, model: str = None, vertex_project_id: str = None, vertex_location: str = None, tools: list = None, max_tokens: int = None, model_info: dict = None):
        self.cli_agent = cli_agent
        self.max_tokens = max_tokens
        # ANY forces a call every turn; AUTO lets a text reply end a turn (the
        # scrape optimizer, whose only tool is `more`). Same override as the
        # other providers' tool_choice, in this API's words.
        self.tool_mode = "AUTO" if (model_info or {}).get("tool_choice") == "auto" else "ANY"
        # Native tool calling: function declarations ({name, description,
        # parameters}) — the model's output contract. None only for
        # mode="text" (plain prose). _clean_schema_for_google is
        # non-mutating, so the shared registries are safe.
        self.tools = [_clean_schema_for_google(t) for t in tools] if tools else None
        
        # Check if model is vertex
        model_info = get_model_info(model) if model else {}
        self.is_vertex = model_info.get("vertex", False)
        
        if self.is_vertex:
            project = vertex_project_id or os.getenv("VERTEX_PROJECT_ID")
            location = vertex_location or os.getenv("VERTEX_LOCATION", "global")
            self.client = genai.Client(vertexai=True, project=project, location=location,
                                       http_options=_http_options())
        else:
            key = api_key or os.getenv("GOOGLE_API_KEY")
            self.client = genai.Client(api_key=key, http_options=_http_options())
    
    def _extract_text(self, content) -> str:
        """Extract text from message content (string or list format with cache_control)"""
        if isinstance(content, str):
            return content
        if isinstance(content, list):
            for item in content:
                if isinstance(item, dict) and item.get("type") == "text":
                    return item.get("text", "")
            return str(content)
        return str(content)
    
    def send_request(self, messages: list, model: str, annotated_screenshot_base64: Optional[str] = None) -> Dict[str, Any]:
        """Send request to Google Gemini API"""
        
        # Convert OpenAI-style messages to Gemini format
        system_instruction = None
        contents = []
        
        # NATIVE TRANSCRIPT translation. The coder speaks the canonical OpenAI
        # shape (assistant messages carrying `tool_calls`, `role: "tool"`
        # results keyed by tool_call_id). Gemini carries the same information
        # as function_call / function_response PARTS, and matches results to
        # calls by NAME — so track id -> name as the transcript is walked.
        call_names = {}
        # Gemini-3 binding state, filled as the assistant turns are walked:
        # the ids Gemini itself minted, which are the only ones that may be
        # echoed back on a functionResponse.
        native_ids = set()
        for msg in messages:
            role = msg.get("role")
            raw_content = msg.get("content") or ""

            if role == "system":
                system_instruction = self._extract_text(raw_content)
            elif role == "tool":
                call_id = msg.get("tool_call_id") or ""
                name = call_names.get(call_id) or msg.get("name") or "tool"
                contents.append(types.Content(role="user", parts=[
                    _function_response_part(
                        name=name,
                        output=self._extract_text(raw_content),
                        call_id=call_id if call_id in native_ids else "",
                    )
                ]))
            elif role == "assistant":
                text = self._extract_text(raw_content)
                meta = msg.get("provider_meta")
                if not (isinstance(meta, dict) and meta.get("provider") == "google"):
                    meta = {}
                sigs = meta.get("signatures") or {}
                native_ids.update(meta.get("native_ids") or [])
                parts = []
                if text.strip():
                    parts.append(types.Part(text=text))
                for tc in (msg.get("tool_calls") or []):
                    fn = (tc or {}).get("function") or {}
                    fname = fn.get("name") or ""
                    tc_id = (tc or {}).get("id")
                    call_names[tc_id] = fname
                    parts.append(_function_call_part(
                        fname, _args_dict(fn.get("arguments")), sigs.get(tc_id, "")))
                if not parts:
                    parts.append(types.Part(text=text))
                contents.append(types.Content(role="model", parts=parts))
            elif role == "user":
                text = self._extract_text(raw_content)
                contents.append(types.Content(role="user", parts=[types.Part(text=text)]))
        
        # Add screenshot to last user message if provided and NOT cli_agent
        if annotated_screenshot_base64 and not self.cli_agent and len(contents) > 0:
            last = contents[-1]
            if last.role == "user":
                for shot in screenshots(annotated_screenshot_base64):
                    image_bytes = base64.b64decode(shot)
                    last.parts.append(types.Part.from_bytes(data=image_bytes, mime_type="image/jpeg"))
        
        # Build generation config
        config_params = {
            # Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
            # everything else keeps this default.
            "max_output_tokens": self.max_tokens or 10000,
        }

        # How hard the model thinks, per view.py's table. Gemini 3 takes a
        # named level rather than a token budget, so this is thinking_level
        # and not the older thinking_budget. Omitted for an unregistered
        # model, which then keeps Google's default — worth having, since the
        # defaults are expensive: Pro's is `high`, the top of its range.
        thinking_level = get_thinking_level(model)
        if thinking_level:
            config_params["thinking_config"] = types.ThinkingConfig(
                thinking_level=thinking_level
            )


        # System instruction
        if system_instruction:
            config_params["system_instruction"] = system_instruction
        
        # No tools (mode="text") -> plain text: no tool config, and no JSON
        # mime coercion either.
        if self.tools:
            config_params["tools"] = [types.Tool(function_declarations=self.tools)]
            # mode="ANY" - every turn must call at least one function; a
            # text-only turn is never a valid step (even termination is the
            # `exit` tool). Canonical rationale: see openrouter/service.py.
            config_params["tool_config"] = types.ToolConfig(
                function_calling_config=types.FunctionCallingConfig(mode=self.tool_mode)
            )
        
        config = types.GenerateContentConfig(**config_params)
        
        try:
            response = self.client.models.generate_content(
                model=model,
                contents=contents,
                config=config
            )
            
            # Normalize to OpenAI-style format
            um = getattr(response, "usage_metadata", None)
            usage = {
                "input_tokens": getattr(um, "prompt_token_count", 0) or 0,
                "output_tokens": getattr(um, "candidates_token_count", 0) or 0,
            } if um else {}

            if self.tools:
                # Tools mode: response.text is None when parts carry function
                # calls — walk the parts, collecting text and function_call
                # parts (fc.args is already a dict-like mapping).
                texts = []
                calls = []
                # Gemini 3 hangs a thought signature off each function_call
                # part. It has to ride back on that same part next request or
                # the model loses the link between its call and the result we
                # return for it — keep it with the call's id (see
                # _function_call_part).
                signatures = {}
                native_ids = []
                candidates = getattr(response, "candidates", None) or []
                content = getattr(candidates[0], "content", None) if candidates else None
                for i, part in enumerate(getattr(content, "parts", None) or []):
                    if getattr(part, "thought", False):
                        continue
                    fc = getattr(part, "function_call", None)
                    if fc is not None:
                        # Gemini 3 mints its own call id; older models don't,
                        # so fall back to a stable synthetic one and remember
                        # which is which (only real ids may be echoed back).
                        native_id = str(getattr(fc, "id", "") or "")
                        call_id = native_id or f"call_{i}"
                        calls.append({
                            "id": call_id,
                            "name": getattr(fc, "name", "") or "",
                            "arguments": dict(getattr(fc, "args", None) or {}),
                        })
                        if native_id:
                            native_ids.append(call_id)
                        sig = _sig_to_text(getattr(part, "thought_signature", None))
                        if sig:
                            signatures[call_id] = sig
                    elif getattr(part, "text", None):
                        texts.append(part.text)
                message = {"content": "\n".join(texts), "tool_calls": calls}
                if signatures or native_ids:
                    message["provider_meta"] = {
                        "provider": "google",
                        "signatures": signatures,
                        "native_ids": native_ids,
                    }
                return {
                    "choices": [{"message": message}],
                    "usage": usage,
                }

            return {
                "choices": [{
                    "message": {
                        "content": response.text
                    }
                }],
                "usage": usage,
            }
        except Exception as e:
            raise Exception(f"Google Gemini API request failed: {str(e)}")
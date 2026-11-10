#!/usr/bin/env python3

"""
Standalone test of the LiteLLM provider (no network, no API key).
Run from the project root:  python3 -m AutoCua.llm_provider.litellm.test

`litellm.completion` is replaced by a recorder, so this checks what the
provider sends and how it reads the answer back:
  1. the request: model, temperature, max_tokens, timeout, drop_params,
     OpenAI-format tools with tool_choice "required", reasoning_effort from
     the table, the screenshot spliced into the last user message;
  2. api_key / api_base go out only when set;
  3. the tool-call answer comes back in the shared {"id", "name",
     "arguments": dict} shape, and mode="text" as plain content;
  4. build_provider: LITELLM_API_KEY / LITELLM_API_BASE for a proxy, else the
     key AutoCua already holds for the model's upstream;
  5. a missing litellm package is reported, not a traceback.

Each step prints PASS/FAIL.
"""

import json
import os
import sys
import types

_FAILED = []


def check(name, ok):
    print(f"{'PASS' if ok else 'FAIL'}  {name}")
    if not ok:
        _FAILED.append(name)


class _Response:
    def __init__(self, data):
        self._data = data

    def model_dump(self):
        return self._data


def _install_fake_litellm(answer):
    fake = types.ModuleType("litellm")
    fake.calls = []

    def completion(**kwargs):
        fake.calls.append(kwargs)
        return _Response(answer)

    fake.completion = completion
    sys.modules["litellm"] = fake
    return fake


TOOLS = [{"type": "function", "function": {"name": "click", "parameters": {"type": "object"}}}]
TOOL_ANSWER = {
    "choices": [{"message": {"content": None, "tool_calls": [
        {"id": "call_1", "type": "function",
         "function": {"name": "click", "arguments": json.dumps({"x": 10, "y": 20})}},
    ]}}],
    "usage": {"prompt_tokens": 120, "completion_tokens": 8, "total_tokens": 128},
}


def test_request_shape():
    from .service import LiteLLMProvider
    fake = _install_fake_litellm(TOOL_ANSWER)
    info = {"api_name": "anthropic/claude-sonnet-5", "effort": "low", "coder_effort": "medium"}
    provider = LiteLLMProvider(None, cli_agent=False, model_info=info, tools=TOOLS)
    messages = [{"role": "system", "content": "sys"}, {"role": "user", "content": "open the file"}]
    result = provider.send_request(messages, "anthropic/claude-sonnet-5", "QUJD")

    sent = fake.calls[-1]
    check("model sent verbatim", sent["model"] == "anthropic/claude-sonnet-5")
    check("temperature 0.2, max_tokens 10000", sent["temperature"] == 0.2 and sent["max_tokens"] == 10000)
    check("timeout is the read timeout", sent["timeout"] == 180)
    check("drop_params on", sent["drop_params"] is True)
    check("tools + tool_choice required", sent["tools"] == TOOLS and sent["tool_choice"] == "required")
    check("reasoning_effort from the table", sent["reasoning_effort"] == "low")
    check("no api_key / api_base when unset", "api_key" not in sent and "api_base" not in sent)
    parts = sent["messages"][-1]["content"]
    check("screenshot spliced as an image part",
          parts[0] == {"type": "text", "text": "open the file"}
          and parts[1]["image_url"]["url"] == "data:image/jpeg;base64,QUJD")
    message = result["choices"][0]["message"]
    check("tool calls normalized",
          message["tool_calls"] == [{"id": "call_1", "name": "click", "arguments": {"x": 10, "y": 20}}])
    check("usage passed through", result["usage"]["prompt_tokens"] == 120)


def test_coder_and_credentials():
    from .service import LiteLLMProvider
    fake = _install_fake_litellm(TOOL_ANSWER)
    info = {"api_name": "anthropic/claude-sonnet-5", "effort": "low", "coder_effort": "medium",
            "coder_tool_choice": "auto"}
    provider = LiteLLMProvider("sk-proxy", cli_agent=True, model_info=info, tools=TOOLS,
                               api_base="http://localhost:4000/")
    provider.send_request([{"role": "user", "content": "fix the test"}], "litellm_proxy/claude", "QUJD")
    sent = fake.calls[-1]
    check("coder: coder_effort and coder_tool_choice", sent["reasoning_effort"] == "medium"
          and sent["tool_choice"] == "auto")
    check("coder: longer timeout and coder output cap", sent["timeout"] == 600 and sent["max_tokens"] == 32768)
    check("coder: no screenshot splice", sent["messages"][-1]["content"] == "fix the test")
    check("api_key / api_base sent when set", sent["api_key"] == "sk-proxy"
          and sent["api_base"] == "http://localhost:4000")


def test_text_mode():
    from .service import LiteLLMProvider
    fake = _install_fake_litellm({"choices": [{"message": {"content": "summary"}}], "usage": {}})
    provider = LiteLLMProvider(None, model_info={"api_name": "gemini/gemini-3.6-flash"}, max_tokens=10000)
    result = provider.send_request([{"role": "user", "content": "compress"}], "gemini/gemini-3.6-flash")
    sent = fake.calls[-1]
    check("text mode: no tools, no tool_choice", "tools" not in sent and "tool_choice" not in sent)
    check("text mode: plain content back", result["choices"][0]["message"]["content"] == "summary")


def test_errors_are_readable():
    from .service import LiteLLMProvider
    fake = types.ModuleType("litellm")

    class AuthenticationError(Exception):
        message = "invalid x-api-key"

    def completion(**kwargs):
        raise AuthenticationError("raw")

    fake.completion = completion
    sys.modules["litellm"] = fake
    provider = LiteLLMProvider(None, model_info={"api_name": "anthropic/claude-sonnet-5"})
    try:
        provider.send_request([{"role": "user", "content": "hi"}], "anthropic/claude-sonnet-5")
        check("error raised", False)
    except Exception as e:
        check("error names the LiteLLM exception", str(e) == "LiteLLM request failed (AuthenticationError): invalid x-api-key")


def test_build_provider_keys():
    from .. import llm_manager
    _install_fake_litellm(TOOL_ANSWER)
    saved = {k: os.environ.pop(k, None) for k in ("LITELLM_API_KEY", "LITELLM_API_BASE", "ANTHROPIC_API_KEY")}
    original_key = llm_manager._key
    store = {}
    llm_manager._key = lambda api_key, name: api_key or os.getenv(name) or store.get(name)
    try:
        store["ANTHROPIC_API_KEY"] = "sk-ant-saved"
        direct = llm_manager.build_provider(
            "litellm", "claude-sonnet-5", llm_manager.resolve_model_info("litellm", "claude-sonnet-5"))
        check("direct route reuses the upstream key AutoCua holds", direct.api_key == "sk-ant-saved"
              and direct.api_base is None)

        bedrock = llm_manager.build_provider(
            "litellm", "bedrock/anthropic.claude-sonnet-5",
            llm_manager.resolve_model_info("litellm", "bedrock/anthropic.claude-sonnet-5"))
        check("unmapped upstream leaves the key to LiteLLM's env vars", bedrock.api_key is None)

        os.environ["LITELLM_API_BASE"] = "http://localhost:4000"
        os.environ["LITELLM_API_KEY"] = "sk-proxy"
        proxy = llm_manager.build_provider(
            "litellm", "litellm_proxy/claude", llm_manager.resolve_model_info("litellm", "litellm_proxy/claude"))
        check("proxy route uses LITELLM_API_KEY / LITELLM_API_BASE", proxy.api_key == "sk-proxy"
              and proxy.api_base == "http://localhost:4000")
        runtime = llm_manager.build_provider(
            "litellm", "litellm_proxy/claude", llm_manager.resolve_model_info("litellm", "litellm_proxy/claude"),
            api_key="sk-from-settings")
        check("runtime key from the app wins", runtime.api_key == "sk-from-settings")
    finally:
        llm_manager._key = original_key
        for k, v in saved.items():
            os.environ.pop(k, None)
            if v is not None:
                os.environ[k] = v


def test_missing_package():
    from .service import LiteLLMProvider
    sys.modules["litellm"] = None
    try:
        LiteLLMProvider(None)
        check("missing package reported", False)
    except ValueError as e:
        check("missing package reported", "pip install 'litellm>=1.97.0,<1.106.0'" in str(e))
    finally:
        sys.modules.pop("litellm", None)


def main():
    real = sys.modules.get("litellm")
    try:
        test_request_shape()
        test_coder_and_credentials()
        test_text_mode()
        test_errors_are_readable()
        test_build_provider_keys()
        test_missing_package()
    finally:
        if real is not None:
            sys.modules["litellm"] = real
    print(f"\n{'All checks passed.' if not _FAILED else f'{len(_FAILED)} check(s) failed.'}")
    sys.exit(1 if _FAILED else 0)


if __name__ == "__main__":
    main()

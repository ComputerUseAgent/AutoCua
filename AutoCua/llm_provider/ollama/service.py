import ipaddress
import json
import os
import requests
from typing import Dict, Any, Optional
from urllib.parse import urlsplit
from .. import entry_value, http_timeout, out_cap, screenshots

# Read bound (seconds) for every call, the coder's included, instead of
# LLM_HTTP_TIMEOUT's 180. The calls are non-streaming, so the socket is silent
# until the whole answer is ready, and a model on this machine reads a long
# prompt slowly (about 400 tokens a second for a 27B model on an M5 Pro: the
# memory handoff's 110k-token transcript takes minutes before the first
# output token), waits behind any other call to the same model (Ollama runs
# them one at a time), and loads 20-odd GB from disk after five idle
# minutes. Ollama drops the work when the client gives up, so a retry starts
# it over from scratch.
READ_TIMEOUT = 900

# A run's prompt grows until memory compression starts (memory_compression/
# controller.py COMPRESS_THRESHOLD: 110k tokens unless
# AutoCua_COMPRESS_THRESHOLD says otherwise), so the model's context window
# has to hold that much plus COMPRESSION_HEADROOM: the handoff's output and
# the steps taken while it runs. _check_context reads the window off Ollama.
# A model on Ollama's MLX engine (Apple Silicon builds, details.runner "mlx")
# runs with its own maximum window (qwen3.8:27b: 262144); on the llama.cpp
# engine (GGUF) the window is OLLAMA_CONTEXT_LENGTH, else a default Ollama
# sizes to the machine's memory, and a longer prompt is cut from the START,
# the agent's instructions first, without an error. Raising the window is
# therefore advice for llama.cpp models only.
COMPRESSION_HEADROOM = 20000

# The smallest window even a run's first step fits in: the driver's system
# prompt and tool definitions (about 16k tokens in quality mode), a
# screenshot (about 2k) and the screen's element list (up to about 7.5k).
# Below it the run stops (_check_context).
MIN_CONTEXT = 32768


def base_url() -> str:
    """Ollama's address from OLLAMA_HOST, read as Ollama's own client reads
    it (envconfig.Host): surrounding spaces and quotes dropped; no scheme
    means http and port 11434, while http:// and https:// default to 80 and
    443; a bare IPv6 address is bracketed; an unreadable port falls back to
    the default; a bare "ollama.com" means https://ollama.com:443. Unlike
    Ollama, 0.0.0.0 and :: (a server listening on every address) mean this
    machine, which Windows cannot dial as such. Unset: 127.0.0.1:11434."""
    raw = (os.getenv("OLLAMA_HOST") or "").strip().strip("\"'")
    scheme, sep, hostport = raw.partition("://")
    if not sep:
        scheme, hostport, default_port = "http", raw, "11434"
        if raw == "ollama.com":
            scheme, hostport = "https", "ollama.com:443"
    else:
        default_port = {"http": "80", "https": "443"}.get(scheme, "11434")
    hostport, _, path = hostport.partition("/")
    host, port = _split_host_port(hostport)
    if host is None:
        host, port = "127.0.0.1", default_port
        try:
            host = str(ipaddress.ip_address(hostport.strip("[]")))
        except ValueError:
            if hostport:
                host = hostport
    if not port.isdigit() or int(port) > 65535:
        port = default_port
    if host in ("", "0.0.0.0", "::"):
        host = "127.0.0.1"
    if ":" in host:
        host = f"[{host}]"
    path = path.rstrip("/")
    return f"{scheme}://{host}:{port}" + (f"/{path}" if path else "")


def _split_host_port(hostport: str):
    """(host, port) like Go's net.SplitHostPort, or (None, None) when
    `hostport` has no port or too many colons (a bare IPv6 address)."""
    if hostport.startswith("["):
        end = hostport.find("]")
        if end < 0 or not hostport[end + 1:].startswith(":"):
            return None, None
        return hostport[1:end], hostport[end + 2:]
    if hostport.count(":") != 1:
        return None, None
    host, _, port = hostport.partition(":")
    return host, port


def _key() -> Optional[str]:
    """OLLAMA_API_KEY from the environment, .env or the app's Settings, the
    way build_provider looks it up; None for an Ollama without one."""
    from ..llm_manager import _key as lookup
    return lookup(None, "OLLAMA_API_KEY")


def _session(url: str, api_key: Optional[str] = None) -> requests.Session:
    """A session for the Ollama at `url`, sending `api_key` (an Ollama
    behind a key) on every call. One on this machine is reached directly:
    requests otherwise sends every call through the proxy the environment or
    macOS names, and macOS's default proxy exceptions do not cover
    127.0.0.1."""
    session = requests.Session()
    if api_key:
        session.headers["Authorization"] = f"Bearer {api_key}"
    host = urlsplit(url).hostname or ""
    try:
        local = host == "localhost" or ipaddress.ip_address(host).is_loopback
    except ValueError:
        local = False
    if local:
        session.trust_env = False
    return session


def reachable(timeout: float = 1.0) -> bool:
    """True when an Ollama answers at base_url(): Telegram offers the
    provider only then."""
    url = base_url()
    try:
        with _session(url, _key()) as session:
            return session.get(f"{url}/api/version", timeout=(0.5, timeout)).ok
    except requests.exceptions.RequestException:
        return False


def pulled_models(timeout: float = 2.0) -> list:
    """The models this Ollama has pulled that can call tools (every agent
    needs it), in `ollama list` order, as (name, capabilities) with
    capabilities None when Ollama does not say: the app's and Telegram's
    model lists (view.listed_models). Raises when Ollama does not answer."""
    url = base_url()
    with _session(url, _key()) as session:
        r = session.get(f"{url}/api/tags", timeout=(0.5, timeout))
        r.raise_for_status()
        models = []
        for m in r.json().get("models") or []:
            name = isinstance(m, dict) and (m.get("name") or m.get("model"))
            if not name:
                continue
            try:
                shown = session.post(f"{url}/api/show", json={"model": name}, timeout=(0.5, timeout))
                caps = shown.json().get("capabilities") if shown.ok else None
            except (requests.exceptions.RequestException, ValueError, AttributeError):
                caps = None
            caps = caps if isinstance(caps, list) else None
            # An Ollama too old to say what a model can do lists it anyway.
            if caps is None or "tools" in caps:
                models.append((name, caps))
        return models


class OllamaProvider:
    """Ollama provider: a model on this machine, or the Ollama at OLLAMA_HOST
    (OpenAI-compatible chat completions at <host>/v1)"""

    def __init__(self, api_key: str = None, cli_agent: bool = False, model_info: dict = None, tools: list = None, max_tokens: int = None):
        # A local Ollama takes no key. One is only sent when given (an Ollama
        # behind a key: ollama.com, a proxy), on every call.
        self.api_key = api_key
        self.max_tokens = max_tokens
        self.base_url = base_url()
        self.api_url = f"{self.base_url}/v1/chat/completions"
        self.session = _session(self.base_url, api_key)
        self.cli_agent = cli_agent
        self.model_info = model_info or {}
        # Native tool calling: OpenAI-format function tools, the model's
        # output contract. None only for mode="text" (plain prose).
        self.tools = tools or None
        # Per model: what /api/show says (_show), whether its context window
        # was read (_check_context), and the error a window too small for
        # any step raised, so every later call fails as fast.
        self._shown = {}
        self._context_checked = set()
        self._too_small = {}

    def send_request(self, messages: list, model: str, annotated_screenshot_base64: Optional[str] = None) -> Dict[str, Any]:
        """Send request to Ollama"""

        if model in self._too_small:
            raise Exception(self._too_small[model])
        caps = self._show(model).get("capabilities")
        if caps is not None and self.tools and "tools" not in caps:
            raise Exception(f"{model} cannot call tools, and every AutoCua agent acts through them. "
                            f"`ollama show {model}` lists what it can do; pick a model with \"tools\".")
        if annotated_screenshot_base64 and not self.cli_agent and caps is not None and "vision" not in caps:
            raise Exception(f"{model} has no vision, and this agent works from screenshots: computer, "
                            f"web and mobile use need a model with \"vision\" in `ollama show {model}`. "
                            f"The coder works without it.")

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
            # Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
            # everything else keeps this default. Thinking counts against it.
            "max_tokens": self.max_tokens or out_cap(self.cli_agent, 10000, self.model_info),
        }
        # How hard the model thinks, from view.py's table; Ollama maps the
        # level onto the model's own. An entry with no level keeps its default.
        effort = entry_value(self.model_info, "effort", self.cli_agent)
        if effort:
            data["reasoning_effort"] = effort

        # No tools (mode="text") -> plain text: omit the tool params entirely.
        if self.tools:
            data["tools"] = self.tools
            # "required" - every turn must call at least one tool; a text-only
            # turn is never a valid step. Canonical rationale: see
            # openrouter/service.py. Ollama takes the field but does not
            # enforce it: a model can still answer in text, which the
            # agents' loops answer with a "no tool called" nudge (and stop
            # after three in a row).
            data["tool_choice"] = entry_value(self.model_info, "tool_choice", self.cli_agent) or "required"

        connect, read = http_timeout(self.cli_agent)
        try:
            response = self.session.post(self.api_url, json=data,
                                         timeout=(connect, max(read, READ_TIMEOUT)))
            response.raise_for_status()
            result = response.json()
        except requests.exceptions.ConnectionError as e:
            raise Exception(f"Could not reach Ollama at {self.base_url} ({e}). Start the Ollama app "
                            f"(or `ollama serve`), or set OLLAMA_HOST to where it runs.")
        except requests.exceptions.RequestException as e:
            error_msg = f"Ollama request failed: {str(e)}"
            if hasattr(e, 'response') and e.response is not None:
                error_msg += f"\nResponse: {e.response.text}"
                if e.response.status_code == 404:
                    error_msg += f"\nIs {model} pulled? `ollama list` shows the models here; `ollama pull {model}` fetches it."
            raise Exception(error_msg)
        # After the answer, as Ollama has the model loaded only now. A window
        # too small for any step raises, so this answer, read from a cut
        # prompt, is never acted on.
        self._check_context(model)
        if self.tools:
            return _normalize_tool_response(result)
        return result

    def _show(self, model: str) -> dict:
        """What /api/show says of `model`: {"capabilities": [...] or None,
        "runner": "mlx", "llamacpp" or None}. Kept once Ollama answers; {}
        while it cannot be asked (not running, the model not pulled, a key
        refused), in which case the request itself reports what is wrong."""
        if model in self._shown:
            return self._shown[model]
        try:
            r = self.session.post(f"{self.base_url}/api/show", json={"model": model}, timeout=(15, 60))
        except requests.exceptions.RequestException:
            return {}
        if not r.ok:
            return {}
        try:
            body = r.json()
        except ValueError:
            body = {}
        if not isinstance(body, dict):
            body = {}
        caps = body.get("capabilities")
        details = body.get("details") if isinstance(body.get("details"), dict) else {}
        shown = {"capabilities": caps if isinstance(caps, list) else None,
                 "runner": details.get("runner")}
        self._shown[model] = shown
        return shown

    def _check_context(self, model: str) -> None:
        """Once per model, after its first answer: read the context window
        Ollama gave it (/api/ps) and compare it with what a run needs (see
        COMPRESSION_HEADROOM). Too small for even a first step: raise, and
        every later call raises the same at once. Too small for a long run:
        say what to set, once, and go on."""
        if model in self._context_checked:
            return
        self._context_checked.add(model)
        try:
            r = self.session.get(f"{self.base_url}/api/ps", timeout=(5, 10))
            loaded = r.json().get("models") or []
        except (requests.exceptions.RequestException, ValueError, AttributeError):
            return
        names = (model, f"{model}:latest")
        size = next((m.get("context_length") for m in loaded
                     if isinstance(m, dict) and (m.get("name") in names or m.get("model") in names)), None)
        if not isinstance(size, int) or size <= 0:
            return
        try:
            from AutoCua.memory_compression.controller import COMPRESS_THRESHOLD as threshold
        except Exception:
            threshold = 110_000
        if size >= threshold + COMPRESSION_HEADROOM:
            return
        mlx = self._show(model).get("runner") == "mlx"
        window = 1 << (threshold + COMPRESSION_HEADROOM - 1).bit_length()   # the next power of two
        fits = size - COMPRESSION_HEADROOM      # the compression threshold this window holds
        if size < MIN_CONTEXT:
            fix = ("Pick a model with a larger window: on Ollama's MLX engine a model gets its own "
                   "maximum." if mlx else f"Restart Ollama with OLLAMA_CONTEXT_LENGTH={window}.")
            self._too_small[model] = (
                f"Ollama runs {model} with a {size}-token context window, too small for even an "
                f"agent's first step (its instructions and tools alone are about 16k tokens). {fix}")
            raise Exception(self._too_small[model])
        options = []
        if not mlx:
            options.append(f"restart Ollama with OLLAMA_CONTEXT_LENGTH={window}")
        if fits >= MIN_CONTEXT:
            options.append(f"set AutoCua_COMPRESS_THRESHOLD={fits} so compression starts in time")
        advice = (" To fix it, " + ", or ".join(options) + "." if options else
                  " This model's window is too small for long runs; pick one with a larger window.")
        print(f"⚠️ Ollama runs {model} with a {size}-token context window, and AutoCua compresses a "
              f"run's memory only at {threshold} tokens, so a long run fails once its prompt passes "
              f"the window.{advice}", flush=True)


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

"""LLM Provider module for managing different language model providers"""

import os
import sys
import threading

# (connect, read) seconds for every provider HTTP call.
#
# `requests` defaults BOTH halves to None, which means block forever: a
# provider that accepts the TCP+TLS handshake and then goes silent hangs the
# agent with no way out but killing the process — the retry ladder never runs
# and the Stop flag is never read again.
#
# The read half is a bound on ONE socket read, not a cap on the whole request,
# and that distinction is the point: these calls are non-streaming, so the
# provider sends nothing at all while the model is still generating. A short
# total cap would kill a slow reasoning response seconds before it landed; a
# generous per-read bound only fires once the socket has genuinely gone quiet.
#
# Every provider call uses it, on every platform and in the web agent: the
# requests ones as is, OpenAI as Timeout(180, connect=15) and Gemini as one
# 180 s figure (its SDK has no separate connect bound, see google/service.py).
# Each provider object posts through its own requests.Session, so the
# connection is kept from one step to the next.
LLM_HTTP_TIMEOUT = (15, 180)


class RefusalError(Exception):
    """The API refused the request itself (Anthropic stop_reason "refusal":
    no content, 0 output tokens, stop_details naming the policy). Not a
    transport error: the same request gets the same answer, so LLMManager
    skips its retries and falls back where it can instead of asking three
    times and reporting an empty response."""


def screenshots(shot) -> list:
    """The images a step's user message carries, in order. The step passes
    one base64 JPEG (the usual case on every platform) or, from the web agent,
    a list of them: the page's screenshot, then the screens a scrape read
    covered."""
    if not shot:
        return []
    if isinstance(shot, (list, tuple)):
        return [s for s in shot if s]
    return [shot]


_TLS_PROBED = set()
_TLS_LOCK = threading.Lock()

# The host each provider's API calls go to (its service.py). The probe only
# ever contacts the provider being built; aws and Vertex are in _tls_probe_url.
_TLS_PROBE_URLS = {
    "openrouter": "https://openrouter.ai/",
    "groq": "https://api.groq.com/",
    "openai": "https://api.openai.com/",
    "anthropic": "https://api.anthropic.com/",
    "google": "https://generativelanguage.googleapis.com/",
    "perplexity": "https://api.perplexity.ai/",
    "together": "https://api.together.xyz/",
    "cerebras": "https://api.cerebras.ai/",
}


def _tls_probe_url(provider: str, model: str):
    """The API host `provider` calls for `model`; None for a provider not
    listed here, which is then not probed."""
    if provider == "google" and str(model).endswith("-vertex"):
        return "https://aiplatform.googleapis.com/"
    if provider == "aws":
        from .aws.view import DEFAULT_REGION
        region = (os.getenv("AWS_BEDROCK_REGION") or "").strip().lower() or DEFAULT_REGION
        return f"https://bedrock-runtime.{region}.amazonaws.com/"
    return _TLS_PROBE_URLS.get(provider)


def _is_tls_cert_error(exc: BaseException) -> bool:
    # Certificate failures only, not a bare "ssl:": every OpenSSL error reads
    # "[SSL: ...]", so a connection a firewall closed mid-handshake passed for
    # interception, and the probe then turned verification off.
    s = str(exc).lower()
    return any(tok in s for tok in ("certificate", "verify failed", "certverify"))


def _ensure_tls_works(provider: str, model: str = "") -> None:
    """If this machine runs HTTPS interception (antivirus / corporate proxy)
    with a certificate Python's certifi bundle can't validate — which breaks
    EVERY https call, including the LLM providers — make requests/httpx use
    the OS trust store (where the interceptor's root is usually installed).
    Only if that also fails do we disable verification. Without this the
    providers get an SSL error, return nothing useful, and the agent
    "finishes" without doing anything.

    This used to probe with httpx against api.openai.com and treat "OS store
    works" as "we're done". That missed the actual failure: OpenRouter uses
    `requests`, which verifies against certifi, not the Windows store. Norton
    (and similar) intercept OpenRouter/OpenAI with a Shield root that Windows
    trusts and certifi does not — Google often still verifies, so a naive
    probe of an unrelated host looks healthy.

    So it probes the host the provider being built calls (_tls_probe_url),
    once per host. A fixed openrouter.ai probe contacted OpenRouter for every
    provider: Defender on a company laptop blocked it for a Gemini user, and
    the blocked handshake was read as interception, turning verification off.

    Secure by default: only acts on a genuine CERTIFICATE error, never on a
    plain network error. Idempotent. Windows only: on macOS and Linux it
    returns at once, and they keep their default verification. Called by
    build_provider (llm_manager.py) before every provider is built, so it runs
    ahead of the first request from every manager, the web agent's included.
    A manager built while another is probing waits for it: a provider's
    requests.Session takes the fix only if it is in place when the Session
    is made.
    """
    if sys.platform != "win32":
        return
    probe = _tls_probe_url(provider, model)
    if not probe:
        return
    with _TLS_LOCK:
        if probe not in _TLS_PROBED:
            _TLS_PROBED.add(probe)
            _probe_tls(probe)


def _probe_tls(probe: str) -> None:
    """The probe itself; _ensure_tls_works runs it once per host."""
    import ssl
    try:
        import requests
        from requests.adapters import HTTPAdapter
    except Exception:
        return

    try:
        requests.get(probe, timeout=6)
        return  # certifi already works → leave everything as-is
    except Exception as e:
        if not _is_tls_cert_error(e):
            return  # network / other error → don't weaken a secure machine

    os_ctx = ssl.create_default_context()
    _orig_pool = HTTPAdapter.init_poolmanager

    def _use_os_store(self, connections, maxsize, block=False, **pool_kwargs):
        pool_kwargs.setdefault("ssl_context", os_ctx)
        return _orig_pool(self, connections, maxsize, block=block, **pool_kwargs)

    HTTPAdapter.init_poolmanager = _use_os_store
    try:
        requests.get(probe, timeout=6)
        print("[tls] an antivirus or proxy re-signs HTTPS on this computer -- the "
              "AI providers' API calls verify against the Windows certificate "
              "store instead. Nothing to do; to go back to the default, turn off "
              "HTTPS/SSL scanning for those domains in the antivirus (e.g. "
              "Norton Web/Mail Shield).",
              file=sys.stderr, flush=True)
        try:
            import httpx
            _orig_client = httpx.Client.__init__

            def _client_init(self, *a, **kw):
                kw.setdefault("verify", os_ctx)
                _orig_client(self, *a, **kw)

            httpx.Client.__init__ = _client_init
            _orig_aclient = httpx.AsyncClient.__init__

            def _aclient_init(self, *a, **kw):
                kw.setdefault("verify", os_ctx)
                _orig_aclient(self, *a, **kw)

            httpx.AsyncClient.__init__ = _aclient_init
        except Exception:
            pass
        return
    except Exception:
        HTTPAdapter.init_poolmanager = _orig_pool

    # OS store also rejected the interceptor cert (malformed Basic Constraints
    # etc.) → last resort: disable verification so the agent can still run.
    try:
        import urllib3
        urllib3.disable_warnings()
    except Exception:
        pass
    try:
        _orig_merge = requests.Session.merge_environment_settings

        def _merge(self, url, proxies, stream, verify, cert):
            settings = _orig_merge(self, url, proxies, stream, verify, cert)
            settings["verify"] = False
            return settings

        requests.Session.merge_environment_settings = _merge
    except Exception:
        pass
    try:
        import httpx
        _orig_client = httpx.Client.__init__

        def _client_init(self, *a, **kw):
            kw["verify"] = False
            _orig_client(self, *a, **kw)

        httpx.Client.__init__ = _client_init
        _orig_aclient = httpx.AsyncClient.__init__

        def _aclient_init(self, *a, **kw):
            kw["verify"] = False
            _orig_aclient(self, *a, **kw)

        httpx.AsyncClient.__init__ = _aclient_init
    except Exception:
        pass
    print("[tls] WARNING: TLS interception detected (antivirus/proxy) — HTTPS "
          "certificate verification DISABLED process-wide so the agent can "
          "reach the LLM provider. To restore secure verification, turn off "
          "HTTPS/SSL scanning for these API domains in your antivirus/proxy.",
          file=sys.stderr, flush=True)

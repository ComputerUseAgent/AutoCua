"""LLM Provider module for managing different language model providers"""

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


_TLS_PROBED = False
_TLS_LOCK = threading.Lock()


def _is_tls_cert_error(exc: BaseException) -> bool:
    s = str(exc).lower()
    return any(tok in s for tok in ("certificate", "verify failed", "ssl:", "certverify"))


def _ensure_tls_works() -> None:
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

    Secure by default: only acts on a genuine CERTIFICATE error, never on a
    plain network error. Idempotent. Windows only: on macOS and Linux it
    returns at once, and they keep their default verification. Called by
    build_provider (llm_manager.py) before every provider is built, so it runs
    ahead of the first request from every manager, the web agent's included.
    A manager built while another is probing waits for it: a provider's
    requests.Session takes the fix only if it is in place when the Session
    is made.
    """
    global _TLS_PROBED
    if sys.platform != "win32":
        return
    with _TLS_LOCK:
        if not _TLS_PROBED:
            _TLS_PROBED = True
            _probe_tls()


def _probe_tls() -> None:
    """The probe itself; _ensure_tls_works runs it once per process."""
    import ssl
    try:
        import requests
        from requests.adapters import HTTPAdapter
    except Exception:
        return

    # Hit a host the LLM path actually uses. Google is a bad probe: antivirus
    # HTTPS scanning often skips it while still intercepting AI APIs.
    probe = "https://openrouter.ai/"
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
        print("[tls] antivirus/proxy HTTPS interception detected -- using the "
              "Windows certificate store so OpenRouter/OpenAI verify. To "
              "restore default verification, turn off HTTPS/SSL scanning for "
              "those API domains in your antivirus (e.g. Norton Web/Mail Shield).",
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

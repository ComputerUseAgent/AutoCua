"""HTTPS certificate verification on Windows, where an antivirus or a company
proxy may re-sign HTTPS.

Such a proxy signs with a root the operating system trusts and certifi's
bundle does not, so Python's default verification fails with
CERTIFICATE_VERIFY_FAILED, and every provider call with it. AutoCua then
verifies against the operating system's own store (os_context: Python's
default context, which loads the Windows roots), and nothing else:
verification is never turned off. A certificate error is exactly what an
attacker in the middle of the connection produces, so it must never be the
thing that weakens the connection. When the system's store rejects the
certificate too, the call stops with TLSError, whose message says what to do.

Used by llm_provider (the probe before every provider is built) and by the
Windows Telegram bot. macOS and Linux keep Python's default verification;
neither calls this.
"""

import os
import ssl
import urllib.error
import urllib.parse
import urllib.request

# One GET per store; the probe is not on the request path.
TIMEOUT = 6

FIX = ("Turn off HTTPS scanning for that host in the antivirus or proxy, or "
       "point SSL_CERT_FILE at its root certificate.")


class TLSError(Exception):
    """No certificate store on this computer verifies the host. The message
    is complete: show it as it is."""


def message(url):
    host = urllib.parse.urlsplit(url).hostname or url
    return (f"AutoCua can't verify the HTTPS connection to {host}. Something on "
            f"this computer re-signs HTTPS (an antivirus or a proxy) with a "
            f"certificate Windows does not trust. {FIX}")


def is_cert_error(exc):
    """True for a certificate failure only, never for an error that merely
    mentions SSL: a firewall closing the handshake reads "[SSL: ...]" too.
    Looks through the wrappers requests and urllib put around the error."""
    for _ in range(8):
        if not isinstance(exc, BaseException):
            return False
        if isinstance(exc, ssl.SSLCertVerificationError):
            return True
        s = str(exc).lower()
        if any(tok in s for tok in ("certificate", "verify failed", "certverify")):
            return True
        # urllib's URLError.reason is the error inside; an OpenSSL error's
        # .reason is a string such as WRONG_VERSION_NUMBER, not a step.
        reason = getattr(exc, "reason", None)
        exc = reason if isinstance(reason, BaseException) else (exc.__cause__ or exc.__context__)
    return False


def certifi_context():
    """Python's default verification: certifi's bundle when it is installed."""
    try:
        import certifi
        return ssl.create_default_context(cafile=certifi.where())
    except ImportError:
        return ssl.create_default_context()


def os_context():
    """The operating system's certificate store, through Python's default
    context: on Windows it loads the system's roots, so one an antivirus or
    a company installed counts. A plain context on purpose: it has no state
    a second thread's handshake can leave behind, and it is the one the
    probe used before (truststore, tried first, flips a shared context to
    CERT_NONE around each handshake and two at once leave it there). The
    root SSL_CERT_FILE names is loaded explicitly as well, so a wrong path
    is an error and not silence: that variable is the message's advice."""
    ctx = ssl.create_default_context()
    cafile = os.environ.get("SSL_CERT_FILE")
    if cafile:
        try:
            ctx.load_verify_locations(cafile)
        except (OSError, ssl.SSLError) as e:
            raise TLSError(f"SSL_CERT_FILE is not a readable PEM certificate file ({cafile}): {e}") from None
    return ctx


def verifies(url, context):
    """True when `context` verifies url's certificate (any HTTP status counts:
    the server answered), False on a certificate error. Any other error
    (offline, a timeout) is raised as it is."""
    req = urllib.request.Request(url, headers={"User-Agent": "AutoCua"})
    try:
        urllib.request.urlopen(req, timeout=TIMEOUT, context=context).close()
    except urllib.error.HTTPError:
        return True
    except Exception as e:
        if is_cert_error(e):
            return False
        raise
    return True


def verified_context(url):
    """The context that verifies url: certifi's, or the operating system's.
    Raises TLSError when neither does, and the network error itself when the
    host cannot be reached at all, so the caller can tell offline from
    re-signed."""
    ctx = certifi_context()
    if verifies(url, ctx):
        return ctx
    ctx = os_context()
    if verifies(url, ctx):
        return ctx
    raise TLSError(message(url))

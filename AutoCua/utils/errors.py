"""The words an exception carries for the person, for a reply or a status line.

Built from the exception's own fields, never from str(e) and never from a
traceback: a reply to the app's window must carry the reason ("adb is not
available", "No such file or directory: /Users/x/gone"), not a stack trace, and
GitHub's code scanning (py/stack-trace-exposure) treats str(e) of a caught
exception as stack-trace text once it reaches a Flask reply.
"""

import subprocess


def error_text(e):
    """One line in the exception's own words.

    RuntimeError("The phone is not on Wi-Fi")   -> "The phone is not on Wi-Fi"
    FileNotFoundError(2, "No such file", "/x")  -> "No such file: /x"
    subprocess.TimeoutExpired(cmd, 30)         -> "timed out after 30 s"
    Exception()                                -> "Exception"
    """
    if isinstance(e, subprocess.TimeoutExpired):
        return f"timed out after {e.timeout:g} s"
    if isinstance(e, subprocess.CalledProcessError):
        return f"exit status {e.returncode}"
    if isinstance(e, OSError) and e.strerror:
        return f"{e.strerror}: {e.filename}" if e.filename else e.strerror
    words = " ".join(str(a) for a in e.args if isinstance(a, (str, int, float))).strip()
    return words or type(e).__name__

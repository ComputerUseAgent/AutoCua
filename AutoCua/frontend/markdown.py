r"""Markdown -> HTML for everything an agent writes to the user.

One job, one place: take the raw Markdown the model produces — `scratchpad`
notes and the `done`/`exit` summary — and hand the frontend clean, structured
HTML instead of broken plain text. Real line breaks for `\n`, real `<a>` for
links, real lists / headings / code blocks instead of literal `-`, `#` and
backticks.

Both Agent-Notes paths render through here, so live and reopened chats look
identical:
  • live run end  — service.send_agent_notes() -> window.showAgentNotes
  • reopened chat — /api/chats/<id> exchanges  -> window.showAgentHistory
                                              -> window.cliShellHistory (shell)

Safety. The input is LLM output and the frontend assigns it with innerHTML, so
every scrap of source text is HTML-escaped BEFORE any tag we emit is inserted,
and link targets are scheme-checked (http/https/mailto/file only). Nothing the
model writes can introduce a tag of its own. Stdlib only — no third-party
dependency to miss in the compiled build.

Public API:
    render(text)          full block Markdown -> HTML   (done / exit summaries)
    render_note(text)     one scratchpad entry -> inline HTML
    render_notes(content) milestone.md -> [HTML, …], one per numbered entry
    render_inline(text)   inline spans only, no block wrapper
"""

import re
import html
import logging

logger = logging.getLogger(__name__)

__all__ = ["render", "render_inline", "render_note", "render_notes"]


# =============================================================================
# Normalisation
# =============================================================================
# Code spans / fences are pulled out before escape-expansion below so a literal
# Windows path inside backticks (`C:\Users\me\notes.txt`) is never mistaken for
# a line break. Unbackticked paths are the one residual ambiguity — the tool
# descriptions tell every agent to backtick paths for exactly this reason.
_PROTECT_RE = re.compile(r"```.*?```|``.+?``|`[^`\n]+`", re.DOTALL)

# The model hand-writes JSON, so it often emits the two characters \ + n where
# it meant a newline. A doubled backslash (\\n) is a real escaped backslash and
# is left alone.
_ESC_NL_RE = re.compile(r"(?<!\\)\\n")
_ESC_TAB_RE = re.compile(r"(?<!\\)\\t")


def _normalize(text):
    """Raw model text -> text with real newlines, safe to parse as Markdown."""
    s = str(text or "")
    if not s:
        return ""
    # NUL is our placeholder sentinel; never let source text carry one.
    s = s.replace("\x00", "")
    s = s.replace("\r\n", "\n").replace("\r", "\n")

    stash = []

    def _keep(m):
        stash.append(m.group(0))
        return "\x00P%d\x00" % (len(stash) - 1)

    s = _PROTECT_RE.sub(_keep, s)
    s = _ESC_NL_RE.sub("\n", s)
    s = _ESC_TAB_RE.sub("\t", s)
    for i, original in enumerate(stash):
        s = s.replace("\x00P%d\x00" % i, original)
    return s.expandtabs(4)


# =============================================================================
# Inline spans
# =============================================================================
_SAFE_SCHEMES = ("http://", "https://", "mailto:", "file://")
_BARE_EMAIL_RE = re.compile(r"^[\w.+-]+@[\w-]+\.[\w.-]+$")

_CODE_SPAN_RE = re.compile(r"``(.+?)``|`([^`\n]+)`", re.DOTALL)
_IMAGE_RE = re.compile(r"!\[([^\]]*)\]\([^)]*\)")
# The URL alternation allows one level of balanced parens, so Wikipedia-style
# targets — and rejected ones like javascript:alert(1) — are consumed whole
# instead of leaving a stray ')' behind.
_LINK_RE = re.compile(
    r"\[([^\]]*)\]\(\s*((?:[^()\s]|\([^()]*\))+)\s*(?:\"[^\"]*\")?\s*\)")
_AUTOLINK_RE = re.compile(r"(?<![\w@/.])((?:https?://|www\.)[^\s<>\"'\[\]]+)")

_STRIKE_RE = re.compile(r"~~(?=\S)(.+?)(?<=\S)~~", re.DOTALL)
_BOLD_RE = re.compile(r"\*\*(?=\S)(.+?)(?<=\S)\*\*", re.DOTALL)
_BOLD_ALT_RE = re.compile(r"(?<![\w_])__(?=\S)(.+?)(?<=\S)__(?![\w_])", re.DOTALL)
# The lookarounds keep snake_case identifiers (`_read_scratchpad_from_file`)
# and glob patterns (`**/*.py`) from being eaten as emphasis.
_ITALIC_RE = re.compile(r"(?<![\w*])\*(?=\S)([^*\n]+?)(?<=\S)\*(?![\w*])")
_ITALIC_ALT_RE = re.compile(r"(?<![\w_])_(?=\S)([^_\n]+?)(?<=\S)_(?![\w_])")


def _safe_href(url):
    """Scheme-check an already-HTML-escaped URL; None means 'not a link'.

    Only `"` and `'` are escaped here — `&`/`<`/`>` were escaped by the caller
    and re-escaping would double them into `&amp;amp;`.
    """
    u = url.strip().rstrip(".,;:!?")
    if not u:
        return None
    low = u.lower()
    if low.startswith("www."):
        u = "https://" + u
    elif not (low.startswith(_SAFE_SCHEMES) or u.startswith("#") or u.startswith("/")):
        if _BARE_EMAIL_RE.match(u):
            u = "mailto:" + u
        else:
            return None
    return u.replace('"', "&quot;").replace("'", "&#x27;")


def render_inline(text):
    """Inline Markdown -> HTML. No block wrapper, no newline handling."""
    s = str(text or "").replace("\x00", "")
    if not s:
        return ""

    stash = []

    def _park(markup):
        stash.append(markup)
        return "\x00S%d\x00" % (len(stash) - 1)

    # 1. Code spans hold their content verbatim — park them before escaping so
    #    `**not bold**` inside backticks stays literal.
    def _code(m):
        raw = m.group(1) if m.group(1) is not None else m.group(2)
        return _park("<code>%s</code>" % html.escape(raw, quote=False))

    s = _CODE_SPAN_RE.sub(_code, s)

    # 2. Everything left is untrusted text.
    s = html.escape(s, quote=False)

    # 3. Images: the app shell blocks remote loads, so keep the alt text only.
    s = _IMAGE_RE.sub(lambda m: m.group(1), s)

    # 4. Explicit links, then bare URLs. Both get parked so step 5 can't chew
    #    through the markup we just built.
    def _link(m):
        label = m.group(1)
        href = _safe_href(m.group(2))
        if not href:
            return label or m.group(2)
        return _park('<a href="%s" target="_blank" rel="noopener noreferrer">%s</a>'
                     % (href, label or href))

    s = _LINK_RE.sub(_link, s)

    def _auto(m):
        raw = m.group(1)
        trail = ""
        while raw and raw[-1] in ".,;:!?":
            trail = raw[-1] + trail
            raw = raw[:-1]
        while raw.endswith(")") and raw.count("(") < raw.count(")"):
            trail = ")" + trail
            raw = raw[:-1]
        href = _safe_href(raw)
        if not href:
            return m.group(1)
        return _park('<a href="%s" target="_blank" rel="noopener noreferrer">%s</a>'
                     % (href, raw)) + trail

    s = _AUTOLINK_RE.sub(_auto, s)

    # 5. Emphasis — strongest delimiter first.
    s = _STRIKE_RE.sub(r"<del>\1</del>", s)
    s = _BOLD_RE.sub(r"<strong>\1</strong>", s)
    s = _BOLD_ALT_RE.sub(r"<strong>\1</strong>", s)
    s = _ITALIC_RE.sub(r"<em>\1</em>", s)
    s = _ITALIC_ALT_RE.sub(r"<em>\1</em>", s)

    for i, markup in enumerate(stash):
        s = s.replace("\x00S%d\x00" % i, markup)
    return s


# =============================================================================
# Block structure
# =============================================================================
_FENCE_RE = re.compile(r"^(```+|~~~+)\s*([\w+#.-]*)\s*$")
_FENCE_END_RE = re.compile(r"^(```+|~~~+)\s*$")
_HEADING_RE = re.compile(r"^(#{1,6})\s+(.*?)\s*#*$")
_HR_RE = re.compile(r"^(?:\*\s*){3,}$|^(?:-\s*){3,}$|^(?:_\s*){3,}$")
_UL_RE = re.compile(r"^(\s*)[-*+]\s+(.*)$")
_OL_RE = re.compile(r"^(\s*)(\d+)[.)]\s+(.*)$")
_TABLE_SEP_RE = re.compile(r"^\|?\s*:?-{2,}:?\s*(?:\|\s*:?-{2,}:?\s*)*\|?$")


def _list_item(line):
    """(indent, 'ul'|'ol', content) for a list line, else None."""
    m = _UL_RE.match(line)
    if m:
        return len(m.group(1)), "ul", m.group(2)
    m = _OL_RE.match(line)
    if m:
        return len(m.group(1)), "ol", m.group(3)
    return None


def _is_block_start(line):
    stripped = line.strip()
    if not stripped:
        return True
    return bool(
        _FENCE_RE.match(stripped)
        or _HEADING_RE.match(stripped)
        or _HR_RE.match(stripped)
        or stripped.startswith(">")
        or _list_item(line)
    )


def _render_list(lines, i, indent):
    """Build one <ul>/<ol> starting at lines[i]; returns (html, next_index)."""
    tag = _list_item(lines[i])[1]
    out = ["<%s>" % tag]
    n = len(lines)
    while i < n:
        item = _list_item(lines[i]) if lines[i].strip() else None
        if item is None:
            break
        item_indent, item_tag, content = item
        if item_indent < indent or (item_indent == indent and item_tag != tag):
            break
        if item_indent > indent:
            # Deeper bullet: nest it inside the <li> we just emitted.
            sub, i = _render_list(lines, i, item_indent)
            if len(out) > 1 and out[-1].endswith("</li>"):
                out[-1] = out[-1][:-len("</li>")] + sub + "</li>"
            else:
                out.append(sub)
            continue

        parts = [render_inline(content)]
        i += 1
        # Wrapped continuation lines: indented, not themselves a new item.
        while (i < n and lines[i].strip() and _list_item(lines[i]) is None
               and (len(lines[i]) - len(lines[i].lstrip())) > item_indent):
            parts.append(render_inline(lines[i].strip()))
            i += 1
        out.append("<li>%s</li>" % "<br>".join(p for p in parts if p))
    out.append("</%s>" % tag)
    return "".join(out), i


def _cells(line):
    s = line.strip()
    if s.startswith("|"):
        s = s[1:]
    if s.endswith("|"):
        s = s[:-1]
    return [c.strip() for c in s.split("|")]


def _render_table(lines, i):
    """Build a <table> from a pipe table at lines[i]; returns (html, next_index)."""
    head = _cells(lines[i])
    i += 2                                   # header row + the |---|---| rule
    body = []
    n = len(lines)
    while i < n and lines[i].strip() and "|" in lines[i]:
        body.append(_cells(lines[i]))
        i += 1
    out = ["<table><thead><tr>"]
    out += ["<th>%s</th>" % render_inline(c) for c in head]
    out.append("</tr></thead><tbody>")
    for row in body:
        out.append("<tr>")
        # Pad/trim ragged rows so the table never renders skewed.
        row = (row + [""] * len(head))[:len(head)]
        out += ["<td>%s</td>" % render_inline(c) for c in row]
        out.append("</tr>")
    out.append("</tbody></table>")
    return "".join(out), i


def render(text):
    """Full block Markdown -> HTML. Use for done / exit summaries."""
    try:
        s = _normalize(text)
        if not s.strip():
            return ""
        lines = s.split("\n")
        out = []
        i = 0
        n = len(lines)
        while i < n:
            line = lines[i]
            stripped = line.strip()
            if not stripped:
                i += 1
                continue

            m = _FENCE_RE.match(stripped)
            if m:
                lang = re.sub(r"[^\w+#.-]", "", m.group(2))
                i += 1
                buf = []
                while i < n and not _FENCE_END_RE.match(lines[i].strip()):
                    buf.append(lines[i])
                    i += 1
                i += 1                        # consume the closing fence
                cls = ' class="language-%s"' % lang if lang else ""
                out.append("<pre><code%s>%s</code></pre>"
                           % (cls, html.escape("\n".join(buf), quote=False)))
                continue

            m = _HEADING_RE.match(stripped)
            if m:
                lvl = len(m.group(1))
                out.append("<h%d>%s</h%d>" % (lvl, render_inline(m.group(2)), lvl))
                i += 1
                continue

            if _HR_RE.match(stripped):
                out.append("<hr>")
                i += 1
                continue

            if stripped.startswith(">"):
                buf = []
                while i < n and lines[i].strip().startswith(">"):
                    buf.append(re.sub(r"^\s*>\s?", "", lines[i]))
                    i += 1
                out.append("<blockquote>%s</blockquote>" % render("\n".join(buf)))
                continue

            if ("|" in stripped and i + 1 < n
                    and _TABLE_SEP_RE.match(lines[i + 1].strip())):
                block, i = _render_table(lines, i)
                out.append(block)
                continue

            item = _list_item(line)
            if item:
                block, i = _render_list(lines, i, item[0])
                out.append(block)
                continue

            buf = []
            while i < n and lines[i].strip() and not _is_block_start(lines[i]):
                buf.append(lines[i].strip())
                i += 1
            if buf:
                out.append("<p>%s</p>" % "<br>".join(render_inline(b) for b in buf))
            else:
                i += 1                        # never spin on an unconsumed line
        return "".join(out)
    except Exception:
        logger.exception("markdown render failed")
        return "<p>%s</p>" % html.escape(str(text or ""), quote=False)


def render_note(text):
    """One scratchpad entry -> HTML.

    Entries are written one-per-line, so this stays inline-level (no <p>
    wrapper to fight the notes-list layout) and turns any newline the model
    slipped in into a <br>. A note that carries a real code fence falls back to
    the full block renderer.
    """
    try:
        s = _normalize(text)
        if not s.strip():
            return ""
        if "```" in s:
            return render(s)
        lines = [ln.strip() for ln in s.split("\n")]
        while lines and not lines[0]:
            lines.pop(0)
        while lines and not lines[-1]:
            lines.pop()
        # A note the model wrote as a bullet list still deserves real bullets.
        if any(_list_item(ln) for ln in lines):
            return render(s)
        return "<br>".join(render_inline(ln) for ln in lines)
    except Exception:
        logger.exception("markdown render_note failed")
        return html.escape(str(text or ""), quote=False)


_NUMBER_PREFIX_RE = re.compile(r"^(\d+)[.)]\s+")


def render_notes(content):
    """milestone.md -> [HTML, …], one entry per line.

    The leading `N. ` written by ScratchpadService is stripped (the frontend
    re-numbers, so the count stays right after blank lines are dropped).
    """
    entries = []
    for raw in str(content or "").split("\n"):
        line = raw.strip()
        if not line:
            continue
        line = _NUMBER_PREFIX_RE.sub("", line, count=1).strip()
        if not line:
            continue
        rendered = render_note(line)
        if rendered:
            entries.append(rendered)
    return entries

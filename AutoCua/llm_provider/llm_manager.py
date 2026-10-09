import copy
import os
import sys
import time
from typing import Optional

from dotenv import load_dotenv

from . import _ensure_tls_works
from .openrouter.service import OpenRouterProvider
from .openrouter.view import get_model_info as get_openrouter_model_info
from .groq.service import GroqProvider
from .groq.view import get_model_info as get_groq_model_info
from .openai.service import OpenAIProvider
from .openai.view import get_model_info as get_openai_model_info
from .anthropic.service import AnthropicProvider
from .anthropic.view import get_model_info as get_anthropic_model_info
from .google.service import GoogleProvider
from .google.view import get_model_info as get_google_model_info
from .perplexity.service import PerplexityProvider
from .perplexity.view import get_model_info as get_perplexity_model_info
from .together.service import TogetherProvider
from .together.view import get_model_info as get_together_model_info
from .cerebras.service import CerebrasProvider
from .cerebras.view import get_model_info as get_cerebras_model_info
from .aws.service import AWSProvider
from .aws.view import get_model_info as get_aws_model_info

# Load environment variables
load_dotenv()

# One manager for every platform (windows, mac, linux, ios): each platform's
# AutoCua/<os>/tool_registry/service.py holds its tool lists and subclasses
# LLMManager below to hand them in. The web agent's Rust manager builds its
# providers here too (build_provider, resolve_model_info).
#
# Every agent here speaks NATIVE TOOL CALLING — the tool registries ARE
# the output contract. There is no JSON-envelope response schema anymore: the
# main driver's AGENT_OUTPUT_SCHEMA / FAST_AGENT_SCHEMA are gone the same way
# CLI_AGENT_SCHEMA and MINION_SCHEMA were, and no provider is handed a
# response_format. The only schema-less, tool-less path left is mode="text"
# (the memory-compression handoff), which wants plain prose.


# The coder/minion transcript carries an optional per-turn `provider_meta` on
# assistant messages — the provider's OWN metadata for that turn (Gemini 3
# thought signatures, OpenRouter reasoning blocks), needed to echo the turn
# back exactly as the model produced it. Only these providers translate it;
# for everyone else the key is stripped before the request is built, because
# openai/groq/perplexity/together forward the message dicts to their API
# verbatim and an unknown key there is a 400.
_META_KEY = "provider_meta"
_META_PROVIDERS = ("openrouter", "google")

# Output cap for the memory-compression handoff agent (mode="text"), the
# same on every provider. It writes one long structured document, and the
# 4000-token defaults on anthropic/openai/groq cut it off silently.
HANDOFF_MAX_TOKENS = 10000


# LEGACY default for tool_calls_to_steps (no registry carries these any more:
# the coder and the minion moved their notes onto the `reasoning` tool). Kept
# so a caller passing no track_params still harvests the old two params.
_TRACK_PARAMS = {
    "thinking": 'Follow the <thinking> rules: "THINK: ... PLAN: ... ACT: ..." (FULL) at decision points, a short freeform paragraph (RECOVERY) on failures, 1–2 judgment lines on plan-driven steps — or exactly "skipped" when the SKIP TEST passes. Never empty. Fill on the FIRST tool call of the step; pass "" on every additional call in the same step.',
    "next_goal": 'Follow the <next_goal> rules — labeled format: "memory: <S<n> verdict + key context> next_goal: Doing: ... If ... → Next: ...". Fill on the FIRST tool call of the step; pass "" on every additional call in the same step.',
}

# The dedicated notes tools: `reasoning` (quality mode, coder, minion),
# `agent_memory` (the same notes on Claude) and `memory` (fast mode). The
# parser and the loop treat a call to any of them as the step's notes,
# never as an action.
NOTE_TOOLS = ("reasoning", "agent_memory", "memory")

# The optional note fields: `thinking` on the `reasoning` tools and, on
# Claude, `assessment` on `agent_memory` (see each tool registry). Left out of
# the schema's `required` list and out of the "no arguments" error text.
REASONING_OPTIONAL = ("thinking", "assessment")


def _tool(name: str, params: dict, description: str = "", track: dict = None,
          optional: tuple = ()) -> dict:
    """Build one canonical tool def — name + parameters + description. The
    REGISTRY is the single source of tool documentation: each tool's rules
    live in its `description`, which flows to every provider dialect. The
    system prompt keeps only the step protocol and operating procedure. Tool
    defs sit in the cached prefix, so this bills once. All params are required
    unless named in `optional`, so the controller always sees every field of
    an action (the only optional one today is the coder's `thinking`, which a
    route step omits). `track` (the minion's and the main driver's per-call
    tracking params) rides ahead of the action fields when given; the coder
    passes none, because its notes live on the `reasoning` tool."""
    props = {b: {"type": "string", "description": d} for b, d in (track or {}).items()}
    props.update(params)
    tool = {
        "name": name,
        "parameters": {
            "type": "object",
            "properties": props,
            "required": [k for k in props if k not in optional],
        },
    }
    if description:
        tool["description"] = description
    return tool


def coder_tools_openai(registry: list) -> list:
    """OpenAI/OpenRouter/Groq/Together chat-completions function format."""
    return [{"type": "function", "function": t} for t in registry]


def _with_description(tool: dict, source: dict) -> dict:
    """Carry the registry description into a dialect dict when present (the
    coder `exit` tool has none — _tool() only sets the key when non-empty)."""
    if source.get("description"):
        tool["description"] = source["description"]
    return tool


def coder_tools_anthropic(registry: list) -> list:
    """Anthropic Messages API tools format."""
    return [_with_description({"name": t["name"], "input_schema": t["parameters"]}, t)
            for t in registry]


def coder_tools_gemini(registry: list) -> list:
    """Gemini function declarations (dicts accepted by google-genai)."""
    return [_with_description({"name": t["name"], "parameters": t["parameters"]}, t)
            for t in registry]


def coder_tools_perplexity(registry: list) -> list:
    """Perplexity agent API (Responses-style flat function tools)."""
    return [_with_description({"type": "function", "name": t["name"],
                               "parameters": t["parameters"]}, t)
            for t in registry]


def _coerce(value, default):
    """Best-effort coercion to the default's type (models sometimes send '5' for 5)."""
    if isinstance(default, bool):
        if isinstance(value, bool):
            return value
        return str(value).strip().lower() in ("true", "1", "yes")
    if isinstance(default, int) and not isinstance(default, bool):
        try:
            return int(value)
        except (TypeError, ValueError):
            return default
    if isinstance(default, str):
        return value if isinstance(value, str) else str(value)
    return value


def _dialect_tool_name(tool: dict) -> str:
    """A tool's name in any provider dialect: at the top (Anthropic, Gemini,
    Perplexity) or under `function` (chat completions)."""
    return tool.get("name") or (tool.get("function") or {}).get("name") or ""


def tool_calls_to_steps(tool_calls: list, allowed, defaults_map, track_params=None) -> tuple:
    """Convert normalized provider tool calls into
    (actions, calls, rejects, track) where:

      actions — the SAME `[{type, ...}]` dicts route_action always consumed,
                with the tracking params STRIPPED (the controller never sees
                them)
      calls   — parallel to actions: {"id", "name", "arguments"} for each,
                so the loop can echo the model's OWN tool calls back in the
                next request and match each result to its call id (the native
                transcript that makes models behave natively). Arguments are
                kept EXACTLY as the model sent them — tracking params
                included — so the echoed transcript never contradicts the
                schema that required them.
      rejects — {"id", "name", "error"} for calls naming a tool that does not
                exist; the loop feeds these back as error tool results
      track   — the tracking params stitched from the step's calls: the first
                call carries them in full, later calls pass "".

    `allowed` and `defaults_map` come from the caller's tool registry. Each
    platform's tool_registry/service.py exports this with its own defaults
    (the coder's tables on the desktops), so the agents' call sites stay as
    they were."""
    actions, calls, rejects = [], [], []
    track = {b: "" for b in (track_params or _TRACK_PARAMS)}
    names = allowed
    dm = defaults_map
    # When the registry has a dedicated notes tool carrying the tracking
    # params (`reasoning`: coder, minion, main driver in quality mode; `memory`:
    # main driver in fast mode), the action tools do NOT, so a malformed call's
    # "required" list must not name them. A registry without one (older saves
    # replayed against an old map) still carries them on every call. Read off
    # the defaults map, which is per registry.
    notes_on_own_tool = any(n in dm and set(track) <= set(dm[n]) for n in NOTE_TOOLS)
    for i, call in enumerate(tool_calls or []):
        name = str((call or {}).get("name") or "").strip()
        args = (call or {}).get("arguments")
        if not isinstance(args, dict):
            args = {}
        call_id = str((call or {}).get("id") or "") or f"call_{i}"
        for b in track:
            v = str(args.get(b) or "").strip()
            if v and not track[b]:
                track[b] = v
        defaults = dm.get(name)
        if defaults is None or name not in names:
            rejects.append({
                "id": call_id,
                "name": name or "(unnamed)",
                "arguments": args,
                "error": f"No tool named '{name or '(unnamed)'}' exists. "
                         f"Available tools: {', '.join(sorted(names))}. "
                         f"Call one of those instead.",
            })
            continue
        # EMPTY arguments is a schema violation, not a set of omitted optional
        # fields — every param on every tool is `required`. Letting the defaults
        # below fill it would silently promote the model's malformed turn into a
        # REAL action: a `left_click` with no arguments becomes id 0 / clicks 1
        # and clicks whatever element 0 happens to be; an `input` with none
        # becomes id 0 / value "", which focuses element 0 and clears it.
        # Observed in the wild on the web driver (gpt-5.6-luna emitted
        # `"arguments": {}` on one step, then a well-formed call on the next).
        # Reject it exactly the way an unknown tool name is rejected, so the
        # model gets an error keyed to its own call id and re-issues the step
        # instead of acting on an element it never chose.
        #
        # Narrow on purpose: ONLY a fully empty argument object. A partial call
        # still gets default-filled as before, and id 0 remains a perfectly
        # valid element — the problem is never the value 0 itself, only a 0 that
        # arrived from a default the model never sent.
        if defaults and not args:
            rejects.append({
                "id": call_id,
                "name": name,
                "arguments": args,
                "error": f"'{name}' was called with no arguments at all. Every "
                         f"field is required: "
                         f"{', '.join(sorted((set(defaults) - (set(REASONING_OPTIONAL) if name in ('reasoning', 'agent_memory') else set())) | (set() if notes_on_own_tool else set(track))))}. "
                         f"Re-issue the call with all of them filled in.",
            })
            continue
        action = {"type": name}
        for key, default in defaults.items():
            action[key] = _coerce(args[key], default) if key in args else default
        actions.append(action)
        calls.append({"id": call_id, "name": name, "arguments": args})
    return actions, calls, rejects, track


# Coder / minion emergency fallbacks. The PRIMARY model is always the one the
# user picked (UI drop-up, cli.py's MODEL, --model) — these cover a SINGLE
# failing call and are then dropped. Ordered: the first candidate that isn't
# the user's own model wins, so the fallback is never the model that just
# failed. Every name below exists in the matching provider view.py
# MODEL_MAPPINGS (the old map's gpt-5.2 / gpt-5.1 /
# claude-sonnet-4.5 did not, which is why the openai + anthropic fallbacks
# could never succeed).
_CLI_FALLBACK_CANDIDATES = {
    # Single entry: groq now registers one model, so a user already on it has
    # nothing to fall back TO and correctly resolves to None. The tuple still
    # earns its place for a hand-typed groq model, which falls back here.
    "groq":       ("qwen3.8-27b",),
    # Single entry: the openai table holds one model, so a user already on
    # it has nothing to fall back TO and correctly resolves to None.
    "openai":     ("gpt-6-astra",),
    "openrouter": ("gemini-3.6-flash", "gpt-6-astra"),
    "anthropic":  ("claude-haiku-4.5", "claude-sonnet-5"),
    "google":     ("gemini-3.6-flash", "gemini-3.1-pro"),
    "perplexity": ("gemini-3.6-flash", "kimi-k3"),
    "together":   ("minimax-m3", "inkling"),
    # Single entry, like groq: one registered model, so nothing to fall back TO.
    "cerebras":   ("qwen-3.8-27b",),
    # Non-Claude on purpose, for the Claude entries too: a fallback keeps the
    # primary's tool registry, and the Claude one (`agent_memory`) suits any
    # model, while a Claude model handed the default `reasoning` tool has
    # refused it (seen on the web Claude path).
    "aws":        ("kimi-k3", "grok-4.7"),
}


def _pick_cli_fallback(provider: str, model: str):
    """First registered secondary for `provider` that isn't `model` itself."""
    candidates = _CLI_FALLBACK_CANDIDATES.get(provider, ())
    # Vertex and AI-Studio are different clients, decided at provider
    # construction — a vertex model may only fall back to another vertex one.
    if provider == "google" and str(model).endswith("-vertex"):
        candidates = tuple(f"{c}-vertex" for c in candidates)
    return next((c for c in candidates if c != model), None)


def drop_bundled_terminal(actions: list, calls: list, rejects: list, terminal: str) -> tuple:
    """Backstop for an `async_tools` model (GPT-6 Astra, see responses.py).

    With every tool async, nothing stops the model from bundling its terminal
    call (`done` for the driver, `exit` for the coder and the minion) after
    this turn's other calls, whose results it has not seen; the controller
    would run them and end the run on the terminal call, unverified. The
    terminal call is dropped from the batch and answered with an error result
    keyed to its id, so the other calls run, the replay stays complete (the
    caller's all_calls still holds the call) and the model calls the terminal
    tool alone once it has verified them. A turn whose only calls are terminal
    ones is returned untouched. Returns (actions, calls, results); the caller
    prints under its own stdout convention."""
    bundled = any(a.get("type") == terminal for a in actions)
    others = any(a.get("type") != terminal for a in actions) or bool(rejects)
    if not (bundled and others):
        return actions, calls, []
    results = [
        {"tool_call_id": c["id"],
         "content": f"error: {terminal} ignored, it must be the only call of its turn. "
                    f"This turn's other calls ran; verify them on the next input, then call {terminal} alone."}
        for a, c in zip(actions, calls) if a.get("type") == terminal
    ]
    kept = [(a, c) for a, c in zip(actions, calls) if a.get("type") != terminal]
    return [a for a, _ in kept], [c for _, c in kept], results


def resolve_model_info(provider: str, short_name: str) -> dict:
    """Provider-appropriate get_model_info; unknown providers pass through.

    Every provider view's get_model_info already falls through to a
    passthrough dict for unregistered names, so any model string the user
    types is usable — nothing here validates against an allowlist.
    """
    getter = {
        "openrouter": get_openrouter_model_info,
        "groq": get_groq_model_info,
        "openai": get_openai_model_info,
        "anthropic": get_anthropic_model_info,
        "google": get_google_model_info,
        "perplexity": get_perplexity_model_info,
        "together": get_together_model_info,
        "cerebras": get_cerebras_model_info,
        "aws": get_aws_model_info,
    }.get(provider)
    if getter is None:
        return {"api_name": short_name, "vision": True, "display_name": short_name}
    return getter(short_name)


def dialect_tools(provider: str, registry: list) -> list:
    """`registry` (the canonical tool list) in `provider`'s own tool format."""
    if provider == "anthropic":
        return coder_tools_anthropic(registry)
    if provider == "google":
        return coder_tools_gemini(registry)
    if provider == "perplexity":
        return coder_tools_perplexity(registry)
    # aws takes this format too and makes Converse toolSpecs of it itself,
    # because it is also what the web agent's Rust manager sends any provider
    # it has no dialect for.
    return coder_tools_openai(registry)


def build_provider(provider: str, model_short_name: str, model_info: dict, tools: list = None,
                   cli_agent: bool = False, max_tokens: int = None, api_key: str = None):
    """The provider object for one manager: `tools` already in the provider's
    own format (None for mode="text"), and the API key (the runtime key from
    the frontend first, then .env). Every platform's LLMManager builds its
    provider here, and so does the web agent's Rust manager. Runs the Windows
    TLS probe first (a no-op on macOS and Linux)."""
    _ensure_tls_works()
    if provider == "openrouter":
        # Priority: Runtime key > .env fallback
        api_key = api_key or os.getenv('OPENROUTER_API_KEY')
        if not api_key:
            raise ValueError("OpenRouter API key not provided and not found in .env file")
        return OpenRouterProvider(api_key, cli_agent, model_info,
                                  tools=tools,
                                  max_tokens=max_tokens)
    elif provider == "groq":
        # Priority: Runtime key > .env fallback
        api_key = api_key or os.getenv('GROQ_API_KEY')
        if not api_key:
            raise ValueError("Groq API key not provided and not found in .env file")
        return GroqProvider(api_key, cli_agent, model_info,
                            tools=tools,
                            max_tokens=max_tokens)
    elif provider == "openai":
        # Priority: Runtime key > .env fallback
        api_key = api_key or os.getenv('OPENAI_API_KEY')
        if not api_key:
            raise ValueError("OpenAI API key not provided and not found in .env file")
        return OpenAIProvider(api_key, cli_agent,
                              tools=tools,
                              max_tokens=max_tokens,
                              model_info=model_info)
    elif provider == "anthropic":
        # Priority: Runtime key > .env fallback
        api_key = api_key or os.getenv('ANTHROPIC_API_KEY')
        if not api_key:
            raise ValueError("Anthropic API key not provided and not found in .env file")
        return AnthropicProvider(api_key, cli_agent,
                                 tools=tools,
                                 max_tokens=max_tokens,
                                 model_info=model_info)
    elif provider == "google":
        # Check if this is a Vertex model
        model_meta = get_google_model_info(model_short_name)
        is_vertex = model_meta.get("vertex", False)
        
        if is_vertex:
            # Read Vertex config from api_key.txt
            vertex_project_id = None
            vertex_location = None
            try:
                # AutoCua_data/api_key/api_key.txt — outside the install
                # folder, and the SAME file the Settings panel writes.
                from AutoCua import api_key_file
                key_file = api_key_file()
                if key_file.exists():
                    with open(key_file, 'r', encoding='utf-8') as f:
                        for line in f:
                            line = line.strip()
                            if line.startswith('VERTEX_PROJECT_ID='):
                                vertex_project_id = line.partition('=')[2]
                            elif line.startswith('VERTEX_LOCATION='):
                                vertex_location = line.partition('=')[2]
            except Exception:
                pass
            return GoogleProvider(
                api_key=None, cli_agent=cli_agent,
                model=model_short_name,
                vertex_project_id=vertex_project_id, vertex_location=vertex_location,
                tools=tools, model_info=model_info,
                max_tokens=max_tokens
            )
        else:
            # AI Studio — needs API key
            api_key = api_key or os.getenv('GOOGLE_API_KEY')
            if not api_key:
                raise ValueError("Google API key not provided and not found in .env file")
            return GoogleProvider(api_key, cli_agent, model=model_short_name,
                                  tools=tools, model_info=model_info,
                                  max_tokens=max_tokens)
    elif provider == "perplexity":
        api_key = api_key or os.getenv('PERPLEXITY_API_KEY')
        if not api_key:
            raise ValueError("Perplexity API key not provided and not found in .env file")
        return PerplexityProvider(api_key, cli_agent, model_info,
                                  tools=tools,
                                  max_tokens=max_tokens)
    elif provider == "together":
        # Priority: Runtime key > .env fallback
        api_key = api_key or os.getenv('TOGETHER_API_KEY')
        if not api_key:
            raise ValueError("Together API key not provided and not found in .env file")
        return TogetherProvider(api_key, cli_agent, model_info,
                                tools=tools,
                                max_tokens=max_tokens)
    elif provider == "cerebras":
        # Priority: Runtime key > .env fallback
        api_key = api_key or os.getenv('CEREBRAS_API_KEY')
        if not api_key:
            raise ValueError("Cerebras API key not provided and not found in .env file")
        return CerebrasProvider(api_key, cli_agent, model_info,
                                tools=tools,
                                max_tokens=max_tokens)
    elif provider == "aws":
        # Priority: Runtime key > .env fallback
        api_key = api_key or os.getenv('AWS_API_KEY')
        if not api_key:
            raise ValueError("AWS API key not provided and not found in .env file")
        return AWSProvider(api_key, cli_agent, model_info,
                           tools=tools,
                           max_tokens=max_tokens)
    else:
        raise ValueError(f"Unsupported provider: {provider}")


class LLMManager:
    """Manager to route requests to the correct LLM provider.

    Shared by every platform: each platform's tool_registry/service.py
    subclasses it and sets the tool lists below (iOS only the main driver's),
    and the agents use that subclass."""

    MAIN_TOOLS = None
    MAIN_TOOLS_FAST = None
    MAIN_TOOLS_CLAUDE = None
    CODER_TOOLS = None
    CODER_TOOLS_CLAUDE = None
    MINION_TOOLS = None
    MINION_TOOLS_CLAUDE = None
    # The tools the scan toggle takes away (show_screen_tools).
    SCREEN_TOOLS = ()

    def __init__(self, provider: str, model: str, api_key: str = None, cli_agent: bool = False, mode: str = "main", speed: str = "quality"):
        self.provider = provider.lower()
        self.model_short_name = model
        self.runtime_api_key = api_key  # Runtime key from frontend (priority)
        # Coder/minion flag. NOT a "native tools" switch — every mode but
        # "text" is native now. It stays False for the MAIN DRIVER because
        # each provider gates its screenshot splice on `not self.cli_agent`,
        # so flipping it would silently kill the driver's vision.
        self.cli_agent = cli_agent
        self.mode = mode  # "main" | "minion" | "text" — picks the tool registry
        self.speed = speed  # "quality" | "fast" — fast trims the main-agent tracking params
        # Output cap handed to the provider: 10k for the handoff agent, None
        # for every other mode (each provider keeps its own default).
        self.max_tokens = HANDOFF_MAX_TOKENS if mode == "text" else None

        # Coder / minion (cli_agent=True) run the SAME model as everyone else —
        # whatever the UI, cli.py or --model handed in. The only per-provider
        # hardcoding left is the emergency fallback, used for one call (see
        # send_request).
        self._cli_fallback_model = _pick_cli_fallback(self.provider, model) if cli_agent else None

        # Get model info based on provider
        model_info = self._resolve_model_info(model)

        self.model = model_info["api_name"]
        self.has_vision = model_info["vision"]
        self.display_name = model_info["display_name"]
        self.model_info = model_info  # Full model info, forwarded to the provider
        self._primary_model_info = model_info  # restored after every fallback call

        # NATIVE TOOL CALLING is the only path: the main driver, the coder and
        # the minion all get tool definitions as their output contract — no
        # JSON envelope, no response_format, nothing to parse. The single
        # exception is mode="text" (the memory-compression handoff), which
        # wants plain prose and so gets neither tools nor a schema.
        self.native_tools = mode != "text"

        # Most recent send_request's normalized token usage (input/output/total).
        # Captured as a side effect so callers (e.g. the memory bar) can read it
        # without changing send_request's return shape.
        self.last_usage = {}
        # Most recent successful API round-trip in seconds (ping → response),
        # measured fresh per attempt — read this in code for the exact number.
        self.last_call_seconds = 0.0
        # Any Claude model, whatever the provider: anthropic direct, or a Claude
        # entry on OpenRouter (api_name anthropic/claude-...). It
        # picks the tool registry below and, in every agent, claude_prompt.md
        # plus the `agent_memory` notes tool. Set once from the primary model,
        # so the one-call fallback never flips it.
        self.claude = self.provider == "anthropic" or "claude" in self.model.lower()
        self.provider_instance = self._initialize_provider()
        
    def _resolve_model_info(self, short_name: str) -> dict:
        """resolve_model_info for this manager's provider."""
        return resolve_model_info(self.provider, short_name)

    def _apply_model_info(self, info: dict) -> None:
        """Point this manager at a model.

        Also re-points provider_instance.model_info, which openrouter/groq/
        perplexity cache at construction — otherwise a swapped model inherits
        the other model's registry entry.
        """
        self.model = info["api_name"]
        self.has_vision = info["vision"]
        self.display_name = info["display_name"]
        self.model_info = info
        provider_instance = getattr(self, "provider_instance", None)
        if provider_instance is not None and hasattr(provider_instance, "model_info"):
            provider_instance.model_info = info

    def _initialize_provider(self):
        """Initialize the appropriate provider based on selection"""
        # Hand each provider its dialect's tool definitions — the registry is
        # picked by mode (minion gets the read-only subset; the coder gets its
        # own; the main driver gets its action tools, thinking-less in fast
        # mode). mode="text" is the only caller that gets no tools at all.
        # A Claude model (self.claude, any provider) gets the CLAUDE variants,
        # whose notes tool is `agent_memory` (assessment in place of thinking).
        claude = self.claude
        if self.mode == "minion":
            registry = self.MINION_TOOLS_CLAUDE if claude else self.MINION_TOOLS
        elif self.cli_agent:
            registry = self.CODER_TOOLS_CLAUDE if claude else self.CODER_TOOLS
        else:
            registry = (self.MAIN_TOOLS_FAST if self.speed == "fast"
                        else self.MAIN_TOOLS_CLAUDE if claude else self.MAIN_TOOLS)
        return build_provider(self.provider, self.model_short_name, self.model_info,
                              dialect_tools(self.provider, registry) if self.native_tools else None,
                              cli_agent=self.cli_agent, max_tokens=self.max_tokens,
                              api_key=self.runtime_api_key)
    
    def _normalize_usage(self, u):
        """Normalize a provider usage dict to {input_tokens, output_tokens,
        total_tokens, context_tokens}, tolerating both key styles (Anthropic-style
        input/output and OpenAI-style prompt/completion). Empty/missing -> zeros.

        context_tokens is the TRUE size of the prompt actually sent this turn — the
        memory-bar number. A cached token still occupies the context window, so we
        add the cache classes back: input_tokens + cache_read + cache_creation.
        This is exact for every provider:
          - Anthropic: input_tokens EXCLUDES cache, so the cache fields are added.
          - OpenAI/Google/Perplexity/OpenRouter/Groq: prompt_tokens already INCLUDES
            cached tokens and the Anthropic cache keys are absent (0), so this
            collapses to the full prompt count — no double-count.
        """
        u = u or {}
        inp = int(u.get("input_tokens", u.get("prompt_tokens", 0)) or 0)
        out = int(u.get("output_tokens", u.get("completion_tokens", 0)) or 0)
        tot = int(u.get("total_tokens", 0) or 0) or (inp + out)
        cache_read = int(u.get("cache_read_input_tokens", 0) or 0)
        cache_create = int(u.get("cache_creation_input_tokens", 0) or 0)
        context_tokens = inp + cache_read + cache_create
        return {
            "input_tokens": inp,
            "output_tokens": out,
            "total_tokens": tot,
            "context_tokens": context_tokens,
        }

    def _attempt(self, messages: list, annotated_screenshot_base64: Optional[str] = None):
        """Three idempotent tries against the model currently loaded. Raises the
        last error if all three fail.

        Return shape: plain content string normally; in native-tools mode
        (coder) a dict {"text": str, "tool_calls": [{"name", "arguments"}],
        "provider_meta": dict} — the provider already normalized the calls."""
        last_error = None
        for attempt in range(3):
            # Providers may mutate messages in-place (e.g. wrapping the last user
            # message into multimodal content blocks); deep-copy per attempt so
            # those mutations cannot compound across retries.
            attempt_messages = copy.deepcopy(messages)
            if self.provider not in _META_PROVIDERS:
                for m in attempt_messages:
                    if isinstance(m, dict):
                        m.pop(_META_KEY, None)
            try:
                # Pure API round-trip: clock starts the moment the provider is
                # pinged, stops the moment its response is back. Fresh from 0
                # on EVERY attempt; the deep-copy above is outside the clock.
                _t0 = time.perf_counter()
                response = self.provider_instance.send_request(
                    attempt_messages, self.model, annotated_screenshot_base64
                )
                self.last_call_seconds = time.perf_counter() - _t0
                self.last_usage = self._normalize_usage(response.get("usage"))
                if sys.stdout.isatty():
                    # \r first: overwrite any spinner residue so the line is
                    # clean. TTY-only — never leak into the UI subprocess pipe.
                    # Token counts ride on the same line: a slow call with a
                    # small input/output is NOT a generation cost — it's cold
                    # prefill, provider-side reasoning, or backend routing.
                    _raw_usage = response.get("usage") or {}
                    _cached = int(
                        _raw_usage.get("cache_read_input_tokens", 0)
                        or ((_raw_usage.get("prompt_tokens_details") or {}).get("cached_tokens", 0))
                        or 0
                    )
                    print(
                        f"\r⏱ LLM call: {self.last_call_seconds:.2f}s ({self.display_name}) "
                        f"| in {self.last_usage['input_tokens']} "
                        f"(cached {_cached}) "
                        f"| out {self.last_usage['output_tokens']}"
                    )
                message = response['choices'][0]['message']
                if self.native_tools:
                    return {
                        "text": message.get('content') or "",
                        "tool_calls": message.get('tool_calls') or [],
                        # Provider metadata for THIS turn, persisted with the
                        # step and handed back to the same provider next
                        # request (see _META_KEY above). {} when the provider
                        # emits none, which is the pre-existing behavior.
                        _META_KEY: message.get(_META_KEY) or {},
                    }
                return message['content']
            except Exception as e:
                last_error = e
                if attempt < 2:
                    print(f"⚠️ {self.display_name} request failed (attempt {attempt + 1}/3): {e}")
                    print("   Retrying in 1 second with a fresh message copy...")
                    time.sleep(1)
                    continue
                print(f"❌ {self.display_name} request failed after 3 attempts: {e}")
        raise last_error

    def send_request(self, messages: list, annotated_screenshot_base64: Optional[str] = None):
        """Send request to the selected provider with idempotent retries.

        Coder/minion only: if the user's model fails all 3 attempts, ONE call is
        served by the per-provider fallback (3 attempts of its own). Win or lose,
        the manager is put back on the user's model afterwards, so the next
        iteration starts on their model again. If the fallback also fails its 3
        attempts the error propagates and the agent stops.

        The main agent has no fallback (cli_agent=False): 3 attempts, then raise.
        """
        try:
            return self._attempt(messages, annotated_screenshot_base64)
        except Exception:
            if not (self.cli_agent and self._cli_fallback_model):
                raise

        fallback_info = self._resolve_model_info(self._cli_fallback_model)
        print(f"⚠️ {self.display_name} failed 3 attempts — this step only, "
              f"falling back to {fallback_info['display_name']}...")
        self._apply_model_info(fallback_info)
        try:
            result = self._attempt(messages, annotated_screenshot_base64)
        except Exception:
            print(f"❌ Fallback {self.display_name} also failed 3 attempts — stopping.")
            raise
        finally:
            # Always revert: the fallback covers this call, not the whole run.
            self._apply_model_info(self._primary_model_info)
        print(f"↩️ Back on {self.display_name} for the next step.")
        return result

    def async_tools(self) -> bool:
        """True when the model batches only on the Responses endpoint with
        async tools (GPT-6 Astra, table flag `async_tools`): the loops add
        the <async_tools> prompt block and the bundled done/exit backstop
        for it. Read from the primary entry, so a one-call fallback does not flip it."""
        return bool(self._primary_model_info.get("async_tools"))

    def show_screen_tools(self, on: bool) -> None:
        """Scan toggle (main driver): send every tool, or every tool but
        SCREEN_TOOLS while the scan is off. The provider holds its tools in its
        own dialect and reads them on every request, so the short list is cut
        from that one once and the two are swapped on the provider."""
        inst = self.provider_instance
        if getattr(self, "_tool_lists_of", None) is not inst:
            full = inst.tools
            short = [t for t in full or [] if _dialect_tool_name(t) not in self.SCREEN_TOOLS]
            self._tool_lists, self._tool_lists_of = (full, short), inst
        inst.tools = self._tool_lists[0] if on else self._tool_lists[1]

    def get_model_name(self) -> str:
        """Get the current model short name (preserves vertex suffix for downstream routing)"""
        return self.model_short_name
    
    def get_provider_name(self) -> str:
        """Get the current provider name"""
        return self.provider
# Model mappings for OpenRouter provider
# Maps user-friendly names to actual API model names

# `effort` is how hard the model thinks, carried to OpenRouter as its unified
# reasoning field: {"reasoning": {"effort": ...}}. OpenRouter translates that
# into whatever each backend actually speaks, so one key covers OpenAI,
# Anthropic, Gemini, Grok, Mistral and Qwen alike.
#
# THE LEVELS ARE NOT A SHARED LADDER. Each model publishes its own set, and a
# level outside that set is not portable: `xhigh`/`max` do not exist on
# Gemini, Grok or Mistral, `medium` does not exist on Kimi, and `minimal`
# exists ONLY on Gemini Flash and Qwen. Every value below was checked against
# that model's own published list — - so treat this table as per-model
# configuration, not as a dial you can sweep uniformly.
#
# Stating the level matters even where it matches the default, because the
# defaults disagree wildly: Kimi K3 defaults to `max`, Qwen to `xhigh`, the
# Anthropic trio to `high`, Grok 4.3 to `low`. Left unset, two models in the
# same drop-up behave nothing alike, and the expensive ones bill against the
# same max_tokens ceiling that has to hold the tool call.
#
# Reasoning cannot be switched off on Gemini, Grok 4.5 or Qwen — those are
# mandatory-reasoning models, so the lowest level they publish is the floor.
MODEL_MAPPINGS = {
    "gemini-3.1-pro": {
        "api_name": "google/gemini-3.1-pro-preview",
        "vision": True,
        "display_name": "Gemini 3.1 Pro Preview",
        # `low` is the floor: Pro publishes only high/medium/low, with no
        # `minimal` (that exists on Flash below, not here). Pinned to the
        # floor because Pro's thinking length is the most volatile in this
        # table — at `medium` a single trivial one-tool prompt was measured
        # spending 9,596 reasoning tokens against the 10,000 max_tokens
        # ceiling, on a step whose whole job was to emit one tool call.
        "effort": "low"
    },
    "gemini-3.6-flash": {
        "api_name": "google/gemini-3.6-flash",
        "vision": True,
        "display_name": "Gemini 3.6 Flash",
        # 3.6 Flash is the one Gemini that publishes `minimal`.
        "effort": "minimal"
    },
    "gemini-3.8-flash": {
        "api_name": "google/gemini-3.8-flash",
        "vision": True,
        "display_name": "Gemini 3.8 Flash",
        # 3.8 Flash publishes low/medium/high only - `minimal` returns an
        # error there - so `low` is its floor.
        "effort": "low"
    },
    # OpenAI is GPT-6 Astra at `medium`, the same entry the direct openai
    # provider carries, so switching a model between the two providers is a
    # routing change and not a capability change.
    "gpt-6-astra": {
        "api_name": "openai/gpt-6-astra",
        # Batches only on the Responses endpoint with async tools; the
        # provider routes it there (see ../responses.py).
        "async_tools": True,
        "vision": True,
        "display_name": "GPT-6 Astra",
        # Astra publishes low/medium/high/xhigh/max and rejects `none` and
        # `minimal`; one entry, at `medium`.
        "effort": "medium"
    },
    # Luna and Sol are OpenRouter only (not in the direct openai table). Both
    # behave like Astra: probed 2026-09-23 with the real driver, coder and
    # minion prompts, chat/completions gave the notes call alone turn after
    # turn at every effort, and the async Responses route batches them.
    "gpt-6-luna": {
        "api_name": "openai/gpt-6-luna",
        "async_tools": True,
        "vision": True,
        "display_name": "GPT-6 Luna",
        # Accepts every level from none to max; `low` matches the table.
        "effort": "low"
    },
    "gpt-6-sol": {
        "api_name": "openai/gpt-6-sol",
        "async_tools": True,
        "vision": True,
        "display_name": "GPT-6 Sol",
        # Accepts every level from none to max; `low` matches the table.
        "effort": "low"
    },
    "claude-opus-5": {
        "api_name": "anthropic/claude-opus-5",
        "vision": True,
        "display_name": "Claude Opus 5",
        "effort": "medium"
    },
    "claude-sonnet-5": {
        "api_name": "anthropic/claude-sonnet-5",
        "vision": True,
        "display_name": "Claude Sonnet 5",
        "effort": "low"
    },
    # Claude 5.5: Anthropic rejects forced tool use on both ("required" is a
    # 400 "tool_choice: type tool and any are not supported for this model",
    # probed 2026-10-01), so both carry `tool_choice`, sent in place of
    # "required" for them only.
    "claude-sonnet-5.5": {
        "api_name": "anthropic/claude-sonnet-5.5",
        "vision": True,
        "display_name": "Claude Sonnet 5.5",
        "effort": "low",
        "tool_choice": "auto"
    },
    "claude-opus-5.5": {
        "api_name": "anthropic/claude-opus-5.5",
        "vision": True,
        "display_name": "Claude Opus 5.5",
        "effort": "medium",
        "tool_choice": "auto"
    },
    "grok-4.7": {
        "api_name": "x-ai/grok-4.7",
        "vision": True,
        "display_name": "Grok 4.7",
        # Replaces 4.3 and 4.5 (2026-09-22). Probed with a forced tool call:
        # minimal, low and medium are all accepted and batch; `low` matches
        # the rest of the table.
        "effort": "low"
    },
    "mimo-v2.6-pro-ultraspeed": {
        "api_name": "xiaomi/mimo-v2.6-pro-ultraspeed",
        "vision": True,
        "display_name": "MiMo V2.6 Pro UltraSpeed",
        # Probed 2026-09-22: minimal and low both batch a forced tool call
        # in about a second; at `medium` the same probe came back with no
        # call at all, so `low` it is.
        "effort": "low"
    },
    "deepseek-v4.1-flash": {
        "api_name": "deepseek/deepseek-v4.1-flash",
        "vision": True,
        "display_name": "DeepSeek V4.1 Flash",
        # Probed 2026-09-22: every level is accepted and batches; the
        # backends (Together, CoreWeave) report zero reasoning tokens at
        # each, so the level is a formality here.
        "effort": "low"
    },
    "muse-spark-1.3": {
        "api_name": "meta/muse-spark-1.3",
        "vision": True,
        "display_name": "Muse Spark 1.3",
        # Needs the 18+ confirmation at openrouter.ai/settings/preferences
        # (done 2026-09-22). Meta's backend accepts only tool_choice "auto"
        # ("required ... not currently supported"), so like Qwen this entry
        # carries `tool_choice`, sent in place of "required" for it only.
        "effort": "low",
        "tool_choice": "auto"
    },
    "kimi-k3": {
        "api_name": "moonshotai/kimi-k3",
        "vision": True,
        "display_name": "Kimi K3",
        # Its ladder is max/high/low — there is no `medium` — and it defaults
        # to `max`, the highest default in the catalog. Setting `low` is the
        # single biggest saving in this table.
        "effort": "low"
    },
    "claude-opus-5-fast": {
        "api_name": "anthropic/claude-opus-5-fast",
        "vision": True,
        "display_name": "Claude Opus 5 Fast",
        "effort": "low"
    },
    # Still the newest Mistral on OpenRouter — the line has not moved since
    # 2026-04-30, so this entry is current, not stale.
    "mistral-medium-3.5": {
        "api_name": "mistralai/mistral-medium-3-5",
        "vision": True,
        "display_name": "Mistral Medium 3.5",
        # Mistral publishes only two levels, high and none, so `none` here
        # means reasoning genuinely off — not "lowest setting" as elsewhere.
        "effort": "none"
    },
    # Qwen: Alibaba's backend rejects tool_choice="required" whenever thinking
    # is on (400 "does not support being set to required or object in
    # thinking mode"), and this loop sends that on every call. Flash accepts
    # it only with thinking off and then emits one call per turn; Max cannot
    # switch thinking off at all. With tool_choice="auto" both batch at `low`
    # (probed 2026-09-22), so these two entries carry `tool_choice`, which
    # the provider sends in place of "required" for them ONLY; the loop's
    # no-tool-called nudge is the backstop for a prose-only turn.
    "qwen3.8-max-0902": {
        "api_name": "qwen/qwen3.8-max-0902",
        "vision": True,
        "display_name": "Qwen3.8 Max (0902)",
        "effort": "low",
        "tool_choice": "auto"
    },
    "qwen3.8-flash": {
        "api_name": "qwen/qwen3.8-flash",
        "vision": True,
        "display_name": "Qwen3.8 Flash",
        "effort": "low",
        "tool_choice": "auto"
    },
}

def get_model_info(short_name: str) -> dict:
    """Get full model information from short name"""
    if short_name in MODEL_MAPPINGS:
        return MODEL_MAPPINGS[short_name]
    # If not found, assume it's already a full model name
    return {
        "api_name": short_name,
        "vision": True,
        "display_name": short_name
    }


def get_reasoning_params(api_name: str, info: dict = None) -> dict:
    """OpenRouter's unified reasoning field for the model being called.

    The resolved table entry (`info`) wins when it carries an effort: two
    entries may share one api_name (one model at two levels), which a scan
    by api_name cannot tell apart. The api_name scan remains the fallback,
    matching the same helper in anthropic/view.py and openai/view.py.

    Returns {} for an entry with no level and for any hand-typed model name.
    That is the safe direction on this provider: the levels are per-model, so
    a guess is as likely to be rejected as honoured, and sending nothing
    leaves the model on the default OpenRouter already publishes for it.
    """
    # The resolved entry wins: two entries may share one api_name (one
    # model at two levels), which a scan by api_name cannot tell apart.
    if info and info.get("effort"):
        return {"reasoning": {"effort": info["effort"]}}
    for entry in MODEL_MAPPINGS.values():
        if entry["api_name"] == api_name and entry.get("effort"):
            return {"reasoning": {"effort": entry["effort"]}}
    return {}

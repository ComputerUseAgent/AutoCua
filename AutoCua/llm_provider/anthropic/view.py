# Model mappings for Anthropic provider
# Maps user-friendly names to actual API model names

# Sampling knobs for the models that still accept them — only Haiku now.
# `temperature` is never sent: the frontier models removed it (400
# "`temperature` is deprecated for this model"), and one knob steers better
# than two anyway. top_p does the real work — it holds generation to the
# confident mass, which is what keeps tool-call arguments and schema JSON
# well-formed. top_k is only a tail guard: it bites when the distribution is
# unusually flat and otherwise never binds.
SAMPLING_PARAMS = {"top_p": 0.5, "top_k": 40}

# Thinking, for the models that take it. ADAPTIVE means Claude decides per
# request whether to think and for how long, rather than being handed a fixed
# token budget; it is the only mode the 5-series accepts, and the old
# budget form (thinking={"type": "enabled", "budget_tokens": N}) is a 400
# there. `effort` is the dial on top: how deep that thinking may go and how
# much the model spends overall.
#
# Levels, cheapest to deepest: low | medium | high | xhigh | max. Opus 5 and
# Sonnet 5 both accept all five and both DEFAULT TO high, so omitting effort
# is not neutral — it is the second-most-expensive setting. Each entry states
# its level for that reason.
#
# `low` also keeps this loop honest about max_tokens: thinking tokens are
# billed against the same 4000-token ceiling as the visible answer and the
# tool-call JSON, and the loop runs tool_choice {"type": "any"}, so a turn
# that spends its budget thinking loses the tool call it owed.
ADAPTIVE_THINKING = {"type": "adaptive"}

MODEL_MAPPINGS = {
    # Haiku takes neither adaptive thinking nor `effort` — it predates both,
    # and each is a 400 here. It is the one model left that still accepts the
    # sampling knobs above, which is why the two features are mutually
    # exclusive across this table.
    "claude-haiku-4.5": {
        "api_name": "claude-haiku-4-5-20251001",
        "vision": True,
        "display_name": "Claude Haiku 4.5",
        "sampling": True
    },
    "claude-opus-5": {
        "api_name": "claude-opus-5",
        "vision": True,
        "display_name": "Claude Opus 5",
        # Opus 5 dropped all three sampling knobs — temperature, top_p and
        # top_k each return 400 "deprecated for this model". Steer it by
        # prompt and by effort.
        "sampling": False,
        "effort": "low"
    },
    "claude-sonnet-5": {
        "api_name": "claude-sonnet-5",
        "vision": True,
        "display_name": "Claude Sonnet 5",
        # Same as Opus 5: non-default temperature/top_p/top_k are rejected.
        "sampling": False,
        "effort": "low"
    },
    "claude-sonnet-5.5": {
        "api_name": "claude-sonnet-5-5",
        "vision": True,
        "display_name": "Claude Sonnet 5.5",
        "sampling": False,
        "effort": "low",
        # Sonnet 5.5 dropped forced tool use: tool_choice "any" (and "tool")
        # is a 400 "not supported for this model", so this entry is sent
        # "auto" instead. The loop's no-tool-called nudge covers a turn that
        # comes back without a call.
        "tool_choice": "auto"
    },
    "claude-opus-5.5": {
        "api_name": "claude-opus-5-5",
        "vision": True,
        "display_name": "Claude Opus 5.5",
        "sampling": False,
        "effort": "low",
        # Same as Sonnet 5.5: forced tool_choice is a 400, so "auto".
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


def get_sampling_params(api_name: str) -> dict:
    """Sampling knobs for the model send_request is about to call.

    Keyed by api_name — what the service actually receives — NOT the short
    name; the two differ for the Haiku entry above. Anything not marked
    sampling-capable returns {}, so neither Opus 5 / Sonnet 5 nor a
    hand-typed model name can be handed a parameter it would reject.
    """
    for info in MODEL_MAPPINGS.values():
        if info["api_name"] == api_name and info.get("sampling"):
            return dict(SAMPLING_PARAMS)
    return {}


def get_thinking_params(api_name: str) -> dict:
    """Adaptive-thinking + effort request fields for the model being called.

    Keyed by api_name for the same reason as get_sampling_params: it is what
    send_request receives, and what the emergency fallback swaps in mid-call,
    so the level follows the model actually on the wire.

    Returns {} for anything without an `effort` entry — Haiku, and any
    hand-typed name. That is the safe direction: a model that predates
    adaptive thinking rejects both fields, so sending nothing leaves it on
    its own default instead of 400-ing every attempt of every step.
    """
    for info in MODEL_MAPPINGS.values():
        if info["api_name"] == api_name and info.get("effort"):
            return {
                "thinking": dict(ADAPTIVE_THINKING),
                "output_config": {"effort": info["effort"]},
            }
    return {}


def get_tool_choice(api_name: str) -> dict:
    """tool_choice for the model being called: {"type": "any"} (every turn
    must call a tool) unless its entry sets `tool_choice`.

    Keyed by api_name like the two helpers above, so the coder's one-call
    fallback (Haiku 4.5 / Sonnet 5) keeps "any" while Sonnet 5.5 gets "auto".
    """
    for info in MODEL_MAPPINGS.values():
        if info["api_name"] == api_name and info.get("tool_choice"):
            return {"type": info["tool_choice"]}
    return {"type": "any"}
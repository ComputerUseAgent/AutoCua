# Model mappings for Google provider
# Maps user-friendly names to actual API model names

# `thinking_level` is Gemini 3's thinking control — a named level, not a token
# budget (thinking_budget is the older field and is not what these models
# want). The SDK carries it as types.ThinkingConfig(thinking_level=...), built
# in service.py so this table stays plain data; the value is coerced
# case-insensitively, so "low" here is the same as the enum's LOW.
#
# Every model is pinned to `low`, but it means a different thing on each,
# because their ladders and defaults differ:
#   gemini-4-argon          `low` accepted (probed 2026-11-04)
#   gemini-4-carbon         low/medium/high (no `minimal` - it errors,
#   gemini-4-hydrogen       probed 2026-11-14) -> floor
#   gemini-3.6-flash        minimal/low/medium/high  default medium  -> one
#                                                                      above
#                                                                      floor
#   gemini-3.8-flash        low/medium/high (no `minimal` - it errors) -> floor
# Gemini 4 Argon replaced Gemini 3.1 Pro Preview on 2026-11-04, here and in
# the openrouter and perplexity tables: probed with the real driver, coder and
# minion prompts, `low` batches the notes call with the actions.
#
# The `-vertex` entries are the SAME models reached through a different
# client — Vertex AI keeps the bare model ID rather than renaming it — so
# api_name is intentionally duplicated across each pair. Only `vertex`
# differs, which is what picks the client in service.py.
from .. import entry_value

MODEL_MAPPINGS = {
    "gemini-4-argon": {
        "api_name": "gemini-4-argon",
        "vision": True,
        "display_name": "Gemini 4 Argon",
        "vertex": False,
        "thinking_level": "low",
        "coder_thinking_level": "medium"
    },
    "gemini-4-carbon": {
        "api_name": "gemini-4-carbon",
        "vision": True,
        "display_name": "Gemini 4 Carbon",
        "vertex": False,
        "thinking_level": "low",
        "coder_thinking_level": "medium"
    },
    "gemini-4-hydrogen": {
        "api_name": "gemini-4-hydrogen",
        "vision": True,
        "display_name": "Gemini 4 Hydrogen",
        "vertex": False,
        "thinking_level": "low",
        "coder_thinking_level": "medium"
    },
    "gemini-3.6-flash": {
        "api_name": "gemini-3.6-flash",
        "vision": True,
        "display_name": "Gemini 3.6 Flash",
        "vertex": False,
        "thinking_level": "low",
        "coder_thinking_level": "medium"
    },
    "gemini-3.8-flash": {
        "api_name": "gemini-3.8-flash",
        "vision": True,
        "display_name": "Gemini 3.8 Flash",
        "vertex": False,
        "thinking_level": "low",
        "coder_thinking_level": "medium"
    },
    "gemini-4-argon-vertex": {
        "api_name": "gemini-4-argon",
        "vision": True,
        "display_name": "Gemini 4 Argon (Vertex)",
        "vertex": True,
        "thinking_level": "low",
        "coder_thinking_level": "medium"
    },
    "gemini-4-carbon-vertex": {
        "api_name": "gemini-4-carbon",
        "vision": True,
        "display_name": "Gemini 4 Carbon (Vertex)",
        "vertex": True,
        "thinking_level": "low",
        "coder_thinking_level": "medium"
    },
    "gemini-4-hydrogen-vertex": {
        "api_name": "gemini-4-hydrogen",
        "vision": True,
        "display_name": "Gemini 4 Hydrogen (Vertex)",
        "vertex": True,
        "thinking_level": "low",
        "coder_thinking_level": "medium"
    },
    "gemini-3.6-flash-vertex": {
        "api_name": "gemini-3.6-flash",
        "vision": True,
        "display_name": "Gemini 3.6 Flash (Vertex)",
        "vertex": True,
        "thinking_level": "low",
        "coder_thinking_level": "medium"
    },
    "gemini-3.8-flash-vertex": {
        "api_name": "gemini-3.8-flash",
        "vision": True,
        "display_name": "Gemini 3.8 Flash (Vertex)",
        "vertex": True,
        "thinking_level": "low",
        "coder_thinking_level": "medium"
    }
}

def get_model_info(short_name: str) -> dict:
    """Get full model information from short name"""
    if short_name in MODEL_MAPPINGS:
        return MODEL_MAPPINGS[short_name]
    return {
        "api_name": short_name,
        "vision": True,
        "display_name": short_name
    }


def get_thinking_level(api_name: str, cli_agent: bool = False):
    """`thinking_level` for the model send_request is about to call, or None.

    Keyed by api_name, matching the same helper in the other providers.

    NOTE the difference from those providers: api_name is NOT unique here.
    Each model appears twice, once as itself and once as its `-vertex` twin,
    since Vertex AI serves the same model ID through a different client. That
    is safe only while both halves of a pair carry the same level, which they
    do — first match wins. If a pair ever needs to differ between AI Studio
    and Vertex, this has to key on the short name instead.

    None for an unregistered model, so a hand-typed name keeps Google's own
    default rather than being handed a level its ladder may not include —
    `minimal`, for instance, does not exist on Pro.
    """
    for info in MODEL_MAPPINGS.values():
        if info["api_name"] == api_name and info.get("thinking_level"):
            return entry_value(info, "thinking_level", cli_agent)
    return None
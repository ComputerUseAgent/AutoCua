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
#   gemini-3.1-pro-preview  low/medium/high          default high    -> floor
#   gemini-3.6-flash        minimal/low/medium/high  default medium  -> one
#                                                                      above
#                                                                      floor
#   gemini-3.8-flash        low/medium/high (no `minimal` - it errors) -> floor
# Neither older default was cheap: Pro would otherwise run at `high`, the top
# of its range, on every step.
#
# The `-vertex` entries are the SAME models reached through a different
# client — Vertex AI keeps the bare model ID rather than renaming it — so
# api_name is intentionally duplicated across each pair. Only `vertex`
# differs, which is what picks the client in service.py.
MODEL_MAPPINGS = {
    "gemini-3.1-pro": {
        "api_name": "gemini-3.1-pro-preview",
        "vision": True,
        "display_name": "Gemini 3.1 Pro",
        "vertex": False,
        "thinking_level": "low"
    },
    "gemini-3.6-flash": {
        "api_name": "gemini-3.6-flash",
        "vision": True,
        "display_name": "Gemini 3.6 Flash",
        "vertex": False,
        "thinking_level": "low"
    },
    "gemini-3.8-flash": {
        "api_name": "gemini-3.8-flash",
        "vision": True,
        "display_name": "Gemini 3.8 Flash",
        "vertex": False,
        "thinking_level": "low"
    },
    "gemini-3.1-pro-vertex": {
        "api_name": "gemini-3.1-pro-preview",
        "vision": True,
        "display_name": "Gemini 3.1 Pro (Vertex)",
        "vertex": True,
        "thinking_level": "low"
    },
    "gemini-3.6-flash-vertex": {
        "api_name": "gemini-3.6-flash",
        "vision": True,
        "display_name": "Gemini 3.6 Flash (Vertex)",
        "vertex": True,
        "thinking_level": "low"
    },
    "gemini-3.8-flash-vertex": {
        "api_name": "gemini-3.8-flash",
        "vision": True,
        "display_name": "Gemini 3.8 Flash (Vertex)",
        "vertex": True,
        "thinking_level": "low"
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


def get_thinking_level(api_name: str):
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
            return info["thinking_level"]
    return None
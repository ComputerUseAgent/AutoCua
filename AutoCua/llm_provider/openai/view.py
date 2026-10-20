# Model mappings for OpenAI provider
# Maps user-friendly names to actual API model names

# GPT-6 Astra is a REASONING model, so `reasoning_effort` is the only real handle
# on how hard the model works. The sampling knobs are gone: temperature, top_p
# and n are locked at 1 and the penalties at 0 — sending any of them returns
# 400 "Unsupported parameter: '<name>' is not supported with this model" — and
# `seed` is not accepted either, so there is no reproducibility lever to reach
# for on this family. send_request therefore sends effort and nothing else.
#
# Levels, cheapest to deepest: none | low | medium | high | xhigh | max.
# Omitting the parameter already yields `medium`; every entry states its level
# anyway so the choice is readable here instead of inherited from the API.
#
# One ceiling to respect when raising a level: max_completion_tokens counts
# REASONING tokens as well as the visible answer and the tool-call JSON. Since
# the loop runs tool_choice="required", a turn that spends its whole budget
# thinking loses the tool call it was supposed to emit.
MODEL_MAPPINGS = {
    "gpt-6-astra": {
        "api_name": "gpt-6-astra",
        # Batches only on the Responses endpoint with async tools; the
        # provider routes it there (see ../responses.py).
        "async_tools": True,
        "vision": True,
        "display_name": "GPT-6 Astra",
        "json_mode": True,
        # Astra publishes low/medium/high/xhigh/max and rejects `none` and
        # `minimal`; one entry, at `medium`. /v1/chat/completions refuses
        # function tools on Astra at every level ("Function tools with
        # reasoning_effort are not supported ... use /v1/responses or set
        # reasoning_effort to 'none'", verified 2026-09-21), so with tools
        # the provider drives it on /v1/responses (`async_tools` above).
        "reasoning_effort": "medium"
    }
}

def get_model_info(short_name: str) -> dict:
    """Get full model information from short name"""
    if short_name in MODEL_MAPPINGS:
        return MODEL_MAPPINGS[short_name]
    # If not found, assume it's already a full model name
    return {
        "api_name": short_name,
        "vision": True,
        "display_name": short_name,
        "json_mode": True  # Default to supporting JSON mode for OpenAI
    }


def get_reasoning_effort(api_name: str, info: dict = None) -> dict:
    """`reasoning_effort` kwarg for the model send_request is about to call.

    The resolved table entry (`info`) wins when it carries a level: two
    entries may share one api_name (one model at two levels), which a scan
    by api_name cannot tell apart. The api_name scan remains the fallback,
    matching anthropic/view.py's get_sampling_params.

    An unregistered model returns {} rather than a guess: a hand-typed name
    may be a non-reasoning model, which rejects `reasoning_effort` outright.
    Sending nothing leaves OpenAI's own default in place.
    """
    # The resolved entry wins: two entries may share one api_name (one
    # model at two levels), which a scan by api_name cannot tell apart.
    if info and info.get("reasoning_effort"):
        return {"reasoning_effort": info["reasoning_effort"]}
    for entry in MODEL_MAPPINGS.values():
        if entry["api_name"] == api_name and entry.get("reasoning_effort"):
            return {"reasoning_effort": entry["reasoning_effort"]}
    return {}
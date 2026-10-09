# Model mappings for Groq provider
# Maps user-friendly names to actual API model names

# One model only: Qwen3.8-27B, which replaced Qwen3.6-27B on 2026-09-15. The
# loop needs tool use AND vision in one model, because a driver step hands the
# model a screenshot; Qwen3.6-27B was the only Groq model with both ("Tool Use,
# JSON Object Mode, Reasoning, Vision") and 3.8 is its upgrade. gpt-oss-120b was
# dropped earlier because it is text-only.
#
# Reasoning is deliberately left unset: the model has thinking and
# non-thinking modes, and with no reasoning field on the request it keeps
# Groq's own default. Temperature is handled once in service.py (0.2).
#
# Caveat worth keeping in view: Groq classes this as a PREVIEW model —
# "intended for evaluation purposes only... may be discontinued at short
# notice" — and Groq has retired preview models before (kimi-k2-instruct in
# March 2026, qwen3-32b in June). With one entry there is nothing left to fall
# back to on this provider if that happens.
MODEL_MAPPINGS = {
    "qwen3.8-27b": {
        "api_name": "qwen/qwen3.8-27b",
        "vision": True,
        "display_name": "Qwen3.8 27B"
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
        "display_name": short_name
    }
# Model mappings for Together AI provider
# Maps user-friendly names to actual API model names

# Three models, all image+text in / text out, all with native (OpenAI-format)
# tool calling — the two things this loop needs on every step.
#
# Reasoning is deliberately left unset. Together exposes `reasoning_effort`
# (low|medium|high) — Inkling's "controllable inference effort" knob — but
# with no field on the request each model keeps Together's own default, the
# same choice groq/ makes. Temperature is handled once in service.py (0.2).
MODEL_MAPPINGS = {
    "inkling": {
        "api_name": "thinkingmachines/Inkling",
        "vision": True,
        "display_name": "Inkling"
    },
    "muse-glimmer-30b": {
        "api_name": "meta-models/Muse-Glimmer-30B",
        "vision": True,
        "display_name": "Muse Glimmer 30B"
    },
    "minimax-m3": {
        "api_name": "MiniMaxAI/MiniMax-M3",
        "vision": True,
        "display_name": "MiniMax M3"
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

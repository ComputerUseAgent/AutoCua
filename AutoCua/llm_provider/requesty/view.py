# Model mappings for Requesty provider
# Maps user-friendly names to actual API model names

# Requesty is an OpenAI-compatible router: one key, `vendor/model` ids, the
# same chat/completions shape for every backend. All entries below are
# image+text in / text out with native (OpenAI-format) tool calling, checked
# against https://router.requesty.ai/v1/models. Any other id from that list
# can be typed as the model name and is sent verbatim.
#
# Reasoning is deliberately left unset, the same choice together/ and groq/
# make: with no field on the request each model keeps its own default.
# Temperature is handled once in service.py (0.2).
MODEL_MAPPINGS = {
    "claude-sonnet-5": {
        "api_name": "anthropic/claude-sonnet-5",
        "vision": True,
        "display_name": "Claude Sonnet 5"
    },
    "gemini-3.6-flash": {
        "api_name": "google/gemini-3.6-flash",
        "vision": True,
        "display_name": "Gemini 3.6 Flash"
    },
    "gpt-5.4": {
        "api_name": "openai/gpt-5.4",
        "vision": True,
        "display_name": "GPT-5.4"
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

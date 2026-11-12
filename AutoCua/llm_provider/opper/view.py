# Model mappings for Opper provider
# Maps user-friendly names to actual API model names

# Opper is a European AI gateway with an OpenAI-compatible API: one key, the
# same chat/completions shape for every backend. A bare model name routes
# across every provider that serves it; a `provider/model` id
# (`aws/claude-sonnet-5`) pins one route. All entries below are image+text in /
# text out with native (OpenAI-format) tool calling, checked against
# https://api.opper.ai/v3/models. Any other id from that list can be typed as
# the model name and is sent verbatim.
#
# `effort` goes out as `reasoning_effort`. An entry without it keeps the
# model's own default. `tool_choice`, where set, replaces "required" for that
# model only, as in openrouter/view.py. Temperature (0.2) is set once in
# service.py.
MODEL_MAPPINGS = {
    "claude-sonnet-5": {
        "api_name": "claude-sonnet-5",
        "vision": True,
        "display_name": "Claude Sonnet 5",
        "effort": "low",
        "coder_effort": "medium",
        "coder_tool_choice": "auto"
    },
    "gemini-3.6-flash": {
        "api_name": "gemini-3.6-flash",
        "vision": True,
        "display_name": "Gemini 3.6 Flash"
    },
    "gpt-5.4": {
        "api_name": "gpt-5.4",
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

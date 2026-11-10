# Model mappings for LiteLLM provider
# Maps user-friendly names to actual API model names

# LiteLLM is an SDK for 100+ providers behind one OpenAI-format call: the
# model id is `<provider>/<model>` (anthropic/, gemini/, vertex_ai/,
# bedrock/, azure/, groq/, mistral/, ...) or `litellm_proxy/<alias>` for a
# model served by a LiteLLM proxy (LITELLM_API_BASE). Any such id can be typed
# as the model name and is sent verbatim; the entries below are shortcuts for
# image+text in / text out models with native tool calling.
#
# `effort` goes out as `reasoning_effort`, which LiteLLM maps onto each
# backend (an effort for OpenAI, a thinking budget for Anthropic and Gemini).
# An entry without it keeps the model's own default. `tool_choice`, where set,
# replaces "required" for that model only, as in openrouter/view.py.
# Temperature (0.2) and drop_params are set once in service.py.
MODEL_MAPPINGS = {
    "claude-sonnet-5": {
        "api_name": "anthropic/claude-sonnet-5",
        "vision": True,
        "display_name": "Claude Sonnet 5",
        "effort": "low",
        "coder_effort": "medium",
        "coder_tool_choice": "auto"
    },
    "gemini-3.6-flash": {
        "api_name": "gemini/gemini-3.6-flash",
        "vision": True,
        "display_name": "Gemini 3.6 Flash"
    },
    "gpt-5.4": {
        "api_name": "openai/gpt-5.4",
        "vision": True,
        "display_name": "GPT-5.4"
    }
}

# The key AutoCua already holds (.env or the app's Settings) for each upstream
# LiteLLM routes to by model prefix, so a direct route needs no second copy of
# it. LiteLLM itself reads GEMINI_API_KEY / TOGETHERAI_API_KEY / ... from the
# environment; these are AutoCua's own names for the same keys.
UPSTREAM_KEYS = {
    "anthropic": "ANTHROPIC_API_KEY",
    "openai": "OPENAI_API_KEY",
    "gemini": "GOOGLE_API_KEY",
    "groq": "GROQ_API_KEY",
    "openrouter": "OPENROUTER_API_KEY",
    "perplexity": "PERPLEXITY_API_KEY",
    "together_ai": "TOGETHER_API_KEY",
    "cerebras": "CEREBRAS_API_KEY",
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

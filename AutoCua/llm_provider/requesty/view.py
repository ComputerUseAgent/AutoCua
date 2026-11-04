# Model mappings for Requesty provider
# Maps user-friendly names to actual API model names

# Requesty is an OpenAI-compatible router: one key, `vendor/model` ids, the
# same chat/completions shape for every backend. All entries below are
# image+text in / text out with native (OpenAI-format) tool calling, checked
# against https://router.requesty.ai/v1/models. Any other id from that list
# can be typed as the model name and is sent verbatim.
#
# `effort` goes out as Requesty's `reasoning_effort`, which it maps onto each
# backend (an effort for OpenAI, a thinking budget for Anthropic and Vertex).
# An entry without it keeps the model's own default. `tool_choice`, where set,
# replaces "required" for that model only, as in openrouter/view.py.
# Temperature (0.2) and prompt caching (requesty.auto_cache) are set once in
# service.py.
#
# Probed 2026-11-04 with the real driver, coder and minion prompts, a step and
# its tool results sent back, and left out:
# - openai/gpt-6.1-terra, gpt-6.1-sol and gpt-6-luna call the notes tool alone
#   turn after turn here, on chat/completions and on /v1/responses alike:
#   Requesty drops the async tool flags that make them batch on OpenRouter.
# - moonshot/kimi-k3.1-pro (temperature 1 only) and meta/muse-spark-1.3
#   (tool_choice "auto" only) mostly called the notes tool alone, the coder
#   at times for three turns running.
# - muse-spark-open (fireworks/, together/) never acted, or timed out.
MODEL_MAPPINGS = {
    "claude-sonnet-5": {
        "api_name": "anthropic/claude-sonnet-5",
        "vision": True,
        "display_name": "Claude Sonnet 5",
        "effort": "low"
    },
    # Claude 5.5 rejects forced tool use (see openrouter/view.py), so both
    # carry tool_choice "auto".
    "claude-sonnet-5.5": {
        "api_name": "anthropic/claude-sonnet-5-5",
        "vision": True,
        "display_name": "Claude Sonnet 5.5",
        "effort": "low",
        "tool_choice": "auto"
    },
    "claude-opus-5": {
        "api_name": "anthropic/claude-opus-5",
        "vision": True,
        "display_name": "Claude Opus 5",
        "effort": "medium"
    },
    "claude-opus-5.5": {
        "api_name": "anthropic/claude-opus-5-5",
        "vision": True,
        "display_name": "Claude Opus 5.5",
        "effort": "medium",
        "tool_choice": "auto"
    },
    "gemini-3.6-flash": {
        "api_name": "google/gemini-3.6-flash",
        "vision": True,
        "display_name": "Gemini 3.6 Flash"
    },
    # Requesty serves Argon through Vertex only.
    "gemini-4-argon": {
        "api_name": "vertex/gemini-4-argon",
        "vision": True,
        "display_name": "Gemini 4 Argon",
        "effort": "low"
    },
    "gpt-5.4": {
        "api_name": "openai/gpt-5.4",
        "vision": True,
        "display_name": "GPT-5.4"
    },
    "grok-4.7": {
        "api_name": "xai/grok-4.7",
        "vision": True,
        "display_name": "Grok 4.7",
        "effort": "low"
    },
    # Particle AI's own model. It thinks at `high` unless told otherwise.
    "particle-1": {
        "api_name": "particle/particle-1",
        "vision": True,
        "display_name": "Particle-1",
        "effort": "low"
    },
    # MiniMax's own route (minimaxi/): with "required" it acts in the first
    # turn; "auto" gave the notes call first.
    "minimax-m3.1-flash": {
        "api_name": "minimaxi/minimax-m3.1-flash",
        "vision": True,
        "display_name": "MiniMax M3.1 Flash",
        "effort": "low"
    },
    # DeepSeek's own route. Its thinking mode refuses a forced tool call (400
    # "Thinking mode does not support this tool_choice") and thinking is on
    # unless asked off, so `none` turns it off and keeps "required".
    "deepseek-v4.1-flash-1017": {
        "api_name": "deepseek/deepseek-v4.1-flash-1017",
        "vision": True,
        "display_name": "DeepSeek V4.1 Flash 1017",
        "effort": "none"
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

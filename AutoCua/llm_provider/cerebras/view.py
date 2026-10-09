# Model mappings for Cerebras provider
# Maps user-friendly names to actual API model names

# One model: Qwen 3.8 27B, image+text in / text out with native (OpenAI-format)
# tool calling — the two things this loop needs on every step.
#
# Sampling and reasoning are set once in service.py, not per model:
# temperature 1, top_p 0.95, seed 42, max_completion_tokens 32768, and
# reasoning_effort "high" for the coder and its minions, "low" for every other
# agent.
MODEL_MAPPINGS = {
    "qwen-3.8-27b": {
        "api_name": "qwen-3.8-27b",
        "vision": True,
        "display_name": "Qwen 3.8 27B"
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

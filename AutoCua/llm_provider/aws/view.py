# Model mappings for the AWS provider (Amazon Bedrock)
# Maps user-friendly names to actual API model names

# Every api_name is a `global.` cross-Region inference profile: the call goes
# to Bedrock's Converse API through one regional endpoint (AWS_BEDROCK_REGION
# in .env, DEFAULT_REGION when unset) and AWS serves it from whichever region
# has capacity.
#
# Request settings, probed against Bedrock in eu-west-2 on 2026-10-05:
#   effort            Claude: output_config.effort. Adaptive thinking is NOT
#                     sent: with it, Opus 4.7 answered a tool_choice "auto"
#                     request with an empty turn 3 times in 3; without it,
#                     every call worked.
#   reasoning_config  Kimi K3 and Grok 4.7: their reasoning level. Kimi's
#                     default is `max` and Grok's is `high`, so leaving it out
#                     is the expensive choice.
#   cache             Claude: cachePoint blocks after the system prompt and on
#                     the turn the agent loop marks with cache_control. Kimi K3
#                     rejects cachePoint with a 400 (it caches on its own) and
#                     Grok 4.7 with a 403.
#   max_tokens        Output cap: 10000 on every model, far under what Bedrock
#                     allows (Opus 4.7 and 4.6: 128K, Sonnet 4.6: 64K, per
#                     the AWS model cards on 2026-10-06; Kimi K3 and Grok 4.7
#                     state no lower limit). Kimi K3 and Grok 4.7 always
#                     reason, and their reasoning tokens count against it,
#                     as on openrouter/.
# No sampling knob is sent to any of them: Kimi K3 and Grok 4.7 reject
# temperature, and Opus 4.7 rejects top_k.

DEFAULT_REGION = "eu-west-2"

# Output cap for a hand-typed model ID, which has no table entry.
DEFAULT_MAX_TOKENS = 10000

MODEL_MAPPINGS = {
    "kimi-k3": {
        "api_name": "global.moonshotai.kimi-k3",
        "vision": True,
        "display_name": "Kimi K3",
        "reasoning_config": "low",
        "max_tokens": 10000
    },
    "grok-4.7": {
        "api_name": "global.xai.grok-4.7",
        "vision": True,
        "display_name": "Grok 4.7",
        "reasoning_config": "low",
        "max_tokens": 10000
    },
    "claude-opus-4.7": {
        "api_name": "global.anthropic.claude-opus-4-7",
        "vision": True,
        "display_name": "Claude Opus 4.7",
        "effort": "low",
        "cache": True,
        "max_tokens": 10000
    },
    "claude-opus-4.6": {
        "api_name": "global.anthropic.claude-opus-4-6-v1",
        "vision": True,
        "display_name": "Claude Opus 4.6",
        "effort": "low",
        "cache": True,
        "max_tokens": 10000
    },
    "claude-sonnet-4.6": {
        "api_name": "global.anthropic.claude-sonnet-4-6",
        "vision": True,
        "display_name": "Claude Sonnet 4.6",
        "effort": "low",
        "cache": True,
        "max_tokens": 10000
    }
}

def get_model_info(short_name: str) -> dict:
    """Get full model information from short name"""
    if short_name in MODEL_MAPPINGS:
        return MODEL_MAPPINGS[short_name]
    # If not found, assume it's already a Bedrock model or profile ID
    return {
        "api_name": short_name,
        "vision": True,
        "display_name": short_name
    }


def find_entry(api_name: str) -> dict:
    """The table entry whose api_name is `api_name`, or {} for a hand-typed ID."""
    for info in MODEL_MAPPINGS.values():
        if info["api_name"] == api_name:
            return info
    return {}


def get_request_fields(info: dict) -> dict:
    """additionalModelRequestFields for the entry being called: Claude's
    effort or Kimi/Grok's reasoning level. {} for a hand-typed ID, which then
    keeps the model's own default rather than a field it may reject."""
    if info.get("effort"):
        return {"output_config": {"effort": info["effort"]}}
    if info.get("reasoning_config"):
        return {"reasoning_config": info["reasoning_config"]}
    return {}

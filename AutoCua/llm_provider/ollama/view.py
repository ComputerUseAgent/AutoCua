# Model mappings for the Ollama provider
# Maps user-friendly names to the model names Ollama serves

# Ollama runs models on this machine (or wherever OLLAMA_HOST points), so the
# list is whatever the person has pulled: any name `ollama list` shows can be
# typed as the model and is sent verbatim. The agents need native
# (OpenAI-format) tool calling, and vision wherever they send screenshots
# (computer use, browser, mobile); `ollama show <model>` lists both under
# Capabilities, as "tools" and "vision".
#
# `effort` goes out as reasoning_effort, which Ollama maps onto the model's
# own thinking levels. A level the model does not have is ignored without an
# error (it thinks at its default), so each entry names levels the model
# lists. An entry without one, like any unlisted model, keeps the model's
# default. Sampling is the model's own (its Modelfile), so service.py sends
# none.
#
# Every agent but the coder runs with thinking off ("none"): on a model on
# this machine each step's thinking is the wait, and the driver plans in its
# notes tool anyway. The coder thinks at "medium", one level up, as it works
# through code.
#
# Probed 2026-11-08/09 on Ollama 0.40.15 (Apple M5 Pro, 64 GB) with the real
# computer-use prompt and tools (step times below: a warm step, fast /
# quality mode), then end to end: a coder task (write and run a script) and
# a browser task in headless Chrome (read a page's heading):
# - qwen3.8:27b: dense 27B, MLX, 18 GB. Thinking levels false/low/medium/
#   xhigh (default medium); `high` is not one of them and thinks at the
#   default. 17 s / 27 s a step; acted on the first step every time; both
#   tasks done (76 s, 87 s). The most reliable for computer use.
# - qwen3.6:35b-a3b-nvfp4: mixture of experts, 36B total and about 3B active
#   per token, MLX, 23 GB. Thinking on or off only, on by default ("none"
#   turns it off, any other level on). 4 s / 6 s a step, about four times
#   as fast; planned before acting on the first step; both tasks done
#   (22 s, 25 s). The fastest that does the work.
# - ornith-1.5:35b: mixture of experts on the same architecture (35.5B total,
#   about 3B active), trained further for agents; Ollama ships it as a
#   Q4_K_M GGUF only (22 GB, 27 GB loaded), so it runs on the llama.cpp
#   engine, as fast here as qwen3.6 on MLX. Thinking on or off. 4 s / 6 s a
#   step; acted on the first step in both modes; both tasks done (33 s,
#   29 s). Fast and decisive: the one for computer use.
#
# Left out, from the same probe: qwen3.5:35b-a3b-nvfp4 (as fast, but sent
# malformed tool calls and failed the coder task), gemma4:26b-nvfp4 (a
# mixture of experts as fast, but only ever wrote notes in the screen
# agents: 100 browser steps without one action), nemotron-3.5-bolt:e4b-nvfp4
# (fastest, 3 s a step, but no tool calls in fast mode, the coder's sent as
# text, and a 65536-token window), mimo-v2.6:9b-nvfp4 (failed both tasks)
# and gemma4:31b-mlx (dense, slowest at 23 s / 36 s a step, the coder's
# tool calls sent as text). In fast mode, most of them answered without any
# tool call, so quality mode suits a local model better.
MODEL_MAPPINGS = {
    "qwen3.8:27b": {
        "api_name": "qwen3.8:27b",
        "vision": True,
        "display_name": "Qwen3.8 27B (Ollama)",
        "effort": "none",
        "coder_effort": "medium"
    },
    "qwen3.6:35b-a3b-nvfp4": {
        "api_name": "qwen3.6:35b-a3b-nvfp4",
        "vision": True,
        "display_name": "Qwen3.6 35B-A3B (Ollama)",
        "effort": "none",
        "coder_effort": "medium"
    },
    "ornith-1.5:35b": {
        "api_name": "ornith-1.5:35b",
        "vision": True,
        "display_name": "Ornith 1.5 35B-A3B (Ollama)",
        "effort": "none",
        "coder_effort": "medium"
    }
}

def get_model_info(short_name: str) -> dict:
    """Get full model information from short name"""
    if short_name in MODEL_MAPPINGS:
        return MODEL_MAPPINGS[short_name]
    # Any other model the person has pulled: sent verbatim, thinking at its
    # own default.
    return {
        "api_name": short_name,
        "vision": True,
        "display_name": short_name
    }


def listed_models() -> dict:
    """The models the app's and Telegram's pickers offer: the table above,
    then every other model this Ollama has pulled that can call tools, a
    model without vision marked so (it suits the coder only). Asked of
    Ollama on each call: Telegram asks on every pick, the app when it opens.
    With Ollama not answering, the table alone."""
    listed = dict(MODEL_MAPPINGS)
    try:
        from .service import pulled_models
        for name, caps in pulled_models():
            if name in listed:
                continue
            info = dict(get_model_info(name))
            if caps is not None and "vision" not in caps:
                info["vision"] = False
                info["display_name"] = f"{name} (no vision)"
            listed[name] = info
    except Exception:
        pass
    return listed

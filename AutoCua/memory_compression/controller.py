"""CompressionController — runtime orchestration for the handoff compression
agent (sibling agent/service.py builds the dump and makes the LLM call).

The compression agent is its OWN agent: everything about WHEN it fires, HOW
the worker runs, and HOW the result is spliced back lives here, in ONE place
shared by all platform agents (mac / ios / windows). A platform
agent wires it with four hooks, all called from its main loop:

    reset()          — at the top of process_request (invalidates stale workers)
    maybe_trigger()  — right after each main-agent LLM call (last_usage fresh)
    apply_pending()  — at the top of each loop iteration (splices IN PLACE)
    finish_run()     — after the loop exits (stops a dangling indicator)

The CODER agent reuses this same controller with two constructor callables
(dump_builder / synthetic_entry) that swap the main-agent history format for
its native tool-calling one; trigger/threading/splice orchestration is shared.

Threading contract (unchanged from the original in-agent version): the dump is
snapshotted on the CALLER's thread; the daemon worker makes one plain-text LLM
call and only deposits {gen, k, text}; every list mutation happens inside
apply_pending, i.e. on the main loop's thread. A generation counter makes
stale results (from a previous run on the same instance) unappliable.
"""

import os
import threading

from .agent.service import MemoryCompressionAgent, make_synthetic_entry

# Rolling-compression trigger: fire when the agent's live context size
# (incl. cached tokens) crosses this. memory_tracker.MEMORY_CAP is the bar's
# display budget; this is the point compression starts keeping context down.
# AutoCua_COMPRESS_THRESHOLD overrides it (test knob) — subprocess agents (the
# coder) inherit the env automatically, so one setting covers every agent.
try:
    COMPRESS_THRESHOLD = int(os.getenv("AutoCua_COMPRESS_THRESHOLD", "") or 110_000)
except (TypeError, ValueError):
    COMPRESS_THRESHOLD = 110_000


class CompressionController:

    def __init__(self, llm_manager, llm_manager_cls, token_callback, stop_event,
                 dump_builder=None, synthetic_entry=None):
        self.llm_manager = llm_manager          # owning agent's manager — read last_usage ONLY
        self.llm_manager_cls = llm_manager_cls  # platform's LLMManager class for the 2nd manager
        self.token_callback = token_callback    # frontend token pipe (indicator rides on it)
        self.stop_event = stop_event
        # Agent-flavor hooks (None = main-agent defaults): dump_builder renders
        # steps [0..k] into the compressor's <input> text; synthetic_entry maps
        # (step_k_entry, handoff_text) -> (entry, tool_slot) for the splice.
        # The coder passes native-format implementations from its view module.
        self._dump_builder = dump_builder
        self._synthetic_entry = synthetic_entry
        self._threshold = COMPRESS_THRESHOLD
        self._inflight = False    # one compression in flight, ever
        self._result = None       # {"gen","k","text"} deposited by the worker
        self._gen = 0             # generation guard: stale results never apply
        self._rearm_len = 0       # no re-trigger until this many entries exist
        self._compressor = None   # lazy MemoryCompressionAgent (own 2nd LLMManager)

    def reset(self):
        """New process_request: invalidate any stale worker from a prior run."""
        self._gen += 1
        self._inflight = False
        self._result = None
        self._rearm_len = 0

    def maybe_trigger(self, assistant_messages, tool_responses, task):
        """Fire-and-forget: spawn ONE background handoff compression when the
        context crosses the threshold. The dump is snapshotted HERE on the main
        thread; the worker never reads the live lists."""
        try:
            ctx = int((self.llm_manager.last_usage or {}).get("context_tokens", 0) or 0)
        except Exception:
            return
        if ctx < self._threshold:
            return
        if self._inflight or self._result is not None:
            return
        if len(assistant_messages) < 2:                 # nothing worth compressing
            return
        if len(assistant_messages) < self._rearm_len:   # anti-thrash guard
            return
        if self._compressor is None:
            try:
                main = self.llm_manager
                # A SECOND manager — same provider/model/key, plain-text mode.
                # Never reuse the main one: its last_usage would race the loop.
                second = self.llm_manager_cls(main.provider, main.model_short_name,
                                              api_key=main.runtime_api_key, mode="text")
                self._compressor = MemoryCompressionAgent(second)
            except Exception as e:
                print(f"🧠 [memory] Compression unavailable: {e}")
                self._rearm_len = len(assistant_messages) + 5
                return
        k = len(assistant_messages) - 1                 # last completed step
        dump = (self._dump_builder or self._compressor.build_dump)(
            assistant_messages, tool_responses, k, task)
        self._inflight = True
        self._notify("start")                           # indicator on
        print(f"🧠 [memory] Context at {ctx:,} tokens — compressing steps 1..{k + 1} in background")
        threading.Thread(target=self._run, args=(dump, k, self._gen), daemon=True).start()

    def _run(self, dump, k, gen):
        """Worker thread: one plain-text LLM call, deposit only. NEVER touches
        the conversation lists — apply_pending splices on the main loop."""
        try:
            if self.stop_event and self.stop_event.is_set():
                return                                  # run ending — skip the call
            text = self._compressor.compress(dump)
            if (text and text.strip()
                    and gen == self._gen
                    and not (self.stop_event and self.stop_event.is_set())):
                self._result = {"gen": gen, "k": k, "text": text.strip()}
        except Exception as e:
            print(f"🧠 [memory] Compression failed, dropping (will re-trigger): {e}")
            self._rearm_len = k + 4                     # back off ~3 steps
        finally:
            self._inflight = False
            # Indicator off the moment the LLM call resolves (any path) — the
            # splice is guaranteed at the caller's next loop top.
            self._notify("end")

    def apply_pending(self, assistant_messages, tool_responses, web_memory_index):
        """MAIN-LOOP ONLY: splice a deposited handoff into the live lists.

        Replaces entries [0..k] with ONE synthetic entry (step k's JSON with
        only its memory field → the <handoff> doc) and keeps step k's tool
        slot; mutates both lists IN PLACE. Returns web_memory_index shifted
        past the splice (None if the slot was inside the compressed span).
        A stale or absent result is a no-op returning web_memory_index as-is."""
        res = self._result
        if res is None:
            return web_memory_index
        self._result = None
        if res.get("gen") != self._gen or not res.get("text"):
            return web_memory_index
        k = res["k"]
        if not (0 <= k < len(assistant_messages)):
            return web_memory_index
        if self._synthetic_entry is not None:
            entry, slot = self._synthetic_entry(assistant_messages[k], res["text"])
        else:
            entry = make_synthetic_entry(assistant_messages[k], res["text"])
            slot = tool_responses[k] if k < len(tool_responses) else None
        assistant_messages[:k + 1] = [entry]
        tool_responses[:k + 1] = [slot]
        if web_memory_index is not None:
            web_memory_index = None if web_memory_index <= k else web_memory_index - k
        # Anti-thrash: no re-trigger until 3 fresh steps exist.
        self._rearm_len = len(assistant_messages) + 3
        print(f"🧠 [memory] Compressed steps 1..{k + 1} into a handoff "
              f"({len(assistant_messages)} entries remain)")
        return web_memory_index

    def finish_run(self):
        """Run over with a compression still pending (in flight or deposited
        but never applied): the result dies at the next reset(); make sure the
        indicator isn't left on."""
        if self._inflight or self._result is not None:
            self._notify("end")

    def _notify(self, state):
        """Frontend indicator: {"memory_compression": "start"|"end"} rides the
        existing token pipe — no new callback wiring."""
        if self.token_callback:
            try:
                self.token_callback({"memory_compression": state})
            except Exception:
                pass

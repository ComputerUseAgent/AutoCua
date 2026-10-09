import json
import re


class MinionResponseFormatter:
    """Validates and normalizes minion JSON responses before they enter agent memory.

    Mirror of CLIAgentResponseFormatter for the minion's output shape — same
    block set as the CLI agent (thinking, memory, next_goal, action), smaller
    tool set (MINION_TOOLS in tool_registry/service.py). The minion runs on
    native tool calling, so this is the fallback path for a model that emits
    JSON in the text channel, plus the stream/terminal render.
    """

    # Required fields for minion schema
    REQUIRED_FIELDS = ["thinking", "memory", "next_goal", "action"]

    @staticmethod
    def _extract_json(raw_response: str) -> dict | None:
        """Extract JSON from various LLM output formats. Returns parsed dict or None."""
        # Case 1: ```json fenced blocks (use LAST one for reasoning models)
        json_blocks = list(re.finditer(r'```json\s*(.*?)\s*```', raw_response, re.DOTALL))
        if json_blocks:
            for match in reversed(json_blocks):
                try:
                    return json.loads(match.group(1).strip())
                except json.JSONDecodeError:
                    continue

        # Case 2: Raw JSON (entire response)
        try:
            return json.loads(raw_response.strip())
        except json.JSONDecodeError:
            pass

        # Case 3: Brace-matched JSON object (last occurrence)
        try:
            lines = raw_response.split('\n')
            potential_starts = [i for i, line in enumerate(lines) if '{' in line]

            for json_start in reversed(potential_starts):
                brace_count = 0
                json_end = -1

                for i in range(json_start, len(lines)):
                    brace_count += lines[i].count('{') - lines[i].count('}')
                    if brace_count == 0 and i > json_start:
                        json_end = i
                        break

                if json_end != -1:
                    first_line = lines[json_start]
                    brace_pos = first_line.find('{')
                    lines[json_start] = first_line[brace_pos:]

                    json_str = '\n'.join(lines[json_start:json_end + 1])
                    try:
                        return json.loads(json_str)
                    except json.JSONDecodeError:
                        continue
        except Exception:
            pass

        return None

    @staticmethod
    def _validate_schema(json_data: dict) -> tuple:
        """Validate required fields exist and are correctly typed.

        Returns:
            (is_valid: bool, missing_fields: list[str])
        """
        missing_fields = []

        for field in MinionResponseFormatter.REQUIRED_FIELDS:
            if field not in json_data:
                missing_fields.append(field)
            elif field == "action" and not isinstance(json_data[field], list):
                missing_fields.append(f"{field} (must be array)")
            elif field != "action" and not isinstance(json_data[field], str):
                missing_fields.append(f"{field} (must be string)")

        return (len(missing_fields) == 0, missing_fields)

    @staticmethod
    def normalize_response(raw_response: str) -> tuple:
        """Normalize and validate LLM response.

        Returns:
            (success: bool, normalized_json: str|None, raw_response: str)
        """
        json_data = MinionResponseFormatter._extract_json(raw_response)
        if json_data is None:
            return (False, None, raw_response)

        is_valid, missing_fields = MinionResponseFormatter._validate_schema(json_data)
        if not is_valid:
            return (False, None, raw_response)

        normalized_json = json.dumps(json_data, indent=2, ensure_ascii=False)
        return (True, normalized_json, raw_response)

    @staticmethod
    def get_action_block(normalized_response: str) -> list:
        """Extract `action` array from a validated minion response."""
        try:
            json_data = json.loads(normalized_response)
            return json_data.get("action", [])
        except Exception:
            pass
        return []

    @staticmethod
    def format_stream_json(normalized_response: str) -> str:
        """Per-step payload for the streaming UI card. For now it returns the COMPLETE
        validated JSON (action INCLUDED). All display shaping is centralized here, so
        changing what the card shows later — e.g. splitting `action` into its own section
        (see get_action_block) — is a one-spot edit. On any parse error, fall back to the
        input unchanged.
        """
        try:
            json_data = json.loads(normalized_response)
            return json.dumps(json_data, indent=2, ensure_ascii=False)
        except Exception:
            return normalized_response

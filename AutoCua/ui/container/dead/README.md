# Parked containers (not loaded)

Kept for reference. Nothing in the app loads these files.

## bottom_right/: the main agent's TODO list

This was the bottom-right zone (`#zoneBottomRight`) of the main grid until
2026-10-14. These connections were removed:

- `index.html`: the `container/bottom_right/bottom_right.css` stylesheet and
  the `container/bottom_right/bottom_right.js` script.
- `chat_input/chat_input.js`, `resetChatUi()`: the calls to
  `window.updateTodoList({ objective: '', tasks: [] })` and
  `window.setWorkState('todo')`.
- `script.js`: `setWorkState()`, which toggled `.is-active` on `#todoState`.
- `cli/cli_stage.css` and `css/style.css`: `#zoneBottomRight` in the CLI-stage
  and Telegram rules.

The todo-row styles (`.todo-list`, `.todo-item`, `.todo-check`,
`.todo-spinner`, `.todo-text` and the `tw-*` keyframes) moved to the end of
`cli/cli_container/coder_card.css`. The CLI's Shell-use todo column still
uses them.

The backend still sends the main agent's `todo.md` to `window.updateTodoList`
(`ui/service.py`, `send_todo_to_frontend`). With this container unloaded,
nothing defines that function, so the call does nothing.

To bring it back, move the folder back to `container/`, then restore the lines
above. Git history has them exactly.

## notes_stage/: the Agent Notes stage

`notes_stage.js` and `notes_stage.html` filled the main container
(`#zoneBigCenter`) with the agent's scratchpad notes when a run ended, on
Stop, and when a saved chat was reopened, until 2026-10-15. Now the run's
video shows in the screen box and its reply in the bottom container. These
connections were removed:

- `index.html`: the `notes_stage/notes_stage.js` script.
  `notes_stage/notes_stage.css` is still loaded: Shell use's notes rows and the
  shared `.md` Markdown styles use it.
- `ui/service.py`: `send_agent_notes()`, called at run end. It pushed
  `window.showAgentNotes` (the scratchpad) or `window.showAgentHistory`.
- `chat/chat.js`, `openChat()`: `showAgentHistory` / `showAgentNotes` for a
  reopened chat, now `showUserMessage` + `showRunReply`.
- `chat_input/chat_input.js`, `resetChatUi()`: `hideAgentNotes()`. It also
  switched the big center off; `resetChatUi()` now does that itself.

`skills/skills.js` still looks for `#agentNotes` and wraps `showAgentNotes`;
both are null-safe, so it works as before without the stage.

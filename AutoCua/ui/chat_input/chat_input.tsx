// Chat input — the bottom text box (the agent-start input), inside
// #chatWrapper (index.tsx). Holds the output / cli / agent-activated strip
// areas plus the input bar (the textarea and the send / stop orbs), with the
// agent mode picker (agent_mode/agent_mode.tsx) and the memory ring
// (memory_bar/memory_bar.tsx) in their places. This was chat_input.html,
// injected by chat_input.js; chat_input.js still wires it (Enter-to-send,
// the orbs, streaming) and fires 'chatinput:ready' for the parts' scripts.
// The two orbs stay separate pages in iframes (send_button.html,
// pc_button.html): the browser extension and the banners load them too.
import { AgentMode } from '../agent_mode/agent_mode.tsx';
import { MemoryBar } from '../memory_bar/memory_bar.tsx';

export function ChatInput() {
    return (
        <div className="chat-container-content">
            {/* New Output Stream Area */}
            <div className="output-stream-area">
                <div className="welcome-overlay" id="welcomeOverlay">
                    <div className="welcome-eye"></div>
                    <div className="welcome-eye"></div>
                    <div className="welcome-glow"></div>
                </div>
                <div className="milestone-stream" id="milestoneStream"></div>
            </div>

            {/* CLI streaming area: takes over the chat box during cli_await */}
            <div className="cli-stream-area" id="cliStreamArea">
                <div className="cli-stream-list" id="cliStreamList"></div>
            </div>

            {/* Agent Response Strip (hidden by default): the thinking orb
                (thinking_orb/thinking_orb.js, mounted by chat_input.js), "Agent
                running…" shimmering (thinking_orb/shimmer.css), then the agent's
                live text. */}
            <div className="agent-response-strip" id="agentResponseStrip">
                <canvas className="agent-orb" width="40" height="40" aria-hidden="true"></canvas>
                <span className="agent-label t-shimmer" data-text="Agent running…">Agent running…</span>
                <span className="agent-text" id="agentText"></span>
            </div>

            {/* Composer that collapses on send. IDLE: a ONE-line textarea on top (grows
                upward as you type) with a tools row below it: the model picker + send/stop
                orb grouped at the RIGHT (the empty left of that row is room for future
                tools). ON SEND: the text shrinks to one line and the tools row is pushed UP
                to sit beside it, so the whole thing becomes a single slim row; the grey
                send orb morphs into the glowing stop orb (same slot, crossfade). */}
            <div className="input-area-wrapper">
                <textarea className="chat-input" placeholder="Select a model in Settings to start..." rows={1} disabled></textarea>

                <div className="input-toolbar">
                    {/* Wireless / Wired flip: Mobile use and Computer use (chat_input.css
                        shows it from the composer's data-agent-mode), in the Fast / Quality
                        pill's own look: Wi-Fi = Wireless, plug = Wired. How the phone connects,
                        Android or iPhone (link_toggle.js). Left of Fast / Quality, so nothing
                        else moves when it shows. Defaults to Wired, the cable: Wi-Fi only when
                        picked. */}
                    <button className="fast-quality-toggle link-toggle wired" id="linkToggle" type="button" role="switch" aria-checked="true" aria-label="Wireless / Wired">
                        <span className="fq-label lk-label-wireless" aria-hidden="true">Wireless</span>
                        <span className="fq-label lk-label-wired" aria-hidden="true">Wired</span>
                        <span className="fq-thumb"></span>
                        <span className="fq-icon lk-wifi">
                            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                                <path d="M12 20h.01" /><path d="M2 8.82a15 15 0 0 1 20 0" />
                                <path d="M5 12.859a10 10 0 0 1 14 0" /><path d="M8.5 16.429a5 5 0 0 1 7 0" />
                            </svg>
                        </span>
                        <span className="fq-icon lk-plug">
                            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                                <path d="M12 22v-5" /><path d="M9 8V2" /><path d="M15 8V2" />
                                <path className="lk-plug-body" d="M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8Z" />
                            </svg>
                        </span>
                    </button>

                    {/* Fast / Quality flip: UI-ONLY toggle (future feature slot, nothing
                        wired behind it yet). Amber bolt = Fast, purple sparkles = Quality;
                        the white thumb spring-bounces across and a small label floats up
                        above the pill on flip/hover. Defaults to Quality. */}
                    <button className="fast-quality-toggle quality" id="fastQualityToggle" type="button" role="switch" aria-checked="true" aria-label="Fast / Quality mode">
                        <span className="fq-label fq-label-fast" aria-hidden="true">Fast</span>
                        <span className="fq-label fq-label-quality" aria-hidden="true">Quality</span>
                        <span className="fq-thumb"></span>
                        <span className="fq-icon fq-zap">
                            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2" /></svg>
                        </span>
                        <span className="fq-icon fq-sparkles">
                            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                                <path className="fq-spark-main" d="M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z" />
                                <path d="M20 3v4" /><path d="M22 5h-4" />
                                <path d="M4 17v2" /><path d="M5 18H3" />
                            </svg>
                        </span>
                    </button>

                    {/* The memory ring, right after the Fast/Quality toggle, so Shell use
                        (where the toggle fades out) leaves no gap before the model picker. */}
                    <MemoryBar />

                    {/* Model picker (drop-up): at the right, just left of the send orb. The
                        label shows the active provider until a model is picked, then the model
                        name. Wired by script.js (wireChatDropdown). Kept a descendant of
                        .input-area-wrapper so the :has(.chat-model-dropdown.open) z-index lift
                        still resolves. */}
                    <div className="model-dropdown chat-model-dropdown" id="chatModelDropdown">
                        <button className="model-dropdown-trigger" type="button" aria-label="Select model">
                            <span className="model-dropdown-value placeholder">select model</span>
                            {/* Same mark as the Computer/Mobile use picker's .am-chev: a single
                                caret that flips 180° on open, so the two drop-ups across the row
                                behave alike. */}
                            <svg className="chat-dropdown-chevron" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M18 15l-6-6-6 6" /></svg>
                        </button>
                        <div className="model-dropdown-menu"></div>
                    </div>

                    {/* Orb slot: the send orb (idle) and stop orb (running) are overlaid here
                        and crossfade into one another so the same-shape circles read as one morph. */}
                    <div className="input-orb-slot">
                        {/* Send orb: embeds send_button.html (grey, idle). Clicking it sends. */}
                        <iframe id="sendBtnFrame" className="send-agent-frame" src="chat_input/send_button.html" title="Send" scrolling="no" frameBorder="0"></iframe>
                        {/* Stop Agent Orb Button: embeds pc_button.html (glowing, agent running). */}
                        <iframe id="stopBtnFrame" className="stop-agent-frame" src="chat_input/pc_button.html" title="Stop Agent" scrolling="no" frameBorder="0"></iframe>
                        {/* The frames are bigger than the orb and take no clicks; this is the
                            orb's click target (chat_input.js presses the showing orb). */}
                        <button id="orbHit" className="orb-hit" type="button" tabIndex={-1} aria-label="Send / stop agent"></button>
                    </div>
                </div>

                {/* The agent mode picker, the composer's bottom-left control. */}
                <AgentMode />
            </div>
        </div>
    );
}

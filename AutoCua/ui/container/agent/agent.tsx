// Agent container — the whole bottom row of the live turn (#liveTurn,
// index.tsx): the tool-response flow (chain) on the left, the live "tracking
// progress" stream on the right (tracking_progress.js); once the run ends,
// its reply takes both their places, like a chat (run_reply.js). The flow
// steps (logos + labels + line) are BUILT FROM CONFIG in agent.js (STEPS /
// SHAPES), so the backend can map its tool events to steps. This was
// agent.html, injected by agent.js; agent.js still attaches its flow to it.
export function AgentContainer() {
    return (
        <div className="container-zone agent-container" id="agentContainer" aria-label="Tool response and tracking progress">
            <div className="bl-panel tool-panel">
                <div className="tool-response-label">Tool response: <span className="tool-used-count" id="toolUsedCount">0</span> tools used</div>

                <div className="tool-flow" aria-hidden="true">
                    {/* agent.js builds one .tool-flow-item per STEP into here */}
                    <div className="tool-flow-tree" id="toolFlowTree"></div>
                </div>

                {/* Shell terminal: while a shell command (or an AppleScript) runs, its
                    lines take the chain's place: a transparent `>` command line (streams
                    + shimmers with a dot loader while it runs), then an L-connector down
                    to the output line that comes back. Then the chain is back. Driven by
                    globe_shell.js shell*(); ids preserved so the backend hooks drive it
                    unchanged. */}
                <div className="sh-panel" id="shellPanel" aria-hidden="true">
                    <div className="shell-term" id="shellTerminalContainer">
                        <div className="sh-body">
                            <div className="sh-lines">
                                <div className="sh-cmd-line" id="shellCmdLine">
                                    <span className="sh-p">{'>'}</span>
                                    <span className="sh-cmd-text" id="shellCmdText"></span>
                                </div>
                                <div className="sh-progress" id="shellProgress" aria-hidden="true">
                                    <span className="sh-fill"></span>
                                </div>
                                <div className="sh-out-line" id="shellOutLine">
                                    <span className="sh-out-text" id="shellOutText"></span>
                                </div>
                            </div>
                        </div>
                    </div>
                </div>
            </div>

            <div className="bl-panel tracking-panel">
                <div className="scratchpad-label">tracking progress</div>

                <div className="br-flow" aria-hidden="true">
                    {/* tracking_progress.js appends one .br-item per scratchpad entry into here */}
                    <div className="br-tree" id="brTree"></div>
                </div>
            </div>

            {/* The run's reply (window.showRunReply), shown once the run ends */}
            <div className="bl-panel reply-panel md" id="runReply" aria-live="polite"></div>
        </div>
    );
}

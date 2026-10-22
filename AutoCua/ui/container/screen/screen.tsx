// Screen container — the live agent screenshot, then the run's video once the
// run is over, the second zone of the live turn (#liveTurn, index.tsx). Keeps
// id="imageStreamContainer" and the .stream-image <img> so the existing
// window.updateAgentImage() hook in script.js keeps working unchanged; the
// video is screen.js's (window.showRunVideo / hideRunVideo), the moving
// background and "Preparing video" run_bot.js's, the globe and the terminal
// globe_shell.js's. This was screen.html, injected by screen.js.
export function ScreenContainer() {
    return (
        <div className="container-zone screen-container" id="screenContainer" aria-label="Screenshot">
            {/* Computer use, Web use, iOS: the moving background behind the screenshot (run_bot.js) */}
            <video className="cu-bg" muted loop playsInline preload="none" aria-hidden="true"></video>

            <div className="screenshot-frame" id="imageStreamContainer">
                <img className="stream-image" alt="Agent screen" style={{ display: 'none' }} />
                {/* The run's recording, in place of the screenshot once the run is over
                    (window.showRunVideo in screen.js). */}
                <video className="run-video" controls playsInline preload="auto" style={{ display: 'none' }}></video>
            </div>

            {/* In the middle while a Computer use run's video is compressed (run_bot.js);
                the run's bot is beside the bottom container (container/agent/chain_bot.js) */}
            <div className="cu-prep-text" aria-hidden="true"><span className="t-shimmer" data-text="Preparing video">Preparing video</span></div>
        </div>
    );
}

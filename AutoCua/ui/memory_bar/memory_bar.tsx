// Memory ring — the composer's context gauge: the main agent's current context
// size vs the fixed 300k memory budget, drawn as a clean circle in the
// composer's toolbar, just RIGHT of the Fast/Quality toggle
// (chat_input/chat_input.tsx). The arc fills clockwise from 12 o'clock and its
// colour moves with it (baby purple when empty, reddish orange when full); the
// live token count sits in the middle (850, 1.2k, 45k). Both glide whenever
// window.updateMemoryBar(used, cap) moves. This was memory_bar.html, injected
// by memory_bar.js; memory_bar.js still draws it.
export function MemoryBar() {
    return (
        <div className="memory-ring" id="memoryRing" role="img" aria-label="Memory">
            <svg className="memory-ring-svg" viewBox="0 0 240 240" aria-hidden="true">
                <circle className="memory-ring-track" cx="120" cy="120" r="112.5" />
                <circle className="memory-ring-arc" cx="120" cy="120" r="112.5" pathLength="100" />
            </svg>
            <span className="memory-ring-value" aria-hidden="true">0</span>
        </div>
    );
}

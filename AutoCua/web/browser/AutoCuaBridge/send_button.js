// The two inline scripts of the desktop app's send orb (AutoCua/frontend/chat_input/
// send_button.html), unchanged: an extension page may not run inline scripts.

// The Linux app is WebKitGTK: flag it before first paint for the
// html.linux rules (macOS and Windows keep the original look).
if (navigator.userAgent.includes('Linux')) document.documentElement.classList.add('linux');

document.addEventListener('DOMContentLoaded', () => {
    // window.parent === window means the file was opened directly (preview), not
    // embedded in the composer iframe.
    const standalone = window.parent === window;
    // Big zoom only for preview; renders at native 42px when embedded.
    if (standalone) document.body.classList.add('preview');

    const sendBtn = document.getElementById('stopAgentBtn');

    // Click the grey orb → tell the parent to send (same path as Enter).
    sendBtn.addEventListener('click', () => {
        if (!standalone) parent.postMessage('sendbtn:clicked', '*');
    });

    // Commands from the parent: hide while the agent runs, show again on idle.
    window.addEventListener('message', (e) => {
        if (e.data === 'sendbtn:hide') sendBtn.classList.add('is-hidden');
        else if (e.data === 'sendbtn:show') sendBtn.classList.remove('is-hidden');
    });
});

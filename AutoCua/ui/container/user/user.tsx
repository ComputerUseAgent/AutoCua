// User container — the user's message for this run, top right of the main
// grid like a chat, the first zone of the live turn (#liveTurn, index.tsx).
// Empty and hidden until a message is sent (user.js fills it:
// window.showUserMessage). This was user.html, injected by user.js.
export function UserContainer() {
    return (
        <div className="container-zone user-container is-empty" id="userContainer" aria-label="Your message">
            {/* Where the message came from, when not this chat box: "sent via: Telegram" (user.js) */}
            <div className="user-message-via" id="userMessageVia" hidden></div>
            <div className="user-message-text" id="userMessageText"></div>
        </div>
    );
}

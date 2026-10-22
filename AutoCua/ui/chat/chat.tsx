// New Chat button — at the top of the left bar body (left_bar/left_bar.tsx).
// This was chat.html, injected by chat.js; chat.js still wires it, builds the
// chat history list under it and the rail's circles, and loads the chats.
export function NewChatButton() {
    return (
        <button className="new-chat-btn" type="button" aria-label="New chat">
            <svg className="new-chat-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M12 20h9"></path>
                <path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4 12.5-12.5z"></path>
            </svg>
            <span className="new-chat-label">New chat</span>
        </button>
    );
}

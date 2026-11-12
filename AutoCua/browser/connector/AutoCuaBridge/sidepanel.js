// The chat panel: Chrome's side panel, opened from the popup's Chat button (test.js).
// Chrome draws the panel's header (the name and the close button) itself.
// A placeholder for now: what is typed shows as a message, nothing is sent anywhere.
const $ = (id) => document.getElementById(id);
const text = $("text");
const messages = $("messages");
const hint = $("hint");

// The box grows with the text, up to its CSS max-height; empty, it is one line.
// Measuring an empty box measured its placeholder, which wraps over many lines
// while Chrome's side panel is still opening (narrow), and kept the box tall.
const fit = () => {
  text.style.height = "";
  if (text.value) text.style.height = text.scrollHeight + "px";
};
text.addEventListener("input", fit);
window.addEventListener("resize", fit);
text.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    $("composer").requestSubmit();
  }
});

// The send orb (send_button.html, the desktop app's own) says when it is clicked.
window.addEventListener("message", (e) => {
  if (e.source === $("orb").contentWindow && e.data === "sendbtn:clicked") $("composer").requestSubmit();
});

// New chat: the conversation goes and the panel starts empty again.
// History and Model are placeholders for now, like Send.
$("new-chat").addEventListener("click", () => {
  messages.replaceChildren(hint);
  text.value = "";
  fit();
  text.focus();
});

$("composer").addEventListener("submit", (e) => {
  e.preventDefault();
  const value = text.value.trim();
  if (!value) return;
  hint.remove();
  messages.append(Object.assign(document.createElement("div"), { className: "msg me", textContent: value }));
  messages.append(Object.assign(document.createElement("div"), { className: "note", textContent: "Not connected yet." }));
  messages.scrollTop = messages.scrollHeight;
  text.value = "";
  fit();
  text.focus();
});

fit();
text.focus();

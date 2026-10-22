// Left bar — the persistent panel (left_bar.css): the logo header, the New
// chat button (chat/chat.tsx) with the chat list chat.js builds under it,
// the rail shown while the bar is collapsed (chat.js fills it with circles),
// and the footer with the Settings cog (settings/settings.tsx) and the
// GitHub button. The collapse toggle sits on the bar's right edge, outside
// the bar (whose overflow is clipped). This was left_bar.html, injected by
// left_bar.js; left_bar.js still wires the toggle and the GitHub button,
// fires 'leftbar:ready' and reveals the bar.
import { NewChatButton } from '../chat/chat.tsx';
import { SettingsButton } from '../settings/settings.tsx';

export function LeftBar() {
    return (
        <>
            <aside className="left-bar" id="leftBar" aria-label="Sidebar">
                <div className="liquidGlass-effect"></div>
                <div className="liquidGlass-tint"></div>
                <div className="liquidGlass-shine"></div>
                <div className="liquidGlass-content left-bar-content">
                    {/* Header: AutoCua logo (top-left). Brand mark: AutoCua's logo shape
                        (logo/logo.html) in grey (left_bar.css draws it from left_bar/logo_mark.svg). */}
                    <div className="left-bar-header" id="leftBarHeader">
                        <span className="left-bar-logo" role="img" aria-label="AutoCua"></span>
                    </div>
                    {/* Body: the New chat button; chat.js adds the history list under it. */}
                    <div className="left-bar-body"><NewChatButton /></div>
                    {/* Rail: shown instead of the body while the bar is collapsed;
                        chat/chat.js fills it with circles (New chat, then one per chat). */}
                    <div className="left-bar-rail" id="leftBarRail"></div>
                    {/* Footer: the settings cog first, bottom-left; the GitHub button beside it.
                        The GitHub button opens the repo in the system browser via
                        /api/open-github, wired by left_bar.js. */}
                    <div className="left-bar-footer" id="leftBarFooter">
                        <SettingsButton />
                        <button className="gh-link" id="ghLink" type="button" aria-label="AutoCua on GitHub" title="AutoCua on GitHub">
                            <svg className="gh-mark" viewBox="0 0 16 16" aria-hidden="true"><path fill="currentColor" d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8z" /></svg>
                        </button>
                    </div>
                </div>
            </aside>
            {/* Collapse toggle: wired by left_bar.js; the arrow turns to point right while collapsed. */}
            <button className="left-bar-toggle" id="leftBarToggle" type="button" aria-controls="leftBar" aria-expanded="true" aria-label="Collapse sidebar" title="Collapse sidebar">
                <svg className="left-bar-toggle-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M14.5 6l-6 6 6 6" /></svg>
            </button>
        </>
    );
}

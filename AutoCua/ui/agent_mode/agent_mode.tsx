// Agent mode — the composer's bottom-left control, the last child of
// .input-area-wrapper (chat_input/chat_input.tsx). A TRANSPARENT trigger
// (mirrors the model picker across the row): the selection's logo + mode name
// + sub-option and a chevron. The floating menu above carries Computer use /
// Mobile use, each with a > flyout: Computer use → This PC / Daisy chain
// (This PC default), Mobile use → iOS / Android (iOS default), then the two
// flat rows with no flyout: Shell use and Web use. Shell use adds the Auto /
// Manual pill. The Skills icon (skills/skills.tsx) stands just right of the
// trigger. This was agent_mode.html, injected by agent_mode.js; agent_mode.js
// still owns the state and wires it all, and fires 'agentmode:ready'.
import type { ReactNode } from 'react';
import { SkillsButton } from '../skills/skills.tsx';

// The marks, drawn once each: stroked (stroke-width 2 unless said) or filled.
function Stroke({ className, width, hidden, children }: { className: string; width?: string; hidden?: boolean; children: ReactNode }) {
    return (
        <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={width || '2'} strokeLinecap="round" strokeLinejoin="round" aria-hidden={hidden ? 'true' : undefined}>{children}</svg>
    );
}
function Fill({ className, hidden, children }: { className: string; hidden?: boolean; children: ReactNode }) {
    return <svg className={className} viewBox="0 0 24 24" fill="currentColor" aria-hidden={hidden ? 'true' : undefined}>{children}</svg>;
}
const MONITOR = <><rect x="3" y="4" width="18" height="12" rx="1" /><path d="M7 20h10" /><path d="M9 16v4" /><path d="M15 16v4" /></>;
const PHONE = <><rect x="5.5" y="2" width="13" height="20" rx="2.5" /><path d="M10.5 18h3" /></>;
// Terminal mark redrawn from tool_icon.html's vecShell painter ("executing
// shell"): window + title bar + > prompt + output line, simplified for 18px.
const SHELL = <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M3 8h18" /><path d="M7 12l2.5 2.5L7 17" /><path d="M13 17h4" /></>;
// Globe mark reduced from the dotted lat/long sphere tool_icons.js paints for
// "searching the web": outline sphere + equator + meridian.
const GLOBE = <><circle cx="12" cy="12" r="9" /><path d="M3 12h18" /><ellipse cx="12" cy="12" rx="4" ry="9" /></>;
const DAISY = <><path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71" /><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71" /></>;
const APPLE = <path d="M12.152 6.896c-.948 0-2.415-1.078-3.96-1.04-2.04.027-3.91 1.183-4.961 3.014-2.117 3.675-.546 9.103 1.519 12.09 1.013 1.454 2.208 3.09 3.792 3.039 1.52-.065 2.09-.987 3.935-.987 1.831 0 2.35.987 3.96.948 1.637-.026 2.676-1.48 3.676-2.948 1.156-1.688 1.636-3.325 1.662-3.415-.039-.013-3.182-1.221-3.22-4.857-.026-3.04 2.48-4.494 2.597-4.559-1.429-2.09-3.623-2.324-4.39-2.376-2-.156-3.675 1.09-4.61 1.09zM15.53 3.83c.843-1.012 1.4-2.427 1.245-3.83-1.207.052-2.662.805-3.532 1.818-.78.896-1.454 2.338-1.273 3.714 1.338.104 2.715-.688 3.559-1.701" />;
const ANDROID = <path d="M6,18C6,18.55 6.45,19 7,19H8V22.5C8,23.33 8.67,24 9.5,24C10.33,24 11,23.33 11,22.5V19H13V22.5C13,23.33 13.67,24 14.5,24C15.33,24 16,23.33 16,22.5V19H17C17.55,19 18,18.55 18,18V8H6V18M3.5,8C2.67,8 2,8.67 2,9.5V16.5C2,17.33 2.67,18 3.5,18C4.33,18 5,17.33 5,16.5V9.5C5,8.67 4.33,8 3.5,8M20.5,8C19.67,8 19,8.67 19,9.5V16.5C19,17.33 19.67,18 20.5,18C21.33,18 22,17.33 22,16.5V9.5C22,8.67 21.33,8 20.5,8M15.53,2.16L16.84,0.85C17.03,0.66 17.03,0.34 16.84,0.14C16.64,-0.05 16.32,-0.05 16.13,0.14L14.65,1.62C13.85,1.23 12.95,1 12,1C11.04,1 10.14,1.23 9.34,1.63L7.85,0.14C7.66,-0.05 7.34,-0.05 7.15,0.14C6.95,0.34 6.95,0.66 7.15,0.85L8.46,2.16C6.97,3.26 6,5.01 6,7H18C18,5.01 17.03,3.25 15.53,2.16M10,5H9V4H10V5M15,5H14V4H15V5Z" />;
const AUTO = <><path d="M12 8V4H8" /><rect x="4" y="8" width="16" height="12" rx="2" /><path d="M2 14h2" /><path d="M20 14h2" /><path d="M15 13v2" /><path d="M9 13v2" /></>;
const MANUAL = <><rect x="2" y="4" width="20" height="16" rx="2" /><path d="M6 8h.01" /><path d="M10 8h.01" /><path d="M14 8h.01" /><path d="M18 8h.01" /><path d="M8 12h.01" /><path d="M12 12h.01" /><path d="M16 12h.01" /><path d="M7 16h10" /></>;
const CHEV_UP = <path d="M18 15l-6-6-6 6" />;
// Pair again: a circular arrow (lucide's rotate-cw).
const RETRY = <><path d="M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8" /><path d="M21 3v5h-5" /></>;

function Check() { return <Stroke className="am-check" width="2.5"><path d="M5 12l5 5L20 7" /></Stroke>; }
function SubChev() { return <Stroke className="am-subchev"><path d="M9 6l6 6-6 6" /></Stroke>; }

export function AgentMode() {
    return (
        <div className="agent-mode-wrap" id="agentModeWrap" data-mode="computer" data-sub="thispc" data-shell-input="auto">

            <div className="agent-menu" id="agentMenu">
                <div className="agent-menu-inner" role="menu">

                    <div className="agent-opt-wrap" data-mode="computer">
                        <button className="agent-opt selected" data-mode="computer" role="menuitemradio" aria-checked="true" type="button">
                            <Stroke className="am-icon">{MONITOR}</Stroke>
                            <span>Computer use</span>
                            <Check />
                            <SubChev />
                        </button>
                        <div className="agent-sub">
                            <div className="agent-sub-inner" role="menu">
                                <button className="agent-subopt selected" data-sub="thispc" role="menuitemradio" aria-checked="true" type="button">
                                    <Stroke className="am-icon">{MONITOR}</Stroke>
                                    <span>This PC</span>
                                    <Check />
                                </button>
                                <button className="agent-subopt" data-sub="daisy" role="menuitemradio" aria-checked="false" type="button">
                                    <Stroke className="am-icon">{DAISY}</Stroke>
                                    <span>Daisy chain</span>
                                    <Check />
                                </button>
                            </div>
                        </div>
                    </div>

                    <div className="agent-opt-wrap" data-mode="mobile">
                        <button className="agent-opt" data-mode="mobile" role="menuitemradio" aria-checked="false" type="button">
                            <Stroke className="am-icon">{PHONE}</Stroke>
                            <span>Mobile use</span>
                            <Check />
                            <SubChev />
                        </button>
                        <div className="agent-sub">
                            <div className="agent-sub-inner" role="menu">
                                <button className="agent-subopt" data-sub="ios" role="menuitemradio" aria-checked="false" type="button">
                                    <Fill className="am-icon am-icon-fill">{APPLE}</Fill>
                                    <span>iOS</span>
                                    <Check />
                                </button>
                                <button className="agent-subopt" data-sub="android" role="menuitemradio" aria-checked="false" type="button">
                                    <Fill className="am-icon am-icon-fill">{ANDROID}</Fill>
                                    <span>Android</span>
                                    <Check />
                                </button>
                            </div>
                        </div>
                    </div>

                    {/* Shell use: no sub-options, so no .agent-sub and no > hint. */}
                    <div className="agent-opt-wrap" data-mode="shell">
                        <button className="agent-opt" data-mode="shell" role="menuitemradio" aria-checked="false" type="button">
                            <Stroke className="am-icon">{SHELL}</Stroke>
                            <span>Shell use</span>
                            <Check />
                        </button>
                    </div>

                    {/* Web use: the browser agent. No sub-options either. */}
                    <div className="agent-opt-wrap" data-mode="web">
                        <button className="agent-opt" data-mode="web" role="menuitemradio" aria-checked="false" type="button">
                            <Stroke className="am-icon">{GLOBE}</Stroke>
                            <span>Web use</span>
                            <Check />
                        </button>
                    </div>

                </div>
            </div>

            <button className="agent-btn" id="agentBtn" aria-expanded="false" aria-haspopup="true" type="button">
                {/* mode logo (monitor / phone / terminal / globe), driven by data-mode */}
                <Stroke className="am-mode-icon am-mode-computer" hidden>{MONITOR}</Stroke>
                <Stroke className="am-mode-icon am-mode-mobile" hidden>{PHONE}</Stroke>
                <Stroke className="am-mode-icon am-mode-shell" hidden>{SHELL}</Stroke>
                <Stroke className="am-mode-icon am-mode-web" hidden>{GLOBE}</Stroke>
                <span className="am-cur-label" id="agentCurLabel">Computer use</span>
                {/* sub-option logo ONLY (no text): chain for Daisy chain, Apple/robot
                    for iOS/Android. This PC (and no-sub) shows nothing. */}
                <Stroke className="am-sub-icon am-sub-daisy" hidden>{DAISY}</Stroke>
                <Fill className="am-sub-icon am-sub-ios" hidden>{APPLE}</Fill>
                <Fill className="am-sub-icon am-sub-android" hidden>{ANDROID}</Fill>
                <svg className="am-chev" id="agentChev" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">{CHEV_UP}</svg>
            </button>

            {/* Mobile use: the phone's pairing. A circle that turns while it pairs;
                if it did not pair, an arrow in its place that pairs again on a click
                (ios_session.js / android_session.js set the wrap's data-pair and
                answer the click; agent_mode.css). Nothing once paired. */}
            <button className="pair-status" id="pairStatus" type="button" aria-label="Pair again" title="Pair again">
                <svg className="ps-spin" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" aria-hidden="true">
                    <circle cx="12" cy="12" r="9" opacity="0.25" />
                    <path d="M21 12a9 9 0 0 0-9-9" />
                </svg>
                <Stroke className="ps-retry" hidden>{RETRY}</Stroke>
            </button>

            {/* Shell use only: Auto / Manual. Auto: your message goes to the agent and the
                terminal shows its work and answer. Manual: you type commands in the terminal
                yourself (the chat box points there), and the terminal grows. Same pill and
                menu look as the mode picker; agent_mode.js owns its state. */}
            <div className="shell-input-wrap" id="shellInputWrap">
                <div className="agent-menu shell-input-menu" id="shellInputMenu">
                    <div className="agent-menu-inner" role="menu">
                        <button className="agent-opt selected" data-input="auto" role="menuitemradio" aria-checked="true" type="button">
                            <Stroke className="am-icon">{AUTO}</Stroke>
                            <span>Auto</span>
                            <Check />
                        </button>
                        <button className="agent-opt" data-input="manual" role="menuitemradio" aria-checked="false" type="button">
                            <Stroke className="am-icon">{MANUAL}</Stroke>
                            <span>Manual</span>
                            <Check />
                        </button>
                    </div>
                </div>
                <button className="agent-btn" id="shellInputBtn" aria-expanded="false" aria-haspopup="true" type="button">
                    <Stroke className="si-icon si-auto" hidden>{AUTO}</Stroke>
                    <Stroke className="si-icon si-manual" hidden>{MANUAL}</Stroke>
                    <span className="am-cur-label" id="shellInputLabel">Auto</span>
                    <svg className="am-chev" id="shellInputChev" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">{CHEV_UP}</svg>
                </button>
            </div>

            {/* The Skills icon, just right of the trigger. */}
            <SkillsButton />
        </div>
    );
}

// The page: what index.html's <body> held until 2026-10-22, as React, drawn
// into #root at the bottom of this file. index.html keeps the stylesheet
// links and the classic scripts and runs the build of this file (index.js)
// first thing in <body>, synchronously, so those scripts find every id and
// class in place as before and keep driving them (they build the chat list,
// the tool chain and so on inside, and toggle classes here). Nothing here
// re-renders, so React leaves what they change alone.
//
// Left out on purpose: the two <template>s of the old CLI pills
// (#cliPillTemplate, #cliMinionPillTemplate). Only script.js's old CLI block
// cloned them, which cli_stage.js replaced, and it checks for them first.
import { flushSync } from 'react-dom';
import { createRoot } from 'react-dom/client';
import { IntroAnimation } from './intro_animation.tsx';
import { BigCenterContainer } from './container/big_center_container/big_center_container.tsx';
import { UserContainer } from './container/user/user.tsx';
import { ScreenContainer } from './container/screen/screen.tsx';
import { AgentContainer } from './container/agent/agent.tsx';
import { ChatInput } from './chat_input/chat_input.tsx';
import { LeftBar } from './left_bar/left_bar.tsx';

// The SVG filters the liquid-glass layers use (filter: url(#...), css/style.css).
function GlassFilters() {
    return (
        <>
            <svg style={{ display: 'none' }}>
                <filter id="glass-distortion" x="-50%" y="-50%" width="200%" height="200%" filterUnits="objectBoundingBox">
                    <feTurbulence type="fractalNoise" baseFrequency="0.015 0.015" numOctaves="2" seed="5" result="turbulence" />
                    <feComponentTransfer in="turbulence" result="mapped">
                        <feFuncR type="gamma" amplitude="1" exponent="8" offset="0.5" />
                        <feFuncG type="gamma" amplitude="0.5" exponent="1" offset="0" />
                        <feFuncB type="gamma" amplitude="0.5" exponent="1" offset="0.5" />
                    </feComponentTransfer>
                    <feGaussianBlur in="turbulence" stdDeviation="4" result="softMap" />
                    <feSpecularLighting in="softMap" surfaceScale="8" specularConstant="1.2" specularExponent="120" lightingColor="white" result="specLight">
                        <fePointLight x="-150" y="-150" z="400" />
                    </feSpecularLighting>
                    <feComposite in="specLight" operator="arithmetic" k1="0" k2="1" k3="1.2" k4="0" result="litImage" />
                    <feDisplacementMap in="SourceGraphic" in2="softMap" scale="200" xChannelSelector="R" yChannelSelector="G" />
                </filter>
            </svg>

            <svg style={{ display: 'none' }}>
                <filter id="liquid-glass-button" x="-50%" y="-50%" width="200%" height="200%" filterUnits="objectBoundingBox">
                    <feTurbulence type="fractalNoise" baseFrequency="0.03 0.05" numOctaves="4" seed="3" result="turbulence" />
                    <feComponentTransfer in="turbulence" result="mapped">
                        <feFuncR type="gamma" amplitude="1" exponent="4" offset="0.4" />
                        <feFuncG type="gamma" amplitude="1" exponent="4" offset="0.4" />
                    </feComponentTransfer>
                    <feGaussianBlur in="mapped" stdDeviation="2.5" result="softMap" />
                    <feDisplacementMap in="SourceGraphic" in2="softMap" scale="25" xChannelSelector="R" yChannelSelector="G" />
                </filter>
            </svg>
        </>
    );
}

function ChevronDown({ className }: { className: string }) {
    return (
        <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><polyline points="6 9 12 15 18 9"></polyline></svg>
    );
}

function TrashIcon() {
    return (
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="3 6 5 6 21 6"></polyline>
            <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path>
        </svg>
    );
}

// The API key rows: provider id, the page to get a key, its name, the input's hint.
const PROVIDERS: [string, string, string, string][] = [
    ['openrouter', 'https://openrouter.ai/keys', 'OpenRouter', 'sk-or-...'],
    ['groq', 'https://console.groq.com/keys', 'Groq', 'gsk_...'],
    ['openai', 'https://platform.openai.com/api-keys', 'OpenAI', 'sk-...'],
    ['anthropic', 'https://console.anthropic.com/settings/keys', 'Anthropic', 'sk-ant-...'],
    ['perplexity', 'https://docs.perplexity.ai/guides/getting-started', 'Perplexity', 'pplx-...'],
    ['together', 'https://api.together.xyz/settings/api-keys', 'Together AI', 'Enter API key...'],
    ['cerebras', 'https://cloud.cerebras.ai', 'Cerebras', 'csk-...'],
    ['aws', 'https://console.aws.amazon.com/bedrock/home#/api-keys', 'AWS', 'ABSK...'],
    ['google', 'https://aistudio.google.com/apikey', 'Google', 'AIza...'],
];

// Settings Panel Overlay (script.js, device_connect.js and left_bar.js drive it).
function SettingsOverlay() {
    return (
        <div className="settings-overlay" id="settingsOverlay">
            <div className="settings-panel">
                <div className="settings-panel-content">

                    {/* Single header: flat layout, all sections shown directly (no menu/nesting) */}
                    <div className="settings-header">
                        <span className="settings-title">Settings</span>
                    </div>

                    {/* Provider Selection (the MODEL is picked in the chat-box drop-up) */}
                    <div className="settings-view settings-modelselect-view" id="settingsModelView">
                        <div className="settings-section-heading">Provider</div>
                        <div className="model-select-row">
                            <div className="model-select-field">
                                <label className="model-select-label">Provider</label>
                                <div className="model-dropdown" id="providerDropdown">
                                    <button className="model-dropdown-trigger" type="button">
                                        <span className="model-dropdown-value placeholder">Select a provider…</span>
                                        <svg className="model-dropdown-chevron" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round"><polyline points="6 9 12 15 18 9"></polyline></svg>
                                    </button>
                                    <div className="model-dropdown-menu"></div>
                                </div>
                            </div>
                        </div>
                    </div>

                    {/* API Keys */}
                    <div className="settings-view settings-apikeys-view" id="settingsApikeysView">
                        <div className="settings-section-heading">API Keys</div>
                        <div className="settings-provider-list">
                            {PROVIDERS.map(([id, url, name, hint]) => (
                                <div className="settings-provider-row" data-provider={id} key={id}>
                                    <a className="settings-provider-name" href={url} target="_blank">{name}</a>
                                    <input type="password" className={id === 'google' ? 'settings-provider-input google-key-input' : 'settings-provider-input'} placeholder={hint} autoComplete="off" />
                                    <button className="settings-delete-btn" title="Remove API key" style={{ display: 'none' }}>
                                        <TrashIcon />
                                    </button>
                                    {id === 'google' && <button className="gcp-vertex-btn" id="gcpVertexBtn" title="Configure GCP Vertex AI">GCP Vertex</button>}
                                </div>
                            ))}
                        </div>
                        <button className="settings-save-btn" id="settingsSaveBtn">Save</button>
                    </div>

                    {/* Remote Connection */}
                    <div className="settings-view settings-remote-view" id="settingsRemoteView">
                        <div className="settings-section-heading">Remote Connection</div>

                        {/* Not connected state */}
                        <div className="remote-setup" id="remoteSetup">
                            <div className="remote-row">
                                <span className="remote-row-label remote-telegram-logo" aria-label="Telegram">
                                    <svg width="22" height="22" viewBox="0 0 24 24" fill="currentColor"><path d="M21.5 4.5L2.5 12l5.5 2 2 6 3-3.5 5.5 4 3-16zM10 14l8.5-7L11 14.5l-1 4.5L10 14z" /></svg>
                                </span>
                                <span className="remote-row-actions">
                                    <button className="remote-connect-btn" id="remoteToggleBtn" type="button">Connect</button>
                                </span>
                            </div>
                            <div className="remote-instruction" id="remoteInstructions" style={{ display: 'none' }}>Please follow the on-screen instructions in the helper banner (top-right of your screen).</div>
                        </div>
                    </div>

                    {/* Connect Device: phone automation. iPhone (iOS/WDA) is live;
                        Android is disabled for now. Wired by device_connect.js. */}
                    <div className="settings-view settings-device-view" id="settingsDeviceView">
                        <div className="settings-section-heading">Connect Device</div>
                        <div className="device-list">

                            {/* iPhone */}
                            <div className="device-option" id="deviceOptioniOS" role="button" tabIndex={0} aria-expanded="false" aria-label="Connect iPhone">
                                <span className="device-logo device-logo-ios" aria-hidden="true">
                                    <svg viewBox="0 0 24 24" fill="currentColor"><path d="M12.152 6.896c-.948 0-2.415-1.078-3.96-1.04-2.04.027-3.91 1.183-4.961 3.014-2.117 3.675-.546 9.103 1.519 12.09 1.013 1.454 2.208 3.09 3.792 3.039 1.52-.065 2.09-.987 3.935-.987 1.831 0 2.35.987 3.96.948 1.637-.026 2.676-1.48 3.676-2.948 1.156-1.688 1.636-3.325 1.662-3.415-.039-.013-3.182-1.221-3.22-4.857-.026-3.04 2.48-4.494 2.597-4.559-1.429-2.09-3.623-2.324-4.39-2.376-2-.156-3.675 1.09-4.61 1.09zM15.53 3.83c.843-1.012 1.4-2.427 1.245-3.83-1.207.052-2.662.805-3.532 1.818-.78.896-1.454 2.338-1.273 3.714 1.338.104 2.715-.688 3.559-1.701" /></svg>
                                </span>
                                <span className="device-name">iPhone</span>
                                <span className="device-chevron" aria-hidden="true">
                                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round"><polyline points="9 6 15 12 9 18"></polyline></svg>
                                </span>
                            </div>

                            {/* iPhone expandable panel: a clean pairing flow in the app's own
                                theme. It drives setup.py's backend (sign / build / install /
                                trust / add-account) but shows only a simple status + one
                                action. Wired by device_connect.js. */}
                            <div className="device-panel" id="devicePaneliOS" hidden>
                                <div className="dc-flow">
                                    {/* Apple Account dropdown (auto-selected; always offers ＋ Add) */}
                                    <div className="dc-field">
                                        <label className="dc-label">Apple Account</label>
                                        <div className="dc-select" id="dcTeamSelect" role="button" tabIndex={0}>
                                            <span className="dc-select-value placeholder" id="dcTeamValue">Loading…</span>
                                            <ChevronDown className="dc-select-chevron" />
                                            <div className="dc-select-menu" id="dcTeamMenu"></div>
                                        </div>
                                    </div>
                                    {/* Device dropdown (auto-selected) */}
                                    <div className="dc-field">
                                        <label className="dc-label">Device</label>
                                        <div className="dc-select" id="dcDeviceSelect" role="button" tabIndex={0}>
                                            <span className="dc-select-value placeholder" id="dcDeviceValue">Looking…</span>
                                            <ChevronDown className="dc-select-chevron" />
                                            <div className="dc-select-menu" id="dcDeviceMenu"></div>
                                        </div>
                                    </div>

                                    <div className="dc-status" id="dcStatus" data-state="loading">
                                        <span className="dc-spinner" id="dcSpinner"></span>
                                        <span className="dc-check" id="dcCheck" hidden></span>
                                        <span className="dc-status-text" id="dcStatusText">Looking for your iPhone…</span>
                                    </div>
                                    <div className="dc-steps" id="dcSteps" hidden></div>
                                    <button className="dc-action" id="dcAction" type="button" hidden></button>

                                    {/* Paired devices (paired_devices.json): a device in this list
                                        is paired: the Apple logo in the chat box just activates it.
                                        × forgets the device. */}
                                    <div className="dc-paired" id="dcPaired" hidden>
                                        <div className="dc-label">Paired devices</div>
                                        <div className="dc-paired-list" id="dcPairedList"></div>
                                    </div>
                                </div>
                            </div>

                            {/* Android (future) */}
                            <div className="device-option is-disabled" id="deviceOptionAndroid" aria-disabled="true" data-tip="Added in a future update">
                                <span className="device-logo device-logo-android" aria-hidden="true">
                                    <svg viewBox="0 0 24 24" fill="currentColor"><path d="M6,18C6,18.55 6.45,19 7,19H8V22.5C8,23.33 8.67,24 9.5,24C10.33,24 11,23.33 11,22.5V19H13V22.5C13,23.33 13.67,24 14.5,24C15.33,24 16,23.33 16,22.5V19H17C17.55,19 18,18.55 18,18V8H6V18M3.5,8C2.67,8 2,8.67 2,9.5V16.5C2,17.33 2.67,18 3.5,18C4.33,18 5,17.33 5,16.5V9.5C5,8.67 4.33,8 3.5,8M20.5,8C19.67,8 19,8.67 19,9.5V16.5C19,17.33 19.67,18 20.5,18C21.33,18 22,17.33 22,16.5V9.5C22,8.67 21.33,8 20.5,8M15.53,2.16L16.84,0.85C17.03,0.66 17.03,0.34 16.84,0.14C16.64,-0.05 16.32,-0.05 16.13,0.14L14.65,1.62C13.85,1.23 12.95,1 12,1C11.04,1 10.14,1.23 9.34,1.63L7.85,0.14C7.66,-0.05 7.34,-0.05 7.15,0.14C6.95,0.34 6.95,0.66 7.15,0.85L8.46,2.16C6.97,3.26 6,5.01 6,7H18C18,5.01 17.03,3.25 15.53,2.16M10,5H9V4H10V5M15,5H14V4H15V5Z" /></svg>
                                </span>
                                <span className="device-name">Android</span>
                                <span className="device-soon">Soon</span>
                            </div>

                        </div>
                    </div>

                </div>
                <div className="settings-footer">
                    <button className="settings-cancel-btn" id="settingsCancelBtn" type="button">Cancel</button>
                    <button className="settings-savebar-btn" id="settingsSaveBarBtn" type="button">Save</button>
                </div>
            </div>
        </div>
    );
}

function App() {
    return (
        <>
            {/* Splash Screen Overlay */}
            <IntroAnimation />

            <GlassFilters />

            {/* Main content: the chat, inside big_center's frame. #chatScroll scrolls
                it like a chat (container/chat_scroll): the chat's earlier turns
                (#chatPast), then the live one (#liveTurn), each top to bottom: the
                user's message (right), the screen (the run's video once it ends),
                then one container: tool-response chain (left half) | tracking
                progress (right half), swapped for the run's reply once it ends. The
                live turn's zones are injected by their components. (The old
                bottom-right todo list and the Agent Notes stage are parked in
                container/dead/.) */}
            {/* Transient top-center notification (driven by window.showToast) */}
            <div id="appToast" className="app-toast" role="status" aria-live="polite"></div>

            {/* Empty-state hero: centered "AutoCua / Agent with million moves." over the
                main area. Shown on a fresh/new chat (default at launch + after New chat);
                hidden the moment a task is sent or a saved chat is reopened. Toggled via
                window.showWelcomeHero / window.hideWelcomeHero. */}
            <div className="welcome-hero" id="welcomeHero" aria-hidden="true">
                <div className="welcome-hero-title">Made to do...</div>
            </div>

            <div className="main-grid" id="mainGrid">
                <div className="chat-scroll" id="chatScroll">
                    <div className="chat-past" id="chatPast"></div>
                    {/* The live turn's zones, top to bottom. */}
                    <div className="chat-turn live" id="liveTurn">
                        <UserContainer />
                        <ScreenContainer />
                        <AgentContainer />
                    </div>
                </div>
                {/* Big center: the main container, the always-on frame around the zones */}
                <BigCenterContainer />
            </div>

            {/* Chat input (the bottom text box): chat_input.js wires it and fires
                'chatinput:ready' for the parts inside it. */}
            <div className="chat-container-wrapper" id="chatWrapper"><ChatInput /></div>

            <SettingsOverlay />

            {/* AutoCua helper info popup (shown when Telegram Connect is clicked) */}
            <div className="telegram-prompt-overlay" id="telegramPromptOverlay">
                <div className="telegram-prompt-popup">
                    <div className="liquidGlass-effect-btn"></div>
                    <div className="liquidGlass-tint-btn"></div>
                    <div className="liquidGlass-shine-btn"></div>
                    <div className="telegram-prompt-content">
                        <div className="telegram-prompt-title">AutoCua helper</div>
                        <div className="telegram-prompt-countdown" id="telegramPromptCountdown">3</div>
                        <div className="telegram-prompt-caption">Starting your helper… it will appear in the <strong>top-right</strong> corner.</div>
                        <div className="telegram-prompt-message">The helper will walk you through Telegram setup. Please click <strong>Next</strong> on the banner in the <strong>top-right</strong> corner of your screen to continue.</div>
                        <button className="telegram-prompt-ok" id="telegramPromptOk" type="button">Got it</button>
                    </div>
                </div>
            </div>

            {/* GCP Vertex AI Popup */}
            <div className="vertex-overlay" id="vertexOverlay">
                <div className="vertex-popup">
                    <div className="liquidGlass-effect-btn"></div>
                    <div className="liquidGlass-tint-btn"></div>
                    <div className="liquidGlass-shine-btn"></div>
                    <div className="vertex-popup-content">
                        <span className="vertex-popup-title">GCP Vertex AI</span>
                        <div className="vertex-field">
                            <label className="vertex-label">Project ID</label>
                            <input type="text" className="vertex-input" id="vertexProjectId" placeholder="my-gcp-project" autoComplete="off" />
                        </div>
                        <div className="vertex-field">
                            <label className="vertex-label">Location</label>
                            <input type="text" className="vertex-input vertex-location-input" id="vertexLocation" defaultValue="global" autoComplete="off" />
                        </div>
                        <button className="vertex-done-btn" id="vertexDoneBtn">Done</button>
                    </div>
                </div>
            </div>

            {/* Shown while a Telegram task occupies the machine (fades the UI away) */}
            <div className="telegram-task-overlay" id="telegramTaskOverlay">
                <div className="telegram-task-content">
                    <div className="telegram-task-orb">
                        {/* The same stop orb the composer and the setup banner show
                            (chat_input/pc_button.html); it starts hidden and pops in
                            on 'pcbtn:show' from telegramOccupiedShow(). */}
                        <iframe id="telegramTaskOrbFrame" src="chat_input/pc_button.html?zoom=2.8" scrolling="no" frameBorder="0"></iframe>
                    </div>
                    <div className="telegram-task-message">Currently occupied by Telegram</div>
                </div>
            </div>

            {/* The left bar and its collapse toggle, last like the old loader appended them. */}
            <LeftBar />
        </>
    );
}

// Drawn right here, synchronously, so index.html's classic scripts below
// index.js find the page in place.
const host = document.getElementById('root');
if (host) {
    flushSync(() => {
        createRoot(host).render(<App />);
    });
}

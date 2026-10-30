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

                    {/* Connect Device: phone automation, iPhone (iOS/WDA) and Android
                        (adb + the phone helper). Wired by device_connect.js. */}
                    <div className="settings-view settings-device-view" id="settingsDeviceView">
                        <div className="settings-section-heading">Connect Device</div>
                        <div className="device-list">

                            {/* iPhone */}
                            <div className="device-option" id="deviceOptioniOS" role="button" tabIndex={0} aria-expanded="false" aria-label="Connect iPhone">
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
                                        is paired: picking iOS in the chat box just activates it.
                                        × forgets the device. */}
                                    <div className="dc-paired" id="dcPaired" hidden>
                                        <div className="dc-label">Paired devices</div>
                                        <div className="dc-paired-list" id="dcPairedList"></div>
                                    </div>
                                </div>
                            </div>

                            {/* Android */}
                            <div className="device-option" id="deviceOptionAndroid" role="button" tabIndex={0} aria-expanded="false" aria-label="Connect Android phone">
                                <span className="device-name">Android</span>
                                <span className="device-chevron" aria-hidden="true">
                                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round"><polyline points="9 6 15 12 9 18"></polyline></svg>
                                </span>
                            </div>

                            {/* Android expandable panel: the iPhone's flow for an Android
                                phone, with no account and nothing to install. Connect
                                reads the phone over USB, starts its helper and reads one
                                screen through it, then remembers it. Wired by
                                device_connect.js. */}
                            <div className="device-panel" id="devicePanelAndroid" hidden>
                                <div className="dc-flow">
                                    {/* Device dropdown (auto-selected) */}
                                    <div className="dc-field">
                                        <label className="dc-label">Device</label>
                                        <div className="dc-select" id="dcAndroidDeviceSelect" role="button" tabIndex={0}>
                                            <span className="dc-select-value placeholder" id="dcAndroidDeviceValue">Looking…</span>
                                            <ChevronDown className="dc-select-chevron" />
                                            <div className="dc-select-menu" id="dcAndroidDeviceMenu"></div>
                                        </div>
                                    </div>

                                    <div className="dc-status" id="dcAndroidStatus" data-state="loading">
                                        <span className="dc-spinner" id="dcAndroidSpinner"></span>
                                        <span className="dc-check" id="dcAndroidCheck" hidden></span>
                                        <span className="dc-status-text" id="dcAndroidStatusText">Looking for your Android phone…</span>
                                    </div>
                                    <div className="dc-steps" id="dcAndroidSteps" hidden></div>
                                    <button className="dc-action" id="dcAndroidAction" type="button" hidden></button>

                                    {/* Connected phones (android_paired/paired_devices.json): the
                                        Android pick in the chat box starts the newest one that is
                                        plugged in. × forgets the phone. */}
                                    <div className="dc-paired" id="dcAndroidPaired" hidden>
                                        <div className="dc-label">Connected devices</div>
                                        <div className="dc-paired-list" id="dcAndroidPairedList"></div>
                                    </div>
                                </div>
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

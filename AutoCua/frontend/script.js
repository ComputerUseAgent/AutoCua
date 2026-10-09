document.addEventListener('DOMContentLoaded', () => {
    // Splash screen: intro_animation.html handles macOS vs Windows rendering
    // internally and posts 'splashDone' back to us when finished.
    const splash = document.getElementById('splashOverlay');
    if (splash) {
        const splashFrame = document.getElementById('splashFrame');
        if (splashFrame && !splashFrame.src) {
            splashFrame.src = 'intro_animation.html';
        }
        window.addEventListener('message', (e) => {
            if (e.data === 'splashDone') {
                splash.style.pointerEvents = 'none';
                splash.classList.add('fade-out');
                splash.addEventListener('transitionend', () => splash.remove());
            }
        });
    }

    // Provider/model selection moved out of the floating button into
    // Settings → Model Selection (two dropdowns). See the selection logic below.

    // API Key Management - existence flags only (true/null), never actual keys
    const apiKeys = {
        openrouter: null,
        groq: null,
        openai: null,
        anthropic: null,
        perplexity: null,
        together: null,
        cerebras: null,
        aws: null
    };


    // ============================================
    // PROVIDER / MODEL SELECTION (Settings → Model Selection)
    // Two dropdowns: pick a provider, then a model. The chosen pair is stored
    // in selectedProvider/selectedModel and drives /api/start-agent.
    // ============================================
    let selectedProvider = null;
    let selectedModel = null;
    let providersData = [];   // [{ id, name, models: [{ id, display_name }] }]

    // Query the chat input on demand (it's declared later in this closure).
    const getChatInput = () => document.querySelector('.chat-input');

    // The chat-box drop-up (model picker) — wired on 'chatinput:ready' below.
    let chatDropdown = null;

    // Expose the live (provider, model) selection so the chat-input wiring
    // (chat_input/chat_input.js) can read it. setProvider/setModel are the writers.
    window.getModelSelection = () => ({ provider: selectedProvider, model: selectedModel });

    // Small transient top-center notification (e.g. "Please set the provider in Settings").
    let _toastTimer = null;
    window.showToast = (message, ms = 4000) => {
        const el = document.getElementById('appToast');
        if (!el || !message) return;
        el.textContent = message;
        el.classList.add('active');
        clearTimeout(_toastTimer);
        _toastTimer = setTimeout(() => el.classList.remove('active'), ms);
    };

    // Persist the choice so it auto-loads next launch (selection only — keys stay
    // server-side). Partial: send just {provider}, just {model}, or both.
    const persistSelection = (updates) => {
        fetch('/api/last-selection', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(updates)
        }).catch(() => { /* non-fatal */ });
    };

    // Enable/disable the chat textarea (queried lazily — chat_input.js injects it).
    const gateInput = (enabled, placeholder) => {
        const ci = getChatInput();
        if (!ci) return;
        ci.disabled = !enabled;
        if (placeholder) ci.placeholder = placeholder;
    };

    // Models of the active provider (drives the chat drop-up menu).
    const activeProviderModels = () => {
        const p = providersData.find(x => x.id === selectedProvider);
        return (p && p.models) || [];
    };

    // Sync the chat drop-up: fill it with the active provider's models, and set its
    // label — the chosen model's name, else the provider name, else "select model".
    const refreshChatDropdown = () => {
        if (!chatDropdown) return;
        const models = activeProviderModels();
        chatDropdown.setItems(models.map(m => ({
            value: m.id, label: m.display_name
        })));
        chatDropdown.setDisabled(!selectedProvider);   // kept clickable via CSS so it can toast
        if (selectedModel && models.some(m => m.id === selectedModel)) {
            chatDropdown.select(selectedModel);          // label = model name
        } else if (selectedProvider) {
            chatDropdown.setPlaceholder(selectedProvider);   // label = provider name
        } else {
            chatDropdown.setPlaceholder('select model');
        }
    };

    // Gate the chat input on whether the provider's key (or Vertex config, for
    // vertex models) is present; toast + reveal key rows if missing.
    const gateOnConfig = (providerId, modelId) => {
        if (!providerId || !modelId) { gateInput(false, 'Select a model…'); return; }
        if (modelId.includes('vertex')) {
            fetch('/api/vertex/status').then(r => r.json()).then(data => {
                if (data.project_id) { gateInput(true, 'Type your task...'); }
                else { gateInput(false, 'Configure GCP Vertex in Settings first...'); loadKeyStatus(); if (window.showToast) window.showToast('Configure GCP Vertex in Settings'); }
            }).catch(() => {});
            return;
        }
        fetch('/api/keys/status').then(r => r.json()).then(status => {
            if (status[providerId]) { gateInput(true, 'Type your task...'); }
            else { gateInput(false, 'Add the API key in Settings first...'); loadKeyStatus(); if (window.showToast) window.showToast('Add the ' + providerId + ' API key in Settings'); }
        }).catch(() => {});
    };

    // Provider chosen in Settings: clear the model, persist, refresh the drop-up,
    // and keep the input disabled until a model is picked.
    const setProvider = (providerId) => {
        selectedProvider = providerId || null;
        selectedModel = null;
        persistSelection({ provider: selectedProvider, model: null });
        refreshChatDropdown();
        gateInput(false, providerId ? 'Select a model…' : 'Select a model in Settings to start...');
        if (providerId) {
            fetch('/api/keys/status').then(r => r.json()).then(s => {
                if (!s[providerId]) { loadKeyStatus(); if (window.showToast) window.showToast('Add the ' + providerId + ' API key in Settings'); }
            }).catch(() => {});
        }
    };

    // Model chosen in the chat drop-up: persist, relabel the drop-up, gate the input.
    const setModel = (modelId) => {
        selectedModel = modelId || null;
        persistSelection({ model: selectedModel });
        if (chatDropdown && selectedModel) chatDropdown.select(selectedModel);
        gateOnConfig(selectedProvider, selectedModel);
    };

    // On launch: load the saved provider/model and apply it, or prompt if unusable.
    const restoreSelection = () => {
        fetch('/api/last-selection').then(r => r.json()).then(sel => {
            sel = sel || {};
            const known = providersData.find(p => p.id === sel.provider);
            if (!sel.provider || !known) {                 // nothing usable saved
                refreshChatDropdown();                     // "select model"
                if (window.showToast) window.showToast('Please set the provider in Settings');
                return;
            }
            fetch('/api/keys/status').then(r => r.json()).then(status => {
                // Apply the saved selection ONLY if the provider still has a key (or, for
                // google, Vertex is configured). A keyless provider can't be selected, so a
                // saved-but-now-keyless one reverts to "select model" + a disabled input.
                const finalize = (available) => {
                    if (!available) {
                        selectedProvider = null;
                        selectedModel = null;
                        if (providerDropdown) providerDropdown.setPlaceholder('Select a provider…');
                        refreshChatDropdown();
                        gateInput(false, 'Add the API key in Settings first...');
                        if (window.showToast) window.showToast('Add the ' + (known.name || sel.provider) + ' API key in Settings');
                        return;
                    }
                    selectedProvider = sel.provider;
                    if (providerDropdown) providerDropdown.select(sel.provider);
                    const models = known.models || [];
                    if (sel.model && models.some(m => m.id === sel.model)) selectedModel = sel.model;
                    refreshChatDropdown();
                    if (selectedModel) gateOnConfig(sel.provider, selectedModel);
                    else gateInput(false, 'Select a model…');
                };

                if (status[sel.provider]) {
                    finalize(true);
                } else if (sel.provider === 'google') {
                    // Google can also be enabled via Vertex (no direct API key).
                    fetch('/api/vertex/status').then(r => r.json())
                        .then(d => finalize(!!d.project_id)).catch(() => finalize(false));
                } else {
                    finalize(false);
                }
            }).catch(() => { refreshChatDropdown(); });
        }).catch(() => { refreshChatDropdown(); });
    };

    // Lightweight custom dropdown — NOT a native <select>, which can't give us the
    // glass menu, the per-option "why it's unavailable" hint, or a disabled option
    // that toasts instead of selecting. items: [{ value, label, disabled, hint, disabledMsg }].
    const makeDropdown = (rootId, onChange) => {
        const root = document.getElementById(rootId);
        if (!root) return null;
        const trigger = root.querySelector('.model-dropdown-trigger');
        const valueEl = root.querySelector('.model-dropdown-value');
        const menu = root.querySelector('.model-dropdown-menu');
        let value = '';

        // Clicks inside the menu (its padding, the scrollbar) must NOT bubble to the
        // document close-handler — only choosing an option closes the dropdown. Without
        // this, interacting with the scrollbar collapses the list.
        if (menu) menu.addEventListener('click', (e) => e.stopPropagation());

        // Wheel/trackpad scrolling: WebKit (the WKWebView host) refuses to natively
        // async-scroll this overflow menu because it sits inside a position:fixed +
        // transform + backdrop-filter subtree, so long lists (e.g. OpenRouter's ~17
        // models) looked frozen and the top items were unreachable. Programmatic
        // scrollTop DOES work there, so drive it ourselves from the wheel event (which
        // real trackpad/wheel input fires regardless). No-op when nothing overflows, so
        // the short settings provider list is unaffected.
        if (menu) menu.addEventListener('wheel', (e) => {
            if (menu.scrollHeight <= menu.clientHeight) return;
            const step = e.deltaMode === 1 ? e.deltaY * 16 : e.deltaY;
            menu.scrollTop += step;
            e.preventDefault();
        }, { passive: false });

        trigger.addEventListener('click', (e) => {
            e.stopPropagation();
            if (root.classList.contains('disabled')) return;
            const willOpen = !root.classList.contains('open');
            document.querySelectorAll('.model-dropdown.open').forEach(d => d.classList.remove('open'));
            if (willOpen) root.classList.add('open');
        });

        const api = {
            get value() { return value; },
            setDisabled(d) { root.classList.toggle('disabled', !!d); },
            setPlaceholder(text) {
                value = '';
                valueEl.innerHTML = '';
                valueEl.textContent = text;
                valueEl.classList.add('placeholder');
            },
            setItems(items) {
                menu.innerHTML = '';
                if (!items.length) {
                    const empty = document.createElement('div');
                    empty.className = 'model-dropdown-empty';
                    empty.textContent = 'None';
                    menu.appendChild(empty);
                    return;
                }
                items.forEach(it => {
                    const opt = document.createElement('div');
                    opt.className = 'model-dropdown-option' + (it.disabled ? ' model-dropdown-option--disabled' : '');
                    opt.dataset.value = it.value;
                    const label = document.createElement('span');
                    label.className = 'model-dropdown-option-label';
                    label.textContent = it.label;
                    opt.appendChild(label);
                    if (it.disabled) {
                        // Show why it can't be picked (e.g. no API key).
                        const hint = document.createElement('span');
                        hint.className = 'model-dropdown-option-hint';
                        hint.textContent = it.hint || 'unavailable';
                        opt.appendChild(hint);
                    }
                    opt.addEventListener('click', (e) => {
                        e.stopPropagation();
                        // A disabled option (no key) can't be selected — toast the disclaimer
                        // and keep the menu open so the rest stay visible.
                        if (it.disabled) {
                            if (window.showToast && it.disabledMsg) window.showToast(it.disabledMsg);
                            return;
                        }
                        api.select(it.value);
                        root.classList.remove('open');
                        if (onChange) onChange(it.value);
                    });
                    menu.appendChild(opt);
                });
            },
            select(v) {
                value = v || '';
                let chosen = null;
                menu.querySelectorAll('.model-dropdown-option').forEach(o => {
                    const match = o.dataset.value === String(v);
                    o.classList.toggle('selected', match);
                    if (match) chosen = o;
                });
                if (chosen) {
                    valueEl.innerHTML = '';
                    const label = document.createElement('span');
                    label.textContent = chosen.querySelector('.model-dropdown-option-label').textContent;
                    valueEl.appendChild(label);
                    valueEl.classList.remove('placeholder');
                }
            },
        };
        return api;
    };

    const providerDropdown = makeDropdown('providerDropdown', (providerId) => {
        if (providerId) setProvider(providerId);   // model is now chosen in the chat drop-up
    });

    // Close any open dropdown when clicking elsewhere.
    document.addEventListener('click', () => {
        document.querySelectorAll('.model-dropdown.open').forEach(d => d.classList.remove('open'));
    });

    // Populate the Provider dropdown from providersData, restoring any current
    // selection so reopening Settings shows the active choice. Providers with NO API
    // key are shown greyed + non-selectable (clicking one toasts a disclaimer) so the
    // user can't pick a model — and thus can't start the agent — without a key. Kept in
    // sync with apiKeys, which loadKeyStatus / save / delete refresh.
    const populateProviderSelect = () => {
        if (!providerDropdown) return;
        providerDropdown.setItems(providersData.map(p => ({
            value: p.id,
            label: p.name,
            disabled: !apiKeys[p.id],
            hint: 'API key needed',
            disabledMsg: 'Add the ' + p.name + ' API key in Settings first',
        })));
        if (selectedProvider) providerDropdown.select(selectedProvider);
        else providerDropdown.setPlaceholder(providersData.length ? 'Select a provider…' : 'No providers found');
    };

    // Fetch providers and populate the dropdowns.
    const loadData = () => {
        fetch('/api/providers')
            .then(response => response.json())
            .then(providers => {
                providersData = Array.isArray(providers) ? providers : [];
                populateProviderSelect();
                restoreSelection();   // auto-load the last provider/model on launch
            })
            .catch(err => {
                console.error('Failed to load providers:', err);
                providersData = [];
                populateProviderSelect();
                restoreSelection();
            });
    };

    // The settings button now lives in the left bar footer (settings/ component),
    // so it no longer needs JS positioning next to the LLM button.

    // Load immediately
    loadData();

    // Load API key status on startup
    fetch('/api/keys/status')
        .then(res => res.json())
        .then(status => {
            Object.keys(status).forEach(provider => {
                apiKeys[provider] = status[provider] ? true : null;
            });
            populateProviderSelect();   // re-gate the provider dropdown now keys are known
        })
        .catch(err => console.error('Failed to load key status:', err));

    // Wire the chat-box drop-up (model picker) once chat_input.js injects it. It
    // reuses makeDropdown; picking a model calls setModel. Clicking it with no
    // provider set nudges the user to Settings.
    const wireChatDropdown = () => {
        if (chatDropdown) return;
        chatDropdown = makeDropdown('chatModelDropdown', (modelId) => { if (modelId) setModel(modelId); });
        if (!chatDropdown) return;
        const trig = document.querySelector('#chatModelDropdown .model-dropdown-trigger');
        if (trig) trig.addEventListener('click', () => {
            if (!selectedProvider && window.showToast) window.showToast('Please set the provider in Settings');
        });
        refreshChatDropdown();   // sync label/menu from any already-restored selection
    };
    if (document.querySelector('#chatModelDropdown')) wireChatDropdown();
    else document.addEventListener('chatinput:ready', wireChatDropdown);

    // ============================================
    // SETTINGS PANEL - Open / Close
    // ============================================
    const settingsOverlay = document.getElementById('settingsOverlay');
    const settingsSaveBtn = document.getElementById('settingsSaveBtn');
    const settingsMenuView = document.getElementById('settingsMenuView');
    const settingsModelView = document.getElementById('settingsModelView');
    const settingsApikeysView = document.getElementById('settingsApikeysView');
    const settingsRemoteView = document.getElementById('settingsRemoteView');

    // Helper: seal a provider row (key exists)
    const sealProviderRow = (row) => {
        const input = row.querySelector('.settings-provider-input');
        const deleteBtn = row.querySelector('.settings-delete-btn');
        input.value = '';
        input.placeholder = 'API Key already exists';
        input.disabled = true;
        deleteBtn.style.display = 'flex';
    };

    // Helper: unseal a provider row (no key)
    const unsealProviderRow = (row) => {
        const input = row.querySelector('.settings-provider-input');
        const deleteBtn = row.querySelector('.settings-delete-btn');
        const provider = row.dataset.provider;
        const placeholders = { openrouter: 'sk-or-...', groq: 'gsk_...', openai: 'sk-...', anthropic: 'sk-ant-...', google: 'AIza...', cerebras: 'csk-...', aws: 'ABSK...' };
        input.value = '';
        input.placeholder = placeholders[provider] || 'Enter API key...';
        input.disabled = false;
        deleteBtn.style.display = 'none';
    };

    // Load key status from backend
    const loadKeyStatus = () => {
        fetch('/api/keys/status')
            .then(res => res.json())
            .then(status => {
                document.querySelectorAll('.settings-provider-row').forEach(row => {
                    const provider = row.dataset.provider;
                    if (status[provider]) {
                        sealProviderRow(row);
                    } else {
                        unsealProviderRow(row);
                    }
                });
                // Mirror into apiKeys so the provider dropdown gates correctly, then re-gate.
                document.querySelectorAll('.settings-provider-row').forEach(row => {
                    const provider = row.dataset.provider;
                    apiKeys[provider] = status[provider] ? true : null;
                });
                populateProviderSelect();
                // If Google has no API key, check if Vertex is configured instead
                if (!status['google']) {
                    fetch('/api/vertex/status')
                        .then(res => res.json())
                        .then(data => {
                            if (data.project_id) {
                                const googleRow = document.querySelector('.settings-provider-row[data-provider="google"]');
                                if (googleRow) {
                                    sealProviderRow(googleRow);
                                    const input = googleRow.querySelector('.settings-provider-input');
                                    if (input) input.placeholder = 'Vertex AI configured';
                                }
                                // Vertex counts as "google available" for provider gating.
                                apiKeys['google'] = true;
                                populateProviderSelect();
                            }
                        })
                        .catch(() => {});
                }
            })
            .catch(err => console.error('Failed to load key status:', err));
    };

    // Delete button handlers
    document.querySelectorAll('.settings-delete-btn').forEach(btn => {
        btn.addEventListener('click', () => {
            const row = btn.closest('.settings-provider-row');
            const provider = row.dataset.provider;

            fetch('/api/keys/delete', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ provider })
            })
            .then(res => res.json())
            .then(() => {
                unsealProviderRow(row);
                // Clear in-memory flag
                apiKeys[provider] = null;
                // Lock input and reset if deleted key belongs to the active provider —
                // the chat box reverts to "select model" and the agent can't start.
                if (provider === selectedProvider) {
                    selectedProvider = null;
                    selectedModel = null;
                    persistSelection({ provider: null, model: null });
                    gateInput(false, 'Select a model in Settings to start...');
                    const ci = getChatInput(); if (ci) ci.value = '';
                    refreshChatDropdown();      // chat drop-up back to "select model"
                }
                // Always re-gate the provider dropdown so the now-keyless provider greys out.
                populateProviderSelect();
            })
            .catch(err => console.error('Failed to delete key:', err));
        });
    });

    // ============================================
    // GCP VERTEX AI POPUP
    // ============================================
    const gcpVertexBtn = document.getElementById('gcpVertexBtn');
    const vertexOverlay = document.getElementById('vertexOverlay');
    const vertexDoneBtn = document.getElementById('vertexDoneBtn');
    const vertexProjectId = document.getElementById('vertexProjectId');
    const vertexLocation = document.getElementById('vertexLocation');

    // Location field — muted by default, vivid on edit
    if (vertexLocation) {
        vertexLocation.addEventListener('focus', () => {
            if (vertexLocation.value === 'global') {
                vertexLocation.select();
            }
            vertexLocation.classList.add('edited');
        });
        vertexLocation.addEventListener('blur', () => {
            if (!vertexLocation.value.trim()) {
                vertexLocation.value = 'global';
            }
            if (vertexLocation.value === 'global') {
                vertexLocation.classList.remove('edited');
            }
        });
    }

    // Open vertex popup
    if (gcpVertexBtn && vertexOverlay) {
        gcpVertexBtn.addEventListener('click', (e) => {
            e.stopPropagation();
            // Load existing vertex config
            fetch('/api/vertex/status')
                .then(res => res.json())
                .then(data => {
                    if (data.project_id) vertexProjectId.value = data.project_id;
                    const loc = data.location || 'global';
                    vertexLocation.value = loc;
                    if (loc !== 'global') vertexLocation.classList.add('edited');
                    else vertexLocation.classList.remove('edited');
                })
                .catch(() => {});
            vertexOverlay.classList.add('active');
        });

        // Close on overlay click
        vertexOverlay.addEventListener('click', (e) => {
            if (e.target === vertexOverlay) {
                vertexOverlay.classList.remove('active');
            }
        });
    }

    // Save vertex config
    if (vertexDoneBtn) {
        vertexDoneBtn.addEventListener('click', () => {
            const projectId = vertexProjectId.value.trim();
            if (!projectId) {
                vertexProjectId.style.animation = 'shake 0.4s ease';
                setTimeout(() => vertexProjectId.style.animation = '', 400);
                return;
            }
            const location = vertexLocation.value.trim() || 'global';
            fetch('/api/vertex/save', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ project_id: projectId, location: location })
            })
            .then(res => res.json())
            .then(() => {
                gcpVertexBtn.classList.add('configured');
                vertexOverlay.classList.remove('active');
                // Unlock chat input if a Vertex model is currently selected
                if (selectedModel && selectedModel.includes('vertex')) {
                    gateInput(true, 'Type your task...');
                }
            })
            .catch(err => console.error('Failed to save vertex config:', err));
        });
    }

    // Load vertex status on startup to tint button
    fetch('/api/vertex/status')
        .then(res => res.json())
        .then(data => {
            if (data.project_id) {
                if (gcpVertexBtn) gcpVertexBtn.classList.add('configured');
                // Vertex configured counts as "google available" for provider gating.
                apiKeys['google'] = true;
                populateProviderSelect();
            }
        })
        .catch(() => {});

    // Helper: switch settings view
    // Flat layout: all sections are always visible, so view-switching is a no-op.
    // (Kept as a function so existing callers don't need changing.)
    const showSettingsView = () => {};

    // Helper: reset to menu view when closing
    const resetSettingsToMenu = () => {
        settingsOverlay.classList.remove('active');
        document.body.classList.remove('settings-open');  // restores the chat text box
        // Reset to menu after transition completes
        setTimeout(() => showSettingsView('menu'), 300);
    };

    if (settingsOverlay) {
        // The settings button now lives in the left bar (settings/ component);
        // it emits 'open-settings'. The overlay + its data loaders stay here.
        document.addEventListener('open-settings', () => {
            // Flat layout: populate every section up front (no menu/nesting).
            loadKeyStatus();
            populateProviderSelect();
            loadRemoteStatus();
            settingsOverlay.classList.add('active');
            document.body.classList.add('settings-open');  // hides the chat text box
        });

        // (No ✕ close button — the footer Cancel closes the panel.)

        // Menu item navigation
        document.querySelectorAll('.settings-menu-item').forEach(item => {
            item.addEventListener('click', () => {
                const view = item.dataset.view;
                if (view === 'modelselect') {
                    showSettingsView('modelselect');
                    populateProviderSelect();
                } else if (view === 'apikeys') {
                    loadKeyStatus();
                    showSettingsView('apikeys');
                } else if (view === 'remote') {
                    showSettingsView('remote');
                    loadRemoteStatus();
                }
            });
        });

        // Back buttons
        document.querySelectorAll('.settings-back-btn').forEach(btn => {
            btn.addEventListener('click', () => {
                showSettingsView('menu');
            });
        });

        // Save button (API Keys)
        settingsSaveBtn.addEventListener('click', () => {
            const savePromises = [];
            const savedProviders = [];   // providers whose key was just added (for auto-select)
            document.querySelectorAll('.settings-provider-row').forEach(row => {
                const input = row.querySelector('.settings-provider-input');
                const provider = row.dataset.provider;
                const value = input.value.trim();

                if (!input.disabled && value) {
                    savePromises.push(
                        fetch('/api/keys/save', {
                            method: 'POST',
                            headers: { 'Content-Type': 'application/json' },
                            body: JSON.stringify({ provider, key: value })
                        })
                        .then(res => res.json())
                        .then(() => {
                            sealProviderRow(row);
                            apiKeys[provider] = true;
                            savedProviders.push(provider);
                        })
                    );
                }
            });

            Promise.all(savePromises)
                .then(() => {
                    // Newly-keyed providers are now selectable — re-gate the dropdown.
                    populateProviderSelect();
                    // Key exists => model selection should now be possible. If the user
                    // hadn't picked a provider yet, auto-select the one they just keyed so
                    // the chat model drop-up unlocks immediately (no separate provider step).
                    if (!selectedProvider && savedProviders.length) {
                        setProvider(savedProviders[0]);
                    }
                    resetSettingsToMenu();
                    if (selectedProvider && selectedModel) {
                        const isVertex = selectedModel.includes('vertex');
                        if (isVertex) {
                            fetch('/api/vertex/status')
                                .then(res => res.json())
                                .then(data => {
                                    gateInput(!!data.project_id, data.project_id ? 'Type your task...' : 'Configure GCP Vertex in Settings first...');
                                })
                                .catch(() => {});
                        } else if (apiKeys[selectedProvider]) {
                            gateInput(true, 'Type your task...');
                        }
                    }
                })
                .catch(err => {
                    console.error('Failed to save keys:', err);
                    resetSettingsToMenu();
                });
        });

        // Right-side panel footer: Cancel closes; Save commits the current view
        // (saves API keys when on that view) then closes.
        const settingsCancelBtn = document.getElementById('settingsCancelBtn');
        const settingsSaveBarBtn = document.getElementById('settingsSaveBarBtn');
        if (settingsCancelBtn) {
            settingsCancelBtn.addEventListener('click', () => resetSettingsToMenu());
        }
        if (settingsSaveBarBtn) {
            settingsSaveBarBtn.addEventListener('click', () => {
                // Flat layout: API-key inputs are always present, so just commit
                // any entered keys; the save handler closes via resetSettingsToMenu.
                settingsSaveBtn.click();
            });
        }

        // Remote Connection — guided Telegram pairing
        const remoteSetup = document.getElementById('remoteSetup');
        const remoteToggleBtn = document.getElementById('remoteToggleBtn');
        const remoteInstructions = document.getElementById('remoteInstructions');
        const telegramPromptOverlay = document.getElementById('telegramPromptOverlay');
        const telegramPromptOk = document.getElementById('telegramPromptOk');
        let telegramConnected = false;

        // One button that transforms: "Connect" when not paired, "Disconnect"
        // (red) when paired.
        function updateRemoteToggle() {
            if (!remoteToggleBtn) return;
            remoteToggleBtn.disabled = false;
            remoteToggleBtn.textContent = telegramConnected ? 'Disconnect' : 'Connect';
            remoteToggleBtn.classList.toggle('disconnect', telegramConnected);
        }

        function loadRemoteStatus() {
            if (remoteSetup) remoteSetup.style.display = 'flex';
            if (remoteInstructions) remoteInstructions.style.display = 'none';
            fetch('/api/telegram/status')
                .then(res => res.json())
                .then(data => { telegramConnected = !!data.connected; updateRemoteToggle(); })
                .catch(() => { telegramConnected = false; updateRemoteToggle(); });
        }

        // 3-2-1 countdown shown in the "AutoCua helper" popup while the helper
        // banner cold-starts (~3s). Pure UI feedback — the banner is spawned by
        // the /api/telegram/connect POST below and appears ~when the count ends.
        const telegramPromptPopup = telegramPromptOverlay
            ? telegramPromptOverlay.querySelector('.telegram-prompt-popup')
            : null;
        const telegramPromptCountdown = document.getElementById('telegramPromptCountdown');
        let telegramCountdownTimer = null;

        function startTelegramCountdown() {
            if (!telegramPromptPopup || !telegramPromptCountdown) return;
            if (telegramCountdownTimer) {
                clearInterval(telegramCountdownTimer);
                telegramCountdownTimer = null;
            }
            let n = 3;
            const render = () => {
                telegramPromptCountdown.textContent = n;
                telegramPromptCountdown.classList.remove('tick');
                void telegramPromptCountdown.offsetWidth; // reflow to restart the pop animation
                telegramPromptCountdown.classList.add('tick');
            };
            telegramPromptPopup.classList.add('counting');
            render();
            telegramCountdownTimer = setInterval(() => {
                n -= 1;
                if (n >= 1) {
                    render();
                } else {
                    clearInterval(telegramCountdownTimer);
                    telegramCountdownTimer = null;
                    // Reveal the static instructions + Got it button.
                    telegramPromptPopup.classList.remove('counting');
                }
            }, 1000);
        }

        function stopTelegramCountdown() {
            if (telegramCountdownTimer) {
                clearInterval(telegramCountdownTimer);
                telegramCountdownTimer = null;
            }
            if (telegramPromptPopup) telegramPromptPopup.classList.remove('counting');
        }

        if (remoteToggleBtn) {
            remoteToggleBtn.addEventListener('click', () => {
                if (telegramConnected) {
                    // Acts as Disconnect — remove the token.
                    remoteToggleBtn.disabled = true;
                    fetch('/api/telegram/disconnect', { method: 'POST' })
                        .then(() => loadRemoteStatus())
                        .catch(() => {})
                        .finally(() => { remoteToggleBtn.disabled = false; });
                } else {
                    // Acts as Connect — start the pairing flow.
                    remoteToggleBtn.disabled = true;
                    if (telegramPromptOverlay) telegramPromptOverlay.classList.add('active');
                    startTelegramCountdown();
                    fetch('/api/telegram/connect', { method: 'POST' })
                        .catch(() => {})
                        .finally(() => {
                            remoteToggleBtn.disabled = false;
                            if (remoteInstructions) remoteInstructions.style.display = 'block';
                        });
                }
            });
        }

        if (telegramPromptOk && telegramPromptOverlay) {
            telegramPromptOk.addEventListener('click', () => {
                telegramPromptOverlay.classList.remove('active');
                stopTelegramCountdown();
            });
            telegramPromptOverlay.addEventListener('click', (e) => {
                if (e.target === telegramPromptOverlay) {
                    telegramPromptOverlay.classList.remove('active');
                    stopTelegramCountdown();
                }
            });
        }

        settingsOverlay.addEventListener('click', (e) => {
            if (e.target === settingsOverlay) {
                resetSettingsToMenu();
            }
        });

        document.addEventListener('keydown', (e) => {
            if (e.key === 'Escape' && settingsOverlay.classList.contains('active')) {
                resetSettingsToMenu();
            }
        });
    }

    // Function to update the image stream (called from Python)
    window.updateAgentImage = (base64Image) => {
        const imgElement = document.querySelector('.stream-image');
        if (imgElement && base64Image) {
            // Tall (phone) frames fit by height via .portrait (object-fit:
            // contain); landscape desktop frames keep the cover crop.
            imgElement.onload = () => {
                imgElement.classList.toggle('portrait', imgElement.naturalHeight > imgElement.naturalWidth);
            };
            // This element receives two different formats: the plain preview is
            // JPEG, but in DEBUG mode it gets the annotated PNG instead. Sniff the
            // base64 magic prefix rather than hardcoding either one.
            const mime = base64Image.startsWith('iVBORw0KGgo') ? 'image/png' : 'image/jpeg';
            imgElement.src = `data:${mime};base64,${base64Image}`;
            imgElement.style.display = 'block'; // Show image when data arrives
        }
    };

    // Clear the streamed screenshot — symmetric with updateAgentImage. Used when
    // resetting the main view between chats/runs so a new chat never shows the
    // previous run's screen.
    window.clearAgentImage = () => {
        const imgElement = document.querySelector('.stream-image');
        if (imgElement) {
            imgElement.removeAttribute('src');
            imgElement.style.display = 'none';
        }
    };
    
    // Scratchpad/milestone streaming — the backend calls this once per entry
    // (~every 1s). It feeds the LIVE "tracking progress" stream in the top-right
    // cell (container/top_right), which types each entry on a circle-bullet line
    // and fades away when the run ends. (The old #milestoneStream node was
    // orphaned/hidden; this is the visible home now.)
    window.streamMilestone = (text) => {
        if (window.trackingProgress) window.trackingProgress.push(text);
    };

    // Letter-by-letter streaming for agent text in the response strip.
    // Each char is its own span with an opacity fade so the line types
    // out smoothly. When a char would overflow the strip's right edge,
    // we clear and restart with that same char at the left (matches the
    // Telegram banner's pager behavior, just inside a single-line strip
    // instead of a pill).
    let streamingTimeout = null;
    // Lets the chat-input stop handler (chat_input/chat_input.js) cancel an in-flight
    // agent-text stream without owning the timer (script.js stays the single owner).
    window.stopStreaming = () => {
        if (streamingTimeout) { clearTimeout(streamingTimeout); streamingTimeout = null; }
    };
    window.streamAgentText = (text) => {
        const agentText = document.getElementById('agentText');
        const agentStrip = document.getElementById('agentResponseStrip');

        if (!agentText || !agentStrip) return;

        agentStrip.classList.add('active');

        if (streamingTimeout) {
            clearTimeout(streamingTimeout);
        }

        // Array.from preserves emoji as single tokens.
        const chars = Array.from(text);
        let i = 0;
        const CHAR_DELAY_MS = 0;       // near-instant tick (browser clamps the minimum)
        const CHARS_PER_TICK = 6;      // append several chars per tick → very fast typing
        const FADE_MS = 45;

        // Start with a clean strip — previous step's text would otherwise
        // be measured into the new overflow check.
        agentText.textContent = '';

        const appendCharFade = (ch) => {
            const span = document.createElement('span');
            span.textContent = ch;
            span.style.opacity = '0';
            span.style.transition = 'opacity ' + FADE_MS + 'ms ease-out';
            agentText.appendChild(span);
            requestAnimationFrame(() => { span.style.opacity = '1'; });
            return span;
        };

        // Pump a batch of chars per tick (fast), checking overflow per char so
        // the strip still wraps to a fresh line at its right edge.
        const streamChar = () => {
            let n = 0;
            while (n < CHARS_PER_TICK && i < chars.length) {
                appendCharFade(chars[i]);

                // Sync layout read forces reflow → did this char push past the
                // strip's right edge? If so (and it isn't the only char), clear
                // the strip and re-place this char at the left of a fresh line.
                if (agentText.scrollWidth > agentText.clientWidth + 0.5) {
                    if (agentText.childElementCount === 1) {
                        i++; n++;                 // single oversized char — accept and move on
                        continue;
                    }
                    agentText.textContent = '';
                    while (i < chars.length && /\s/.test(chars[i])) i++;  // skip leading whitespace
                    continue;                     // re-process chars[i] on the fresh line
                }

                i++; n++;
            }
            if (i < chars.length) streamingTimeout = setTimeout(streamChar, CHAR_DELAY_MS);
        };

        streamChar();
    };

    // Agent completion handler (called from Python when agent finishes naturally)
    // Active chat session id (null => brand-new chat). Owned here so chat_input.js
    // and chat.js can read/adopt it; the durable memory lives in the backend
    // (AutoCua/agent_conversation). A continued chat carries its id; a new chat
    // adopts the id the backend mints on the first send.
    if (typeof window.currentSessionId === 'undefined') window.currentSessionId = null;

    // Empty-state hero ("AutoCua / Agent with million moves."): visible on a
    // fresh/new chat (default at launch), hidden the moment a task is sent or a
    // saved chat is reopened.
    window.showWelcomeHero = () => {
        const el = document.getElementById('welcomeHero');
        if (el) el.classList.remove('hidden');
    };
    window.hideWelcomeHero = () => {
        const el = document.getElementById('welcomeHero');
        if (el) el.classList.add('hidden');
    };

    window.agentComplete = () => {
        const stopBtnFrame = document.getElementById('stopBtnFrame');
        const agentStrip = document.getElementById('agentResponseStrip');
        const chatInput = document.querySelector('.chat-input');
        
        // Clear any streaming
        if (streamingTimeout) {
            clearTimeout(streamingTimeout);
            streamingTimeout = null;
        }
        
        // Hide Stop Button: let the embedded orb play its pop-vanish, then drop pointer-events.
        if (stopBtnFrame) {
            stopBtnFrame.contentWindow.postMessage('pcbtn:vanish', '*');
            stopBtnFrame.classList.remove('active');
        }
        
        // Force-close any active tool animations immediately
        if (window.webSearchEnd) window.webSearchEnd();
        if (window.shellEnd) window.shellEnd();
        if (window.trackingProgress) window.trackingProgress.end();   // fade the tracking-progress stream out

        // Agent finished. If it called `done` but left a task still spinning (it
        // never marked it complete), freeze the list and ✕ those tasks so the
        // spinner doesn't rotate forever. Delayed briefly so the run's FINAL todo
        // update (an all-done state pushed right as it ends) can land first — if
        // every task ends up done, markTodoInterrupted no-ops.
        setTimeout(function () {
            if (window.markTodoInterrupted) window.markTodoInterrupted();
        }, 500);

        // Hide Strip
        if (agentStrip) agentStrip.classList.remove('active');

        // Enable Input
        if (chatInput) {
            chatInput.disabled = false;
            chatInput.classList.remove('agent-active');
            chatInput.focus();
        }

        // Morph the composer back to idle: re-show the grey send orb and re-expand the
        // box to its ~2-line height. Owned by chat_input.js (single, idempotent teardown
        // shared with the manual-stop path); guarded in case it injects after this fires.
        if (window.chatInputRestoreIdle) window.chatInputRestoreIdle();

        // The run just ended (success / error / stop all route through here) — the
        // backend has saved this session, so refresh the sidebar history list to
        // pick up the new title / last-done message (and surface a brand-new chat).
        document.dispatchEvent(new CustomEvent('chats:refresh'));
    };

    // Agent failure handler (called from Python when the run ends in error or
    // stops without completing). Surfaces the reason in the persistent
    // milestone stream — which is only cleared on the NEXT run — so the user
    // sees WHY it ended instead of a silent return to idle, then performs the
    // same teardown as agentComplete.
    window.agentError = (msg) => {
        const milestoneStream = document.getElementById('milestoneStream');
        if (milestoneStream && msg) {
            const line = document.createElement('div');
            line.className = 'milestone-line milestone-line--error';
            line.textContent = msg;
            milestoneStream.appendChild(line);
            if (milestoneStream.parentElement) {
                milestoneStream.parentElement.scrollTop = milestoneStream.parentElement.scrollHeight;
            }
        }
        window.agentComplete();
    };

    // ============================================
    // BOTTOM-RIGHT WORK STATE — todo only. (The old web globe / shell states were
    // removed; the globe + shell now live in container/top_left/globe_shell.*.)
    // setWorkState keeps the todo list as the single state of this cell; it's still
    // used on run reset.
    // ============================================
    const setWorkState = (state) => {
        const map = { todo: 'todoState' };
        Object.keys(map).forEach((k) => {
            const el = document.getElementById(map[k]);
            if (el) el.classList.toggle('is-active', k === state);
        });
    };
    // Exposed so the chat-input wiring (chat_input/chat_input.js) can reset this
    // cell to 'todo' when a run starts.
    window.setWorkState = setWorkState;

    // Web globe + shell terminal now live in container/top_left/globe_shell.js —
    // they swap in over the screenshot while their tool runs. The backend hooks
    // window.webSearchStart/End and window.shellStart/Result/End are defined there.

    // ============================================
    // TELEGRAM "OCCUPIED" OVERLAY
    // Shown while a Telegram task runs (instead of minimizing the window).
    // ============================================
    const telegramTaskOverlay = document.getElementById('telegramTaskOverlay');
    // The overlay's orb is the stop orb (chat_input/pc_button.html) — the one
    // the composer and the setup banner show — so it looks the same everywhere.
    // That page starts hidden and shows on 'pcbtn:show'; it may still be
    // loading the first time the overlay opens, so the load handler catches up.
    const telegramTaskOrb = document.getElementById('telegramTaskOrbFrame');
    const telegramOrbPost = (msg) => {
        try { telegramTaskOrb.contentWindow.postMessage(msg, '*'); } catch (e) {}
    };
    if (telegramTaskOrb) telegramTaskOrb.addEventListener('load', () => {
        if (telegramTaskOverlay && telegramTaskOverlay.classList.contains('active')) telegramOrbPost('pcbtn:show');
    });
    window.telegramOccupiedShow = () => {
        document.body.classList.add('tg-occupied');   // fade UI components away
        if (telegramTaskOverlay) telegramTaskOverlay.classList.add('active');
        if (telegramTaskOrb) telegramOrbPost('pcbtn:show');
    };
    window.telegramOccupiedHide = () => {
        document.body.classList.remove('tg-occupied');
        if (telegramTaskOverlay) telegramTaskOverlay.classList.remove('active');
        if (telegramTaskOrb) telegramOrbPost('pcbtn:hide');
    };

    // (shell terminal moved to container/top_left/globe_shell.js — swaps over the screenshot)

    // =====================================================================
    // CLI streaming UI — pills for parallel CLI subprocesses during cli_await.
    // Emitted by app.py:send_cli_event_to_frontend (macOS only for now).
    // =====================================================================

    // ---------- Particle engine (loader / globe / tick) ----------
    // Adapted from all_cli_animation.html. 800 particles morph between three
    // shapes via a 1.5s pinch+swirl ease. State-driven (not on a timeline)
    // so pill events (web start, web end, complete) trigger transitions.
    const PARTICLE_COLOR         = '#9ca3af';
    const P_OUTER_SPOKES         = 12;
    const P_INNER_SPOKES         = 8;
    const P_PER_SPOKE            = 40;
    const P_TOTAL                = (P_OUTER_SPOKES + P_INNER_SPOKES) * P_PER_SPOKE;
    const P_GLOBE_R              = 36;
    const P_TICK_R               = 36;
    const P_TRANSITION_MS        = 1500;
    const P_CANVAS_CSS           = 20;
    const P_LOCAL_SCALE          = (P_CANVAS_CSS / 2) / 42;

    function _easeInOutCubic(x) {
        return x < 0.5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2;
    }
    function _distToSegment(p, v, w) {
        const l2 = (w.x - v.x) * (w.x - v.x) + (w.y - v.y) * (w.y - v.y);
        if (l2 === 0) return Math.hypot(p.x - v.x, p.y - v.y);
        let t = ((p.x - v.x) * (w.x - v.x) + (p.y - v.y) * (w.y - v.y)) / l2;
        t = Math.max(0, Math.min(1, t));
        return Math.hypot(p.x - (v.x + t * (w.x - v.x)), p.y - (v.y + t * (w.y - v.y)));
    }
    function _generateTickData(targetCount) {
        let bestData = [];
        let minDiff = Infinity;
        const p1 = { x: -14, y: 2 }, p2 = { x: -4, y: 12 }, p3 = { x: 16, y: -8 };
        const CUTOUT_THICKNESS = 6.5;
        for (let density = 0.8; density < 3.0; density += 0.01) {
            const temp = [];
            const rings = Math.floor(P_TICK_R * density);
            for (let i = 0; i <= rings; i++) {
                const r = (i / rings) * P_TICK_R;
                const pts = i === 0 ? 1 : Math.floor(2 * Math.PI * r * density);
                for (let j = 0; j < pts; j++) {
                    const theta = (j / pts) * Math.PI * 2;
                    const px = r * Math.cos(theta);
                    const py = r * Math.sin(theta);
                    const d1 = _distToSegment({ x: px, y: py }, p1, p2);
                    const d2 = _distToSegment({ x: px, y: py }, p2, p3);
                    if (d1 > CUTOUT_THICKNESS && d2 > CUTOUT_THICKNESS) {
                        temp.push({ x: px, y: py });
                    }
                }
            }
            if (Math.abs(temp.length - targetCount) < minDiff) {
                minDiff = Math.abs(temp.length - targetCount);
                bestData = temp;
                if (minDiff === 0) break;
            }
        }
        while (bestData.length > targetCount) bestData.splice(Math.floor(Math.random() * bestData.length), 1);
        while (bestData.length < targetCount) bestData.push({ x: bestData[0].x, y: bestData[0].y });
        return bestData;
    }
    let _cachedTickData = null;
    function _getTickDataCached() {
        if (!_cachedTickData) _cachedTickData = _generateTickData(P_TOTAL);
        return _cachedTickData;
    }
    function _initParticleSet() {
        const loaderData = [];
        const globeData = [];
        for (let i = 0; i < P_OUTER_SPOKES; i++) {
            const angle = (i / P_OUTER_SPOKES) * Math.PI * 2;
            for (let j = 0; j < P_PER_SPOKE; j++) {
                const t = j / (P_PER_SPOKE - 1);
                loaderData.push({ type: 'outer', angle, r: 32 + t * 10, pRadius: 2.5 });
            }
        }
        for (let i = 0; i < P_INNER_SPOKES; i++) {
            const angle = (i / P_INNER_SPOKES) * Math.PI * 2;
            for (let j = 0; j < P_PER_SPOKE; j++) {
                const t = j / (P_PER_SPOKE - 1);
                loaderData.push({ type: 'inner', angle, r: 16 + t * 6, pRadius: 1.8 });
            }
        }
        for (let i = 0; i < 12; i++) {
            const theta = (i / 12) * Math.PI * 2;
            for (let j = 0; j < 40; j++) {
                const phi = (j / 39) * Math.PI;
                globeData.push({ theta, phi });
            }
        }
        for (let i = 0; i < 8; i++) {
            const phi = ((i + 1) / 9) * Math.PI;
            for (let j = 0; j < 40; j++) {
                const theta = (j / 40) * Math.PI * 2;
                globeData.push({ theta, phi });
            }
        }
        const tickData = _getTickDataCached().slice();
        // Per-engine destination shuffle so swarm scatters differently each spawn.
        for (let i = globeData.length - 1; i > 0; i--) {
            const j = Math.floor(Math.random() * (i + 1));
            [globeData[i], globeData[j]] = [globeData[j], globeData[i]];
            const k = Math.floor(Math.random() * (i + 1));
            [tickData[i], tickData[k]] = [tickData[k], tickData[i]];
        }
        const out = new Array(P_TOTAL);
        for (let i = 0; i < P_TOTAL; i++) {
            out[i] = { l: loaderData[i], g: globeData[i], t: tickData[i] };
        }
        return out;
    }
    function _getShapePos(shape, p, time) {
        if (shape === 'l') {
            const outerA = time * 0.0015;
            const innerA = -time * 0.0025;
            const a = p.l.angle + (p.l.type === 'outer' ? outerA : innerA);
            return { x: Math.cos(a) * p.l.r, y: Math.sin(a) * p.l.r, z: 0, pRadius: p.l.pRadius };
        }
        if (shape === 'g') {
            const spin = time * 0.001;
            const theta = p.g.theta + spin;
            return {
                x: Math.sin(p.g.phi) * Math.cos(theta) * P_GLOBE_R,
                y: Math.cos(p.g.phi) * P_GLOBE_R,
                z: Math.sin(p.g.phi) * Math.sin(theta) * P_GLOBE_R,
                pRadius: 1.2,
            };
        }
        return { x: p.t.x, y: p.t.y, z: 0, pRadius: 2.4 };
    }

    // Single shared rAF — every active engine renders in lockstep.
    const _engineManager = { engines: new Set(), rafId: null };
    function _engineFrame(time) {
        _engineManager.rafId = null;
        for (const eng of _engineManager.engines) {
            try { eng.render(time); } catch (e) { console.warn('[particle] render', e); }
        }
        if (_engineManager.engines.size > 0) {
            _engineManager.rafId = requestAnimationFrame(_engineFrame);
        }
    }
    function _registerEngine(eng) {
        _engineManager.engines.add(eng);
        if (!_engineManager.rafId) {
            _engineManager.rafId = requestAnimationFrame(_engineFrame);
        }
    }
    function _unregisterEngine(eng) {
        _engineManager.engines.delete(eng);
    }

    function createParticleEngine(canvas) {
        if (!canvas) return null;
        const ctx = canvas.getContext('2d');
        const dpr = window.devicePixelRatio || 1;
        canvas.width = P_CANVAS_CSS * dpr;
        canvas.height = P_CANVAS_CSS * dpr;
        canvas.style.width = P_CANVAS_CSS + 'px';
        canvas.style.height = P_CANVAS_CSS + 'px';
        ctx.scale(dpr, dpr);

        const cx = P_CANVAS_CSS / 2;
        const cy = P_CANVAS_CSS / 2;
        const particles = _initParticleSet();

        let originShape = 'l';
        let currentShape = 'l';
        let targetShape = 'l';
        let transitionStart = 0;
        let isTransitioning = false;
        let isStatic = false;        // true once tick has fully settled
        let destroyed = false;

        const engine = {
            canvas,
            get shape() { return currentShape; },
            setShape(name) {
                if (destroyed) return;
                const code = name === 'globe' ? 'g' : name === 'tick' ? 't' : 'l';
                if (code === targetShape && !isTransitioning) return;
                originShape = currentShape;
                targetShape = code;
                isTransitioning = true;
                transitionStart = performance.now();
                isStatic = false;
                _registerEngine(engine);
            },
            render(time) {
                if (destroyed || isStatic) return;
                let ePhase = 0;
                if (isTransitioning) {
                    const elapsed = time - transitionStart;
                    if (elapsed >= P_TRANSITION_MS) {
                        currentShape = targetShape;
                        isTransitioning = false;
                        ePhase = 0;
                    } else {
                        ePhase = _easeInOutCubic(elapsed / P_TRANSITION_MS);
                    }
                }
                const shape1 = isTransitioning ? originShape : currentShape;
                const shape2 = isTransitioning ? targetShape : currentShape;
                const pinchFactor = 1.0 - Math.sin(ePhase * Math.PI) * 0.9;
                const swirlFactor = Math.sin(ePhase * Math.PI) * Math.PI * 1.5;

                let vOpacity = 0.0;
                let pTargetAlpha = 1.0;
                if (shape1 === 't' && shape2 === 't') {
                    vOpacity = 1.0; pTargetAlpha = 0.0;
                } else if (shape2 === 't') {
                    vOpacity = Math.pow(ePhase, 4);
                    pTargetAlpha = 1.0 - Math.pow(ePhase, 4);
                } else if (shape1 === 't') {
                    vOpacity = Math.pow(1 - ePhase, 4);
                    pTargetAlpha = 1.0 - Math.pow(1 - ePhase, 4);
                }

                ctx.clearRect(0, 0, P_CANVAS_CSS, P_CANVAS_CSS);
                ctx.fillStyle = PARTICLE_COLOR;
                const perspective = 300;

                for (let i = 0; i < particles.length; i++) {
                    const p = particles[i];
                    const pos1 = _getShapePos(shape1, p, time);
                    const pos2 = _getShapePos(shape2, p, time);
                    let mX = (pos1.x + (pos2.x - pos1.x) * ePhase) * pinchFactor;
                    let mY = (pos1.y + (pos2.y - pos1.y) * ePhase) * pinchFactor;
                    let mZ = (pos1.z + (pos2.z - pos1.z) * ePhase) * pinchFactor;
                    const sN = Math.sin(swirlFactor);
                    const cN = Math.cos(swirlFactor);
                    const rx = mX * cN - mY * sN;
                    const ry = mX * sN + mY * cN;
                    mX = rx; mY = ry;
                    const projScale = perspective / (perspective + mZ);
                    const pX = cx + mX * P_LOCAL_SCALE * projScale;
                    const pY = cy + mY * P_LOCAL_SCALE * projScale;
                    let opacity = 1.0;
                    const is3DMix = (shape1 === 'g' ? 1 - ePhase : 0) + (shape2 === 'g' ? ePhase : 0);
                    if (is3DMix > 0) {
                        const depthOpacity = 0.3 + 0.7 * (1 - (mZ + P_GLOBE_R) / (2 * P_GLOBE_R));
                        opacity = 1.0 - is3DMix * (1.0 - depthOpacity);
                    }
                    const finalOpacity = opacity * pTargetAlpha;
                    if (finalOpacity > 0.01) {
                        ctx.globalAlpha = Math.max(0.01, finalOpacity);
                        const pR = pos1.pRadius + (pos2.pRadius - pos1.pRadius) * ePhase;
                        const size = pR * P_LOCAL_SCALE * projScale;
                        ctx.beginPath();
                        ctx.arc(pX, pY, Math.max(0.4, size), 0, Math.PI * 2);
                        ctx.fill();
                    }
                }

                if (vOpacity > 0.01) {
                    ctx.save();
                    ctx.translate(cx, cy);
                    ctx.scale(pinchFactor * P_LOCAL_SCALE, pinchFactor * P_LOCAL_SCALE);
                    ctx.rotate(swirlFactor);
                    ctx.globalAlpha = vOpacity;
                    ctx.fillStyle = PARTICLE_COLOR;
                    ctx.beginPath();
                    ctx.arc(0, 0, P_TICK_R, 0, Math.PI * 2);
                    ctx.fill();
                    ctx.globalCompositeOperation = 'destination-out';
                    ctx.lineWidth = 13;
                    ctx.lineCap = 'round';
                    ctx.lineJoin = 'round';
                    ctx.beginPath();
                    ctx.moveTo(-14, 2);
                    ctx.lineTo(-4, 12);
                    ctx.lineTo(16, -8);
                    ctx.stroke();
                    ctx.restore();
                }
                ctx.globalAlpha = 1.0;

                // Tick steady-state: final frame is drawn — stop animating. The
                // canvas keeps the last buffer, so the solid tick stays visible.
                if (!isTransitioning && currentShape === 't') {
                    isStatic = true;
                    _unregisterEngine(engine);
                }
            },
            destroy() {
                destroyed = true;
                isStatic = true;
                _unregisterEngine(engine);
            },
        };
        _registerEngine(engine);
        return engine;
    }

    // Pill ↔ engine attachment so we can find an engine by pill or by taskId.
    const _pillEngines = new WeakMap();  // pill element -> engine
    function _attachEngine(pill) {
        if (!pill || _pillEngines.has(pill)) return null;
        const canvas = pill.querySelector('.cli-pill-canvas');
        if (!canvas) return null;
        const eng = createParticleEngine(canvas);
        if (eng) _pillEngines.set(pill, eng);
        return eng;
    }
    function _detachEngine(pill) {
        if (!pill) return;
        const eng = _pillEngines.get(pill);
        if (eng) {
            eng.destroy();
            _pillEngines.delete(pill);
        }
    }
    function _engineForPill(pill) {
        return pill ? _pillEngines.get(pill) : null;
    }

    const cliStreamList = document.getElementById('cliStreamList');
    const cliPillTemplate = document.getElementById('cliPillTemplate');

    function findCliPill(taskId) {
        if (!cliStreamList) return null;
        return cliStreamList.querySelector(
            `.cli-pill[data-task-id="${CSS.escape(String(taskId))}"]`
        );
    }

    window.cliTaskStart = (taskId, description) => {
        console.log('[cli] task_start', taskId, description);
        if (!cliStreamList || !cliPillTemplate) {
            console.warn('[cli] missing cliStreamList or cliPillTemplate');
            return;
        }
        if (findCliPill(taskId)) return;  // dedupe if event arrives twice
        const pill = cliPillTemplate.content.firstElementChild.cloneNode(true);
        pill.dataset.taskId = String(taskId);
        const cmdEl = pill.querySelector('.cli-pill-cmd');
        if (cmdEl) cmdEl.textContent = description || '';
        cliStreamList.appendChild(pill);
        _attachEngine(pill);
        // A new running CLI pill means "all siblings complete" is no longer
        // true — cancel any in-flight tick-fade timer. It'll re-arm when this
        // new pill eventually completes.
        if (_tickFadeTimer) { clearTimeout(_tickFadeTimer); _tickFadeTimer = null; }
    };

    // Per-character cadence — matches the Telegram banner's 8 ms feel.
    const CLI_CHAR_STAGGER_MS  = 8;
    // Per-character fade-in duration (opacity 0→1).
    const CLI_CHAR_FADE_MS     = 60;
    // Hold a finished page (full pill width filled) before clearing for the next page.
    const CLI_PAGE_HOLD_MS     = 550;
    // Hold between distinct lines (after the final page of a line completes).
    const CLI_LINE_HOLD_MS     = 260;

    // Per-pill line queue + runner. Incoming task_line events get queued and
    // played back one at a time so the user always sees each line stream
    // smoothly, even when the agent dumps 10 lines of raw response in one
    // millisecond. We're explicitly OK with the UI lagging behind real time —
    // smoothness matters more than catching up.
    //
    // Each line is "paginated": characters stream left-to-right letter by
    // letter; when the next char would overflow the pill width, the current
    // page holds, the output clears, and that char starts the next page
    // from the left.
    const _cliPillRunners = new WeakMap();

    function _getRunner(pill) {
        let runner = _cliPillRunners.get(pill);
        if (!runner) {
            runner = { queue: [], running: false };
            _cliPillRunners.set(pill, runner);
        }
        return runner;
    }

    function _pumpCliRunner(pill, runner) {
        if (runner.running) return;
        if (runner.queue.length === 0) return;
        runner.running = true;
        const { text, stream } = runner.queue.shift();

        const out = pill.querySelector('.cli-output');
        if (!out) {
            runner.running = false;
            _pumpCliRunner(pill, runner);
            return;
        }

        // Array.from splits by code point so emoji stay intact (text.split('')
        // would split them into surrogate halves).
        const chars = Array.from(String(text));
        const lineClass = stream === 'err' ? 'cli-line cli-line-err' : 'cli-line cli-line-out';

        if (chars.length === 0) {
            runner.running = false;
            _pumpCliRunner(pill, runner);
            return;
        }

        let pageDiv = null;
        const startNewPage = () => {
            pageDiv = document.createElement('div');
            pageDiv.className = lineClass;
            out.replaceChildren(pageDiv);
        };
        startNewPage();

        let i = 0;
        const tick = () => {
            if (i >= chars.length) {
                // Final page rendered — hold, then drop the run flag so the
                // next queued line gets pulled.
                setTimeout(() => {
                    runner.running = false;
                    _pumpCliRunner(pill, runner);
                }, CLI_LINE_HOLD_MS);
                return;
            }

            const isFirstOnPage = pageDiv.childElementCount === 0;
            const span = document.createElement('span');
            span.className = 'cli-char';
            span.textContent = chars[i];
            span.style.opacity = '0';
            span.style.transition = 'opacity ' + CLI_CHAR_FADE_MS + 'ms ease-out';
            pageDiv.appendChild(span);

            // Did this char push past the pill's right edge? Measure on the
            // page div itself — `.cli-line` has overflow:hidden so the
            // overflow doesn't propagate up to `.cli-output`. If the char
            // doesn't fit (and it isn't the only char on this page), retract
            // it, hold the current page, then start fresh with this char at
            // the left edge of a new page.
            const overflowed = pageDiv.scrollWidth > pageDiv.clientWidth + 1;
            if (overflowed && !isFirstOnPage) {
                pageDiv.removeChild(span);
                setTimeout(() => {
                    startNewPage();
                    // Skip leading whitespace so the new page doesn't open
                    // with a hanging space.
                    while (i < chars.length && /\s/.test(chars[i])) i++;
                    tick();  // retry placing this char as the start of the new page
                }, CLI_PAGE_HOLD_MS);
                return;
            }

            // Char fits (or it's a lone oversized char we accept as-is). Kick
            // off the opacity fade on the next frame.
            requestAnimationFrame(() => { span.style.opacity = '1'; });
            i++;
            setTimeout(tick, CLI_CHAR_STAGGER_MS);
        };

        tick();
    }

    window.cliTaskLine = (taskId, line, stream) => {
        console.log('[cli] task_line', taskId, stream, line);
        const pill = findCliPill(taskId);
        if (!pill) {
            console.warn('[cli] no pill found for', taskId);
            return;
        }
        const text = line == null ? '' : String(line);
        if (text.trim() === '') return;  // skip blanks so the pill never flashes empty
        // Real line wins over filler. Stop the filler loop; if this pill still
        // has running minions, the next idle window will re-arm filler.
        const fs = _fillerState.get(taskId);
        if (fs) {
            _stopFiller(taskId);
            if (fs.hasMinion) _scheduleFillerStart(taskId);
        }
        const runner = _getRunner(pill);
        runner.queue.push({ text, stream });
        _pumpCliRunner(pill, runner);
    };

    window.cliTaskEnd = (taskId, status, summary) => {
        const pill = findCliPill(taskId);
        if (!pill) return;
        const finalStatus = status || 'complete';
        pill.dataset.status = finalStatus;
        pill.classList.add('complete');
        // Parent's done — stop any in-flight filler chatter (minion or web).
        _stopFiller(taskId);
        _fillerState.delete(taskId);
        _stopWebFiller(taskId);
        // Trigger the canvas tick on success; on error/stopped the canvas
        // hides via CSS and the legacy ✕ glyph paints over.
        const eng = _engineForPill(pill);
        if (eng && finalStatus === 'complete') {
            eng.setShape('tick');
            _registerPendingTickFade(pill);
        }
        if (summary) {
            // Route the summary through the same paginated queue as regular
            // lines so it streams in with the same calm pacing.
            const runner = _getRunner(pill);
            runner.queue.push({ text: summary, stream: 'summary' });
            _pumpCliRunner(pill, runner);
        }
    };

    const chatWrapper = document.getElementById('chatWrapper');

    // Whether split-layout was on the chat wrapper when cli-await began,
    // so cliAwaitEnd can restore them to that state cleanly.
    let _cliPrevSplit = false;

    window.cliAwaitStart = (reason) => {
        console.log('[cli] await_start', reason);
        // Drop split-layout so the chat box snaps back to its initial big
        // centered state (the base .chat-container-wrapper rule wins). This
        // is what the user wants: full-width chat box during cli_await.
        if (chatWrapper) {
            _cliPrevSplit = chatWrapper.classList.contains('split-layout');
            chatWrapper.classList.remove('split-layout');
            chatWrapper.classList.add('cli-mode');
        }
        document.body.classList.add('cli-mode');  // hides settings cog
    };

    window.cliAwaitEnd = () => {
        console.log('[cli] await_end');
        if (chatWrapper) {
            chatWrapper.classList.remove('cli-mode');
            if (_cliPrevSplit) chatWrapper.classList.add('split-layout');
        }
        document.body.classList.remove('cli-mode');
        // Clear pills after the fade-out finishes (matches the 0.4s transition).
        setTimeout(() => {
            if (!cliStreamList) return;
            // Free every engine first so the shared rAF loop drains.
            for (const pill of cliStreamList.querySelectorAll('.cli-pill')) {
                _detachEngine(pill);
            }
            _pendingTickFadeReset();
            cliStreamList.innerHTML = '';
        }, 450);
    };

    // =====================================================================
    // Minion pill choreography (narrower, darker children of CLI pills).
    // Birth: drop top-to-bottom, one at a time (~120ms apart).
    // Death: when ALL minion siblings of the same parent are complete,
    //        collapse bottom-to-top (last absorbed first), each absorb
    //        staggered ~150ms apart, fading into the parent's bottom edge.
    // =====================================================================

    const cliMinionPillTemplate = document.getElementById('cliMinionPillTemplate');

    const MINION_DROP_STAGGER_MS    = 120;
    // Wait long enough for the last-completing minion's loader→tick morph
    // (1500 ms) to fully form before the absorb cascade starts swallowing
    // pills upward. Without this you'd see the tick begin morphing in then
    // get yanked away mid-frame.
    const MINION_ABSORB_DELAY_MS    = 1700;
    const MINION_ABSORB_STAGGER_MS  = 150;

    // Per-parent drop queue so events arriving in a burst still cascade visually.
    const _minionDropQueue = new Map();  // parentTaskId -> { queue: [...], running: bool }

    // ---------- Filler phrase loop ----------
    // While a parent CLI pill has running minions but is itself silent (no new
    // lines streaming), the pill goes blank and looks frozen. Push playful
    // status phrases into the parent's runner so the user knows it's alive and
    // waiting on its minions. Stops the moment a real line comes in or the
    // minion batch finishes.
    const FILLER_PHRASES = [
        'summoned minions…',
        'digging through the codebase…',
        "don't bother me, i'm busy still",
        'reticulating splines…',
        'consulting the rubber duck…',
        'untangling spaghetti…',
        'watching minions go brrr…',
        'this is fine, everything is fine…',
        'spinning up neurons…',
        'arguing with the linter…',
        'minions are reading… patience, human',
        'pondering the orb…',
        'grepping the unknown…',
        'feeding the hamsters…',
        'still thinking, hold tight…',
        'writing tiny love letters to stdout…',
        'asking stack overflow nicely…',
        'looking for the missing semicolon…',
        'blaming the intern…',
        'performing arcane git rituals…',
        'have you tried turning it off and on…',
        'just one more refactor, i promise…',
        'regex go brrr…',
        'negotiating with the type checker…',
        "Schrödinger's bug: works on my machine…",
        'pretending to understand recursion…',
        'this codebase has feelings too…',
        'arguing with prettier…',
        'i swear i tested this earlier…',
        "checking if it's a feature, not a bug…",
        'the code works, nobody knows why…',
        'reading documentation as last resort…',
        'blaming the cache…',
        'praying to the build gods…',
        'git blame says it was past me…',
        'compiling existential dread…',
        'tabs vs spaces war ongoing…',
        'training a goldfish to write tests…',
        'minions arguing over indentation…',
        'still cheaper than a senior dev…',
        'pushing to prod on a friday…',
        'one does not simply async in python…',
        '404: motivation not found…',
        'rewriting it in rust… mentally…',
        "petting the dog, brb…",
        'yelling politely at the json…',
        "checking if it's plugged in…",
        'the cake is a bug…',
        'explaining mondays to the AI…',
        'deploying vibes…',
        'this stack trace feels personal…',
        'i was promised flying cars, got jira…',
        'writing tests… eventually…',
        'minions on coffee break…',
        "this wasn't in the spec…",
        'ctrl-z is my therapist…',
        'running from technical debt…',
        'console.log debugger gang…',
        'the docs lied to us…',
        'trying to remember what i was doing…',
        'rebooting reality…',
        "yes, that's a feature now…",
        'speedrun: any% blame git…',
        'thinking too hard, please wait…',
        'yet another deeply nested if…',
        'promise resolved with disappointment…',
        'hot reload, cold coffee…',
        'aligning ducks in rows…',
        'one liner that took two hours…',
        'naming things, the hardest problem…',
        'off-by-one somewhere, definitely…',
        'loading more excuses…',
        'the bug is in another castle…',
        'your code is fine, the universe is broken…',
        'the linter has strong opinions…',
        'minion overheard saying lgtm…',
        'convincing the tests to pass…',
        'renaming the variable to fix it…',
        'drowning in callback hell…',
        '73 unread warnings, vibes only…',
        "yes it works, no i don't know why…",
        'minions found 47 todos, ignored all…',
        'scrolling error logs like reels…',
        'two minions, one task…',
        'sacrificing a keyboard to the demo gods…',
        'undoing the undo…',
        'reading the error message, finally…',
        'minions whispering to each other…',
        'the algorithm has thoughts…',
        'trying not to break prod…',
        'minion union meeting in progress…',
        'shaking the magic 8-ball…',
        'asking the cat for code review…',
        'running tests with fingers crossed…',
        'exorcising the legacy code…',
        'i promise this is the last bug…',
        'binary search through 200 tabs…',
        'feature creep is a feature now…',
        'minions found a TODO from 2014…',
        'deprecated, but still working…',
        'putting console.logs in production…',
        'redefining what "done" means…',
    ];
    const FILLER_IDLE_MS     = 1800;  // silence before filler kicks in
    const FILLER_INTERVAL_MS = 3500;  // gap between filler phrases

    const _fillerState = new Map();  // parentTaskId -> { active, hasMinion, bag, lastIdx, idleTimer, tickTimer }

    function _ensureFiller(parentTaskId) {
        let s = _fillerState.get(parentTaskId);
        if (!s) {
            s = {
                active: false,
                hasMinion: false,
                bag: [],         // shuffle-bag: indices yet to show this cycle
                lastIdx: -1,     // last shown index (used to dedupe across reshuffles)
                idleTimer: null,
                tickTimer: null,
            };
            _fillerState.set(parentTaskId, s);
        }
        return s;
    }

    // Shuffle-bag picker: pop a random unseen phrase. When the bag empties
    // (full pool consumed), reshuffle the whole pool — so we get the entire
    // 100 covered before any repeats, then 100 again, etc. Tiny dedupe step
    // ensures we never get the same phrase back-to-back across a reshuffle.
    function _nextFillerPhrase(s) {
        if (!s.bag || s.bag.length === 0) {
            const bag = FILLER_PHRASES.map((_, i) => i);
            for (let i = bag.length - 1; i > 0; i--) {
                const j = Math.floor(Math.random() * (i + 1));
                [bag[i], bag[j]] = [bag[j], bag[i]];
            }
            // Avoid repeating the most recent phrase as the first of the new
            // cycle. If the top of the bag (next to pop) matches lastIdx,
            // swap it with the bottom.
            if (s.lastIdx >= 0 && bag.length > 1 && bag[bag.length - 1] === s.lastIdx) {
                [bag[bag.length - 1], bag[0]] = [bag[0], bag[bag.length - 1]];
            }
            s.bag = bag;
        }
        const idx = s.bag.pop();
        s.lastIdx = idx;
        return FILLER_PHRASES[idx];
    }

    function _stopFiller(parentTaskId) {
        const s = _fillerState.get(parentTaskId);
        if (!s) return;
        s.active = false;
        if (s.idleTimer) { clearTimeout(s.idleTimer); s.idleTimer = null; }
        if (s.tickTimer) { clearTimeout(s.tickTimer); s.tickTimer = null; }
    }

    function _scheduleFillerStart(parentTaskId) {
        const s = _ensureFiller(parentTaskId);
        if (!s.hasMinion) return;
        if (s.idleTimer) clearTimeout(s.idleTimer);
        s.idleTimer = setTimeout(() => {
            s.idleTimer = null;
            if (!s.hasMinion) return;
            s.active = true;
            const tickFiller = () => {
                if (!s.active) return;
                const pill = findCliPill(parentTaskId);
                if (!pill) { s.active = false; return; }
                const phrase = _nextFillerPhrase(s);
                const runner = _getRunner(pill);
                runner.queue.push({ text: phrase, stream: 'stdout' });
                _pumpCliRunner(pill, runner);
                s.tickTimer = setTimeout(tickFiller, FILLER_INTERVAL_MS);
            };
            tickFiller();
        }, FILLER_IDLE_MS);
    }

    // ---------- Web-loading filler (per-pill, looped while web tool active) ----------
    const WEB_FILLER_PHRASES = [
        'scraping the interwebs…',
        'intent searching…',
        'asking the search gods nicely…',
        'crawling the web like a polite spider…',
        'opening 1000 tabs in spirit…',
        'fetching truth from the void…',
        'interrogating wikipedia…',
        'reading 47 stack overflow answers…',
        'ranking results by vibes…',
        'cleaning the data with bare hands…',
    ];
    const WEB_FILLER_INTERVAL_MS = 3500;
    const _webFillerState = new Map();  // taskId -> { bag, lastIdx, tickTimer, active }

    function _nextWebFillerPhrase(s) {
        if (!s.bag || s.bag.length === 0) {
            const bag = WEB_FILLER_PHRASES.map((_, i) => i);
            for (let i = bag.length - 1; i > 0; i--) {
                const j = Math.floor(Math.random() * (i + 1));
                [bag[i], bag[j]] = [bag[j], bag[i]];
            }
            if (s.lastIdx >= 0 && bag.length > 1 && bag[bag.length - 1] === s.lastIdx) {
                [bag[bag.length - 1], bag[0]] = [bag[0], bag[bag.length - 1]];
            }
            s.bag = bag;
        }
        const idx = s.bag.pop();
        s.lastIdx = idx;
        return WEB_FILLER_PHRASES[idx];
    }

    function _startWebFiller(taskId) {
        if (_webFillerState.has(taskId) && _webFillerState.get(taskId).active) return;
        const s = { bag: [], lastIdx: -1, tickTimer: null, active: true };
        _webFillerState.set(taskId, s);
        const tick = () => {
            if (!s.active) return;
            const pill = findCliPill(taskId);
            if (!pill) { s.active = false; return; }
            const phrase = _nextWebFillerPhrase(s);
            const runner = _getRunner(pill);
            runner.queue.push({ text: phrase, stream: 'stdout' });
            _pumpCliRunner(pill, runner);
            s.tickTimer = setTimeout(tick, WEB_FILLER_INTERVAL_MS);
        };
        tick();
    }

    function _stopWebFiller(taskId) {
        const s = _webFillerState.get(taskId);
        if (!s) return;
        s.active = false;
        if (s.tickTimer) { clearTimeout(s.tickTimer); s.tickTimer = null; }
        _webFillerState.delete(taskId);
    }

    // ---------- Tick-fade timing (CLI parent pills only) ----------
    // Each CLI parent pill that completes successfully shows its canvas tick.
    // Tick stays visible while *any* sibling CLI pill is still running. Once
    // every CLI pill in the stream is .complete, we hold for 1s and fade
    // every pending pill's canvas together.
    const TICK_FADE_HOLD_MS = 1000;
    const _pendingTickFade = new Set();  // pill elements
    let _tickFadeTimer = null;

    function _allCliPillsComplete() {
        if (!cliStreamList) return false;
        const all = cliStreamList.querySelectorAll('.cli-pill:not(.minion)');
        if (all.length === 0) return false;
        for (const p of all) if (!p.classList.contains('complete')) return false;
        return true;
    }
    function _registerPendingTickFade(pill) {
        _pendingTickFade.add(pill);
        _maybeStartTickFadeTimer();
    }
    function _maybeStartTickFadeTimer() {
        if (_tickFadeTimer) { clearTimeout(_tickFadeTimer); _tickFadeTimer = null; }
        if (!_allCliPillsComplete()) return;
        _tickFadeTimer = setTimeout(() => {
            _tickFadeTimer = null;
            for (const pill of _pendingTickFade) {
                if (!pill.isConnected) continue;
                const canvas = pill.querySelector('.cli-pill-canvas');
                if (canvas) canvas.classList.add('canvas-faded');
                pill.classList.add('show-status-glyph');
            }
            _pendingTickFade.clear();
        }, TICK_FADE_HOLD_MS);
    }
    function _pendingTickFadeReset() {
        if (_tickFadeTimer) { clearTimeout(_tickFadeTimer); _tickFadeTimer = null; }
        _pendingTickFade.clear();
    }

    function _findMinionPill(taskId) {
        if (!cliStreamList) return null;
        return cliStreamList.querySelector(
            `.cli-pill.minion[data-task-id="${CSS.escape(String(taskId))}"]`
        );
    }

    function _findLastMinionForParent(parentTaskId) {
        if (!cliStreamList) return null;
        // Look up wrappers (which carry the layout slot in flex flow) so insertion
        // appends below the previous wrapper, not inside it.
        const all = cliStreamList.querySelectorAll(
            `.cli-minion-wrap[data-parent-task-id="${CSS.escape(String(parentTaskId))}"]`
        );
        return all.length > 0 ? all[all.length - 1] : null;
    }

    function _siblingMinions(parentTaskId) {
        if (!cliStreamList) return [];
        return Array.from(cliStreamList.querySelectorAll(
            `.cli-pill.minion[data-parent-task-id="${CSS.escape(String(parentTaskId))}"]`
        ));
    }

    function _spawnMinionPill(parentTaskId, taskId, query) {
        if (!cliStreamList || !cliMinionPillTemplate) return;
        if (_findMinionPill(taskId)) return;  // dedupe
        const parentPill = findCliPill(parentTaskId);
        if (!parentPill) {
            console.warn('[minion] no parent pill found for', parentTaskId);
            return;
        }
        const pill = cliMinionPillTemplate.content.firstElementChild.cloneNode(true);
        pill.dataset.taskId = String(taskId);
        pill.dataset.parentTaskId = String(parentTaskId);
        const cmdEl = pill.querySelector('.cli-pill-cmd');
        if (cmdEl) cmdEl.textContent = query || '';

        // Wrap in a clip container. The wrapper owns the layout slot (height
        // animates 0 → --minion-h) while the inner pill translates -100% → 0.
        // Together they produce a real geometric "slide out from behind the
        // pill above" — the wrapper's overflow:hidden clips against its top
        // edge, which sits right under the predecessor's bottom edge.
        const wrap = document.createElement('div');
        wrap.className = 'cli-minion-wrap';
        wrap.dataset.parentTaskId = String(parentTaskId);
        wrap.dataset.taskId = String(taskId);
        wrap.appendChild(pill);

        // Insert below the last existing minion wrapper of this parent (so the
        // stack grows downward in dispatch order); otherwise right after the
        // parent pill.
        const insertAfter = _findLastMinionForParent(parentTaskId) || parentPill;
        insertAfter.insertAdjacentElement('afterend', wrap);
        _attachEngine(pill);

        // Measure the inner pill's natural rendered height and lock the wrapper
        // to that exact value via --minion-h. Without this, the wrapper used a
        // hardcoded fallback that overshot the pill's actual height, leaving
        // empty space inside the wrapper that bloated the gap to the next
        // sibling. We measure on rAF so the browser has computed layout once.
        requestAnimationFrame(() => {
            const h = pill.offsetHeight;
            if (h > 0) wrap.style.setProperty('--minion-h', h + 'px');
        });
    }

    function _runMinionDropQueue(parentTaskId) {
        const state = _minionDropQueue.get(parentTaskId);
        if (!state || state.running) return;
        if (state.queue.length === 0) return;
        state.running = true;
        const dropOne = () => {
            const next = state.queue.shift();
            if (!next) {
                state.running = false;
                return;
            }
            _spawnMinionPill(parentTaskId, next.taskId, next.query);
            setTimeout(dropOne, MINION_DROP_STAGGER_MS);
        };
        dropOne();
    }

    window.cliMinionStart = (parentTaskId, taskId, query) => {
        console.log('[minion] start', parentTaskId, taskId, query);
        if (!parentTaskId || !taskId) return;
        if (!_minionDropQueue.has(parentTaskId)) {
            _minionDropQueue.set(parentTaskId, { queue: [], running: false });
        }
        _minionDropQueue.get(parentTaskId).queue.push({ taskId, query });
        _runMinionDropQueue(parentTaskId);
        // Mark parent as having minions and arm the idle filler watcher. If the
        // parent goes silent for FILLER_IDLE_MS, filler phrases start streaming.
        const fs = _ensureFiller(parentTaskId);
        fs.hasMinion = true;
        _scheduleFillerStart(parentTaskId);
    };

    // Stream a line from a running minion's stdout/stderr into its pill body.
    // Reuses the parent-pill word-pagination runner so minion output looks identical
    // to CLI agent output — same word-by-word fade, same per-page hold.
    window.cliMinionLine = (taskId, line, stream) => {
        const pill = _findMinionPill(taskId);
        if (!pill) return;
        const text = line == null ? '' : String(line);
        if (text.trim() === '') return;  // skip blanks so pill body never flashes empty
        const runner = _getRunner(pill);
        runner.queue.push({ text, stream });
        _pumpCliRunner(pill, runner);
    };

    // Web-loading visual on a pill: when the parent CLI agent (or any pill that owns
    // the web tool) starts a web search, flip the pill into web-loading mode. CSS
    // hides the streamed output and shows a clean "web" + 3 pulsing dots indicator.
    // Triggered from the marker bridge so it works for piped CLI subprocesses
    // (which can't fire web_callback directly to the frontend).
    window.cliPillWebLoadingStart = (taskId) => {
        const pill = findCliPill(taskId);
        if (!pill) return;
        pill.classList.add('web-loading');
        const eng = _engineForPill(pill);
        if (eng) eng.setShape('globe');
        _startWebFiller(taskId);
    };
    window.cliPillWebLoadingEnd = (taskId) => {
        const pill = findCliPill(taskId);
        if (!pill) return;
        pill.classList.remove('web-loading');
        const eng = _engineForPill(pill);
        if (eng) eng.setShape('loader');
        _stopWebFiller(taskId);
    };

    window.cliMinionEnd = (taskId, status, summary) => {
        console.log('[minion] end', taskId, status, summary);
        const pill = _findMinionPill(taskId);
        if (!pill) return;
        const finalStatus = status || 'complete';
        pill.dataset.status = finalStatus;
        pill.classList.add('complete');
        // Minion tick stays until absorb cascade removes the wrapper — no
        // sibling-wait + 1s fade like CLI parent pills get.
        const eng = _engineForPill(pill);
        if (eng && finalStatus === 'complete') eng.setShape('tick');

        // Wait for ALL siblings (same parent) to complete, then cascade absorb
        // bottom-to-top. We re-check on each end event — last one tips the cascade.
        const parentId = pill.dataset.parentTaskId;
        const siblings = _siblingMinions(parentId);
        const allComplete = siblings.length > 0
            && siblings.every(s => s.classList.contains('complete'));
        if (!allComplete) return;

        const reversed = siblings.slice().reverse();  // bottom-most absorbed first
        setTimeout(() => {
            reversed.forEach((s, i) => {
                setTimeout(() => {
                    if (!s.isConnected) return;
                    // Drive the absorb on the wrapper so its height collapses in
                    // sync with the inner pill's translateY → -100%. Removing the
                    // wrapper takes the pill with it.
                    const wrap = s.parentElement && s.parentElement.classList.contains('cli-minion-wrap')
                        ? s.parentElement
                        : s;
                    wrap.classList.add('absorbing');
                    wrap.addEventListener('animationend', (ev) => {
                        // Both wrapper and inner pill fire animationend; only act
                        // on the wrapper's own height animation so we don't remove
                        // mid-flight from the inner pill's transform animation.
                        if (ev.target !== wrap) return;
                        // Free the engine before the DOM goes — drops it from the
                        // shared rAF loop and lets the canvas GC.
                        const innerPill = wrap.querySelector('.cli-pill.minion');
                        _detachEngine(innerPill);
                        if (wrap.isConnected) wrap.remove();
                    });
                }, i * MINION_ABSORB_STAGGER_MS);
            });
            // Drop the per-parent drop queue state — the next batch (if any)
            // starts fresh.
            _minionDropQueue.delete(parentId);
            // Batch is done — minions have all completed and are absorbing.
            // Kill any active/pending filler so the parent pill doesn't keep
            // muttering jokes after its children are gone.
            _stopFiller(parentId);
            _fillerState.delete(parentId);
        }, MINION_ABSORB_DELAY_MS);
    };
});

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod desktop {
    use std::{
        error::Error,
        fs,
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex, RwLock,
        },
        time::Duration,
    };

    use adblock::Engine;
    use rustcloud::{build_filter_engine, should_block};
    use webview2_com::{
        CoTaskMemPWSTR,
        Microsoft::Web::WebView2::Win32::{
            ICoreWebView2, ICoreWebView2_22, COREWEBVIEW2_WEB_RESOURCE_CONTEXT,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_PING, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_STYLESHEET,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST,
            COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
        },
        WebResourceRequestedEventHandler,
    };
    use windows::{
        core::{w, Interface, PWSTR},
        Win32::System::Com::IStream,
    };
    use winit::{
        application::ApplicationHandler,
        dpi::LogicalSize,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
        window::{Icon, Window, WindowId},
    };
    use wry::{WebView, WebViewBuilder, WebViewExtWindows};

    const SOUNDCLOUD_URL: &str = "https://soundcloud.com/";
    const FILTER_LIST_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
    const APP_ICON_PNG: &[u8] = include_bytes!("../assets/rustcloud.png");
    const FILTER_LIST_URLS: &[&str] = &[
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/easylist/easylist.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/peter-lowe/serverlist.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/ublock-origin/badware.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/ublock-origin/filters-2020.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/ublock-origin/filters-2021.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/ublock-origin/filters-2022.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/ublock-origin/filters-2023.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/ublock-origin/filters-2024.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/ublock-origin/filters.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/ublock-origin/quick-fixes.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/ublock-origin/resource-abuse.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/ublock-origin/unbreak.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/easylist/easyprivacy.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/ublock-origin/privacy.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/easylist/easylist-cookie.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/ublock-origin/annoyances-others.txt",
        "https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets/ublock-origin/annoyances-cookies.txt",
    ];

    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    #[serde(default)]
    struct AppSettings {
        ad_block_enabled: bool,
        accent_color: String,
        theme: String,
        cover_glow_enabled: bool,
        full_playlist_shuffle_enabled: bool,
    }

    impl Default for AppSettings {
        fn default() -> Self {
            Self {
                ad_block_enabled: true,
                accent_color: String::from("#ff5500"),
                theme: String::from("midnight-glass"),
                cover_glow_enabled: false,
                full_playlist_shuffle_enabled: false,
            }
        }
    }

    #[derive(serde::Deserialize)]
    #[serde(tag = "type")]
    enum SettingsMessage {
        #[serde(rename = "adblock")]
        AdBlock { enabled: bool },
        #[serde(rename = "accent")]
        Accent { color: String },
        #[serde(rename = "theme")]
        Theme { name: String },
        #[serde(rename = "coverGlow")]
        CoverGlow { enabled: bool },
        #[serde(rename = "fullPlaylistShuffle")]
        FullPlaylistShuffle { enabled: bool },
    }

    const SETTINGS_UI_SCRIPT: &str = r##"
(() => {
    if (window.top !== window || window.__rustcloudSettingsMounted) return;
    window.__rustcloudSettingsMounted = true;

    const settings = {
        ad_block_enabled: true,
        accent_color: '#ff5500',
        theme: 'midnight-glass',
        cover_glow_enabled: false,
        full_playlist_shuffle_enabled: false,
        ...(window.__RUSTCLOUD_SETTINGS__ || {})
    };
    const uiPreferencesKey = 'rustcloud.ui-preferences.v1';
    const supportedThemes = [
        'default', 'midnight-glass', 'neon-pulse', 'warm-vinyl',
        'forest-glow', 'rose-quartz', 'ocean-breeze'
    ];
    try {
        const savedPreferences = JSON.parse(localStorage.getItem(uiPreferencesKey) || 'null');
        if (savedPreferences && typeof savedPreferences === 'object') {
            if (supportedThemes.includes(savedPreferences.theme)) settings.theme = savedPreferences.theme;
            if (typeof savedPreferences.accent_color === 'string' &&
                /^#[0-9a-f]{6}$/i.test(savedPreferences.accent_color)) {
                settings.accent_color = savedPreferences.accent_color;
            }
        }
    } catch (error) {
        console.warn('[RustCloud] Could not restore theme preferences:', error);
    }
    const saveUiPreferences = () => {
        try {
            localStorage.setItem(uiPreferencesKey, JSON.stringify({
                theme: settings.theme,
                accent_color: settings.accent_color
            }));
        } catch (error) {
            console.warn('[RustCloud] Could not save theme preferences:', error);
        }
    };

    let shufflePatchTimer = null;
    let shufflePatchAttempts = 0;
    let shuffleLoading = false;
    const shufflePageSize = 100;

    const showShuffleStatus = message => {
        if (typeof window.__RUSTCLOUD_SHUFFLE_NOTIFY__ === 'function') {
            window.__RUSTCLOUD_SHUFFLE_NOTIFY__(message);
        } else {
            console.info(`[RustCloud] ${message}`);
        }
    };

    const findWebpackRequire = () => {
        for (const key of Object.keys(window)) {
            if (!key.startsWith('webpackChunk') || !Array.isArray(window[key])) continue;
            let runtime;
            try {
                window[key].push([[], {}, candidate => { runtime = candidate; }]);
            } catch (error) {
                console.warn('[RustCloud] Could not inspect SoundCloud modules:', error);
            }
            if (runtime && runtime.c) return runtime;
        }

        const legacyChunks = window.webpackJsonp;
        if (Array.isArray(legacyChunks)) {
            let runtime;
            const moduleId = `rustcloud_runtime_${Date.now()}`;
            try {
                legacyChunks.push([[], {
                    [moduleId]: (_module, _exports, require) => { runtime = require; }
                }, [[moduleId]]]);
            } catch (error) {
                console.warn('[RustCloud] Could not inspect SoundCloud modules:', error);
            }
            if (runtime && runtime.c) return runtime;
        }

        return null;
    };

    const findShuffleController = runtime => {
        for (const cachedModule of Object.values(runtime.c || {})) {
            const exported = cachedModule && cachedModule.exports;
            for (const candidate of [exported, exported && exported.default]) {
                if (
                    candidate &&
                    typeof candidate.toggleShuffle === 'function' &&
                    typeof candidate.getQueue === 'function' &&
                    typeof candidate.hasMoreAhead === 'function' &&
                    typeof candidate.pullNext === 'function'
                ) {
                    return candidate;
                }
            }
        }
        return null;
    };

    const isShuffleActive = () => {
        const control = document.querySelector(
            'button[aria-label*="shuffle" i], button[title*="shuffle" i], ' +
            'button[class*="shuffle"], [role="button"][aria-label*="shuffle" i]'
        );
        if (!control) return false;
        return control.getAttribute('aria-pressed') === 'true' ||
            [...control.classList].some(name => /(?:active|shuffling)/i.test(name));
    };

    const installShuffleEnhancement = () => {
        if (!settings.full_playlist_shuffle_enabled || window.__rustcloudShuffleController) return;
        const runtime = findWebpackRequire();
        const controller = runtime && findShuffleController(runtime);
        if (!controller) {
            if (!shufflePatchTimer) {
                shufflePatchAttempts = 0;
                shufflePatchTimer = window.setInterval(() => {
                    if (!settings.full_playlist_shuffle_enabled) {
                        clearInterval(shufflePatchTimer);
                        shufflePatchTimer = null;
                        return;
                    }
                    shufflePatchAttempts++;
                    const nextRuntime = findWebpackRequire();
                    const nextController = nextRuntime && findShuffleController(nextRuntime);
                    if (nextController) {
                        clearInterval(shufflePatchTimer);
                        shufflePatchTimer = null;
                        patchShuffleController(nextController);
                    } else if (shufflePatchAttempts >= 80) {
                        clearInterval(shufflePatchTimer);
                        shufflePatchTimer = null;
                        showShuffleStatus('Full-playlist shuffle is unavailable on this SoundCloud version.');
                    }
                }, 500);
            }
            return;
        }
        patchShuffleController(controller);
    };

    const patchShuffleController = controller => {
        if (window.__rustcloudShuffleController) return;
        const originalToggle = controller.toggleShuffle;
        controller.toggleShuffle = async function(...args) {
            if (!settings.full_playlist_shuffle_enabled || isShuffleActive()) {
                return originalToggle.apply(this, args);
            }
            if (shuffleLoading) return;

            const queue = controller.getQueue();
            if (!queue || !controller.hasMoreAhead()) {
                return originalToggle.apply(this, args);
            }

            shuffleLoading = true;
            const startedAt = Date.now();
            showShuffleStatus('Loading the rest of this playlist before shuffling…');
            try {
                while (controller.hasMoreAhead()) {
                    if (!settings.full_playlist_shuffle_enabled) {
                        throw new Error('Full-playlist shuffle was disabled.');
                    }
                    if (Date.now() - startedAt > 60000) {
                        throw new Error('Loading the playlist timed out.');
                    }

                    const currentQueue = controller.getQueue();
                    const previousLength = currentQueue ? currentQueue.length : 0;
                    let requestError = null;
                    Promise.resolve(controller.pullNext(shufflePageSize))
                        .catch(error => { requestError = error; });

                    const requestDeadline = Date.now() + 10000;
                    let advanced = false;
                    while (Date.now() < requestDeadline) {
                        await new Promise(resolve => setTimeout(resolve, 200));
                        if (requestError) throw requestError;
                        const updatedQueue = controller.getQueue();
                        const updatedLength = updatedQueue ? updatedQueue.length : 0;
                        if (updatedLength > previousLength || !controller.hasMoreAhead()) {
                            advanced = true;
                            break;
                        }
                    }
                    if (!advanced) throw new Error('SoundCloud did not load another playlist page.');
                }
                const loadedQueue = controller.getQueue();
                showShuffleStatus(`Loaded ${loadedQueue ? loadedQueue.length : 'all'} tracks. Shuffling now.`);
            } catch (error) {
                console.warn('[RustCloud] Full-playlist shuffle could not load every track:', error);
                showShuffleStatus('Could not load every track; shuffling the tracks currently loaded.');
            } finally {
                shuffleLoading = false;
            }

            return originalToggle.apply(this, args);
        };
        window.__rustcloudShuffleController = controller;
    };

    const mount = () => {
        if (!document.documentElement || document.getElementById('rustcloud-settings-host')) return;

        const host = document.createElement('div');
        host.id = 'rustcloud-settings-host';
        host.style.cssText = 'position:fixed;z-index:2147483647;inset:0;pointer-events:none;';
        host.style.setProperty('--accent', settings.accent_color);

        const root = host.attachShadow({ mode: 'open' });
        root.innerHTML = `
            <style>
                * { box-sizing: border-box; }
                :host {
                    color-scheme: dark; font-family: "Segoe UI", system-ui, sans-serif;
                    --theme-surface:#17171b; --theme-card:#ffffff0d; --theme-border:#ffffff18;
                }
                :host([data-theme="neon-pulse"]) {
                    --theme-surface:#100d1c; --theme-card:#d0baff0d; --theme-border:#e1caff24;
                }
                :host([data-theme="warm-vinyl"]) {
                    --theme-surface:#211711; --theme-card:#f2bd740d; --theme-border:#f4c18c24;
                }
                button, input { font: inherit; }
                #cover-art-glow-a, #cover-art-glow-b {
                    position: fixed; inset: -14%; z-index: 0; opacity: 0;
                    background-position: center; background-size: cover;
                    filter: blur(78px) saturate(1.65); transform: scale(1.08);
                    mix-blend-mode: screen; pointer-events: none;
                    mask-image: radial-gradient(ellipse at center, #000 12%, transparent 78%);
                    -webkit-mask-image: radial-gradient(ellipse at center, #000 12%, transparent 78%);
                    transition: opacity 1400ms cubic-bezier(.22,.61,.36,1);
                }
                kbd {
                    padding: 2px 6px; border-radius: 5px; background: #ffffff16;
                    color: #c9c9ce; font-size: 11px;
                }
                #scrim {
                    position: fixed; inset: 0; z-index: 1; background: #0008;
                    opacity: 0; pointer-events: none; transition: opacity 180ms ease;
                }
                #scrim.open { opacity: 1; pointer-events: auto; }
                #panel {
                    position: fixed; inset: 0 0 0 auto; z-index: 2;
                    width: min(390px, calc(100vw - 24px)); padding: 28px;
                    color: #f5f5f7; background: var(--theme-surface);
                    border-left: 1px solid var(--theme-border); box-shadow: -20px 0 60px #0007;
                    transform: translateX(105%); transition: transform 220ms ease;
                    pointer-events: auto; overflow-y: auto;
                }
                #panel.open { transform: translateX(0); }
                .topline { display:flex; align-items:center; justify-content:space-between; }
                .eyebrow {
                    margin: 0 0 8px; color: var(--accent); font-size: 11px;
                    font-weight: 700; letter-spacing: .14em; text-transform: uppercase;
                }
                h1 { margin: 0; font-size: 24px; letter-spacing: -.03em; }
                #close {
                    width: 36px; height: 36px; border: 1px solid #ffffff1c;
                    border-radius: 50%; color: #eee; background: #ffffff0c;
                    cursor: pointer; font-size: 20px;
                }
                .intro { margin: 10px 0 26px; color: #a7a7b0; font-size: 13px; line-height: 1.5; }
                .section-label {
                    margin: 22px 0 10px; color: #85858e; font-size: 11px;
                    font-weight: 700; letter-spacing: .12em; text-transform: uppercase;
                }
                .card {
                    padding: 16px; border: 1px solid var(--theme-border); border-radius: 15px;
                    background: linear-gradient(145deg,var(--theme-card),#ffffff05);
                }
                .row { display:flex; align-items:center; justify-content:space-between; gap:16px; }
                .title { color:#f5f5f7; font-size:14px; font-weight:600; }
                .description { margin-top:5px; color:#96969f; font-size:12px; line-height:1.45; }
                .switch { position:relative; width:44px; height:26px; flex:none; }
                .switch input { position:absolute; opacity:0; width:1px; height:1px; }
                .track {
                    position:absolute; inset:0; border-radius:99px; background:#45454d;
                    cursor:pointer; transition:background 160ms ease;
                }
                .track::after {
                    content:""; position:absolute; top:3px; left:3px; width:20px; height:20px;
                    border-radius:50%; background:#fff; transition:transform 160ms ease;
                }
                .switch input:checked + .track { background:var(--accent); }
                .switch input:checked + .track::after { transform:translateX(18px); }
                .switch input:focus-visible + .track { outline:2px solid #fff; outline-offset:3px; }
                .palette { display:flex; flex-wrap:wrap; gap:11px; align-items:center; }
                .swatch {
                    width:27px; height:27px; padding:0; border:2px solid transparent;
                    border-radius:50%; cursor:pointer;
                }
                .swatch.active { outline:2px solid #fff; outline-offset:3px; }
                .theme-grid { display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:8px; }
                .theme-choice {
                    min-width:0; padding:8px 6px; border:1px solid var(--theme-border);
                    border-radius:11px; color:#d5d5da; background:#ffffff06;
                    cursor:pointer; text-align:center; transition:transform 150ms ease,border-color 150ms ease;
                }
                .theme-choice:hover { transform:translateY(-2px); border-color:var(--accent); }
                .theme-choice[aria-pressed="true"] { border-color:var(--accent); background:#ffffff10; }
                .theme-preview {
                    display:flex; align-items:center; justify-content:center; gap:4px;
                    height:31px; margin-bottom:7px; border-radius:7px; background:#111620;
                }
                .theme-preview span { width:13px; height:13px; border-radius:50%; }
                .theme-preview i { width:24px; height:6px; border-radius:99px; background:#ffffff28; }
                .theme-choice[data-theme="midnight-glass"] .theme-preview { background:linear-gradient(135deg,#101827,#23314a); }
                .theme-choice[data-theme="midnight-glass"] .theme-preview span { background:#74c9ff; box-shadow:0 0 8px #74c9ff; }
                .theme-choice[data-theme="neon-pulse"] .theme-preview { background:linear-gradient(135deg,#160e29,#281143); }
                .theme-choice[data-theme="neon-pulse"] .theme-preview span { background:#ef65ff; box-shadow:0 0 8px #ef65ff; }
                .theme-choice[data-theme="warm-vinyl"] .theme-preview { background:linear-gradient(135deg,#211710,#43281d); }
                .theme-choice[data-theme="warm-vinyl"] .theme-preview span { background:#f2ae59; box-shadow:0 0 8px #f2ae59; }
                .theme-choice[data-theme="forest-glow"] .theme-preview { background:linear-gradient(135deg,#0d1813,#1b3528); }
                .theme-choice[data-theme="forest-glow"] .theme-preview span { background:#69d6a8; box-shadow:0 0 8px #69d6a8; }
                .theme-choice[data-theme="rose-quartz"] .theme-preview { background:linear-gradient(135deg,#1c101b,#392036); }
                .theme-choice[data-theme="rose-quartz"] .theme-preview span { background:#ff82b7; box-shadow:0 0 8px #ff82b7; }
                .theme-choice[data-theme="ocean-breeze"] .theme-preview { background:linear-gradient(135deg,#0c1720,#153247); }
                .theme-choice[data-theme="ocean-breeze"] .theme-preview span { background:#67dce5; box-shadow:0 0 8px #67dce5; }
                .theme-choice[data-theme="default"] .theme-preview { background:linear-gradient(135deg,#262626,#414141); }
                .theme-choice[data-theme="default"] .theme-preview span { background:#ff5500; }
                .theme-choice-name { display:block; font-size:10px; font-weight:650; line-height:1.2; }
                .theme-choice-note { display:block; margin-top:3px; color:#92929b; font-size:9px; }
                :host([data-theme="neon-pulse"]) #panel,
                :host([data-theme="neon-pulse"]) #library-panel { box-shadow:0 0 38px #ad42ff22,20px 0 60px #0006; }
                :host([data-theme="warm-vinyl"]) .card { border-radius:11px; }
                :host([data-theme="warm-vinyl"]) #panel,
                :host([data-theme="warm-vinyl"]) #library-panel { border-color:#f2ae5940; }
                #custom-color {
                    width:32px; height:32px; padding:0; overflow:hidden; border:0;
                    border-radius:50%; background:transparent; cursor:pointer;
                }
                #custom-color::-webkit-color-swatch-wrapper { padding:0; }
                #custom-color::-webkit-color-swatch { border:2px solid #ffffff65; border-radius:50%; }
                .footer { margin-top:28px; color:#777780; font-size:11px; }
                #shuffle-status {
                    position:fixed; right:22px; bottom:22px; z-index:4;
                    max-width:min(360px,calc(100vw - 32px)); padding:12px 16px;
                    border:1px solid #ffffff1c; border-radius:12px; color:#f5f5f7;
                    background:#17171bf2; box-shadow:0 10px 32px #0007;
                    font-size:13px; line-height:1.45; opacity:0;
                    transform:translateY(8px); transition:opacity 180ms ease,transform 180ms ease;
                    pointer-events:none;
                }
                #shuffle-status.visible { opacity:1; transform:translateY(0); }
                #library-launcher {
                    position:fixed; top:16px; left:20px; z-index:3;
                    display:flex; align-items:center; gap:9px; padding:10px 14px;
                    border:1px solid #ffffff20; border-radius:999px; color:#fff;
                    background:#17171be8; box-shadow:0 8px 26px #0005;
                    cursor:pointer; pointer-events:auto; backdrop-filter:blur(14px);
                    opacity:1; transform:translateY(0);
                    transition:opacity 180ms ease,transform 180ms ease;
                }
                #library-launcher:hover { border-color:var(--accent); }
                #library-launcher.hidden {
                    opacity:0; transform:translateY(-6px); pointer-events:none;
                }
                #library-mark { color:var(--accent); font-size:17px; line-height:1; }
                #library-panel {
                    position:fixed; inset:0 auto 0 0; z-index:2;
                    display:flex; flex-direction:column; gap:18px;
                    width:min(320px,calc(100vw - 24px)); padding:24px 16px;
                    color:#f5f5f7; background:#17171bf7;
                    border-right:1px solid #ffffff14; box-shadow:20px 0 60px #0006;
                    transform:translateX(-105%); transition:transform 220ms ease;
                    pointer-events:auto;
                }
                #library-panel.open { transform:translateX(0); }
                .library-header { display:flex; align-items:center; justify-content:space-between; }
                .library-header h2 { margin:0; font-size:21px; letter-spacing:-.03em; }
                .library-actions { display:flex; gap:7px; }
                .library-action {
                    width:34px; height:34px; border:1px solid #ffffff1c;
                    border-radius:50%; color:#ddd; background:#ffffff0c;
                    cursor:pointer; font-size:17px;
                }
                .library-action:hover { color:#fff; border-color:var(--accent); }
                .library-action:disabled { opacity:.55; cursor:default; }
                #library-refresh.refreshing { animation:library-spin 800ms linear infinite; }
                @keyframes library-spin { to { transform:rotate(360deg); } }
                #library-search {
                    width:100%; padding:10px 12px; border:1px solid #ffffff18;
                    border-radius:10px; outline:none; color:#f5f5f7;
                    background:#ffffff0b; font-size:13px;
                }
                #library-search:focus { border-color:var(--accent); }
                #library-status { min-height:16px; margin:-9px 2px -8px; color:#96969f; font-size:11px; }
                #playlist-items { overflow-y:auto; flex:1; min-height:0; }
                .library-group { margin:4px 0 16px; }
                .library-group-title {
                    margin:0 0 8px; color:#8d8d96; font-size:10px;
                    font-weight:700; letter-spacing:.13em; text-transform:uppercase;
                }
                .library-item {
                    display:flex; align-items:center; gap:11px; width:100%;
                    padding:8px; border:0; border-radius:9px; color:#e7e7ea;
                    background:transparent; text-align:left; cursor:pointer;
                }
                .library-item:hover { background:#ffffff10; color:#fff; }
                .library-item img, .library-art-placeholder {
                    width:42px; height:42px; flex:none; border-radius:6px;
                    object-fit:cover; background:#ffffff12;
                }
                .library-art-placeholder { display:grid; place-items:center; color:#999; font-size:17px; }
                .library-item-title {
                    overflow:hidden; display:-webkit-box; -webkit-box-orient:vertical;
                    -webkit-line-clamp:2; font-size:12px; font-weight:600; line-height:1.35;
                }
                .library-empty { padding:18px 8px; color:#96969f; font-size:12px; line-height:1.55; }
            </style>
            <button id="library-launcher" aria-expanded="false" aria-controls="library-panel">
                <span id="library-mark" aria-hidden="true">♫</span><span>Library</span>
            </button>
            <aside id="library-panel" aria-label="Playlist library">
                <div class="library-header">
                    <div><p class="eyebrow">Your SoundCloud</p><h2>Playlists</h2></div>
                    <div class="library-actions">
                        <button class="library-action" id="library-refresh" aria-label="Refresh playlists" title="Refresh">↻</button>
                        <button class="library-action" id="library-close" aria-label="Close library">×</button>
                    </div>
                </div>
                <input id="library-search" type="search" placeholder="Find a playlist" aria-label="Search playlists">
                <p id="library-status" role="status" aria-live="polite"></p>
                <div id="playlist-items"><div class="library-empty">Press refresh to load your playlists.</div></div>
            </aside>
            <div id="scrim"></div>
            <aside id="panel" role="dialog" aria-modal="true" aria-label="RustCloud settings">
                <div class="topline">
                    <div><p class="eyebrow">Personalize</p><h1>Settings</h1></div>
                    <button id="close" aria-label="Close settings">×</button>
                </div>
                <p class="intro">Make RustCloud feel like yours. Your choices are saved on this device.</p>
                <p class="section-label">Protection</p>
                <div class="card row">
                    <div>
                        <div class="title">Ad blocking</div>
                        <div class="description">Blocks new requests matched by your filter lists.</div>
                    </div>
                    <label class="switch" aria-label="Enable ad blocking">
                        <input id="adblock" type="checkbox">
                        <span class="track"></span>
                    </label>
                </div>
                <p class="section-label">Appearance</p>
                <div class="card">
                    <div class="title" style="margin-bottom:12px">SoundCloud theme</div>
                    <div class="theme-grid" role="group" aria-label="SoundCloud theme">
                        <button class="theme-choice" data-theme="default" aria-pressed="false">
                            <span class="theme-preview"><span></span><i></i></span>
                            <span class="theme-choice-name">Default</span>
                            <span class="theme-choice-note">Original SoundCloud</span>
                        </button>
                        <button class="theme-choice" data-theme="midnight-glass" aria-pressed="false">
                            <span class="theme-preview"><span></span><i></i></span>
                            <span class="theme-choice-name">Midnight Glass</span>
                            <span class="theme-choice-note">Cool &amp; smooth</span>
                        </button>
                        <button class="theme-choice" data-theme="neon-pulse" aria-pressed="false">
                            <span class="theme-preview"><span></span><i></i></span>
                            <span class="theme-choice-name">Neon Pulse</span>
                            <span class="theme-choice-note">Bright &amp; vivid</span>
                        </button>
                        <button class="theme-choice" data-theme="warm-vinyl" aria-pressed="false">
                            <span class="theme-preview"><span></span><i></i></span>
                            <span class="theme-choice-name">Warm Vinyl</span>
                            <span class="theme-choice-note">Amber &amp; cozy</span>
                        </button>
                        <button class="theme-choice" data-theme="forest-glow" aria-pressed="false">
                            <span class="theme-preview"><span></span><i></i></span>
                            <span class="theme-choice-name">Forest Glow</span>
                            <span class="theme-choice-note">Calm &amp; green</span>
                        </button>
                        <button class="theme-choice" data-theme="rose-quartz" aria-pressed="false">
                            <span class="theme-preview"><span></span><i></i></span>
                            <span class="theme-choice-name">Rose Quartz</span>
                            <span class="theme-choice-note">Soft &amp; rosy</span>
                        </button>
                        <button class="theme-choice" data-theme="ocean-breeze" aria-pressed="false">
                            <span class="theme-preview"><span></span><i></i></span>
                            <span class="theme-choice-name">Ocean Breeze</span>
                            <span class="theme-choice-note">Fresh &amp; cool</span>
                        </button>
                    </div>
                    <div class="title" style="margin-bottom:14px">Accent color</div>
                    <div class="palette" role="group" aria-label="Accent color">
                        <button class="swatch" data-color="#ff5500" aria-label="SoundCloud orange"></button>
                        <button class="swatch" data-color="#a78bfa" aria-label="Violet"></button>
                        <button class="swatch" data-color="#38bdf8" aria-label="Sky blue"></button>
                        <button class="swatch" data-color="#34d399" aria-label="Mint green"></button>
                        <button class="swatch" data-color="#fb7185" aria-label="Rose"></button>
                        <input id="custom-color" type="color" aria-label="Choose a custom accent color">
                    </div>
                </div>
                <p class="section-label">Ambience</p>
                <div class="card row">
                    <div>
                        <div class="title">Cover art glow</div>
                        <div class="description">Add a soft, color-shifting glow from the current track artwork.</div>
                    </div>
                    <label class="switch" aria-label="Enable cover art glow">
                        <input id="cover-glow" type="checkbox">
                        <span class="track"></span>
                    </label>
                </div>
                <p class="section-label">Playback</p>
                <div class="card row">
                    <div>
                        <div class="title">Full-playlist shuffle</div>
                        <div class="description">Load the rest of a playlist before shuffling. Large playlists may take a little while.</div>
                    </div>
                    <label class="switch" aria-label="Enable full-playlist shuffle">
                        <input id="full-playlist-shuffle" type="checkbox">
                        <span class="track"></span>
                    </label>
                </div>
                <p class="footer">Press F1 to toggle settings · Esc to close</p>
            </aside>
            <div id="cover-art-glow-a" aria-hidden="true"></div>
            <div id="cover-art-glow-b" aria-hidden="true"></div>
            <div id="shuffle-status" role="status" aria-live="polite"></div>
        `;

        document.documentElement.appendChild(host);
        const libraryLauncher = root.getElementById('library-launcher');
        const libraryPanel = root.getElementById('library-panel');
        const libraryClose = root.getElementById('library-close');
        const libraryRefresh = root.getElementById('library-refresh');
        const librarySearch = root.getElementById('library-search');
        const libraryStatus = root.getElementById('library-status');
        const playlistItemsElement = root.getElementById('playlist-items');
        const panel = root.getElementById('panel');
        const scrim = root.getElementById('scrim');
        const adblock = root.getElementById('adblock');
        const coverGlow = root.getElementById('cover-glow');
        const fullPlaylistShuffle = root.getElementById('full-playlist-shuffle');
        const shuffleStatus = root.getElementById('shuffle-status');
        const artGlowLayers = [
            root.getElementById('cover-art-glow-a'),
            root.getElementById('cover-art-glow-b')
        ];
        const colorPicker = root.getElementById('custom-color');
        const swatches = [...root.querySelectorAll('.swatch')];
        const themeButtons = [...root.querySelectorAll('.theme-choice')];
        const themeStyle = document.createElement('style');
        themeStyle.id = 'rustcloud-theme-styles';
        themeStyle.textContent = `
            :root[data-rustcloud-theme="midnight-glass"] {
                --rustcloud-accent:#78caff; --rustcloud-accent-deep:#347ebc;
                --rustcloud-glow:#78caff66; --rustcloud-glow-soft:#78caff35;
                --rustcloud-glow-faint:#78caff18;
                --rustcloud-player:#111827e8; --rustcloud-page:#0c121d;
                --rustcloud-surface:#152131; --rustcloud-text:#e5eef8;
                --rustcloud-muted:#9aafc4; --rustcloud-edge:#2b3d51;
                --rustcloud-font:"Segoe UI Variable","Segoe UI",sans-serif;
            }
            :root[data-rustcloud-theme="neon-pulse"] {
                --rustcloud-accent:#f16bff; --rustcloud-accent-deep:#833bdb;
                --rustcloud-glow:#f16bff99; --rustcloud-glow-soft:#f16bff38;
                --rustcloud-glow-faint:#61e7ff1c;
                --rustcloud-player:#170f24ed; --rustcloud-page:#100b18;
                --rustcloud-surface:#1c1328; --rustcloud-text:#f6eafa;
                --rustcloud-muted:#c0a9ce; --rustcloud-edge:#483054;
                --rustcloud-font:"Bahnschrift","Segoe UI",sans-serif;
            }
            :root[data-rustcloud-theme="warm-vinyl"] {
                --rustcloud-accent:#f3ad59; --rustcloud-accent-deep:#a34e32;
                --rustcloud-glow:#f3ad5966; --rustcloud-glow-soft:#f3ad5935;
                --rustcloud-glow-faint:#a34e321a;
                --rustcloud-player:#211710ed; --rustcloud-page:#1b1410;
                --rustcloud-surface:#291d16; --rustcloud-text:#f5e8d8;
                --rustcloud-muted:#c3a98c; --rustcloud-edge:#4d3728;
                --rustcloud-font:"Trebuchet MS","Segoe UI",sans-serif;
            }
            :root[data-rustcloud-theme="forest-glow"] {
                --rustcloud-accent:#69d6a8; --rustcloud-accent-deep:#28785f;
                --rustcloud-glow:#69d6a866; --rustcloud-glow-soft:#69d6a835;
                --rustcloud-glow-faint:#45c9aa18;
                --rustcloud-player:#111e1aeb; --rustcloud-page:#0c1511;
                --rustcloud-surface:#14231b; --rustcloud-text:#e4f3e9;
                --rustcloud-muted:#a0b9a9; --rustcloud-edge:#2d4939;
                --rustcloud-font:"Segoe UI Variable","Segoe UI",sans-serif;
            }
            :root[data-rustcloud-theme="rose-quartz"] {
                --rustcloud-accent:#ff82b7; --rustcloud-accent-deep:#a84b7b;
                --rustcloud-glow:#ff82b777; --rustcloud-glow-soft:#ff82b735;
                --rustcloud-glow-faint:#d46bff18;
                --rustcloud-player:#1d111bed; --rustcloud-page:#150d15;
                --rustcloud-surface:#241625; --rustcloud-text:#f8eaf3;
                --rustcloud-muted:#c6a9bc; --rustcloud-edge:#50334a;
                --rustcloud-font:"Georgia","Times New Roman",serif;
            }
            :root[data-rustcloud-theme="ocean-breeze"] {
                --rustcloud-accent:#67dce5; --rustcloud-accent-deep:#347eb3;
                --rustcloud-glow:#67dce577; --rustcloud-glow-soft:#67dce535;
                --rustcloud-glow-faint:#4c9dff18;
                --rustcloud-player:#0e1a24ed; --rustcloud-page:#0a121a;
                --rustcloud-surface:#122332; --rustcloud-text:#e5f3f8;
                --rustcloud-muted:#9eb8c5; --rustcloud-edge:#2b4659;
                --rustcloud-font:"Calibri","Segoe UI",sans-serif;
            }
            html[data-rustcloud-theme], body {
                background-color:var(--rustcloud-page)!important;
                color:var(--rustcloud-text)!important;
            }
            body {
                background-image:
                    radial-gradient(ellipse at 12% 8%,var(--rustcloud-glow-soft),transparent 43%),
                    radial-gradient(ellipse at 88% 48%,var(--rustcloud-glow-faint),transparent 39%)!important;
                background-attachment:fixed!important;
                font-family:var(--rustcloud-font)!important;
            }
            body button, body input, body textarea,
            body .sc-type, body .sc-font,
            body [class*="soundTitle"], body [class*="playlistHeader"] {
                font-family:var(--rustcloud-font)!important;
            }
            body .header, body .header__inner, body .l-header,
            body .playControls, body .playControls__bg,
            body .playControls__background, body .playControls__inner {
                background-color:var(--rustcloud-surface)!important;
                background-image:none!important;
                border-color:var(--rustcloud-edge)!important;
                backdrop-filter:blur(16px);
            }
            body .playControls__bg::before, body .playControls__bg::after {
                background-color:var(--rustcloud-surface)!important;
                background-image:none!important;
            }
            body .sc-type-light, body .sc-type-medium,
            body .soundTitle__username, body .soundTitle__tag,
            body .userInfo__followers, body .soundStats, body .soundStats__stat {
                color:var(--rustcloud-muted)!important;
            }
            body .soundTitle__title, body .sound__soundTitle,
            body .playlistHeader__title, body .userInfo__username,
            body .header__navMenu, body .header__navItem {
                color:var(--rustcloud-text)!important;
            }
            body .soundTitle__title[data-rustcloud-playing="true"],
            body .soundList__item .soundTitle__title[data-rustcloud-playing="true"],
            body .soundList__item [data-rustcloud-playing="true"],
            body .playbackSoundBadge__title,
            body .playbackSoundBadge__title.sc-link-primary {
                color:var(--rustcloud-accent)!important;
                text-shadow:0 0 9px var(--rustcloud-glow-soft);
            }
            body .soundList__item[data-rustcloud-playing="true"] {
                background-color:var(--rustcloud-glow-soft)!important;
            }
            body .soundList__item--playing,
            body .soundList__item.playing,
            body .soundList__item.sc-pause,
            body .soundList__item[aria-current="true"] {
                background-color:var(--rustcloud-glow-soft)!important;
            }
            body .soundList__item:hover, body .userStream__item:hover,
            body .sound__soundActions .sc-button:hover {
                background-color:color-mix(in srgb,var(--rustcloud-accent) 7%,transparent)!important;
                box-shadow:0 2px 12px var(--rustcloud-glow-soft);
            }
            body .playControl, body .playControls__play, body .sc-button-cta {
                color:#fff!important;
                background:linear-gradient(140deg,var(--rustcloud-accent),var(--rustcloud-accent-deep))!important;
                border:1px solid color-mix(in srgb,var(--rustcloud-accent) 70%,white)!important;
                box-shadow:0 3px 14px var(--rustcloud-glow)!important;
                transition:transform 160ms ease,filter 160ms ease,box-shadow 160ms ease!important;
            }
            body .playControl:hover, body .playControls__play:hover, body .sc-button-cta:hover {
                filter:brightness(1.12)!important; transform:translateY(-1px);
                box-shadow:0 5px 20px var(--rustcloud-glow)!important;
            }
            body .sc-button, body .sc-button-small, body .sc-button-medium,
            body .sc-button-large, body .soundActions .sc-button,
            body .playControls__control, body .volume__button {
                border-radius:999px!important;
                transition:background-color 160ms ease,border-color 160ms ease,
                    color 160ms ease,transform 160ms ease!important;
            }
            body .playControls, body .playControls__bg,
            body .playControls__background, body .playControls__inner {
                background:var(--rustcloud-player)!important;
                box-shadow:0 -3px 24px var(--rustcloud-glow-soft);
            }
            body .sc-button, body .sc-button-small, body .sc-button-medium,
            body .sc-button-large, body .soundActions .sc-button,
            body .playControls__control, body .volume__button {
                border-color:var(--rustcloud-edge)!important;
            }
            body .playbackTimeline__progress,
            body .playbackTimeline__progressBar,
            body .volume__sliderProgress {
                background:linear-gradient(90deg,var(--rustcloud-accent-deep),var(--rustcloud-accent))!important;
                border-radius:999px!important;
                box-shadow:0 0 8px var(--rustcloud-glow)!important;
            }
            body .playbackTimeline__progressBackground,
            body .volume__sliderBackground {
                background-color:var(--rustcloud-edge)!important;
                border-radius:999px!important;
            }
            body .playbackTimeline__progressHandle {
                background:var(--rustcloud-accent)!important;
                box-shadow:0 0 10px var(--rustcloud-glow)!important;
            }
            :root[data-rustcloud-theme="midnight-glass"] body .sc-button,
            :root[data-rustcloud-theme="midnight-glass"] body .playControls__control {
                background-color:#c8e5ff12;
            }
            :root[data-rustcloud-theme="neon-pulse"] body .sc-button,
            :root[data-rustcloud-theme="neon-pulse"] body .playControls__control {
                border:1px solid #f16bff45!important;
                background-image:linear-gradient(120deg,#f16bff14,#61e7ff10);
            }
            :root[data-rustcloud-theme="warm-vinyl"] body .playControl,
            :root[data-rustcloud-theme="warm-vinyl"] body .playControls__play,
            :root[data-rustcloud-theme="warm-vinyl"] body .sc-button-cta {
                border-radius:10px!important;
                box-shadow:inset 0 1px #ffffff42,0 3px 12px var(--rustcloud-glow)!important;
            }
            body .playControls .playControls__control,
            body .playControls .playControl,
            body .playControls .volume__button,
            body .playControls .shuffleControl,
            body .playControls .repeatControl {
                color:#f5f5f5!important;
                background-color:transparent!important;
                background-image:none!important;
                border-color:#ffffff1c!important;
                box-shadow:none!important;
                filter:none!important;
            }
            body .playControls .playControl {
                background-color:#ffffff18!important;
                border-color:#ffffff38!important;
            }
            body .playControls .volume__button {
                box-sizing:border-box!important;
                width:32px!important;
                min-width:32px!important;
                height:32px!important;
                min-height:32px!important;
                padding:0!important;
                border-radius:50%!important;
            }
            body .playControls .playControls__control svg,
            body .playControls .playControl svg,
            body .playControls .volume__button svg,
            body .playControls .shuffleControl svg,
            body .playControls .repeatControl svg {
                fill:currentColor!important;
            }
            body .playControls .playControls__control:hover,
            body .playControls .playControl:hover,
            body .playControls .volume__button:hover,
            body .playControls .shuffleControl:hover,
            body .playControls .repeatControl:hover {
                color:#111!important;
                background-color:#f2f2f2!important;
                border-color:#ffffff!important;
            }
            body .playControls .playControl.sc-pause,
            body .playControls .shuffleControl.m-shuffling,
            body .playControls .repeatControl.m-active,
            body .playControls .playControls__control.m-active {
                color:#111!important;
                background-color:#f2f2f2!important;
                border-color:#ffffff!important;
            }
        `;
        const themeNonce = document.querySelector('script[nonce]')?.nonce;
        if (themeNonce) themeStyle.nonce = themeNonce;
        (document.head || document.documentElement).appendChild(themeStyle);
        let artworkObserver = null;
        let artworkRootObserver = null;
        let observedArtworkNode = null;
        let artworkUpdateScheduled = false;
        let activeGlowIndex = 0;
        let artworkGeneration = 0;
        let lastArtworkUrl = '';
        let shuffleStatusTimer = null;
        let libraryItems = [];
        let libraryLoadPromise = null;

        window.__RUSTCLOUD_SHUFFLE_NOTIFY__ = message => {
            shuffleStatus.textContent = message;
            shuffleStatus.classList.add('visible');
            clearTimeout(shuffleStatusTimer);
            shuffleStatusTimer = window.setTimeout(
                () => shuffleStatus.classList.remove('visible'),
                5000
            );
        };

        const post = message => {
            if (window.ipc && typeof window.ipc.postMessage === 'function') {
                window.ipc.postMessage(JSON.stringify(message));
            }
        };
        const setOpen = open => {
            panel.classList.toggle('open', open);
            scrim.classList.toggle('open', open);
        };
        const setLibraryOpen = open => {
            libraryPanel.classList.toggle('open', open);
            libraryLauncher.classList.toggle('hidden', open);
            libraryLauncher.setAttribute('aria-expanded', String(open));
            if (open) setOpen(false);
        };
        const setAccent = color => {
            if (!/^#[0-9a-f]{6}$/i.test(color)) return;
            host.style.setProperty('--accent', color);
            colorPicker.value = color;
            swatches.forEach(button => button.classList.toggle('active', button.dataset.color === color));
            settings.accent_color = color;
            saveUiPreferences();
            post({ type: 'accent', color });
        };
        const themes = {
            default: { accent: null },
            'midnight-glass': { accent: '#78caff' },
            'neon-pulse': { accent: '#f16bff' },
            'warm-vinyl': { accent: '#f3ad59' },
            'forest-glow': { accent: '#69d6a8' },
            'rose-quartz': { accent: '#ff82b7' },
            'ocean-breeze': { accent: '#67dce5' }
        };
        const applyTheme = (name, updateAccent) => {
            if (!Object.prototype.hasOwnProperty.call(themes, name)) name = 'midnight-glass';
            settings.theme = name;
            if (name === 'default') {
                document.documentElement.removeAttribute('data-rustcloud-theme');
                host.removeAttribute('data-theme');
                themeStyle.disabled = true;
            } else {
                document.documentElement.dataset.rustcloudTheme = name;
                host.dataset.theme = name;
                themeStyle.disabled = false;
            }
            themeButtons.forEach(button => {
                button.setAttribute('aria-pressed', String(button.dataset.theme === name));
            });
            if (updateAccent && themes[name].accent) setAccent(themes[name].accent);
            saveUiPreferences();
            post({ type: 'theme', name });
        };
        const showLibraryMessage = message => {
            playlistItemsElement.replaceChildren();
            const empty = document.createElement('div');
            empty.className = 'library-empty';
            empty.textContent = message;
            playlistItemsElement.appendChild(empty);
        };
        const renderLibrary = () => {
            const query = librarySearch.value.trim().toLocaleLowerCase();
            const filtered = libraryItems.filter(item => item.title.toLocaleLowerCase().includes(query));
            playlistItemsElement.replaceChildren();

            for (const [kind, label] of [['created', 'Your playlists'], ['saved', 'Liked playlists']]) {
                const items = filtered.filter(item => item.kind === kind);
                if (!items.length) continue;

                const group = document.createElement('section');
                group.className = 'library-group';
                const heading = document.createElement('h3');
                heading.className = 'library-group-title';
                heading.textContent = label;
                group.appendChild(heading);

                for (const item of items) {
                    const button = document.createElement('button');
                    button.className = 'library-item';
                    button.type = 'button';
                    button.setAttribute('aria-label', `Open playlist ${item.title}`);
                    if (item.artwork) {
                        const image = document.createElement('img');
                        image.src = item.artwork;
                        image.alt = '';
                        image.loading = 'lazy';
                        button.appendChild(image);
                    } else {
                        const placeholder = document.createElement('span');
                        placeholder.className = 'library-art-placeholder';
                        placeholder.setAttribute('aria-hidden', 'true');
                        placeholder.textContent = '♫';
                        button.appendChild(placeholder);
                    }

                    const title = document.createElement('span');
                    title.className = 'library-item-title';
                    title.textContent = item.title;
                    button.appendChild(title);
                    button.addEventListener('click', () => {
                        const url = new URL(item.url, window.location.origin);
                        if (url.origin === window.location.origin) window.location.assign(url.href);
                    });
                    group.appendChild(button);
                }
                playlistItemsElement.appendChild(group);
            }

            if (!filtered.length) {
                showLibraryMessage(query ? 'No playlists match that search.' : 'No playlists were found. Make sure you’re signed in and refresh.');
            }
        };
        const scrapePlaylistPage = (path, kind) => new Promise(resolve => {
            const frame = document.createElement('iframe');
            frame.title = 'RustCloud playlist library source';
            frame.style.cssText =
                'position:fixed;left:-10000px;top:0;width:1280px;height:900px;opacity:0;' +
                'pointer-events:none;border:0;';

            const startedAt = Date.now();
            let interval = null;
            let timeout = null;
            let stableTicks = 0;
            let previousCount = 0;
            let clickedPlaylistTab = false;
            let finished = false;
            const finish = items => {
                if (finished) return;
                finished = true;
                if (interval !== null) clearInterval(interval);
                if (timeout !== null) clearTimeout(timeout);
                frame.remove();
                resolve(items);
            };
            const readItems = doc => {
                const results = new Map();
                for (const link of doc.querySelectorAll('a[href]')) {
                    let url;
                    try {
                        url = new URL(link.href, doc.location.href);
                    } catch (_) {
                        continue;
                    }
                    const segments = url.pathname.split('/').filter(Boolean);
                    const setsIndex = segments.indexOf('sets');
                    if (url.origin !== window.location.origin ||
                        setsIndex < 1 || setsIndex !== segments.length - 2) continue;

                    const image = link.querySelector('img');
                    const heading = link.querySelector('h2,h3,[class*="title"]');
                    const title = [
                        heading && heading.textContent,
                        link.getAttribute('aria-label'),
                        link.textContent,
                        image && image.alt
                    ].map(value => (value || '').trim()).find(Boolean) || '';
                    const key = `${url.origin}${url.pathname.replace(/\/$/, '')}`;
                    if (!title || results.has(key)) continue;
                    const artwork = image && (image.currentSrc || image.src);
                    results.set(key, {
                        kind,
                        title,
                        url: key,
                        artwork: artwork && artwork.startsWith('https://') ? artwork : ''
                    });
                }
                return [...results.values()];
            };

            const poll = () => {
                if (finished) return;
                if (Date.now() - startedAt > 25000) {
                    finish([]);
                    return;
                }

                try {
                    const doc = frame.contentDocument;
                    if (!doc || !doc.documentElement) return;
                    const elapsed = Date.now() - startedAt;

                    if (kind === 'saved' && !clickedPlaylistTab && elapsed > 3500) {
                        const playlistTab = [...doc.querySelectorAll('a,button,[role="tab"]')]
                            .find(element => /^(playlists?|albums)$/i.test(
                                (element.getAttribute('aria-label') || element.textContent || '').trim()
                            ));
                        if (playlistTab) {
                            clickedPlaylistTab = true;
                            playlistTab.click();
                            previousCount = 0;
                            stableTicks = 0;
                            return;
                        }
                        if (elapsed > 12000) clickedPlaylistTab = true;
                    }

                    const items = readItems(doc);
                    const count = items.length;
                    if (count > previousCount) {
                        previousCount = count;
                        stableTicks = 0;
                    } else if (elapsed > 6000) {
                        stableTicks++;
                    }

                    const scroller = doc.scrollingElement || doc.documentElement;
                    frame.contentWindow.scrollTo(0, scroller.scrollHeight);
                    if (stableTicks >= 6) finish(items);
                } catch (error) {
                    console.warn(`[RustCloud] Could not read ${kind} playlists:`, error);
                    finish([]);
                }
            };

            frame.addEventListener('load', () => {
                if (interval === null) interval = window.setInterval(poll, 500);
            });
            frame.addEventListener('error', () => finish([]), { once: true });
            frame.src = new URL(path, window.location.origin).href;
            document.documentElement.appendChild(frame);
            timeout = window.setTimeout(() => finish([]), 26000);
        });
        const playlistCacheKey = 'rustcloud.playlist-library.v1';
        const readPlaylistCache = () => {
            try {
                const cached = JSON.parse(localStorage.getItem(playlistCacheKey) || 'null');
                if (!Array.isArray(cached)) return [];
                return cached.filter(item => {
                    if (!item || typeof item.title !== 'string' ||
                        typeof item.url !== 'string' ||
                        !['created', 'saved'].includes(item.kind)) return false;
                    try {
                        const url = new URL(item.url, window.location.origin);
                        return url.origin === window.location.origin &&
                            /\/[^/]+\/sets\/[^/]+/.test(url.pathname);
                    } catch (_) {
                        return false;
                    }
                }).map(item => ({
                    kind: item.kind,
                    title: item.title,
                    url: item.url,
                    artwork: typeof item.artwork === 'string' &&
                        item.artwork.startsWith('https://') ? item.artwork : ''
                }));
            } catch (error) {
                console.warn('[RustCloud] Could not read the playlist cache:', error);
                return [];
            }
        };
        const loadLibrary = () => {
            if (libraryLoadPromise) return libraryLoadPromise;

            libraryStatus.textContent = libraryItems.length ? 'Refreshing playlists…' : '';
            if (!libraryItems.length) showLibraryMessage('Loading your playlists…');
            libraryRefresh.disabled = true;
            libraryRefresh.classList.add('refreshing');
            libraryLoadPromise = Promise.all([
                scrapePlaylistPage('/you/sets', 'created'),
                scrapePlaylistPage('/you/likes', 'saved')
            ]).then(([created, saved]) => {
                const createdUrls = new Set(created.map(item => item.url));
                libraryItems = [
                    ...created,
                    ...saved.filter(item => !createdUrls.has(item.url))
                ];
                try {
                    localStorage.setItem(playlistCacheKey, JSON.stringify(libraryItems));
                } catch (error) {
                    console.warn('[RustCloud] Could not save the playlist cache:', error);
                }
                renderLibrary();
                libraryStatus.textContent = 'Playlists refreshed.';
            }).catch(error => {
                console.error('[RustCloud] Could not load your playlist library:', error);
                libraryStatus.textContent = libraryItems.length
                    ? 'Refresh failed; showing the saved list.'
                    : 'Refresh failed. Check your connection and try again.';
                if (!libraryItems.length) showLibraryMessage('No playlists loaded. Press refresh to try again.');
            }).finally(() => {
                libraryLoadPromise = null;
                libraryRefresh.disabled = false;
                libraryRefresh.classList.remove('refreshing');
            });
            return libraryLoadPromise;
        };

        adblock.checked = settings.ad_block_enabled !== false;
        coverGlow.checked = settings.cover_glow_enabled === true;
        fullPlaylistShuffle.checked = settings.full_playlist_shuffle_enabled === true;
        colorPicker.value = settings.accent_color;
        themeButtons.forEach(button => {
            button.addEventListener('click', () => {
                applyTheme(button.dataset.theme, true);
                syncPlayingTrackTheme();
            });
        });
        swatches.forEach(button => {
            button.style.background = button.dataset.color;
            button.addEventListener('click', () => setAccent(button.dataset.color));
        });
        setAccent(settings.accent_color);
        applyTheme(settings.theme, false);
        libraryItems = readPlaylistCache();
        if (libraryItems.length) renderLibrary();
        libraryLauncher.addEventListener('click', () => {
            setLibraryOpen(!libraryPanel.classList.contains('open'));
        });
        libraryClose.addEventListener('click', () => setLibraryOpen(false));
        libraryRefresh.addEventListener('click', loadLibrary);
        librarySearch.addEventListener('input', renderLibrary);
        document.addEventListener('pointerdown', event => {
            if (!libraryPanel.classList.contains('open')) return;
            if (!event.composedPath().includes(host)) setLibraryOpen(false);
        }, true);
        root.getElementById('close').addEventListener('click', () => setOpen(false));
        scrim.addEventListener('click', () => setOpen(false));
        adblock.addEventListener('change', () => {
            settings.ad_block_enabled = adblock.checked;
            post({ type: 'adblock', enabled: adblock.checked });
        });
        const getArtworkUrl = () => {
            const artwork = document.querySelector('.playbackSoundBadge__avatar .image__lightOutline span');
            const background = artwork && (
                artwork.style.backgroundImage || getComputedStyle(artwork).backgroundImage
            );
            if (background && background !== 'none') {
                const url = background
                    .replace(/^url\((['"]?)/, '')
                    .replace(/(['"]?)\)$/, '');
                if (url.startsWith('https://')) return url;
            }
            const image = document.querySelector('.playbackSoundBadge__avatar img');
            return image && (image.currentSrc || image.src).startsWith('https://')
                ? (image.currentSrc || image.src)
                : '';
        };
        const updateArtworkGlow = () => {
            if (!settings.cover_glow_enabled) {
                artworkGeneration++;
                artGlowLayers.forEach(layer => { layer.style.opacity = '0'; });
                return;
            }
            const artworkUrl = getArtworkUrl();
            if (!artworkUrl) {
                artworkGeneration++;
                lastArtworkUrl = '';
                artGlowLayers.forEach(layer => { layer.style.opacity = '0'; });
                return;
            }
            if (artworkUrl === lastArtworkUrl && Number(artGlowLayers[activeGlowIndex].style.opacity) > 0) return;

            lastArtworkUrl = artworkUrl;
            const generation = ++artworkGeneration;
            const previousGlow = artGlowLayers[activeGlowIndex];
            const nextGlowIndex = 1 - activeGlowIndex;
            const nextGlow = artGlowLayers[nextGlowIndex];
            const safeUrl = artworkUrl.replace(/["\\]/g, '\\$&');
            nextGlow.style.opacity = '0';
            nextGlow.style.backgroundImage =
                `radial-gradient(ellipse at center, transparent 22%, #101014a8 100%), url("${safeUrl}")`;

            const artworkImage = new Image();
            artworkImage.onload = () => {
                if (!settings.cover_glow_enabled || generation !== artworkGeneration) return;
                requestAnimationFrame(() => {
                    if (!settings.cover_glow_enabled || generation !== artworkGeneration) return;
                    nextGlow.style.opacity = '0.24';
                    previousGlow.style.opacity = '0';
                    activeGlowIndex = nextGlowIndex;
                });
            };
            artworkImage.src = artworkUrl;
        };
        const originalPlayingTitleStyles = new WeakMap();
        const isSoundCloudOrange = color => {
            const match = color.match(/^rgba?\((\d+),\s*(\d+),\s*(\d+)/i);
            return match &&
                Number(match[1]) >= 245 &&
                Number(match[2]) >= 55 && Number(match[2]) <= 115 &&
                Number(match[3]) <= 12;
        };
        const syncPlayingTrackTheme = () => {
            const previousTitles = [
                ...document.querySelectorAll('.soundTitle__title[data-rustcloud-playing="true"]')
            ];
            for (const title of previousTitles) {
                const originalStyle = originalPlayingTitleStyles.get(title);
                if (originalStyle) {
                    if (originalStyle.color) {
                        title.style.setProperty('color', originalStyle.color, originalStyle.colorPriority);
                    } else {
                        title.style.removeProperty('color');
                    }
                    if (originalStyle.textShadow) {
                        title.style.setProperty(
                            'text-shadow',
                            originalStyle.textShadow,
                            originalStyle.textShadowPriority
                        );
                    } else {
                        title.style.removeProperty('text-shadow');
                    }
                    originalPlayingTitleStyles.delete(title);
                }
                title.removeAttribute('data-rustcloud-playing');
            }
            document.querySelectorAll('.soundList__item[data-rustcloud-playing="true"]')
                .forEach(item => item.removeAttribute('data-rustcloud-playing'));
            if (settings.theme === 'default') return;

            const badgeTitle = document.querySelector('.playbackSoundBadge__title');
            const badgeLink = badgeTitle && (
                badgeTitle.matches('a[href]') ? badgeTitle : badgeTitle.closest('a[href]')
            );
            let currentUrl = '';
            try {
                if (badgeLink) {
                    currentUrl = new URL(badgeLink.href, window.location.origin).pathname
                        .replace(/\/+$/, '').toLocaleLowerCase();
                }
            } catch (_) {}
            const currentTitle = badgeTitle
                ? badgeTitle.textContent.trim().replace(/\s+/g, ' ').toLocaleLowerCase()
                : '';
            const titleElements = [...document.querySelectorAll('.soundTitle__title')];
            const currentUrlMatches = currentUrl
                ? titleElements.filter(element => {
                    const link = element.matches('a[href]')
                        ? element
                        : element.closest('a[href]') || element.querySelector('a[href]');
                    if (!link) return false;
                    try {
                        return new URL(link.href, window.location.origin).pathname
                            .replace(/\/+$/, '').toLocaleLowerCase() === currentUrl;
                    } catch (_) {
                        return false;
                    }
                })
                : [];
            let matchingTitles = currentUrlMatches.length
                ? currentUrlMatches
                : currentTitle
                    ? titleElements.filter(element =>
                        element.textContent.trim().replace(/\s+/g, ' ').toLocaleLowerCase() === currentTitle
                    )
                    : [];
            if (!matchingTitles.length) {
                const orangeTitles = titleElements.filter(element =>
                    isSoundCloudOrange(getComputedStyle(element).color)
                );
                const activeRows = orangeTitles.filter(element => {
                    const row = element.closest('.soundList__item');
                    return row && (
                        row.classList.contains('playing') ||
                        row.classList.contains('sc-pause') ||
                        row.classList.contains('soundList__item--playing')
                    );
                });
                matchingTitles = activeRows.length ? activeRows : orangeTitles.slice(0, 1);
            }

            for (const title of matchingTitles) {
                originalPlayingTitleStyles.set(title, {
                    color: title.style.getPropertyValue('color'),
                    colorPriority: title.style.getPropertyPriority('color'),
                    textShadow: title.style.getPropertyValue('text-shadow'),
                    textShadowPriority: title.style.getPropertyPriority('text-shadow')
                });
                title.setAttribute('data-rustcloud-playing', 'true');
                title.style.setProperty('color', 'var(--rustcloud-accent)', 'important');
                title.style.setProperty('text-shadow', '0 0 9px var(--rustcloud-glow-soft)');
                title.closest('.soundList__item')
                    ?.setAttribute('data-rustcloud-playing', 'true');
            }
        };
        let playingTrackUpdateScheduled = false;
        const schedulePlayingTrackThemeSync = () => {
            if (playingTrackUpdateScheduled) return;
            playingTrackUpdateScheduled = true;
            requestAnimationFrame(() => {
                playingTrackUpdateScheduled = false;
                syncPlayingTrackTheme();
            });
        };
        const observePlayingTrack = () => {
            const root = document.body || document.documentElement;
            if (!root) {
                document.addEventListener('DOMContentLoaded', observePlayingTrack, { once: true });
                return;
            }
            const observer = new MutationObserver(schedulePlayingTrackThemeSync);
            observer.observe(root, {
                attributes: true,
                attributeFilter: ['class', 'href'],
                childList: true,
                subtree: true
            });
            syncPlayingTrackTheme();
        };
        const scheduleArtworkGlowUpdate = () => {
            if (artworkUpdateScheduled) return;
            artworkUpdateScheduled = true;
            requestAnimationFrame(() => {
                artworkUpdateScheduled = false;
                updateArtworkGlow();
            });
        };
        const observeArtwork = () => {
            if (!settings.cover_glow_enabled) {
                if (artworkObserver) artworkObserver.disconnect();
                observedArtworkNode = null;
                return;
            }
            const artworkNode = document.querySelector('.playbackSoundBadge__avatar');
            if (artworkNode === observedArtworkNode) return;
            if (artworkObserver) artworkObserver.disconnect();
            observedArtworkNode = artworkNode;
            if (artworkNode) {
                artworkObserver = new MutationObserver(scheduleArtworkGlowUpdate);
                artworkObserver.observe(artworkNode, {
                    attributes: true,
                    attributeFilter: ['style', 'src'],
                    childList: true,
                    subtree: true
                });
            }
            scheduleArtworkGlowUpdate();
        };
        artworkObserver = new MutationObserver(scheduleArtworkGlowUpdate);
        artworkRootObserver = new MutationObserver(() => {
            observeArtwork();
        });
        const observeArtworkRoot = () => {
            if (!settings.cover_glow_enabled) {
                artworkRootObserver.disconnect();
                if (artworkObserver) artworkObserver.disconnect();
                observedArtworkNode = null;
                return;
            }
            const root = document.body || document.documentElement;
            if (root) {
                artworkRootObserver.disconnect();
                artworkRootObserver.observe(root, { childList: true, subtree: true });
                observeArtwork();
            } else {
                document.addEventListener('DOMContentLoaded', observeArtworkRoot, { once: true });
            }
        };
        coverGlow.addEventListener('change', () => {
            settings.cover_glow_enabled = coverGlow.checked;
            observeArtworkRoot();
            updateArtworkGlow();
            post({ type: 'coverGlow', enabled: coverGlow.checked });
        });
        fullPlaylistShuffle.addEventListener('change', () => {
            settings.full_playlist_shuffle_enabled = fullPlaylistShuffle.checked;
            post({ type: 'fullPlaylistShuffle', enabled: fullPlaylistShuffle.checked });
            if (fullPlaylistShuffle.checked) installShuffleEnhancement();
        });
        colorPicker.addEventListener('change', () => setAccent(colorPicker.value));
        window.addEventListener('keydown', event => {
            if (event.key === 'F1') {
                event.preventDefault();
                event.stopPropagation();
                setLibraryOpen(false);
                setOpen(!panel.classList.contains('open'));
            } else if (event.key === 'Escape' && (
                panel.classList.contains('open') || libraryPanel.classList.contains('open')
            )) {
                event.preventDefault();
                setOpen(false);
                setLibraryOpen(false);
            }
        }, true);
        observeArtworkRoot();
        updateArtworkGlow();
        observePlayingTrack();
        installShuffleEnhancement();
    };

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', mount, { once: true });
    } else {
        mount();
    }
})();
"##;

    const DEMO_HTML: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>RustCloud blocker check</title>
<style>body{font:16px sans-serif;max-width:42rem;margin:4rem auto;line-height:1.5}</style>
<h1>Request-blocking check</h1>
<p>This page requests an image from <code>ads.soundcloud.com</code> and an audio file
from the observed SoundCloud ad-delivery path.</p>
<p>RustCloud should intercept both and show a blocked-request count in the window title.
The requests are answered locally; they are not sent to those hosts.</p>
<img src="https://ads.soundcloud.com/rustcloud-block-test.gif" alt="Blocked test request">
<audio controls autoplay src="https://delivery-cdn-cf.adswizz.com/soundcloud_newEnco/ad_test.mp3"></audio>
"#;

    enum UserEvent {
        RequestBlocked(usize),
    }

    struct App {
        window: Option<Window>,
        webview: Option<WebView>,
        block_test: bool,
        event_proxy: EventLoopProxy<UserEvent>,
        filter_engine: Arc<RwLock<Engine>>,
        settings: Arc<Mutex<AppSettings>>,
        blocking_enabled: Arc<AtomicBool>,
    }

    fn app_icon() -> Icon {
        let image = image::load_from_memory_with_format(APP_ICON_PNG, image::ImageFormat::Png)
            .expect("The embedded RustCloud icon must be a valid PNG")
            .into_rgba8();
        let (width, height) = image.dimensions();
        Icon::from_rgba(image.into_raw(), width, height)
            .expect("The embedded RustCloud icon must have valid RGBA dimensions")
    }

    impl ApplicationHandler<UserEvent> for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }

            let window = match event_loop.create_window(
                Window::default_attributes()
                    .with_title("RustCloud - network ad blocking enabled")
                    .with_inner_size(LogicalSize::new(1200.0, 800.0))
                    .with_window_icon(Some(app_icon())),
            ) {
                Ok(window) => window,
                Err(error) => {
                    eprintln!("Could not create the application window: {error}");
                    event_loop.exit();
                    return;
                }
            };

            let settings = match self.settings.lock() {
                Ok(settings) => settings.clone(),
                Err(error) => {
                    eprintln!("Could not read RustCloud settings: {error}");
                    event_loop.exit();
                    return;
                }
            };
            let initialization_script = match settings_initialization_script(&settings) {
                Ok(script) => script,
                Err(error) => {
                    eprintln!("Could not prepare RustCloud settings UI: {error}");
                    event_loop.exit();
                    return;
                }
            };
            let shared_settings = Arc::clone(&self.settings);
            let blocking_enabled = Arc::clone(&self.blocking_enabled);
            let webview = match WebViewBuilder::new()
                .with_initialization_script(initialization_script)
                .with_ipc_handler(move |request| {
                    handle_settings_message(request.body(), &shared_settings, &blocking_enabled);
                })
                .with_url("about:blank")
                .build(&window)
            {
                Ok(webview) => webview,
                Err(error) => {
                    eprintln!("Could not start WebView2. Install or update the Microsoft Edge WebView2 Runtime: {error}");
                    event_loop.exit();
                    return;
                }
            };

            if let Err(error) = install_blocker(
                &webview,
                self.event_proxy.clone(),
                Arc::clone(&self.filter_engine),
                Arc::clone(&self.blocking_enabled),
            ) {
                eprintln!("Could not enable WebView2 request blocking: {error}");
                event_loop.exit();
                return;
            }

            let navigation = if self.block_test {
                webview.load_html(DEMO_HTML)
            } else {
                webview.load_url(SOUNDCLOUD_URL)
            };
            if let Err(error) = navigation {
                eprintln!("Could not load the requested page: {error}");
                event_loop.exit();
                return;
            }

            self.window = Some(window);
            self.webview = Some(webview);
        }

        fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
            if let (Some(window), UserEvent::RequestBlocked(count)) = (&self.window, event) {
                window.set_title(&format!(
                    "RustCloud - {count} ad request{} blocked",
                    if count == 1 { "" } else { "s" }
                ));
            }
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            _window_id: WindowId,
            event: WindowEvent,
        ) {
            if event == WindowEvent::CloseRequested {
                event_loop.exit();
            }
        }
    }

    fn install_blocker(
        webview: &WebView,
        event_proxy: EventLoopProxy<UserEvent>,
        filter_engine: Arc<RwLock<Engine>>,
        blocking_enabled: Arc<AtomicBool>,
    ) -> Result<(), Box<dyn Error>> {
        let core_webview: ICoreWebView2 = webview.webview();
        let filtering_webview: ICoreWebView2_22 = core_webview.cast()?;
        let environment = webview.environment();

        unsafe {
            filtering_webview.AddWebResourceRequestedFilterWithRequestSourceKinds(
                w!("*"),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
            )?;
        }

        let blocked_count = Arc::new(AtomicUsize::new(0));
        let handler = WebResourceRequestedEventHandler::create(Box::new(move |_sender, args| {
            let Some(args) = args else {
                return Ok(());
            };
            let request = unsafe { args.Request()? };
            let mut request_uri = PWSTR::null();
            unsafe {
                request.Uri(&mut request_uri)?;
            }
            let request_uri = CoTaskMemPWSTR::from(request_uri);
            let request_url = request_uri.to_string();
            let mut resource_context = COREWEBVIEW2_WEB_RESOURCE_CONTEXT(0);
            unsafe {
                args.ResourceContext(&mut resource_context)?;
            }
            let request_type = match resource_context {
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT => "document",
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_STYLESHEET => "stylesheet",
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE => "image",
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT => "font",
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT => "script",
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA => "media",
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST => "xmlhttprequest",
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH => "fetch",
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET => "websocket",
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_PING => "ping",
                _ => "other",
            };

            let should_block_request = if blocking_enabled.load(Ordering::Relaxed) {
                match filter_engine.read() {
                    Ok(engine) => should_block(&engine, &request_url, SOUNDCLOUD_URL, request_type),
                    Err(error) => {
                        eprintln!("Could not read the ad filter engine: {error}");
                        false
                    }
                }
            } else {
                false
            };

            if should_block_request {
                let response = unsafe {
                    environment.CreateWebResourceResponse(
                        None::<&IStream>,
                        204,
                        w!("No Content"),
                        w!(""),
                    )?
                };
                unsafe {
                    args.SetResponse(&response)?;
                }

                let count = blocked_count.fetch_add(1, Ordering::Relaxed) + 1;
                println!("BLOCKED ({request_type}, filter list): {request_url}");
                let _ = event_proxy.send_event(UserEvent::RequestBlocked(count));
            }

            Ok(())
        }));

        let mut token = 0i64;
        unsafe {
            core_webview.add_WebResourceRequested(&handler, &mut token)?;
        }
        println!("WebView2 network request blocking is active.");
        Ok(())
    }

    fn app_data_directory() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|path| path.join("RustCloud"))
    }

    fn app_settings_path() -> Option<PathBuf> {
        app_data_directory().map(|path| path.join("settings.json"))
    }

    fn load_settings() -> AppSettings {
        let Some(path) = app_settings_path() else {
            eprintln!("LOCALAPPDATA is unavailable; RustCloud settings will not persist.");
            return AppSettings::default();
        };
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return AppSettings::default();
            }
            Err(error) => {
                eprintln!("Could not read RustCloud settings: {error}");
                return AppSettings::default();
            }
        };

        match serde_json::from_str::<AppSettings>(&contents) {
            Ok(mut settings) => {
                if !is_valid_accent_color(&settings.accent_color) {
                    eprintln!("Ignoring invalid saved accent color.");
                    settings.accent_color = AppSettings::default().accent_color;
                }
                if !is_valid_theme(&settings.theme) {
                    eprintln!("Ignoring invalid saved theme.");
                    settings.theme = AppSettings::default().theme;
                }
                settings
            }
            Err(error) => {
                eprintln!("Could not parse RustCloud settings: {error}");
                AppSettings::default()
            }
        }
    }

    fn save_settings(settings: &AppSettings) {
        let Some(path) = app_settings_path() else {
            eprintln!("LOCALAPPDATA is unavailable; RustCloud settings could not be saved.");
            return;
        };
        let Some(parent) = path.parent() else {
            eprintln!("Could not determine the RustCloud settings directory.");
            return;
        };
        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!("Could not create RustCloud settings directory: {error}");
            return;
        }
        let contents = match serde_json::to_string_pretty(settings) {
            Ok(contents) => contents,
            Err(error) => {
                eprintln!("Could not serialize RustCloud settings: {error}");
                return;
            }
        };
        if let Err(error) = fs::write(path, contents) {
            eprintln!("Could not save RustCloud settings: {error}");
        }
    }

    fn is_valid_accent_color(color: &str) -> bool {
        let bytes = color.as_bytes();
        bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(u8::is_ascii_hexdigit)
    }

    fn is_valid_theme(theme: &str) -> bool {
        matches!(
            theme,
            "default"
                | "midnight-glass"
                | "neon-pulse"
                | "warm-vinyl"
                | "forest-glow"
                | "rose-quartz"
                | "ocean-breeze"
        )
    }

    fn settings_initialization_script(settings: &AppSettings) -> Result<String, serde_json::Error> {
        Ok(format!(
            "window.__RUSTCLOUD_SETTINGS__={};\n{}",
            serde_json::to_string(settings)?,
            SETTINGS_UI_SCRIPT
        ))
    }

    fn handle_settings_message(
        raw_message: &str,
        settings: &Mutex<AppSettings>,
        blocking_enabled: &AtomicBool,
    ) {
        let message = match serde_json::from_str::<SettingsMessage>(raw_message) {
            Ok(message) => message,
            Err(error) => {
                eprintln!("Ignored invalid RustCloud settings message: {error}");
                return;
            }
        };

        let mut settings = match settings.lock() {
            Ok(settings) => settings,
            Err(error) => {
                eprintln!("Could not update RustCloud settings: {error}");
                return;
            }
        };
        match message {
            SettingsMessage::AdBlock { enabled } => {
                settings.ad_block_enabled = enabled;
                blocking_enabled.store(enabled, Ordering::Relaxed);
                println!(
                    "Ad blocking {}",
                    if enabled { "enabled" } else { "disabled" }
                );
            }
            SettingsMessage::Accent { color } => {
                if !is_valid_accent_color(&color) {
                    eprintln!("Ignored invalid RustCloud accent color.");
                    return;
                }
                settings.accent_color = color;
            }
            SettingsMessage::Theme { name } => {
                if !is_valid_theme(&name) {
                    eprintln!("Ignored invalid RustCloud theme.");
                    return;
                }
                settings.theme = name;
            }
            SettingsMessage::CoverGlow { enabled } => {
                settings.cover_glow_enabled = enabled;
            }
            SettingsMessage::FullPlaylistShuffle { enabled } => {
                settings.full_playlist_shuffle_enabled = enabled;
            }
        }
        save_settings(&settings);
    }

    fn filter_list_cache_path() -> Option<PathBuf> {
        app_data_directory().map(|path| path.join("filter-lists.txt"))
    }

    fn read_filter_cache(path: &PathBuf) -> Option<String> {
        match fs::read_to_string(path) {
            Ok(contents) if !contents.trim().is_empty() => Some(contents),
            Ok(_) => {
                eprintln!("The cached ad filter lists are empty: {}", path.display());
                None
            }
            Err(error) => {
                eprintln!("Could not read cached ad filter lists: {error}");
                None
            }
        }
    }

    fn download_filter_lists() -> Result<Vec<String>, String> {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(25)))
            .build();
        let agent = ureq::Agent::new_with_config(config);

        std::thread::scope(|scope| {
            let downloads = FILTER_LIST_URLS
                .iter()
                .map(|url| {
                    let url = *url;
                    let agent = agent.clone();
                    scope.spawn(move || {
                        let response = agent
                            .get(url)
                            .call()
                            .map_err(|error| format!("{url}: {error}"))?;
                        response
                            .into_body()
                            .read_to_string()
                            .map_err(|error| format!("{url}: {error}"))
                    })
                })
                .collect::<Vec<_>>();

            downloads
                .into_iter()
                .map(|download| {
                    download
                        .join()
                        .map_err(|_| String::from("An ad filter list download worker panicked"))?
                })
                .collect()
        })
    }

    fn cached_filter_lists() -> (Option<String>, bool) {
        let cache_path = filter_list_cache_path();
        if let Some(path) = &cache_path {
            match fs::metadata(path) {
                Ok(metadata) => {
                    let cache_is_fresh = metadata
                        .modified()
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age < FILTER_LIST_CACHE_TTL);
                    return (read_filter_cache(path), !cache_is_fresh);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    eprintln!("Could not inspect cached ad filter lists: {error}");
                }
            }
        }
        (None, true)
    }

    fn cache_filter_lists(path: &PathBuf, lists: &[String]) {
        if let Some(parent) = path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                eprintln!("Could not create ad filter cache directory: {error}");
                return;
            }
        }
        if let Err(error) = fs::write(path, lists.join("\n")) {
            eprintln!("Could not cache the downloaded ad filter lists: {error}");
        }
    }

    fn refresh_filter_engine(filter_engine: Arc<RwLock<Engine>>, cache_path: Option<PathBuf>) {
        println!("Refreshing ad filter lists from Ghostery in the background...");
        match download_filter_lists() {
            Ok(lists) => {
                println!("Loaded {} ad filter lists.", lists.len());
                if let Some(path) = cache_path {
                    cache_filter_lists(&path, &lists);
                }
                let updated_engine = build_filter_engine(lists);
                match filter_engine.write() {
                    Ok(mut engine) => {
                        *engine = updated_engine;
                        println!("Updated the active ad filter engine.");
                    }
                    Err(error) => {
                        eprintln!("Could not update the active ad filter engine: {error}");
                    }
                }
            }
            Err(error) => eprintln!("Could not refresh Ghostery ad filter lists: {error}"),
        }
    }

    pub fn run() -> Result<(), Box<dyn Error>> {
        let settings = Arc::new(Mutex::new(load_settings()));
        let saved_ad_block_enabled = settings
            .lock()
            .map_err(|error| format!("Could not read RustCloud settings: {error}"))?
            .ad_block_enabled;
        let blocking_enabled = Arc::new(AtomicBool::new(saved_ad_block_enabled));
        let (cached_lists, cache_is_stale) = cached_filter_lists();
        let filter_engine = Arc::new(RwLock::new(match cached_lists {
            Some(contents) => {
                println!("Using cached Ghostery filter lists at startup.");
                build_filter_engine([contents])
            }
            None => {
                println!("Using built-in ad rules while filter lists load.");
                build_filter_engine([])
            }
        }));
        if cache_is_stale {
            let refresh_engine = Arc::clone(&filter_engine);
            let cache_path = filter_list_cache_path();
            if let Err(error) = std::thread::Builder::new()
                .name(String::from("rustcloud-filter-refresh"))
                .spawn(move || refresh_filter_engine(refresh_engine, cache_path))
            {
                eprintln!("Could not start background ad filter refresh: {error}");
            }
        }
        let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
        event_loop.set_control_flow(ControlFlow::Wait);
        let event_proxy = event_loop.create_proxy();
        let mut app = App {
            window: None,
            webview: None,
            block_test: std::env::args().any(|arg| arg == "--block-test"),
            event_proxy,
            filter_engine,
            settings,
            blocking_enabled,
        };
        event_loop.run_app(&mut app)?;
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    desktop::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("RustCloud currently supports Windows only.");
}

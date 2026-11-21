#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod desktop {
    use std::{
        error::Error,
        fs,
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc::{self, Receiver, Sender},
            Arc, Mutex, RwLock,
        },
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use adblock::Engine;
    use discord_rich_presence::{
        activity::{self, ActivityType, Assets, Timestamps},
        DiscordIpc, DiscordIpcClient,
    };
    use rustcloud::{build_filter_engine, should_block};
    use webview2_com::{
        AcceleratorKeyPressedEventHandler, CoTaskMemPWSTR,
        Microsoft::Web::WebView2::Win32::{
            ICoreWebView2, ICoreWebView2_22, COREWEBVIEW2_KEY_EVENT_KIND,
            COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN, COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN,
            COREWEBVIEW2_PHYSICAL_KEY_STATUS, COREWEBVIEW2_WEB_RESOURCE_CONTEXT,
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
        dpi::{LogicalPosition, LogicalSize},
        event::WindowEvent,
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
        window::{Icon, ResizeDirection, Window, WindowId},
    };
    use wry::{Rect, WebView, WebViewBuilder, WebViewExtWindows};

    const SOUNDCLOUD_URL: &str = "https://soundcloud.com/";
    const DISCORD_APPLICATION_ID: &str = "1090770350251458592";
    const TITLEBAR_HEIGHT: u32 = 34;
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
        discord_rich_presence_enabled: bool,
    }

    impl Default for AppSettings {
        fn default() -> Self {
            Self {
                ad_block_enabled: true,
                accent_color: String::from("#ff5500"),
                theme: String::from("midnight-glass"),
                cover_glow_enabled: false,
                full_playlist_shuffle_enabled: false,
                discord_rich_presence_enabled: true,
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
        #[serde(rename = "discordRichPresence")]
        DiscordRichPresence { enabled: bool },
        #[serde(rename = "trackUpdate")]
        TrackUpdate { track: PlaybackTrack },
        #[serde(rename = "windowControl")]
        WindowControl { action: WindowAction },
    }

    #[derive(Clone, Copy, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    enum WindowAction {
        Minimize,
        ToggleMaximize,
        Close,
        Drag,
        ResizeNorth,
        ResizeNorthEast,
        ResizeEast,
        ResizeSouthEast,
        ResizeSouth,
        ResizeSouthWest,
        ResizeWest,
        ResizeNorthWest,
    }

    #[derive(Clone, Debug, PartialEq, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PlaybackTrack {
        title: String,
        author: String,
        artwork: String,
        elapsed: String,
        duration: String,
        is_playing: bool,
    }

    enum PresenceCommand {
        SetEnabled(bool),
        Track(PlaybackTrack),
    }

    impl PlaybackTrack {
        fn has_same_presence_as(&self, other: &Self) -> bool {
            self.title == other.title
                && self.author == other.author
                && self.artwork == other.artwork
                && self.duration == other.duration
                && self.is_playing == other.is_playing
        }
    }

    fn spawn_presence_worker(enabled: bool) -> std::io::Result<Sender<PresenceCommand>> {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name(String::from("rustcloud-discord-presence"))
            .spawn(move || run_presence_worker(receiver, enabled))?;
        Ok(sender)
    }

    fn run_presence_worker(receiver: Receiver<PresenceCommand>, mut enabled: bool) {
        const RETRY_INTERVAL: Duration = Duration::from_secs(30);
        const ACTIVITY_REFRESH_INTERVAL: Duration = Duration::from_secs(15);

        let mut client: Option<DiscordIpcClient> = None;
        let mut latest_track: Option<PlaybackTrack> = None;
        let mut activity_set = false;
        let mut activity_dirty = true;
        let mut next_connect_attempt = Instant::now();
        let mut next_activity_refresh = Instant::now();

        loop {
            match receiver.recv_timeout(Duration::from_secs(1)) {
                Ok(PresenceCommand::SetEnabled(next_enabled)) => {
                    if enabled != next_enabled {
                        enabled = next_enabled;
                        if enabled {
                            activity_dirty = true;
                            next_connect_attempt = Instant::now();
                            next_activity_refresh = Instant::now();
                        } else {
                            clear_discord_activity(&mut client, &mut activity_set);
                            close_discord_client(&mut client);
                        }
                    }
                }
                Ok(PresenceCommand::Track(track)) => {
                    let metadata_changed = latest_track
                        .as_ref()
                        .is_none_or(|current| !current.has_same_presence_as(&track));
                    latest_track = Some(track);
                    if metadata_changed {
                        activity_dirty = true;
                        next_activity_refresh = Instant::now();
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }

            if !enabled {
                continue;
            }
            let Some(track) = latest_track.as_ref() else {
                continue;
            };
            if !track.is_playing || track.title.trim().is_empty() || track.author.trim().is_empty()
            {
                clear_discord_activity(&mut client, &mut activity_set);
                activity_dirty = false;
                continue;
            }

            let now = Instant::now();
            if client.is_none() && now >= next_connect_attempt {
                next_connect_attempt = now + RETRY_INTERVAL;
                let mut new_client = DiscordIpcClient::new(DISCORD_APPLICATION_ID);
                match new_client.connect() {
                    Ok(()) => {
                        client = Some(new_client);
                        activity_dirty = true;
                        next_activity_refresh = now;
                        println!("Connected to Discord Rich Presence.");
                    }
                    Err(error) => {
                        eprintln!("Could not initialize Discord Rich Presence: {error}");
                    }
                }
            }
            if !activity_dirty && now < next_activity_refresh {
                continue;
            }

            let result = client
                .as_mut()
                .map(|client| client.set_activity(discord_activity(track)));
            match result {
                Some(Ok(())) => {
                    activity_set = true;
                    activity_dirty = false;
                    next_activity_refresh = now + ACTIVITY_REFRESH_INTERVAL;
                }
                Some(Err(error)) => {
                    eprintln!("Could not update Discord Rich Presence: {error}");
                    client.take();
                    activity_set = false;
                    activity_dirty = true;
                    next_connect_attempt = now + RETRY_INTERVAL;
                }
                None => {}
            }
        }

        clear_discord_activity(&mut client, &mut activity_set);
        close_discord_client(&mut client);
    }

    fn clear_discord_activity(client: &mut Option<DiscordIpcClient>, activity_set: &mut bool) {
        if !*activity_set {
            return;
        }
        if let Some(Err(error)) = client.as_mut().map(DiscordIpc::clear_activity) {
            eprintln!("Could not clear Discord Rich Presence: {error}");
            client.take();
        }
        *activity_set = false;
    }

    fn close_discord_client(client: &mut Option<DiscordIpcClient>) {
        if let Some(mut client) = client.take() {
            if let Err(error) = client.close() {
                eprintln!("Could not close the Discord Rich Presence connection: {error}");
            }
        }
    }

    fn discord_activity(track: &PlaybackTrack) -> activity::Activity<'static> {
        let title = limit_discord_text(&track.title);
        let author = limit_discord_text(&track.author);
        let artwork = if is_https_url(&track.artwork) {
            track.artwork.replace("50x50.", "500x500.")
        } else {
            String::from("soundcloud-logo")
        };
        let mut activity = activity::Activity::new()
            .details(title)
            .state(author)
            .activity_type(ActivityType::Listening)
            .assets(
                Assets::new()
                    .large_image(artwork)
                    .large_text("SoundCloud")
                    .small_image("soundcloud-logo")
                    .small_text("SoundCloud"),
            );
        if let Some((start, end)) = presence_timestamps(track) {
            activity = activity.timestamps(Timestamps::new().start(start).end(end));
        }
        activity
    }

    fn limit_discord_text(value: &str) -> String {
        let value = value.trim();
        if value.chars().count() > 128 {
            format!("{}...", value.chars().take(125).collect::<String>())
        } else {
            value.to_string()
        }
    }

    fn is_https_url(value: &str) -> bool {
        value.starts_with("https://")
            && value.len() <= 2048
            && !value.chars().any(char::is_whitespace)
    }

    fn parse_time_millis(value: &str) -> Option<i64> {
        let value = value.trim();
        let (sign, value) = match value.strip_prefix('-') {
            Some(value) => (-1_i64, value),
            None => (1_i64, value),
        };
        if value.is_empty() {
            return None;
        }
        let seconds = value.split(':').try_fold(0_i64, |total, part| {
            let part = part.parse::<i64>().ok()?;
            if part < 0 {
                return None;
            }
            total.checked_mul(60)?.checked_add(part)
        })?;
        seconds.checked_mul(1000)?.checked_mul(sign)
    }

    fn presence_timestamps(track: &PlaybackTrack) -> Option<(i64, i64)> {
        presence_timestamps_at(track, unix_timestamp_millis())
    }

    fn presence_timestamps_at(track: &PlaybackTrack, now: i64) -> Option<(i64, i64)> {
        let elapsed = parse_time_millis(&track.elapsed)?.max(0);
        let parsed_duration = parse_time_millis(&track.duration)?;
        let duration = if parsed_duration < 0 {
            elapsed.saturating_add(parsed_duration.saturating_abs())
        } else {
            parsed_duration
        };
        if duration <= 0 {
            return None;
        }
        let start = now.saturating_sub(elapsed);
        Some((start, start.saturating_add(duration)))
    }

    fn unix_timestamp_millis() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64
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
        discord_rich_presence_enabled: true,
        ...(window.__RUSTCLOUD_SETTINGS__ || {})
    };
    const uiPreferencesKey = 'rustcloud.ui-preferences.v1';
    const customFonts = [
        'Verdana', 'Bahnschrift', 'Trebuchet MS', 'Consolas', 'Segoe UI',
        'Calibri', 'Arial', 'Georgia', 'Cambria', 'Courier New'
    ];
    const supportedThemes = [
        'default', 'midnight-glass', 'neon-pulse', 'warm-vinyl',
        'forest-glow', 'rose-quartz', 'ocean-breeze', 'custom-glass'
    ];
    settings.custom_font = 'Verdana';
    try {
        const savedPreferences = JSON.parse(localStorage.getItem(uiPreferencesKey) || 'null');
        if (savedPreferences && typeof savedPreferences === 'object') {
            if (supportedThemes.includes(savedPreferences.theme)) settings.theme = savedPreferences.theme;
            if (typeof savedPreferences.accent_color === 'string' &&
                /^#[0-9a-f]{6}$/i.test(savedPreferences.accent_color)) {
                settings.accent_color = savedPreferences.accent_color;
            }
            if (customFonts.includes(savedPreferences.custom_font)) {
                settings.custom_font = savedPreferences.custom_font;
            }
        }
    } catch (error) {
        console.warn('[RustCloud] Could not restore theme preferences:', error);
    }
    const saveUiPreferences = () => {
        try {
            localStorage.setItem(uiPreferencesKey, JSON.stringify({
                theme: settings.theme,
                accent_color: settings.accent_color,
                custom_font: settings.custom_font
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
                :host([data-theme="custom-glass"]) {
                    --theme-surface:#152131;
                    --theme-card:color-mix(in srgb,var(--accent) 5%,transparent);
                    --theme-border:color-mix(in srgb,var(--accent) 15%,transparent);
                }
                button, input { font: inherit; }
                *::-webkit-scrollbar { width:10px; height:10px; }
                *::-webkit-scrollbar-track { background:var(--theme-surface); }
                *::-webkit-scrollbar-thumb {
                    border:2px solid var(--theme-surface); border-radius:999px;
                    background:var(--theme-border);
                }
                *::-webkit-scrollbar-thumb:hover { background:var(--accent); }
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
                #window-titlebar {
                    position:fixed; inset:0 0 auto; z-index:5; height:34px;
                    display:flex; align-items:stretch; color:#f5f5f7;
                    background:#17171bf5; border-bottom:1px solid #ffffff18;
                    user-select:none; pointer-events:auto;
                }
                #window-titlebar,
                #resize-north, #resize-north-east, #resize-north-west { display:none; }
                #window-titlebar-drag {
                    display:flex; flex:1; align-items:center; gap:9px;
                    min-width:0; cursor:default;
                }
                #window-titlebar-controls { position:relative; z-index:7; display:flex; }
                .window-control {
                    display:grid; place-items:center; width:46px; height:33px;
                    padding:0; border:0; color:inherit; background:transparent; cursor:pointer;
                }
                .window-control:hover { background:#ffffff1a; }
                .window-control:focus-visible { outline:1px solid var(--accent); outline-offset:-2px; }
                .window-control svg { width:10px; height:10px; fill:none; stroke:currentColor; stroke-width:1.2; }
                #window-close:hover { background:#e81123; }
                .resize-edge {
                    position:fixed; z-index:6; pointer-events:auto; touch-action:none;
                }
                #resize-north, #resize-south { left:10px; right:10px; height:7px; }
                #resize-north { top:0; cursor:n-resize; }
                #resize-south { bottom:0; cursor:s-resize; }
                #resize-east, #resize-west { top:0; bottom:10px; width:7px; }
                #resize-east { right:0; cursor:e-resize; }
                #resize-west { left:0; cursor:w-resize; }
                #resize-north-east, #resize-south-west {
                    width:12px; height:12px; cursor:nesw-resize;
                }
                #resize-north-west, #resize-south-east {
                    width:12px; height:12px; cursor:nwse-resize;
                }
                #resize-north-east { top:0; right:0; }
                #resize-north-west { top:0; left:0; }
                #resize-south-east { right:0; bottom:0; }
                #resize-south-west { left:0; bottom:0; }
                :host([data-maximized="true"]) .resize-edge { display:none; }
                :host([data-theme="midnight-glass"]) #window-titlebar { background:#111827f5; border-color:#2b3d51; }
                :host([data-theme="neon-pulse"]) #window-titlebar { background:#170f24f5; border-color:#483054; }
                :host([data-theme="warm-vinyl"]) #window-titlebar { background:#211710f5; border-color:#4d3728; }
                :host([data-theme="forest-glow"]) #window-titlebar { background:#111e1af5; border-color:#2b4638; }
                :host([data-theme="rose-quartz"]) #window-titlebar { background:#1d111bf5; border-color:#493044; }
                :host([data-theme="ocean-breeze"]) #window-titlebar { background:#0e1a24f5; border-color:#294858; }
                :host([data-theme="custom-glass"]) #window-titlebar {
                    background:color-mix(in srgb,var(--accent) 8%,#111827f5);
                    border-color:color-mix(in srgb,var(--accent) 24%,transparent);
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
                .theme-choice[data-theme="custom-glass"] .theme-preview { background:linear-gradient(135deg,#101827,#23314a); }
                .theme-choice[data-theme="custom-glass"] .theme-preview span { background:#78caff; box-shadow:0 0 8px #78caff; }
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
                #custom-theme-controls { margin-top:14px; }
                #custom-font {
                    width:100%; margin-top:8px; padding:9px 10px;
                    border:1px solid var(--theme-border); border-radius:9px;
                    color:#f5f5f7; background:#17171b; font-size:12px;
                }
                #custom-font:focus-visible { outline:2px solid var(--accent); outline-offset:2px; }
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
                    color:#f5f5f7; background:var(--theme-surface);
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
                .library-item-row {
                    display:flex; align-items:center; gap:5px; border-radius:9px;
                }
                .library-item-row:hover { background:#ffffff08; }
                .library-item-row .library-item { flex:1; min-width:0; }
                .library-pin {
                    width:28px; height:28px; flex:none; padding:0;
                    border:0; border-radius:50%; color:#85858e;
                    background:transparent; cursor:pointer; font-size:14px;
                }
                .library-pin:hover { color:#fff; background:#ffffff10; }
                .library-pin.pinned { color:var(--accent); }
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
            <div id="window-titlebar">
                <div id="window-titlebar-drag" aria-label="Drag window"></div>
                <div id="window-titlebar-controls">
                    <button class="window-control" id="window-minimize" aria-label="Minimize">
                        <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M1 5h8"/></svg>
                    </button>
                    <button class="window-control" id="window-maximize" aria-label="Maximize">
                        <svg viewBox="0 0 10 10" aria-hidden="true"><rect x="1.5" y="1.5" width="7" height="7"/></svg>
                    </button>
                    <button class="window-control" id="window-close" aria-label="Close">
                        <svg viewBox="0 0 10 10" aria-hidden="true"><path d="m1.5 1.5 7 7m0-7-7 7"/></svg>
                    </button>
                </div>
            </div>
            <div class="resize-edge" id="resize-north" data-action="resizeNorth" aria-hidden="true"></div>
            <div class="resize-edge" id="resize-north-east" data-action="resizeNorthEast" aria-hidden="true"></div>
            <div class="resize-edge" id="resize-east" data-action="resizeEast" aria-hidden="true"></div>
            <div class="resize-edge" id="resize-south-east" data-action="resizeSouthEast" aria-hidden="true"></div>
            <div class="resize-edge" id="resize-south" data-action="resizeSouth" aria-hidden="true"></div>
            <div class="resize-edge" id="resize-south-west" data-action="resizeSouthWest" aria-hidden="true"></div>
            <div class="resize-edge" id="resize-west" data-action="resizeWest" aria-hidden="true"></div>
            <div class="resize-edge" id="resize-north-west" data-action="resizeNorthWest" aria-hidden="true"></div>
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
                        <div id="blocked-count" class="description" role="status" aria-live="polite">
                            0 ad requests blocked this session
                        </div>
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
                        <button class="theme-choice" data-theme="custom-glass" aria-pressed="false">
                            <span class="theme-preview"><span></span><i></i></span>
                            <span class="theme-choice-name">Custom Glass</span>
                            <span class="theme-choice-note">Your color &amp; font</span>
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
                    <div id="custom-theme-controls" hidden>
                        <label class="title" for="custom-font">Custom Glass font</label>
                        <select id="custom-font" aria-label="Choose a font for Custom Glass">
                            <option>Verdana</option>
                            <option>Bahnschrift</option>
                            <option>Trebuchet MS</option>
                            <option>Consolas</option>
                            <option>Segoe UI</option>
                            <option>Calibri</option>
                            <option>Arial</option>
                            <option>Georgia</option>
                            <option>Cambria</option>
                            <option>Courier New</option>
                        </select>
                        <div class="description">Choose the accent color above, then select a font for your custom Midnight Glass-style theme.</div>
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
                <p class="section-label">Discord</p>
                <div class="card row">
                    <div>
                        <div class="title">Rich Presence</div>
                        <div class="description">Show the current track, artist, cover art, and playback progress in Discord. Discord must be running.</div>
                    </div>
                    <label class="switch" aria-label="Enable Discord Rich Presence">
                        <input id="discord-rich-presence" type="checkbox">
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
        window.__RUSTCLOUD_RENDER_BLOCKED_COUNT__ = () => {
            const display = root.getElementById('blocked-count');
            const count = Number(window.__RUSTCLOUD_BLOCKED_COUNT__) || 0;
            display.textContent = `${count} ad request${count === 1 ? '' : 's'} blocked this session`;
        };
        window.__RUSTCLOUD_RENDER_BLOCKED_COUNT__();
        const libraryLauncher = root.getElementById('library-launcher');
        const libraryPanel = root.getElementById('library-panel');
        const libraryClose = root.getElementById('library-close');
        const libraryRefresh = root.getElementById('library-refresh');
        const librarySearch = root.getElementById('library-search');
        const libraryStatus = root.getElementById('library-status');
        const playlistItemsElement = root.getElementById('playlist-items');
        const panel = root.getElementById('panel');
        const scrim = root.getElementById('scrim');
        const titlebarDrag = root.getElementById('window-titlebar-drag');
        const minimizeButton = root.getElementById('window-minimize');
        const maximizeButton = root.getElementById('window-maximize');
        const closeButton = root.getElementById('window-close');
        const resizeEdges = [...root.querySelectorAll('.resize-edge')];
        const adblock = root.getElementById('adblock');
        const coverGlow = root.getElementById('cover-glow');
        const fullPlaylistShuffle = root.getElementById('full-playlist-shuffle');
        const discordRichPresence = root.getElementById('discord-rich-presence');
        const shuffleStatus = root.getElementById('shuffle-status');
        const artGlowLayers = [
            root.getElementById('cover-art-glow-a'),
            root.getElementById('cover-art-glow-b')
        ];
        const colorPicker = root.getElementById('custom-color');
        const customThemeControls = root.getElementById('custom-theme-controls');
        const customFontPicker = root.getElementById('custom-font');
        const swatches = [...root.querySelectorAll('.swatch')];
        const themeButtons = [...root.querySelectorAll('.theme-choice')];
        const customThemeButton = root.querySelector('.theme-choice[data-theme="custom-glass"]');
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
                --rustcloud-font:"Verdana","Segoe UI",sans-serif;
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
                --rustcloud-font:"Georgia","Times New Roman",serif;
            }
            :root[data-rustcloud-theme="forest-glow"] {
                --rustcloud-accent:#69d6a8; --rustcloud-accent-deep:#28785f;
                --rustcloud-glow:#69d6a866; --rustcloud-glow-soft:#69d6a835;
                --rustcloud-glow-faint:#45c9aa18;
                --rustcloud-player:#111e1aeb; --rustcloud-page:#0c1511;
                --rustcloud-surface:#14231b; --rustcloud-text:#e4f3e9;
                --rustcloud-muted:#a0b9a9; --rustcloud-edge:#2d4939;
                --rustcloud-font:"Calibri","Segoe UI",sans-serif;
            }
            :root[data-rustcloud-theme="rose-quartz"] {
                --rustcloud-accent:#ff82b7; --rustcloud-accent-deep:#a84b7b;
                --rustcloud-glow:#ff82b777; --rustcloud-glow-soft:#ff82b735;
                --rustcloud-glow-faint:#d46bff18;
                --rustcloud-player:#1d111bed; --rustcloud-page:#150d15;
                --rustcloud-surface:#241625; --rustcloud-text:#f8eaf3;
                --rustcloud-muted:#c6a9bc; --rustcloud-edge:#50334a;
                --rustcloud-font:"Trebuchet MS","Segoe UI",sans-serif;
            }
            :root[data-rustcloud-theme="ocean-breeze"] {
                --rustcloud-accent:#67dce5; --rustcloud-accent-deep:#347eb3;
                --rustcloud-glow:#67dce577; --rustcloud-glow-soft:#67dce535;
                --rustcloud-glow-faint:#4c9dff18;
                --rustcloud-player:#0e1a24ed; --rustcloud-page:#0a121a;
                --rustcloud-surface:#122332; --rustcloud-text:#e5f3f8;
                --rustcloud-muted:#9eb8c5; --rustcloud-edge:#2b4659;
                --rustcloud-font:"Cambria","Georgia",serif;
            }
            :root[data-rustcloud-theme="custom-glass"] {
                --rustcloud-accent:var(--rustcloud-custom-accent,#78caff);
                --rustcloud-accent-deep:color-mix(in srgb,var(--rustcloud-custom-accent,#78caff) 60%,#152131);
                --rustcloud-glow:color-mix(in srgb,var(--rustcloud-custom-accent,#78caff) 40%,transparent);
                --rustcloud-glow-soft:color-mix(in srgb,var(--rustcloud-custom-accent,#78caff) 22%,transparent);
                --rustcloud-glow-faint:color-mix(in srgb,var(--rustcloud-custom-accent,#78caff) 11%,transparent);
                --rustcloud-player:#111827e8; --rustcloud-page:#0c121d;
                --rustcloud-surface:color-mix(in srgb,var(--rustcloud-custom-accent,#78caff) 9%,#152131);
                --rustcloud-text:#e5eef8; --rustcloud-muted:#9aafc4;
                --rustcloud-edge:color-mix(in srgb,var(--rustcloud-custom-accent,#78caff) 23%,#2b3d51);
                --rustcloud-font:var(--rustcloud-custom-font,"Verdana"),"Segoe UI",sans-serif;
            }
            html[data-rustcloud-theme], body {
                background-color:var(--rustcloud-page)!important;
                color:var(--rustcloud-text)!important;
            }
            html[data-rustcloud-theme] {
                scrollbar-color:var(--rustcloud-edge) var(--rustcloud-page);
            }
            html[data-rustcloud-theme]::-webkit-scrollbar,
            html[data-rustcloud-theme] *::-webkit-scrollbar {
                width:12px; height:12px;
            }
            html[data-rustcloud-theme]::-webkit-scrollbar-track,
            html[data-rustcloud-theme] *::-webkit-scrollbar-track {
                background:var(--rustcloud-page);
            }
            html[data-rustcloud-theme]::-webkit-scrollbar-thumb,
            html[data-rustcloud-theme] *::-webkit-scrollbar-thumb {
                border:3px solid var(--rustcloud-page); border-radius:999px;
                background:var(--rustcloud-edge);
            }
            html[data-rustcloud-theme]::-webkit-scrollbar-thumb:hover,
            html[data-rustcloud-theme] *::-webkit-scrollbar-thumb:hover {
                background:var(--rustcloud-accent);
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
            :root[data-rustcloud-theme] body
            :not(svg):not(svg *):not([class*="icon"]):not([class*="Icon"])
            :not([class*="symbol"]):not([class*="Symbol"]) {
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
        const playlistPinsKey = 'rustcloud.playlist-pins.v1';
        let pinnedPlaylistUrls = new Set();

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
        const maximizeIcon = '<svg viewBox="0 0 10 10" aria-hidden="true"><rect x="1.5" y="1.5" width="7" height="7"/></svg>';
        const restoreIcon = '<svg viewBox="0 0 10 10" aria-hidden="true"><path d="M3 1.5h5.5V7M7 3H1.5v5.5H7V3"/></svg>';
        window.__RUSTCLOUD_SET_MAXIMIZED__ = maximized => {
            host.dataset.maximized = String(maximized);
            maximizeButton.innerHTML = maximized ? restoreIcon : maximizeIcon;
            maximizeButton.setAttribute('aria-label', maximized ? 'Restore' : 'Maximize');
        };
        const toggleSettings = () => {
            setLibraryOpen(false);
            setOpen(!panel.classList.contains('open'));
        };
        window.__RUSTCLOUD_TOGGLE_SETTINGS__ = toggleSettings;
        resizeEdges.forEach(edge => {
            edge.addEventListener('pointerdown', event => {
                if (event.button === 0) {
                    event.preventDefault();
                    post({ type: 'windowControl', action: edge.dataset.action });
                }
            });
        });
        titlebarDrag.addEventListener('pointerdown', event => {
            if (event.button === 0) post({ type: 'windowControl', action: 'drag' });
        });
        titlebarDrag.addEventListener('dblclick', () => {
            post({ type: 'windowControl', action: 'toggleMaximize' });
        });
        minimizeButton.addEventListener('click', () => {
            post({ type: 'windowControl', action: 'minimize' });
        });
        maximizeButton.addEventListener('click', () => {
            post({ type: 'windowControl', action: 'toggleMaximize' });
        });
        closeButton.addEventListener('click', () => {
            post({ type: 'windowControl', action: 'close' });
        });
        let lastPresenceIdentity = '';
        let lastPresenceUpdateAt = 0;
        let presenceUpdateTimer = 0;
        let presenceInterval = null;
        let presenceObservedRoots = [];
        const presenceObserver = new MutationObserver(() => schedulePresenceUpdate());
        const presenceRootObserver = new MutationObserver(observePresenceRoots);
        const readPlaybackTrack = () => {
            const artworkNode = document.querySelector(
                '.playbackSoundBadge__avatar .image__lightOutline span'
            );
            const artworkImage = document.querySelector('.playbackSoundBadge__avatar img');
            const artworkStyle = artworkNode
                ? (artworkNode.style.backgroundImage || getComputedStyle(artworkNode).backgroundImage)
                : '';
            const artworkMatch = artworkStyle.match(/url\((["']?)(.*?)\1\)/);
            const artwork = artworkMatch ? artworkMatch[2] : artworkImage?.src || '';
            const title = artworkNode?.getAttribute('aria-label') ||
                document.querySelector('.playbackSoundBadge__title')?.textContent?.trim() || '';
            const author = document.querySelector('.playbackSoundBadge__lightLink')
                ?.textContent?.trim() || '';
            const playControl = document.querySelector('.playControls__play');
            return {
                title,
                author,
                artwork: artwork.replace('50x50.', '500x500.'),
                elapsed: document.querySelector(
                    '.playbackTimeline__timePassed span:last-child'
                )?.textContent?.trim() || '',
                duration: document.querySelector(
                    '.playbackTimeline__duration span:last-child'
                )?.textContent?.trim() || '',
                isPlaying: !!playControl?.classList.contains('playing')
            };
        };
        const sendPresenceUpdate = (force = false) => {
            if (!settings.discord_rich_presence_enabled) return;
            const track = readPlaybackTrack();
            const identity = JSON.stringify({
                title: track.title,
                author: track.author,
                artwork: track.artwork,
                duration: track.duration,
                isPlaying: track.isPlaying
            });
            const now = Date.now();
            if (!force && identity === lastPresenceIdentity &&
                (!track.isPlaying || now - lastPresenceUpdateAt < 15000)) return;
            lastPresenceIdentity = identity;
            lastPresenceUpdateAt = now;
            post({ type: 'trackUpdate', track });
        };
        const schedulePresenceUpdate = () => {
            clearTimeout(presenceUpdateTimer);
            presenceUpdateTimer = window.setTimeout(() => sendPresenceUpdate(), 250);
        };
        function observePresenceRoots() {
            if (!settings.discord_rich_presence_enabled) return;
            const roots = [...document.querySelectorAll('.playControls, .playbackSoundBadge')];
            if (roots.length === presenceObservedRoots.length &&
                roots.every((root, index) => root === presenceObservedRoots[index])) return;
            presenceObservedRoots = roots;
            presenceObserver.disconnect();
            for (const root of roots) {
                presenceObserver.observe(root, {
                    attributes: true,
                    childList: true,
                    characterData: true,
                    subtree: true
                });
            }
            sendPresenceUpdate(true);
        }
        const startPresenceObserver = () => {
            if (!settings.discord_rich_presence_enabled || presenceInterval !== null) return;
            const root = document.body || document.documentElement;
            if (!root) {
                document.addEventListener('DOMContentLoaded', startPresenceObserver, { once: true });
                return;
            }
            presenceRootObserver.observe(root, { childList: true, subtree: true });
            observePresenceRoots();
            presenceInterval = window.setInterval(() => sendPresenceUpdate(), 15000);
        };
        const stopPresenceObserver = () => {
            presenceRootObserver.disconnect();
            presenceObserver.disconnect();
            clearTimeout(presenceUpdateTimer);
            if (presenceInterval !== null) {
                clearInterval(presenceInterval);
                presenceInterval = null;
            }
            presenceObservedRoots = [];
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
            document.documentElement.style.setProperty('--rustcloud-custom-accent', color);
            colorPicker.value = color;
            customThemeButton.querySelector('.theme-preview span').style.background = color;
            customThemeButton.querySelector('.theme-preview span').style.boxShadow = `0 0 8px ${color}`;
            swatches.forEach(button => button.classList.toggle('active', button.dataset.color === color));
            settings.accent_color = color;
            saveUiPreferences();
            post({ type: 'accent', color });
        };
        const setCustomFont = font => {
            if (!customFonts.includes(font)) return;
            settings.custom_font = font;
            customFontPicker.value = font;
            document.documentElement.style.setProperty('--rustcloud-custom-font', JSON.stringify(font));
            saveUiPreferences();
        };
        const themes = {
            default: { accent: null },
            'midnight-glass': { accent: '#78caff' },
            'neon-pulse': { accent: '#f16bff' },
            'warm-vinyl': { accent: '#f3ad59' },
            'forest-glow': { accent: '#69d6a8' },
            'rose-quartz': { accent: '#ff82b7' },
            'ocean-breeze': { accent: '#67dce5' },
            'custom-glass': { accent: null }
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
            customThemeControls.hidden = name !== 'custom-glass';
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

            const pinned = filtered.filter(item => pinnedPlaylistUrls.has(item.url));
            const groups = [
                ['pinned', 'Pinned', pinned],
                ['created', 'Your playlists', filtered.filter(item =>
                    item.kind === 'created' && !pinnedPlaylistUrls.has(item.url)
                )],
                ['saved', 'Liked playlists', filtered.filter(item =>
                    item.kind === 'saved' && !pinnedPlaylistUrls.has(item.url)
                )]
            ];

            for (const [kind, label, items] of groups) {
                if (!items.length) continue;

                const group = document.createElement('section');
                group.className = 'library-group';
                const heading = document.createElement('h3');
                heading.className = 'library-group-title';
                heading.textContent = label;
                group.appendChild(heading);

                for (const item of items) {
                    const row = document.createElement('div');
                    row.className = 'library-item-row';
                    const button = document.createElement('button');
                    button.className = 'library-item';
                    button.type = 'button';
                    button.setAttribute('aria-label', `Open playlist ${item.title}`);
                    if (item.artwork) {
                        const image = document.createElement('img');
                        image.src = item.artwork;
                        image.alt = '';
                        image.loading = 'lazy';
                        image.referrerPolicy = 'no-referrer';
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
                    row.appendChild(button);

                    const pin = document.createElement('button');
                    pin.className = 'library-pin';
                    pin.type = 'button';
                    pin.classList.toggle('pinned', pinnedPlaylistUrls.has(item.url));
                    pin.textContent = pinnedPlaylistUrls.has(item.url) ? '◆' : '◇';
                    pin.title = pinnedPlaylistUrls.has(item.url) ? 'Unpin playlist' : 'Pin playlist';
                    pin.setAttribute('aria-label',
                        `${pinnedPlaylistUrls.has(item.url) ? 'Unpin' : 'Pin'} playlist ${item.title}`
                    );
                    pin.addEventListener('click', event => {
                        event.stopPropagation();
                        if (pinnedPlaylistUrls.has(item.url)) {
                            pinnedPlaylistUrls.delete(item.url);
                        } else {
                            pinnedPlaylistUrls.add(item.url);
                        }
                        try {
                            localStorage.setItem(playlistPinsKey,
                                JSON.stringify([...pinnedPlaylistUrls])
                            );
                        } catch (error) {
                            console.warn('[RustCloud] Could not save pinned playlists:', error);
                        }
                        renderLibrary();
                    });
                    row.appendChild(pin);
                    group.appendChild(row);
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
                    let artwork = '';
                    const row = link.closest('.soundList__item, .userStreamItem, li, article') || link;
                    const candidates = [
                        link.querySelector('img[src], img[data-src], img[srcset], picture source[srcset]'),
                        row.querySelector('img[src], img[data-src], img[srcset], picture source[srcset]'),
                        ...row.querySelectorAll('[style*="background-image"]')
                    ];
                    for (const candidate of candidates) {
                        if (!candidate) continue;
                        if (candidate.matches && candidate.matches('source')) {
                            artwork = candidate.srcset || candidate.getAttribute('srcset') || '';
                        } else if (candidate.style && candidate.style.backgroundImage) {
                            const match = candidate.style.backgroundImage.match(/url\(["']?([^"')]+)["']?\)/);
                            artwork = match ? match[1] : '';
                        } else {
                            artwork =
                                candidate.currentSrc ||
                                candidate.dataset.src ||
                                candidate.src ||
                                candidate.getAttribute('srcset') ||
                                '';
                            if (artwork.includes(',')) artwork = artwork.split(',')[0].trim().split(/\s+/)[0];
                        }
                        if (artwork) break;
                    }
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
        discordRichPresence.checked = settings.discord_rich_presence_enabled !== false;
        colorPicker.value = settings.accent_color;
        customFontPicker.value = settings.custom_font;
        themeButtons.forEach(button => {
            button.addEventListener('click', () => {
                applyTheme(button.dataset.theme, true);
                observePlayingTrack();
            });
        });
        swatches.forEach(button => {
            button.style.background = button.dataset.color;
            button.addEventListener('click', () => setAccent(button.dataset.color));
        });
        setAccent(settings.accent_color);
        setCustomFont(settings.custom_font);
        applyTheme(settings.theme, false);
        libraryItems = readPlaylistCache();
        try {
            const pinned = JSON.parse(localStorage.getItem(playlistPinsKey) || '[]');
            if (Array.isArray(pinned)) {
                pinnedPlaylistUrls = new Set(pinned.filter(url =>
                    typeof url === 'string' &&
                    /^https:\/\/soundcloud\.com\/[^/]+\/sets\/[^/]+/.test(url)
                ));
            }
        } catch (error) {
            console.warn('[RustCloud] Could not read pinned playlists:', error);
        }
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
        let playingTrackObserver = null;
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
            if (settings.theme === 'default') {
                if (playingTrackObserver) {
                    playingTrackObserver.disconnect();
                    playingTrackObserver = null;
                }
                syncPlayingTrackTheme();
                return;
            }
            const root = document.body || document.documentElement;
            if (!root) {
                document.addEventListener('DOMContentLoaded', observePlayingTrack, { once: true });
                return;
            }
            if (!playingTrackObserver) {
                playingTrackObserver = new MutationObserver(schedulePlayingTrackThemeSync);
            }
            playingTrackObserver.disconnect();
            playingTrackObserver.observe(root, {
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
        discordRichPresence.addEventListener('change', () => {
            settings.discord_rich_presence_enabled = discordRichPresence.checked;
            post({ type: 'discordRichPresence', enabled: discordRichPresence.checked });
            if (discordRichPresence.checked) {
                lastPresenceIdentity = '';
                startPresenceObserver();
            } else {
                stopPresenceObserver();
            }
        });
        colorPicker.addEventListener('change', () => setAccent(colorPicker.value));
        customFontPicker.addEventListener('change', () => setCustomFont(customFontPicker.value));
        window.addEventListener('keydown', event => {
            if (event.key === 'F1') {
                event.preventDefault();
                event.stopPropagation();
                toggleSettings();
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
        startPresenceObserver();
        installShuffleEnhancement();
    };

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', mount, { once: true });
    } else {
        mount();
    }
})();
"##;

    const TITLEBAR_HTML: &str = r#"<!doctype html>
<meta charset="utf-8">
<style>
    :root {
        color-scheme: dark; --accent:#78caff; --bar:#17171bf5; --edge:#ffffff18;
        font-family:"Segoe UI",system-ui,sans-serif;
    }
    * { box-sizing:border-box; }
    html, body { width:100%; height:100%; margin:0; overflow:hidden; }
    #window-titlebar {
        position:relative; display:flex; align-items:stretch; width:100%; height:34px;
        color:#f5f5f7; background:var(--bar); border-bottom:1px solid var(--edge);
        user-select:none;
    }
    #window-titlebar-drag { flex:1; min-width:0; }
    #window-titlebar-controls { position:relative; z-index:2; display:flex; }
    .window-control {
        display:grid; place-items:center; width:46px; height:33px; padding:0;
        border:0; color:inherit; background:transparent; cursor:pointer;
    }
    .window-control:hover { background:#ffffff1a; }
    .window-control:focus-visible { outline:1px solid var(--accent); outline-offset:-2px; }
    .window-control svg {
        width:10px; height:10px; fill:none; stroke:currentColor; stroke-width:1.2;
    }
    #window-close:hover { background:#e81123; }
    .resize-edge { position:absolute; z-index:4; touch-action:none; }
    #resize-north { top:0; left:10px; right:10px; height:7px; cursor:n-resize; }
    #resize-north-east { top:0; right:0; width:12px; height:12px; cursor:nesw-resize; }
    #resize-north-west { top:0; left:0; width:12px; height:12px; cursor:nwse-resize; }
    #window-titlebar[data-maximized="true"] .resize-edge { display:none; }
    #window-titlebar[data-theme="midnight-glass"] { --bar:#111827f5; --edge:#2b3d51; }
    #window-titlebar[data-theme="neon-pulse"] { --bar:#170f24f5; --edge:#483054; }
    #window-titlebar[data-theme="warm-vinyl"] { --bar:#211710f5; --edge:#4d3728; }
    #window-titlebar[data-theme="forest-glow"] { --bar:#111e1af5; --edge:#2b4638; }
    #window-titlebar[data-theme="rose-quartz"] { --bar:#1d111bf5; --edge:#493044; }
    #window-titlebar[data-theme="ocean-breeze"] { --bar:#0e1a24f5; --edge:#294858; }
    #window-titlebar[data-theme="custom-glass"] {
        --bar:color-mix(in srgb,var(--accent) 8%,#111827f5);
        --edge:color-mix(in srgb,var(--accent) 24%,transparent);
    }
</style>
<div id="window-titlebar" data-theme="default">
    <div id="window-titlebar-drag" aria-label="Drag window"></div>
    <div id="window-titlebar-controls">
        <button class="window-control" id="window-minimize" aria-label="Minimize">
            <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M1 5h8"/></svg>
        </button>
        <button class="window-control" id="window-maximize" aria-label="Maximize">
            <svg viewBox="0 0 10 10" aria-hidden="true"><rect x="1.5" y="1.5" width="7" height="7"/></svg>
        </button>
        <button class="window-control" id="window-close" aria-label="Close">
            <svg viewBox="0 0 10 10" aria-hidden="true"><path d="m1.5 1.5 7 7m0-7-7 7"/></svg>
        </button>
    </div>
</div>
<div class="resize-edge" id="resize-north" data-action="resizeNorth"></div>
<div class="resize-edge" id="resize-north-east" data-action="resizeNorthEast"></div>
<div class="resize-edge" id="resize-north-west" data-action="resizeNorthWest"></div>
<script>
    (() => {
        const titlebar = document.getElementById('window-titlebar');
        const maximizeButton = document.getElementById('window-maximize');
        const maximizeIcon = '<svg viewBox="0 0 10 10" aria-hidden="true"><rect x="1.5" y="1.5" width="7" height="7"/></svg>';
        const restoreIcon = '<svg viewBox="0 0 10 10" aria-hidden="true"><path d="M3 1.5h5.5V7M7 3H1.5v5.5H7V3"/></svg>';
        const post = action => {
            if (window.ipc && typeof window.ipc.postMessage === 'function') {
                window.ipc.postMessage(JSON.stringify({ type:'windowControl', action }));
            }
        };
        window.__RUSTCLOUD_SET_MAXIMIZED__ = maximized => {
            titlebar.dataset.maximized = String(maximized);
            maximizeButton.innerHTML = maximized ? restoreIcon : maximizeIcon;
            maximizeButton.setAttribute('aria-label', maximized ? 'Restore' : 'Maximize');
        };
        window.__RUSTCLOUD_SET_APPEARANCE__ = (theme, accent) => {
            titlebar.dataset.theme = theme;
            titlebar.style.setProperty('--accent', accent);
        };
        const appearance = window.__RUSTCLOUD_TITLEBAR_APPEARANCE__;
        if (appearance) window.__RUSTCLOUD_SET_APPEARANCE__(appearance[0], appearance[1]);
        document.querySelectorAll('.window-control').forEach(button => {
            button.addEventListener('click', () => post({
                'window-minimize':'minimize',
                'window-maximize':'toggleMaximize',
                'window-close':'close'
            }[button.id]));
        });
        const drag = document.getElementById('window-titlebar-drag');
        drag.addEventListener('pointerdown', event => {
            if (event.button === 0) post('drag');
        });
        drag.addEventListener('dblclick', () => post('toggleMaximize'));
        document.querySelectorAll('.resize-edge').forEach(edge => {
            edge.addEventListener('pointerdown', event => {
                if (event.button === 0) {
                    event.preventDefault();
                    post(edge.dataset.action);
                }
            });
        });
    })();
</script>
"#;

    const DEMO_HTML: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>RustCloud blocker check</title>
<style>body{font:16px sans-serif;max-width:42rem;margin:4rem auto;line-height:1.5}</style>
<h1>Request-blocking check</h1>
<p>This page requests an image from <code>ads.soundcloud.com</code> and an audio file
from the observed SoundCloud ad-delivery path.</p>
<p>RustCloud should intercept both and show a blocked-request count in F1 Settings, under Ad blocking.
The requests are answered locally; they are not sent to those hosts.</p>
<img src="https://ads.soundcloud.com/rustcloud-block-test.gif" alt="Blocked test request">
<audio controls autoplay src="https://delivery-cdn-cf.adswizz.com/soundcloud_newEnco/ad_test.mp3"></audio>
"#;

    fn titlebar_bounds(window: &Window) -> Rect {
        let size = window.inner_size().to_logical::<u32>(window.scale_factor());
        Rect {
            position: LogicalPosition::new(0, 0).into(),
            size: LogicalSize::new(size.width, TITLEBAR_HEIGHT).into(),
        }
    }

    fn page_bounds(window: &Window) -> Rect {
        let size = window.inner_size().to_logical::<u32>(window.scale_factor());
        Rect {
            position: LogicalPosition::new(0, TITLEBAR_HEIGHT).into(),
            size: LogicalSize::new(
                size.width,
                size.height.saturating_sub(TITLEBAR_HEIGHT).max(1),
            )
            .into(),
        }
    }

    fn install_settings_shortcut(
        webview: &WebView,
        event_proxy: EventLoopProxy<UserEvent>,
    ) -> windows::core::Result<()> {
        const VK_F1: u32 = 0x70;

        let controller = webview.controller();
        let handler =
            AcceleratorKeyPressedEventHandler::create(Box::new(move |_controller, args| {
                if let Some(args) = args {
                    let mut virtual_key = 0;
                    if let Err(error) = unsafe { args.VirtualKey(&mut virtual_key) } {
                        eprintln!("Could not read the WebView2 accelerator key: {error}");
                        return Ok(());
                    }
                    if virtual_key != VK_F1 {
                        return Ok(());
                    }

                    let mut key_event_kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                    if let Err(error) = unsafe { args.KeyEventKind(&mut key_event_kind) } {
                        eprintln!("Could not read the WebView2 accelerator event kind: {error}");
                        return Ok(());
                    }
                    if key_event_kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                        && key_event_kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
                    {
                        return Ok(());
                    }

                    let mut key_status = COREWEBVIEW2_PHYSICAL_KEY_STATUS::default();
                    let first_press = match unsafe { args.PhysicalKeyStatus(&mut key_status) } {
                        Ok(()) => !key_status.WasKeyDown.as_bool(),
                        Err(error) => {
                            eprintln!("Could not read the WebView2 accelerator key state: {error}");
                            true
                        }
                    };
                    if let Err(error) = unsafe { args.SetHandled(true) } {
                        eprintln!("Could not handle the F1 accelerator in WebView2: {error}");
                        return Ok(());
                    }
                    if first_press {
                        if let Err(error) = event_proxy.send_event(UserEvent::ToggleSettings) {
                            eprintln!("Could not send the RustCloud settings shortcut: {error}");
                        }
                    }
                }
                Ok(())
            }));
        let mut token = 0;
        unsafe { controller.add_AcceleratorKeyPressed(&handler, &mut token) }
    }

    enum UserEvent {
        RequestBlocked(usize),
        WindowControl(WindowAction),
        TitlebarAppearance { theme: String, accent: String },
        ToggleSettings,
    }

    struct App {
        window: Option<Window>,
        webview: Option<WebView>,
        titlebar_webview: Option<WebView>,
        block_test: bool,
        event_proxy: EventLoopProxy<UserEvent>,
        filter_engine: Arc<RwLock<Engine>>,
        settings: Arc<Mutex<AppSettings>>,
        blocking_enabled: Arc<AtomicBool>,
        presence_sender: Sender<PresenceCommand>,
        maximized: bool,
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
                    .with_title("RustCloud")
                    .with_decorations(false)
                    .with_resizable(true)
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
            let titlebar_initialization_script = match titlebar_initialization_script(&settings) {
                Ok(script) => script,
                Err(error) => {
                    eprintln!("Could not prepare the RustCloud title bar: {error}");
                    event_loop.exit();
                    return;
                }
            };
            let shared_settings = Arc::clone(&self.settings);
            let blocking_enabled = Arc::clone(&self.blocking_enabled);
            let presence_sender = self.presence_sender.clone();
            let event_proxy = self.event_proxy.clone();
            let webview = match WebViewBuilder::new()
                .with_initialization_script(initialization_script)
                .with_ipc_handler(move |request| {
                    handle_settings_message(
                        request.body(),
                        &shared_settings,
                        &blocking_enabled,
                        &presence_sender,
                        &event_proxy,
                    );
                })
                .with_url("about:blank")
                .with_bounds(page_bounds(&window))
                .build_as_child(&window)
            {
                Ok(webview) => webview,
                Err(error) => {
                    eprintln!("Could not start WebView2. Install or update the Microsoft Edge WebView2 Runtime: {error}");
                    event_loop.exit();
                    return;
                }
            };
            if let Err(error) = install_settings_shortcut(&webview, self.event_proxy.clone()) {
                eprintln!("Could not install the RustCloud F1 shortcut: {error}");
            }

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

            let titlebar_settings = Arc::clone(&self.settings);
            let titlebar_blocking_enabled = Arc::clone(&self.blocking_enabled);
            let titlebar_presence_sender = self.presence_sender.clone();
            let titlebar_event_proxy = self.event_proxy.clone();
            let titlebar_webview = match WebViewBuilder::new()
                .with_initialization_script(titlebar_initialization_script)
                .with_ipc_handler(move |request| {
                    handle_settings_message(
                        request.body(),
                        &titlebar_settings,
                        &titlebar_blocking_enabled,
                        &titlebar_presence_sender,
                        &titlebar_event_proxy,
                    );
                })
                .with_html(TITLEBAR_HTML)
                .with_bounds(titlebar_bounds(&window))
                .build_as_child(&window)
            {
                Ok(webview) => webview,
                Err(error) => {
                    eprintln!("Could not create the RustCloud title bar: {error}");
                    event_loop.exit();
                    return;
                }
            };
            if let Err(error) =
                install_settings_shortcut(&titlebar_webview, self.event_proxy.clone())
            {
                eprintln!("Could not install the RustCloud F1 shortcut for the title bar: {error}");
            }

            self.window = Some(window);
            self.webview = Some(webview);
            self.titlebar_webview = Some(titlebar_webview);
        }

        fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
            match event {
                UserEvent::RequestBlocked(count) => {
                    if let Some(webview) = &self.webview {
                        let script = format!(
                            "window.__RUSTCLOUD_BLOCKED_COUNT__={count};\
                            if (typeof window.__RUSTCLOUD_RENDER_BLOCKED_COUNT__ === 'function') \
                            window.__RUSTCLOUD_RENDER_BLOCKED_COUNT__();"
                        );
                        if let Err(error) = webview.evaluate_script(&script) {
                            eprintln!("Could not update the ad-blocking counter: {error}");
                        }
                    }
                }
                UserEvent::TitlebarAppearance { theme, accent } => {
                    if let Some(webview) = &self.titlebar_webview {
                        match serde_json::to_string(&[theme, accent]) {
                            Ok(appearance) => {
                                let script = format!(
                                    "if (typeof window.__RUSTCLOUD_SET_APPEARANCE__ === 'function') \
                                    window.__RUSTCLOUD_SET_APPEARANCE__({appearance}[0],{appearance}[1]);"
                                );
                                if let Err(error) = webview.evaluate_script(&script) {
                                    eprintln!(
                                        "Could not update the RustCloud title-bar theme: {error}"
                                    );
                                }
                            }
                            Err(error) => {
                                eprintln!(
                                    "Could not serialize the RustCloud title-bar theme: {error}"
                                );
                            }
                        }
                    }
                }
                UserEvent::ToggleSettings => {
                    if let Some(webview) = &self.webview {
                        let script =
                            "if (typeof window.__RUSTCLOUD_TOGGLE_SETTINGS__ === 'function') \
                            window.__RUSTCLOUD_TOGGLE_SETTINGS__();";
                        if let Err(error) = webview.evaluate_script(script) {
                            eprintln!("Could not toggle RustCloud settings with F1: {error}");
                        }
                    }
                }
                UserEvent::WindowControl(action) => match action {
                    WindowAction::Minimize => {
                        if let Some(window) = &self.window {
                            window.set_minimized(true);
                        }
                    }
                    WindowAction::ToggleMaximize => {
                        if let Some(window) = &self.window {
                            window.set_maximized(!window.is_maximized());
                        }
                    }
                    WindowAction::Close => event_loop.exit(),
                    WindowAction::Drag => {
                        if let Some(window) = &self.window {
                            if let Err(error) = window.drag_window() {
                                eprintln!("Could not move the RustCloud window: {error}");
                            }
                        }
                    }
                    WindowAction::ResizeNorth => {
                        start_window_resize(self.window.as_ref(), ResizeDirection::North)
                    }
                    WindowAction::ResizeNorthEast => {
                        start_window_resize(self.window.as_ref(), ResizeDirection::NorthEast)
                    }
                    WindowAction::ResizeEast => {
                        start_window_resize(self.window.as_ref(), ResizeDirection::East)
                    }
                    WindowAction::ResizeSouthEast => {
                        start_window_resize(self.window.as_ref(), ResizeDirection::SouthEast)
                    }
                    WindowAction::ResizeSouth => {
                        start_window_resize(self.window.as_ref(), ResizeDirection::South)
                    }
                    WindowAction::ResizeSouthWest => {
                        start_window_resize(self.window.as_ref(), ResizeDirection::SouthWest)
                    }
                    WindowAction::ResizeWest => {
                        start_window_resize(self.window.as_ref(), ResizeDirection::West)
                    }
                    WindowAction::ResizeNorthWest => {
                        start_window_resize(self.window.as_ref(), ResizeDirection::NorthWest)
                    }
                },
            }
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            _window_id: WindowId,
            event: WindowEvent,
        ) {
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    if let Some(window) = &self.window {
                        if let Some(webview) = &self.webview {
                            if let Err(error) = webview.set_bounds(page_bounds(window)) {
                                eprintln!("Could not resize the SoundCloud page viewport: {error}");
                            }
                        }
                        if let Some(webview) = &self.titlebar_webview {
                            if let Err(error) = webview.set_bounds(titlebar_bounds(window)) {
                                eprintln!("Could not resize the RustCloud title bar: {error}");
                            }
                        }
                    }
                    let maximized = self
                        .window
                        .as_ref()
                        .is_some_and(|window| window.is_maximized());
                    if maximized != self.maximized {
                        self.maximized = maximized;
                        if let Some(webview) = &self.webview {
                            let script = format!(
                                "if (typeof window.__RUSTCLOUD_SET_MAXIMIZED__ === 'function') \
                                window.__RUSTCLOUD_SET_MAXIMIZED__({maximized});"
                            );
                            if let Err(error) = webview.evaluate_script(&script) {
                                eprintln!(
                                    "Could not update the title-bar maximize button: {error}"
                                );
                            }
                        }
                        if let Some(webview) = &self.titlebar_webview {
                            let script = format!(
                                "if (typeof window.__RUSTCLOUD_SET_MAXIMIZED__ === 'function') \
                                window.__RUSTCLOUD_SET_MAXIMIZED__({maximized});"
                            );
                            if let Err(error) = webview.evaluate_script(&script) {
                                eprintln!(
                                    "Could not update the title-bar maximize button: {error}"
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn start_window_resize(window: Option<&Window>, direction: ResizeDirection) {
        if let Some(window) = window {
            if let Err(error) = window.drag_resize_window(direction) {
                eprintln!("Could not resize the RustCloud window: {error}");
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
                | "custom-glass"
        )
    }

    fn settings_initialization_script(settings: &AppSettings) -> Result<String, serde_json::Error> {
        Ok(format!(
            "window.__RUSTCLOUD_SETTINGS__={};\n{}",
            serde_json::to_string(settings)?,
            SETTINGS_UI_SCRIPT
        ))
    }

    fn titlebar_initialization_script(settings: &AppSettings) -> Result<String, serde_json::Error> {
        Ok(format!(
            "window.__RUSTCLOUD_TITLEBAR_APPEARANCE__={};",
            serde_json::to_string(&[&settings.theme, &settings.accent_color])?
        ))
    }

    fn handle_settings_message(
        raw_message: &str,
        settings: &Mutex<AppSettings>,
        blocking_enabled: &AtomicBool,
        presence_sender: &Sender<PresenceCommand>,
        event_proxy: &EventLoopProxy<UserEvent>,
    ) {
        let message = match serde_json::from_str::<SettingsMessage>(raw_message) {
            Ok(message) => message,
            Err(error) => {
                eprintln!("Ignored invalid RustCloud settings message: {error}");
                return;
            }
        };
        if let SettingsMessage::WindowControl { action } = &message {
            if let Err(error) = event_proxy.send_event(UserEvent::WindowControl(*action)) {
                eprintln!("Could not send RustCloud window command: {error}");
            }
            return;
        }
        if let SettingsMessage::TrackUpdate { track } = &message {
            if track.title.len() > 1024
                || track.author.len() > 1024
                || track.artwork.len() > 2048
                || track.elapsed.len() > 32
                || track.duration.len() > 32
            {
                eprintln!("Ignored invalid Discord Rich Presence track update.");
                return;
            }
            if let Err(error) = presence_sender.send(PresenceCommand::Track(track.clone())) {
                eprintln!("Could not send track data to Discord Rich Presence: {error}");
            }
            return;
        }

        let mut settings = match settings.lock() {
            Ok(settings) => settings,
            Err(error) => {
                eprintln!("Could not update RustCloud settings: {error}");
                return;
            }
        };
        let mut titlebar_appearance = None;
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
                titlebar_appearance = Some((settings.theme.clone(), settings.accent_color.clone()));
            }
            SettingsMessage::Theme { name } => {
                if !is_valid_theme(&name) {
                    eprintln!("Ignored invalid RustCloud theme.");
                    return;
                }
                settings.theme = name;
                titlebar_appearance = Some((settings.theme.clone(), settings.accent_color.clone()));
            }
            SettingsMessage::CoverGlow { enabled } => {
                settings.cover_glow_enabled = enabled;
            }
            SettingsMessage::FullPlaylistShuffle { enabled } => {
                settings.full_playlist_shuffle_enabled = enabled;
            }
            SettingsMessage::DiscordRichPresence { enabled } => {
                settings.discord_rich_presence_enabled = enabled;
                if let Err(error) = presence_sender.send(PresenceCommand::SetEnabled(enabled)) {
                    eprintln!("Could not update Discord Rich Presence setting: {error}");
                }
            }
            SettingsMessage::TrackUpdate { .. } => unreachable!(),
            SettingsMessage::WindowControl { .. } => unreachable!(),
        }
        save_settings(&settings);
        drop(settings);
        if let Some((theme, accent)) = titlebar_appearance {
            if let Err(error) =
                event_proxy.send_event(UserEvent::TitlebarAppearance { theme, accent })
            {
                eprintln!("Could not send the RustCloud title-bar theme update: {error}");
            }
        }
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
        let (saved_ad_block_enabled, discord_presence_enabled) = {
            let settings = settings
                .lock()
                .map_err(|error| format!("Could not read RustCloud settings: {error}"))?;
            (
                settings.ad_block_enabled,
                settings.discord_rich_presence_enabled,
            )
        };
        let blocking_enabled = Arc::new(AtomicBool::new(saved_ad_block_enabled));
        let presence_sender = spawn_presence_worker(discord_presence_enabled)?;
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
            titlebar_webview: None,
            block_test: std::env::args().any(|arg| arg == "--block-test"),
            event_proxy,
            filter_engine,
            settings,
            blocking_enabled,
            presence_sender,
            maximized: false,
        };
        event_loop.run_app(&mut app)?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::{parse_time_millis, presence_timestamps_at, PlaybackTrack};

        #[test]
        fn parses_elapsed_and_remaining_track_times() {
            assert_eq!(parse_time_millis("1:02:03"), Some(3_723_000));
            assert_eq!(parse_time_millis("-2:10"), Some(-130_000));
            assert_eq!(parse_time_millis(""), None);
            assert_eq!(parse_time_millis("not-a-time"), None);
        }

        #[test]
        fn creates_discord_timestamps_in_milliseconds() {
            let track = PlaybackTrack {
                title: String::from("Title"),
                author: String::from("Artist"),
                artwork: String::new(),
                elapsed: String::from("1:15"),
                duration: String::from("3:00"),
                is_playing: true,
            };
            assert_eq!(
                presence_timestamps_at(&track, 10_000_000),
                Some((9_925_000, 10_105_000))
            );
        }
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

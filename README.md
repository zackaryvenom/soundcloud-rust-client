# RustCloud

A small, Windows-first SoundCloud desktop client. RustCloud opens SoundCloud in WebView2, blocks many advertising requests, and adds a playlist sidebar, optional full-playlist shuffle, and customizable themes. The app includes an original grayscale cloud-and-waveform icon.

## Requirements

- Windows 10 or 11.
- The Microsoft Edge WebView2 Runtime (normally already installed; if the app reports it is missing or outdated, install the Evergreen Runtime from [Microsoft](https://developer.microsoft.com/microsoft-edge/webview2/)).
- To build from source: Rust stable with the MSVC toolchain. If Rust reports that the linker or Windows SDK is missing, install Visual Studio Build Tools and select **Desktop development with C++**.

## Build and install

To run from source, open PowerShell in the repository folder and run:

```powershell
cargo run
```

To build the optimized release app:

```powershell
cargo build --release
```

The standalone executable is `target\release\rustcloud.exe`. It embeds the app and taskbar icon and opens without a command prompt. Download `rustcloud.exe` from the [latest GitHub Release](https://github.com/zackaryvenom/soundcloud-rust-client/releases/latest), or copy the build output to another Windows 10/11 computer. Rust and the build tools are not needed on the destination computer, but the Microsoft Edge WebView2 Runtime is required. There is no installer; run the executable directly. RustCloud saves settings and cached filter lists under `%LOCALAPPDATA%\RustCloud`.

Debug runs via `cargo run` keep the PowerShell console available for logs. Release builds hide the console.

## Features

On startup, RustCloud immediately uses cached Ghostery filter lists (or built-in rules if no cache exists), then refreshes stale lists in the background. Updated rules take effect without restarting. Lists are cached for 24 hours under `%LOCALAPPDATA%\RustCloud`.

Use **Library** in the top-left corner to open a searchable sidebar for your created and liked/saved playlists. It reads the lists from SoundCloud in the existing signed-in WebView session; no separate login is needed. The playlist snapshot is cached locally; use the refresh button to look for changes. Click a playlist to open it.

Press **F1** to open settings. Choose **Default** to restore SoundCloud's original look, or pick **Midnight Glass**, **Neon Pulse**, **Warm Vinyl**, **Forest Glow**, **Rose Quartz**, or **Ocean Breeze** to theme the page background, text, player controls, and progress bar with a subtle matching glow and per-theme font. RustCloud's panels follow the theme too. Player controls use monochrome styling. Theme and accent choices survive in-app navigation.

Settings also include ad blocking, optional cover-art glow that follows the current track with a smooth crossfade, and **Full-playlist shuffle** (off by default). The artwork glow observes changes instead of polling while enabled. Full-playlist shuffle loads the remaining tracks in the active queue before shuffling; this may take a little while for large playlists. Preferences are saved to `%LOCALAPPDATA%\RustCloud\settings.json`.

To prove the request hook works without waiting for SoundCloud to load an ad, run:

```powershell
cargo run -- --block-test
```

This opens a small test page that requests an image from `ads.soundcloud.com` and a synthetic MP3 from the observed SoundCloud ad-delivery path. The app should log both requests as `BLOCKED` in PowerShell and increment its window title. WebView2 supplies a local empty `204 No Content` response for blocked requests instead of sending them to the host.

Run the filtering unit tests with:

```powershell
cargo test
```

## Scope

This prototype uses WebView2's `WebResourceRequested` event and the `ICoreWebView2_22` request-source filter, so it checks matching requests before they reach the network, including requests from frames. It uses the same filter-list sources as the other SoundCloud client—EasyList, Peter Lowe, and uBlock Origin lists—through the Rust `adblock` engine, plus a small set of app-specific rules. The list-based approach is much broader than a hand-maintained host list, but it does not guarantee every ad will be blocked: SoundCloud can change delivery, and this prototype does not implement cosmetic page filtering or every Ghostery/Electron integration feature. If list downloads fail and no cached lists are available, RustCloud logs the error and falls back to its built-in rules. List sources are maintained by [Ghostery's adblocker project](https://github.com/ghostery/adblocker); individual lists may have their own licensing terms.

Full-playlist shuffle uses SoundCloud's in-page player queue and pagination. Because these are SoundCloud internals, the feature may need updating if SoundCloud changes its player; if loading stalls, RustCloud reports that and shuffles the tracks currently available.

The playlist sidebar reads the signed-in SoundCloud playlist views in hidden same-origin frames and extracts playlist links and artwork. The sidebar can stop working if SoundCloud changes those pages or prevents the views from loading in a frame.

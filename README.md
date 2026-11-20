# RustCloud

A small, Windows-first SoundCloud desktop prototype. It opens SoundCloud in WebView2 and blocks a short, built-in list of advertising hosts plus SoundCloud's `/ads` API endpoints by intercepting browser network requests.

## Requirements

- Windows 10 or 11.
- The Microsoft Edge WebView2 Runtime (normally already installed; if the app reports it is missing or outdated, install the Evergreen Runtime from [Microsoft](https://developer.microsoft.com/microsoft-edge/webview2/)).
- Rust stable with the MSVC toolchain. If Rust reports that the linker or Windows SDK is missing, install Visual Studio Build Tools and select **Desktop development with C++**.

## Run it

Open PowerShell in this folder and run:

```powershell
cargo run
```

The app opens `https://soundcloud.com/`. Its window title counts blocked requests; the PowerShell window prints each blocked URL.

To prove the request hook works without waiting for SoundCloud to load an ad, run:

```powershell
cargo run -- --block-test
```

This opens a small test page that requests an image from `ads.soundcloud.com`. The app should log `BLOCKED` in PowerShell and increment its window title. WebView2 supplies a local empty `204 No Content` response for blocked requests instead of sending them to the host.

Run the filtering unit tests with:

```powershell
cargo test
```

Build a release executable with `cargo build --release`; it will be at `target\release\rustcloud.exe`.

## Scope

This prototype uses WebView2's `WebResourceRequested` event and the `ICoreWebView2_22` request-source filter, so it blocks matching requests before they reach the network, including requests from frames. Its bundled domain and path rules are intentionally small and are **not** a complete EasyList/uBlock filter set; some ads may still appear, and SoundCloud can change its ad endpoints. The native request hook is Windows/WebView2-specific.

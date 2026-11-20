#[cfg(windows)]
mod desktop {
    use std::{
        error::Error,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    use rustcloud::blocked_reason;
    use webview2_com::{
        CoTaskMemPWSTR,
        Microsoft::Web::WebView2::Win32::{
            ICoreWebView2, ICoreWebView2_22, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
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
        window::{Window, WindowId},
    };
    use wry::{WebView, WebViewBuilder, WebViewExtWindows};

    const SOUNDCLOUD_URL: &str = "https://soundcloud.com/";
    const DEMO_HTML: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>RustCloud blocker check</title>
<style>body{font:16px sans-serif;max-width:42rem;margin:4rem auto;line-height:1.5}</style>
<h1>Request-blocking check</h1>
<p>This page requests an image from <code>ads.soundcloud.com</code>.</p>
<p>RustCloud should intercept it and show a blocked-request count in the window title.
The request is answered locally; it is not sent to that host.</p>
<img src="https://ads.soundcloud.com/rustcloud-block-test.gif" alt="Blocked test request">
"#;

    enum UserEvent {
        RequestBlocked(usize),
    }

    struct App {
        window: Option<Window>,
        webview: Option<WebView>,
        block_test: bool,
        event_proxy: EventLoopProxy<UserEvent>,
    }

    impl ApplicationHandler<UserEvent> for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }

            let window = match event_loop.create_window(
                Window::default_attributes()
                    .with_title("RustCloud - network ad blocking enabled")
                    .with_inner_size(LogicalSize::new(1200.0, 800.0)),
            ) {
                Ok(window) => window,
                Err(error) => {
                    eprintln!("Could not create the application window: {error}");
                    event_loop.exit();
                    return;
                }
            };

            let webview = match WebViewBuilder::new().with_url("about:blank").build(&window) {
                Ok(webview) => webview,
                Err(error) => {
                    eprintln!("Could not start WebView2. Install or update the Microsoft Edge WebView2 Runtime: {error}");
                    event_loop.exit();
                    return;
                }
            };

            if let Err(error) = install_blocker(&webview, self.event_proxy.clone()) {
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

            if let Some(reason) = blocked_reason(&request_url) {
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
                println!("BLOCKED ({reason}): {request_url}");
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

    pub fn run() -> Result<(), Box<dyn Error>> {
        let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
        event_loop.set_control_flow(ControlFlow::Wait);
        let event_proxy = event_loop.create_proxy();
        let mut app = App {
            window: None,
            webview: None,
            block_test: std::env::args().any(|arg| arg == "--block-test"),
            event_proxy,
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

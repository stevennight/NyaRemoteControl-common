//! Web pages (built from `common/web`, embedded in the executable) shown in a
//! WebView2 child window that covers a winit window.
//!
//! The page talks to Rust with JSON messages (see `common/web/src/lib/ipc.ts`):
//!
//! * page → Rust: `{"id": 7, "cmd": "connect", "args": {...}}` — delivered as a
//!   [`Call`] to the callback given to [`WebUi::new`]; answer it with
//!   [`WebUi::reply`] (every call must be answered once).
//! * Rust → page: [`WebUi::emit`] fires an event the page subscribed to.
//!
//! Set `NYA_WEB_DEV=http://localhost:5173` to load the Vite dev server instead
//! of the embedded pages (live reload while working on the UI).

use std::borrow::Cow;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;
use winit::dpi::PhysicalSize;
use winit::raw_window_handle::HasWindowHandle;
use wry::http::{header::CONTENT_TYPE, Response, StatusCode};
use wry::{Rect, WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows};

mod assets {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

/// A request from the page.
#[derive(Debug)]
pub struct Call {
    pub id: u64,
    pub cmd: String,
    pub args: Value,
}

impl Call {
    /// Deserialize the arguments.
    pub fn args<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_value(self.args.clone()).map_err(|e| anyhow!("{}: bad arguments: {e}", self.cmd))
    }
}

fn parse(msg: &str) -> Option<Call> {
    let v: Value = serde_json::from_str(msg).ok()?;
    Some(Call {
        id: v.get("id")?.as_u64()?,
        cmd: v.get("cmd")?.as_str()?.to_owned(),
        args: v.get("args").cloned().unwrap_or(Value::Null),
    })
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" => "application/json",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// The embedded file at `path` (`client.html`, `assets/x.js`, …).
pub fn asset(path: &str) -> Option<&'static [u8]> {
    assets::ASSETS.iter().find(|(p, _)| *p == path).map(|(_, b)| *b)
}

pub struct Options {
    /// Page to show, e.g. `client.html`.
    pub page: &'static str,
    /// WebView2 profile folder (cache, local storage).
    pub data_dir: PathBuf,
    /// Background painted before the page loads (matches the page's colours).
    pub background: (u8, u8, u8),
}

pub struct WebUi {
    webview: WebView,
    _context: WebContext,
}

impl WebUi {
    /// Create the page as a child of `window`, filling `size` (physical pixels).
    /// `on_call` runs on the UI thread for every request from the page.
    pub fn new<W: HasWindowHandle>(window: &W, size: PhysicalSize<u32>, opts: Options, on_call: impl Fn(Call) + 'static) -> Result<Self> {
        let _ = std::fs::create_dir_all(&opts.data_dir);
        let mut context = WebContext::new(Some(opts.data_dir.clone()));
        let url = match std::env::var("NYA_WEB_DEV") {
            Ok(dev) if !dev.is_empty() => format!("{}/{}", dev.trim_end_matches('/'), opts.page),
            _ => format!("nya://localhost/{}", opts.page),
        };
        let (r, g, b) = opts.background;
        let webview = WebViewBuilder::new_with_web_context(&mut context)
            .with_custom_protocol("nya".into(), |_id, req| {
                let path = req.uri().path().trim_start_matches('/');
                let path = if path.is_empty() { "index.html" } else { path };
                match asset(path) {
                    Some(bytes) => Response::builder()
                        .header(CONTENT_TYPE, mime(path))
                        .body(Cow::Borrowed(bytes))
                        .unwrap(),
                    None => Response::builder().status(StatusCode::NOT_FOUND).body(Cow::Borrowed(&[][..])).unwrap(),
                }
            })
            .with_ipc_handler(move |req| match parse(req.body()) {
                Some(c) => on_call(c),
                None => tracing::warn!("bad message from the page: {}", req.body()),
            })
            // Links open in the default browser, never inside the app.
            .with_navigation_handler(|url| {
                let internal = url.starts_with("nya://") || url.starts_with("http://nya.localhost") || url.starts_with("http://localhost");
                if !internal {
                    let _ = std::process::Command::new("explorer").arg(&url).spawn();
                }
                internal
            })
            .with_new_window_req_handler(|url, _features| {
                let _ = std::process::Command::new("explorer").arg(&url).spawn();
                wry::NewWindowResponse::Deny
            })
            .with_background_color((r, g, b, 255))
            .with_devtools(cfg!(debug_assertions) || std::env::var_os("NYA_WEB_DEVTOOLS").is_some())
            .with_browser_accelerator_keys(false)
            .with_default_context_menus(cfg!(debug_assertions))
            .with_bounds(bounds(size))
            .with_url(url)
            .build_as_child(window)
            .map_err(|e| {
                anyhow!("无法创建界面（WebView2）：{e}。请安装 Microsoft Edge WebView2 运行库：https://go.microsoft.com/fwlink/p/?LinkId=2124703")
            })?;
        Ok(Self { webview, _context: context })
    }

    pub fn resize(&self, size: PhysicalSize<u32>) {
        let _ = self.webview.set_bounds(bounds(size));
    }

    pub fn set_visible(&self, visible: bool) {
        let _ = self.webview.set_visible(visible);
        if visible {
            let _ = self.webview.focus();
        }
    }

    /// Answer a [`Call`].
    pub fn reply(&self, id: u64, result: Result<Value, String>) {
        let msg = match result {
            Ok(v) => serde_json::json!({ "id": id, "ok": true, "data": v }),
            Err(e) => serde_json::json!({ "id": id, "ok": false, "error": e }),
        };
        self.eval(&format!("window.__nya && window.__nya.reply({msg})"));
    }

    /// Fire `event` with `data` on the page.
    pub fn emit(&self, event: &str, data: &impl Serialize) {
        let data = serde_json::to_string(data).unwrap_or_else(|_| "null".into());
        let name = serde_json::to_string(event).unwrap();
        self.eval(&format!("window.__nya && window.__nya.event({name}, {data})"));
    }

    fn eval(&self, js: &str) {
        if let Err(e) = self.webview.evaluate_script(js) {
            tracing::warn!("webview script: {e}");
        }
    }
}

fn bounds(size: PhysicalSize<u32>) -> Rect {
    Rect {
        position: wry::dpi::PhysicalPosition::new(0, 0).into(),
        size: wry::dpi::PhysicalSize::new(size.width.max(1), size.height.max(1)).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_calls() {
        let c = parse(r#"{"id":3,"cmd":"connect","args":{"address":"1.2.3.4"}}"#).unwrap();
        assert_eq!((c.id, c.cmd.as_str()), (3, "connect"));
        assert_eq!(c.args["address"], "1.2.3.4");
        assert!(parse(r#"{"cmd":"x"}"#).is_none());
        assert!(parse("nope").is_none());
        let c = parse(r#"{"id":1,"cmd":"state"}"#).unwrap();
        assert!(c.args.is_null());
    }

    #[test]
    fn pages_are_embedded() {
        assert!(asset("client.html").is_some());
        assert_eq!(mime("assets/a.js"), "text/javascript; charset=utf-8");
    }
}

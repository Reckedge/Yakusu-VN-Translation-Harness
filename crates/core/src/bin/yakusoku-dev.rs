//! Browser dev server: serves `ui/` and forwards `POST /api/call` to the core,
//! so the interface can be developed and tested without the desktop shell.
//!
//!   cargo run -p yakusoku-core --features dev-server --bin yakusoku-dev -- [port]
//!
//! YAKUSOKU_FAKE_LLM=1 uses the built-in stand-in model (no API key, no network).
//! YAKUSOKU_FAKE_DELAY_MS slows it down so progress can be watched.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_http::{Header, Method, Response, Server};
use yakusoku_core::app::App;
use yakusoku_core::config::Env;
use yakusoku_core::llm;

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(8765);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ui = std::env::var_os("YAKUSOKU_UI").map(PathBuf::from).unwrap_or_else(|| root.join("ui"));
    let tools = std::env::var_os("YAKUSOKU_TOOLS").map(PathBuf::from).unwrap_or_else(|| root.join("tools"));
    let env = Env::system(tools);
    let factory = if std::env::var("YAKUSOKU_FAKE_LLM").as_deref() == Ok("1") {
        let delay = std::env::var("YAKUSOKU_FAKE_DELAY_MS").ok().and_then(|d| d.parse().ok()).unwrap_or(0);
        eprintln!("Using the stand-in model (YAKUSOKU_FAKE_LLM=1).");
        llm::fake::factory(Default::default(), None, std::time::Duration::from_millis(delay))
    } else {
        llm::http_factory()
    };
    let app: Arc<App> = App::with_opener(
        env,
        factory,
        Box::new(|p| {
            eprintln!("(would open {})", p.display());
            Ok(())
        }),
    );
    let token = format!(
        "{:016x}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64
            ^ 0x9e37_79b9_7f4a_7c15
    );
    let server = Server::http(("127.0.0.1", port)).expect("port in use?");
    eprintln!("Yakusoku dev server: http://127.0.0.1:{port}/");
    for mut req in server.incoming_requests() {
        let url = req.url().split('?').next().unwrap_or("/").to_string();
        if url == "/api/call" && *req.method() == Method::Post {
            let ok = req.headers().iter().any(|h| h.field.equiv("X-Yakusoku-Token") && h.value.as_str() == token);
            if !ok {
                let _ = req.respond(Response::from_string("{\"error\":\"forbidden\"}").with_status_code(403));
                continue;
            }
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let app = app.clone();
            std::thread::spawn(move || {
                let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                let cmd = v.get("cmd").and_then(Value::as_str).unwrap_or("");
                let args = v.get("args").cloned().unwrap_or(json!({}));
                let (code, out) = match app.call(cmd, &args) {
                    Ok(v) => (200, v),
                    Err(e) => (400, json!({"error": e})),
                };
                let _ = req.respond(
                    Response::from_string(out.to_string())
                        .with_status_code(code)
                        .with_header(header("Content-Type", "application/json"))
                        .with_header(header("Cache-Control", "no-store")),
                );
            });
            continue;
        }
        let rel = if url == "/" { "index.html".to_string() } else { url.trim_start_matches('/').to_string() };
        let path = ui.join(&rel);
        let safe = path.canonicalize().ok().filter(|p| p.starts_with(ui.canonicalize().unwrap_or_default()));
        match safe.and_then(|p| std::fs::read(&p).ok().map(|d| (p, d))) {
            Some((p, mut data)) => {
                if rel == "index.html" {
                    let html = String::from_utf8_lossy(&data)
                        .replace("<head>", &format!("<head><script>window.__YAKUSOKU_TOKEN__={token:?}</script>"));
                    data = html.into_bytes();
                }
                let _ = req.respond(
                    Response::from_data(data)
                        .with_header(header("Content-Type", mime(&p)))
                        .with_header(header("Cache-Control", "no-store")),
                );
            }
            None => {
                let _ = req.respond(Response::from_string("not found").with_status_code(404));
            }
        }
    }
}

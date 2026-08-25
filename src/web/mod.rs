//! Web console backend: a minimal hand-written HTTP/1.1 server sharing the
//! in-process command backend (`cli::run_command`) with the TUI. No new
//! dependencies — std::net + existing serde_json only. Request parsing and
//! response writing follow the patterns in `runtime::http`.
//!
//! Listen: `127.0.0.1:<port>` — default 8123 (`XU_WEB_PORT` overrides), kept
//! clear of the runtime adapter port (`agents::serve_port()`, default 9316).
//! The static frontend (index.html/app.js/style.css) is embedded via
//! `include_str!`; the frontend itself is authored by a parallel agent.

pub mod api;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

const DEFAULT_PORT: u16 = 8123;
const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

const INDEX_HTML: &str = include_str!("static/index.html");
const APP_JS: &str = include_str!("static/app.js");
const STYLE_CSS: &str = include_str!("static/style.css");

/// Port for the web console: `XU_WEB_PORT` env override, else 8123.
pub fn default_port() -> u16 {
    std::env::var("XU_WEB_PORT")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// Blocking serve loop. Exits after the current connection once
/// `stop_flag` is set (via `/api/web/stop` or the TUI). Runnable both from
/// the TUI and from a standalone `spec web` command.
pub fn serve(port: u16, stop_flag: Arc<AtomicBool>) -> Result<(), String> {
    let listen = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&listen).map_err(|error| format!("bind {listen}: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("configure {listen}: {error}"))?;
    serve_listener(listener, stop_flag)
}

fn serve_listener(listener: TcpListener, stop_flag: Arc<AtomicBool>) -> Result<(), String> {
    let _ = listener.set_nonblocking(true);
    let home = crate::config::home();
    loop {
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let home = home.clone();
                let flag = Arc::clone(&stop_flag);
                std::thread::spawn(move || {
                    let _ = handle_connection(&mut stream, &home, &flag);
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Ok(())
}

fn handle_connection(
    stream: &mut TcpStream,
    home: &Path,
    stop_flag: &Arc<AtomicBool>,
) -> Result<(), String> {
    let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
    let request = match read_request(stream) {
        Ok(request) => request,
        Err(error) => {
            let _ = write_error(stream, 400, &error);
            return Err(error);
        }
    };
    let response = route(home, &request);
    match response {
        RouteResponse::Json { status, value } => write_json(stream, (status, value)),
        RouteResponse::Text {
            status,
            content_type,
            body,
        } => write_text(stream, status, content_type, body.as_bytes()),
        RouteResponse::StopWeb { status, value } => {
            let result = write_json(stream, (status, value));
            // 字节已写入 socket 缓冲并 flush 后再停服（前端必收 200）。
            stop_flag.store(true, Ordering::SeqCst);
            result
        }
    }
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
    host: Option<String>,
}

/// 写操作 CSRF/DNS-rebinding 防线：浏览器发起的跨站 simple request 无法伪造
/// Host 头；仅接受本机回环形态的 Host。
fn host_is_local(request: &Request) -> bool {
    request.host.as_deref().is_some_and(|host| {
        host == "127.0.0.1"
            || host.starts_with("127.0.0.1:")
            || host == "localhost"
            || host.starts_with("localhost:")
            || host == "[::1]"
            || host == "[::1]:"
    })
}

/// Minimal HTTP/1.1 request parse: request line + exact-path target (query
/// stripped), content-length body. Mirrors `runtime::http` semantics.
fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    let mut temp = [0; 1024];
    let mut buffer = Vec::new();
    let header_end = loop {
        let read = stream.read(&mut temp).map_err(|e| e.to_string())?;
        if read == 0 {
            return Err("connection closed before headers".to_string());
        }
        buffer.extend_from_slice(&temp[..read]);
        if buffer.len() > MAX_BODY_BYTES {
            return Err("request too large".to_string());
        }
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index;
        }
    };

    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = headers.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| "missing request line".to_string())?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let raw_target = parts.next().unwrap_or_default();
    let path = raw_target
        .split_once('?')
        .map_or(raw_target, |(path, _)| path);

    let mut content_length = 0;
    let mut host: Option<String> = None;
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.eq_ignore_ascii_case("host") {
            host = Some(value.trim().to_ascii_lowercase());
        } else if key.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse::<usize>().unwrap_or(0);
        }
    }
    if content_length > MAX_BODY_BYTES {
        return Err("request body too large".to_string());
    }

    let body_start = header_end + 4;
    let mut body = buffer.get(body_start..).unwrap_or_default().to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut temp).map_err(|e| e.to_string())?;
        if read == 0 {
            return Err("connection closed before body".to_string());
        }
        body.extend_from_slice(&temp[..read]);
    }
    body.truncate(content_length);

    Ok(Request {
        method,
        path: path.to_string(),
        body,
        host,
    })
}

/// Dispatcher result: either embedded static text or a JSON envelope.
enum RouteResponse {
    Json {
        status: u16,
        value: Value,
    },
    /// 响应写回后再置 stop 标志：保证前端必然收到 200，服务随后退出。
    StopWeb {
        status: u16,
        value: Value,
    },
    Text {
        status: u16,
        content_type: &'static str,
        body: &'static str,
    },
}

/// Route dispatch. Static files and `/api/*` JSON handlers; everything else
/// falls through to a 404 error envelope.
fn route(home: &Path, request: &Request) -> RouteResponse {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => RouteResponse::Text {
            status: 200,
            content_type: "text/html",
            body: INDEX_HTML,
        },
        ("GET", "/static/index.html") => RouteResponse::Text {
            status: 200,
            content_type: "text/html",
            body: INDEX_HTML,
        },
        ("GET", "/static/app.js") => RouteResponse::Text {
            status: 200,
            content_type: "application/javascript",
            body: APP_JS,
        },
        ("GET", "/static/style.css") => RouteResponse::Text {
            status: 200,
            content_type: "text/css",
            body: STYLE_CSS,
        },
        ("GET", "/api/overview") => json_route(api::overview(home)),
        ("GET", "/api/providers") => json_route(api::providers(home)),
        ("POST", "/api/command") if !host_is_local(request) => json_route((
            403,
            serde_json::json!({ "ok": false, "error": "cross-origin command rejected" }),
        )),
        ("POST", "/api/command") => json_route(api::command(home, &request.body)),
        ("GET", "/api/mcp") => json_route(api::mcp_list(home)),
        ("GET", "/api/skills") => json_route(api::skills_list(home)),
        ("GET", "/api/stats") => json_route(api::stats(home)),
        ("GET", "/api/sessions") => json_route(api::sessions()),
        ("POST", "/api/web/stop") if !host_is_local(request) => json_route((
            403,
            serde_json::json!({ "ok": false, "error": "cross-origin request rejected" }),
        )),
        ("POST", "/api/web/stop") => {
            let (status, value) = api::web_stop(home);
            RouteResponse::StopWeb { status, value }
        }
        _ => RouteResponse::Json {
            status: 404,
            value: json!({ "ok": false, "error": "not found" }),
        },
    }
}

fn json_route((status, value): (u16, Value)) -> RouteResponse {
    RouteResponse::Json { status, value }
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

fn write_text(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<(), String> {
    let headers = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        status_text(status),
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .and_then(|_| stream.write_all(body))
        .map_err(|e| e.to_string())
}

fn write_json(stream: &mut TcpStream, (status, value): (u16, Value)) -> Result<(), String> {
    let body = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    let headers = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        status_text(status),
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .and_then(|_| stream.write_all(&body))
        .map_err(|e| e.to_string())
}

fn write_error(stream: &mut TcpStream, status: u16, message: &str) -> Result<(), String> {
    write_json(stream, (status, json!({ "ok": false, "error": message })))
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    fn client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap()
    }

    /// Bind an ephemeral port and serve in a thread; return port, stop flag,
    /// join handle and a channel that fires when the serve loop exits
    /// (mirrors the runtime test pattern of binding 127.0.0.1:0 and
    /// extracting the address).
    fn spawn_server() -> (
        u16,
        Arc<AtomicBool>,
        thread::JoinHandle<()>,
        mpsc::Receiver<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let flag = Arc::new(AtomicBool::new(false));
        let server_flag = Arc::clone(&flag);
        let (done_tx, done_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let _ = serve_listener(listener, server_flag);
            let _ = done_tx.send(());
        });
        (port, flag, handle, done_rx)
    }

    fn get(port: u16, path: &str) -> reqwest::blocking::Response {
        client()
            .get(format!("http://127.0.0.1:{port}{path}"))
            .send()
            .unwrap()
    }

    #[test]
    fn route_parsing() {
        let (port, flag, handle, done_rx) = spawn_server();

        let html = get(port, "/").text().unwrap();
        assert!(
            html.starts_with('<'),
            "index must start with '<', got: {html}"
        );

        let js = get(port, "/static/app.js").text().unwrap();
        assert!(!js.is_empty());

        let missing = get(port, "/static/nope.js").text().unwrap();
        let value: Value = serde_json::from_str(&missing).unwrap();
        assert_eq!(value["ok"], false);

        let api_nope = get(port, "/api/nope").text().unwrap();
        let value: Value = serde_json::from_str(&api_nope).unwrap();
        assert_eq!(value["ok"], false);

        flag.store(true, Ordering::SeqCst);
        assert!(done_rx.recv_timeout(Duration::from_secs(1)).is_ok());
        handle.join().unwrap();
    }

    #[test]
    fn frontend_panel_helper_only_sets_loading_state() {
        assert!(APP_JS.contains("const c = $(\"#content\");"));
        assert!(APP_JS.contains("c.innerHTML = '<div class=\"loading\">加载中…</div>';"));
        assert!(
            !APP_JS.contains("handlers[name] ? handlers[name]"),
            "panel must not recursively dispatch the renderer that called it"
        );
    }

    #[test]
    fn frontend_swiss_ledger_has_dual_layout_contract() {
        assert!(
            INDEX_HTML.contains("status-strip"),
            "index must carry the ledger status strip"
        );
        assert!(
            APP_JS.contains("className = \"ledger\""),
            "providers view must render a ledger table"
        );
        assert!(
            APP_JS.contains("dataset.label"),
            "table cells need data-label for the stacked mobile layout"
        );
        assert!(
            STYLE_CSS.contains("@media (max-width: 768px)"),
            "style must switch layouts at the 768px breakpoint"
        );
        assert!(
            STYLE_CSS.contains("--blue") && STYLE_CSS.contains("#0000ee"),
            "swiss palette keeps #0000ee as the single accent"
        );
    }

    #[test]
    fn frontend_entries_support_expand_and_per_target_toggles() {
        assert!(
            APP_JS.contains("entry-head"),
            "rows must be expandable entries"
        );
        assert!(
            APP_JS.contains("next ? \"enable\" : \"disable\"") && APP_JS.contains("\"--target\""),
            "toggles must drive the per-target CLI verbs"
        );
        assert!(
            APP_JS.contains("[\"mcp\",\"update\""),
            "web console must expose MCP field editing"
        );
        assert!(
            !APP_JS.contains("\"OC\"") && !APP_JS.contains("\"CL\"") && !APP_JS.contains("\"CX\""),
            "agent names must be spelled out, not abbreviated"
        );
    }

    #[test]
    fn api_overview_shape() {
        let (port, flag, handle, done_rx) = spawn_server();

        let response = get(port, "/api/overview");
        let value: Value = serde_json::from_str(&response.text().unwrap()).unwrap();
        assert_eq!(value["ok"], true);
        let agents = value["data"]["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 3);
        let names: Vec<&str> = agents
            .iter()
            .filter_map(|agent| agent["name"].as_str())
            .collect();
        assert_eq!(names, vec!["opencode", "claude", "codex"]);
        assert!(value["data"]["runtime"]["running"].is_boolean());

        flag.store(true, Ordering::SeqCst);
        assert!(done_rx.recv_timeout(Duration::from_secs(1)).is_ok());
        handle.join().unwrap();
    }

    #[test]
    fn api_command_passthrough() {
        let (port, flag, handle, done_rx) = spawn_server();

        let response = client()
            .post(format!("http://127.0.0.1:{port}/api/command"))
            .json(&json!({ "args": ["help"] }))
            .send()
            .unwrap();
        let value: Value = serde_json::from_str(&response.text().unwrap()).unwrap();
        assert_eq!(value["ok"], true);
        assert!(
            value["output"].as_str().unwrap().contains("spec"),
            "help output must mention spec"
        );

        let response = client()
            .post(format!("http://127.0.0.1:{port}/api/command"))
            .json(&json!({ "args": ["no-such-command"] }))
            .send()
            .unwrap();
        let value: Value = serde_json::from_str(&response.text().unwrap()).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"], "unknown command");

        let response = client()
            .post(format!("http://127.0.0.1:{port}/api/command"))
            .json(&json!({ "nope": true }))
            .send()
            .unwrap();
        let value: Value = serde_json::from_str(&response.text().unwrap()).unwrap();
        assert_eq!(value["ok"], false);

        flag.store(true, Ordering::SeqCst);
        assert!(done_rx.recv_timeout(Duration::from_secs(1)).is_ok());
        handle.join().unwrap();
    }

    #[test]
    fn api_stats_shape() {
        let (port, flag, handle, done_rx) = spawn_server();

        let response = get(port, "/api/stats");
        assert_eq!(response.status(), 200);
        let value: Value = serde_json::from_str(&response.text().unwrap()).unwrap();
        assert_eq!(value["ok"], true);
        let periods = value["data"]["periods"].as_object().unwrap();
        for key in ["24h", "48h", "7d", "30d"] {
            assert!(periods.contains_key(key), "missing period {key}");
        }

        flag.store(true, Ordering::SeqCst);
        assert!(done_rx.recv_timeout(Duration::from_secs(1)).is_ok());
        handle.join().unwrap();
    }

    #[test]
    fn api_web_stop_tui_mode() {
        let (port, _flag, handle, done_rx) = spawn_server();

        let response = client()
            .post(format!("http://127.0.0.1:{port}/api/web/stop"))
            .send()
            .unwrap();
        let value: Value = serde_json::from_str(&response.text().unwrap()).unwrap();
        assert_eq!(value["ok"], true);
        assert!(
            done_rx.recv_timeout(Duration::from_secs(1)).is_ok(),
            "serve loop must exit within ~1s after /api/web/stop"
        );
        handle.join().unwrap();
    }
}

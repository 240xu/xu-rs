use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use serde_json::{json, Value};

use crate::domain::CacheMode;
#[cfg(test)]
use crate::domain::ProtocolKind;
use crate::domain::ProviderProfile;
use crate::providers::read_profiles;
use bridge::{DefaultRectifier, RequestRectifier};

pub mod bridge;
#[cfg(test)]
pub mod http;
#[cfg(not(test))]
mod http;
use http::{
    drain_incoming, read_http_request, write_json, write_raw_headers, write_sse, write_sse_done,
    write_sse_frame, HttpRequest, MAX_BODY_BYTES,
};

const RUNTIME_HOP_HEADER: &str = "x-spec-runtime-hop";
const MAX_RUNTIME_HOPS: u8 = 1;
const RUNTIME_SOCKET_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_RECTIFY_ERROR_BODY_BYTES: usize = 8 * 1024;
/// Cap on relayed upstream error text (characters) so a hostile or verbose
/// upstream cannot flood the client envelope; the full text stays in logs.
const MAX_RELAYED_ERROR_TEXT_CHARS: usize = 512;

/// Upstream failure carrying optional error text relayed from the upstream
/// error body. `kind` still decides the HTTP status and `invalid_upstream`
/// code; `relay` (truncated) replaces the generic
/// `invalid upstream response` message so clients can see the real cause.
struct UpstreamFailure {
    kind: bridge::BridgeError,
    relay: Option<String>,
    /// 上游实际 HTTP 状态码（转换路径保留 429/4xx，供客户端退避/修正）。
    status: Option<u16>,
    /// Optional `error.type` value emitted in the client envelope; ordinary
    /// upstream failures leave it `None` so the envelope stays unchanged.
    error_type: Option<&'static str>,
}

impl From<bridge::BridgeError> for UpstreamFailure {
    fn from(kind: bridge::BridgeError) -> Self {
        Self {
            kind,
            relay: None,
            status: None,
            error_type: None,
        }
    }
}

/// Strips terminal-injection vectors from upstream-relayed text: C0 control
/// characters (except tab/newline/carriage return), C1 controls
/// (U+0080..U+009F) and ESC escape sequences (`\x1b[31m` and friends) so a
/// hostile upstream cannot inject ANSI sequences into the client's terminal.
fn sanitize_relay_text(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\t' | '\n' | '\r' => sanitized.push(ch),
            '\u{1b}' => {
                // Drop the ESC and a following CSI-style parameter run
                // (`[0-9;?]*` then a final letter); anything else after the
                // ESC resumes normal handling.
                while let Some(&next) = chars.peek() {
                    if next.is_ascii_digit() || matches!(next, '[' | ';' | '?') {
                        chars.next();
                    } else if next.is_ascii_alphabetic() {
                        chars.next();
                        break;
                    } else {
                        break;
                    }
                }
            }
            ch if (ch as u32) < 0x20 => {}
            ch if (0x80..=0x9f).contains(&(ch as u32)) => {}
            ch => sanitized.push(ch),
        }
    }
    sanitized
}

/// Extracts the client-safe relay message from an upstream JSON error body:
/// an `error.message` / top-level `message` string, sanitized of control
/// characters and truncated to MAX_RELAYED_ERROR_TEXT_CHARS. Anything else
/// returns None and the caller falls back to the generic envelope message.
fn relay_from_error_json(value: &Value) -> Option<String> {
    let message = value
        .get("error")
        .and_then(|error| error.get("message"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)?;
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(
        sanitize_relay_text(trimmed)
            .chars()
            .take(MAX_RELAYED_ERROR_TEXT_CHARS)
            .collect(),
    )
}

fn relay_from_error_body(body: &[u8]) -> Option<String> {
    relay_from_error_json(&serde_json::from_slice(body).ok()?)
}

/// Builds an `invalid_upstream` failure from a non-success upstream response,
/// reading the error body bounded to MAX_RECTIFY_ERROR_BODY_BYTES.
fn upstream_failure_from_response(response: &mut reqwest::blocking::Response) -> UpstreamFailure {
    let mut body = Vec::new();
    response
        .take(MAX_RECTIFY_ERROR_BODY_BYTES as u64)
        .read_to_end(&mut body)
        .ok();
    UpstreamFailure {
        kind: bridge::BridgeError::InvalidUpstream,
        relay: relay_from_error_body(&body),
        status: Some(response.status().as_u16()),
        error_type: None,
    }
}

pub(super) const MAX_CONCURRENT_CONNECTIONS: usize = 8;

struct ConnectionSlots {
    state: Mutex<SlotState>,
    available: Condvar,
}

struct SlotState {
    used: usize,
    capacity: usize,
}

impl ConnectionSlots {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(SlotState { used: 0, capacity }),
            available: Condvar::new(),
        }
    }

    /// 非阻塞获取；槽满返回 None（accept 层立即 503，防止无界线程堆积）。
    fn try_acquire(self: &Arc<Self>) -> Option<OwnedSlotGuard> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.used >= state.capacity {
            return None;
        }
        state.used += 1;
        Some(OwnedSlotGuard {
            slots: Arc::clone(self),
        })
    }

    /// 阻塞获取（测试专用语义）：生产 accept 路径已改用 try_acquire 有界排队。
    #[allow(dead_code)]
    fn acquire(&self) -> SlotGuard<'_> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while state.used >= state.capacity {
            state = self
                .available
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        state.used += 1;
        SlotGuard { slots: self }
    }

    fn release(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.used = state.used.saturating_sub(1);
        self.available.notify_one();
    }
}

/// 线程安全持有版槽位守卫：accept 循环获取、随 worker 线程 move，
/// 保证槽位在连接整个生命周期内被占用。
struct OwnedSlotGuard {
    slots: Arc<ConnectionSlots>,
}

impl Drop for OwnedSlotGuard {
    fn drop(&mut self) {
        self.slots.release();
    }
}

struct SlotGuard<'a> {
    slots: &'a ConnectionSlots,
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        self.slots.release();
    }
}

struct RuntimeStats {
    started_at: Instant,
    active_connections: AtomicUsize,
    pending_connections: AtomicUsize,
    success_count: AtomicU64,
    failure_count: AtomicU64,
}

impl RuntimeStats {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            active_connections: AtomicUsize::new(0),
            pending_connections: AtomicUsize::new(0),
            success_count: AtomicU64::new(0),
            failure_count: AtomicU64::new(0),
        }
    }
}

struct ConnectionCountGuard {
    stats: Arc<RuntimeStats>,
    active: bool,
}

impl ConnectionCountGuard {
    fn new(stats: Arc<RuntimeStats>) -> Self {
        stats.pending_connections.fetch_add(1, Ordering::SeqCst);
        Self {
            stats,
            active: false,
        }
    }

    fn mark_active(&mut self) {
        self.stats.active_connections.fetch_add(1, Ordering::SeqCst);
        self.active = true;
    }
}

impl Drop for ConnectionCountGuard {
    fn drop(&mut self) {
        if self.active {
            self.stats.active_connections.fetch_sub(1, Ordering::SeqCst);
        }
        self.stats
            .pending_connections
            .fetch_sub(1, Ordering::SeqCst);
    }
}

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn shutdown_signal_handler(_signal: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

fn install_shutdown_handlers() {
    let handler = shutdown_signal_handler as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
}

pub fn listen_addr() -> String {
    format!("127.0.0.1:{}", crate::agents::serve_port())
}

pub fn is_running() -> bool {
    health_check().is_ok()
}

pub fn pid_path(home: &Path) -> PathBuf {
    home.join(".codex/spec-runtime.pid")
}

pub fn log_path(home: &Path) -> PathBuf {
    home.join(".codex/spec-runtime.log")
}

pub fn daemon_status(home: &Path) -> String {
    let pid = std::fs::read_to_string(pid_path(home))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "not recorded".to_string());
    format!(
        "runtime: {}\nlisten: {}\npid: {}\npid_file: {}\nlog_file: {}",
        if is_running() {
            "running"
        } else {
            "not running"
        },
        listen_addr(),
        pid,
        pid_path(home).display(),
        log_path(home).display(),
    )
}

pub fn start_daemon(home: &Path) -> Result<String, String> {
    if is_running() {
        return Ok(format!("runtime already running\n{}", daemon_status(home)));
    }
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir)
        .map_err(|e| format!("create {}: {e}", codex_dir.display()))?;
    let log_path = log_path(home);
    // 日志轮转：超过 10MB 时旧日志改名 .1（保留一份），防无限增长。
    if std::fs::metadata(&log_path).map_or(0, |m| m.len()) > 10 * 1024 * 1024 {
        let rotated = log_path.with_extension("log.1");
        let _ = std::fs::remove_file(&rotated);
        let _ = std::fs::rename(&log_path, &rotated);
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("open {}: {e}", log_path.display()))?;
    let _ = std::fs::set_permissions(&log_path, std::fs::Permissions::from_mode(0o600));
    let stderr = log
        .try_clone()
        .map_err(|e| format!("clone {}: {e}", log_path.display()))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .arg("serve")
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|e| format!("start spec serve: {e}"))?;
    let pid = child.id();
    let pid_path = pid_path(home);
    std::fs::write(&pid_path, format!("{pid}\n"))
        .map_err(|e| format!("write {}: {e}", pid_path.display()))?;
    let _ = std::fs::set_permissions(&pid_path, std::fs::Permissions::from_mode(0o600));

    std::thread::sleep(Duration::from_millis(200));
    if health_check().is_ok() {
        return Ok(format!("runtime started\n{}", daemon_status(home)));
    }
    if let Ok(Some(status)) = child.try_wait() {
        let _ = std::fs::remove_file(&pid_path);
        return Err(format!(
            "runtime exited early with {status}; see {}",
            log_path.display()
        ));
    }
    Ok(format!(
        "runtime started but health is not ready yet\n{}",
        daemon_status(home)
    ))
}

pub fn stop_daemon(home: &Path) -> Result<String, String> {
    let pid_path = pid_path(home);
    let pid = std::fs::read_to_string(&pid_path)
        .map_err(|e| format!("read {}: {e}", pid_path.display()))?;
    let pid = pid.trim();
    if pid.is_empty() || !pid.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("invalid runtime pid file: {}", pid_path.display()));
    }
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(pid)
        .status()
        .map_err(|e| format!("kill {pid}: {e}"))?;
    if !status.success() {
        return Err(format!("kill {pid} failed with {status}"));
    }
    let _ = std::fs::remove_file(&pid_path);
    Ok(format!("runtime stop requested\npid: {pid}"))
}

pub fn health_check() -> Result<Value, String> {
    let listen = listen_addr();
    let Ok(addr) = listen.parse::<SocketAddr>() else {
        return Err("invalid runtime listen addr".to_string());
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(200))
        .map_err(|e| format!("connect {listen}: {e}"))?;
    let value: Value = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(500))
        .build()
        .map_err(|e| e.to_string())?
        .get(format!("http://{listen}/health"))
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    if value.get("service").and_then(Value::as_str) == Some("spec-runtime") {
        Ok(value)
    } else {
        Err("listener is not spec-runtime".to_string())
    }
}

pub fn serve(home: &Path) -> Result<(), String> {
    let profiles = read_profiles(&home.join(".codex/xu-chat-providers.json"))?;
    if profiles.is_empty() {
        return Err("no providers in ~/.codex/xu-chat-providers.json".to_string());
    }
    let home = home.to_path_buf();

    let listen = listen_addr();
    let listener = TcpListener::bind(&listen).map_err(|e| format!("bind {listen}: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("configure {listen}: {e}"))?;
    eprintln!(
        "spec local adapter listening on http://{listen} · providers: {}",
        profiles.len()
    );
    install_shutdown_handlers();
    SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
    let stats = Arc::new(RuntimeStats::new());
    let slots = Arc::new(ConnectionSlots::new(MAX_CONCURRENT_CONNECTIONS));
    loop {
        if SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let home = home.clone();
                let profiles = profiles.clone();
                let slots = Arc::clone(&slots);
                let stats = Arc::clone(&stats);
                let Some(_slot_guard) = slots.try_acquire() else {
                    let _ = write_json(
                        &mut stream,
                        503,
                        json!({ "error": { "type": "overloaded", "message": "server busy" } }),
                    );
                    // 排空未读请求字节再关：否则 drop 触发 RST，客户端收不到 503。
                    drain_incoming(&mut stream, Duration::from_millis(30));
                    continue;
                };
                std::thread::spawn(move || {
                    // guard 必须随线程持有（OwnedSlotGuard）：留在 accept 作用域
                    // 会提前 release，并发上限形同虚设（P0 教训）。
                    let _slot_guard = _slot_guard;
                    let mut count_guard = ConnectionCountGuard::new(Arc::clone(&stats));
                    count_guard.mark_active();
                    let result = handle_connection(&mut stream, &home, &profiles, &stats);
                    match result {
                        Ok(()) => {
                            stats.success_count.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(error) => {
                            stats.failure_count.fetch_add(1, Ordering::SeqCst);
                            let _ = write_json(&mut stream, 500, json!({ "error": error }));
                        }
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(error) => {
                eprintln!("accept error: {error}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    eprintln!(
        "runtime terminating: draining {} pending connection(s)",
        stats.pending_connections.load(Ordering::SeqCst)
    );
    wait_for_drain(&stats);
    eprintln!("runtime stopped");
    Ok(())
}

fn wait_for_drain(stats: &RuntimeStats) {
    while stats.pending_connections.load(Ordering::SeqCst) > 0 {
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn handle_connection(
    stream: &mut TcpStream,
    home: &Path,
    providers: &[ProviderProfile],
    stats: &RuntimeStats,
) -> Result<(), String> {
    handle_connection_with_timeout(stream, home, providers, RUNTIME_SOCKET_TIMEOUT, stats)
}

fn handle_connection_with_timeout(
    stream: &mut TcpStream,
    home: &Path,
    providers: &[ProviderProfile],
    timeout: Duration,
    stats: &RuntimeStats,
) -> Result<(), String> {
    if let Err(error) = stream.set_read_timeout(Some(timeout)) {
        let _ = write_bridge_error(stream, bridge::BridgeError::Internal);
        return Err(format!("set read timeout: {error}"));
    }
    if let Err(error) = stream.set_write_timeout(Some(timeout)) {
        let _ = write_bridge_error(stream, bridge::BridgeError::Internal);
        return Err(format!("set write timeout: {error}"));
    }
    let request = match read_http_request(stream) {
        Ok(request) => request,
        Err(error) if is_socket_read_timeout(&error) => {
            write_bridge_error(stream, bridge::BridgeError::Timeout)?;
            // 504 信封已写出：返回 Ok 防止外层再补写第二个响应（双响应 P1）。
            return Ok(());
        }
        Err(error) => {
            let error = if error.contains("too large") {
                bridge::BridgeError::ResourceLimit
            } else {
                bridge::BridgeError::InvalidRequest
            };
            return write_bridge_error(stream, error);
        }
    };
    handle_request(stream, home, &request, providers, stats)
}

fn is_socket_read_timeout(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "timed out",
        "would block",
        "try again",
        "temporarily unavailable",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

fn path_matches_count_tokens(path: &str) -> bool {
    path == "/v1/messages/count_tokens"
}

fn path_matches_stats_tokens(path: &str) -> bool {
    path == "/api/stats/tokens"
}

/// The `period` query parameter when it names one of the known windows;
/// None for absent or unknown values (caller then returns every period).
fn stats_period_filter(query: &BTreeMap<String, String>) -> Option<String> {
    let period = query.get("period")?;
    match period.as_str() {
        crate::stats::PERIOD_24H
        | crate::stats::PERIOD_48H
        | crate::stats::PERIOD_7D
        | crate::stats::PERIOD_30D => Some(period.clone()),
        _ => None,
    }
}

/// Local-only token statistics endpoint: `GET /api/stats/tokens[?period=…]`
/// returning `{"periods": {"24h": {...}, …}}`. Same access level as /health
/// (bound to 127.0.0.1 by the runtime listener).
fn handle_stats_tokens(
    stream: &mut TcpStream,
    home: &Path,
    query: &BTreeMap<String, String>,
) -> Result<(), String> {
    let filter = stats_period_filter(query);
    let periods: Value = crate::stats::aggregate(home)
        .into_iter()
        .filter(|(key, _)| match filter.as_deref() {
            Some(period) => key == period,
            None => true,
        })
        .map(|(key, stat)| (key, serde_json::to_value(stat).unwrap_or(Value::Null)))
        .collect();
    write_json(stream, 200, json!({ "periods": periods }))
}

fn path_models_retrieve(path: &str) -> Option<String> {
    let id = path.strip_prefix("/v1/models/")?;
    if id.is_empty() || id.contains('/') {
        return None;
    }
    Some(id.to_string())
}

fn models_created_at() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn model_entry(provider: &ProviderProfile, model: &str) -> Value {
    json!({
        "id": provider.model_slug(model),
        "type": "model",
        "display_name": format!("{} · {}", model, provider.id),
        "description": format!("mapped from {} ({})", model, provider.id),
        "created_at": models_created_at(),
        "object": "model",
        "owned_by": provider.id,
    })
}

fn models_find(providers: &[ProviderProfile], id: &str) -> Option<Value> {
    providers
        .iter()
        .flat_map(|provider| provider.models.iter().map(move |model| (provider, model)))
        .find(|(provider, model)| provider.model_slug(model) == id)
        .map(|(provider, model)| model_entry(provider, model))
}

fn models_list_response(providers: &[ProviderProfile]) -> Value {
    json!({
        "object": "list",
        "data": providers.iter().flat_map(|provider| {
            provider.models.iter().map(move |model| model_entry(provider, model))
        }).collect::<Vec<_>>()
    })
}

fn handle_count_tokens(stream: &mut TcpStream, request: &HttpRequest) -> Result<(), String> {
    let input: Value = match serde_json::from_slice(&request.body) {
        Ok(value) => value,
        Err(_) => return write_bridge_error(stream, bridge::BridgeError::InvalidRequest),
    };
    let messages = match input.get("messages").and_then(Value::as_array) {
        Some(messages) => messages,
        None => return write_bridge_error(stream, bridge::BridgeError::InvalidRequest),
    };
    let text_chars: usize = messages
        .iter()
        .map(|message| match message.get("content") {
            Some(Value::String(text)) => text.chars().count(),
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .map(|text| text.chars().count())
                .sum(),
            _ => 0,
        })
        .sum();
    let input_tokens = text_chars.div_ceil(4);
    write_json(stream, 200, json!({ "input_tokens": input_tokens }))
}

fn handle_request(
    stream: &mut TcpStream,
    home: &Path,
    request: &HttpRequest,
    providers: &[ProviderProfile],
    stats: &RuntimeStats,
) -> Result<(), String> {
    if request.hop >= MAX_RUNTIME_HOPS {
        return write_bridge_error(stream, bridge::BridgeError::InvalidRequest);
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("HEAD", "/api/hello") => write_json(stream, 200, json!({ "ok": true })),
        ("GET", "/health") | ("GET", "/v1/health") => {
            write_json(stream, 200, health_response(providers, Some(stats)))
        }
        ("GET", "/v1/models") => write_json(stream, 200, models_list_response(providers)),
        ("GET", path) if path_matches_stats_tokens(path) => {
            handle_stats_tokens(stream, home, &request.query)
        }
        ("GET", path) => match path_models_retrieve(path) {
            Some(id) => match models_find(providers, &id) {
                Some(entry) => write_json(stream, 200, entry),
                None => write_bridge_error(stream, bridge::BridgeError::InvalidRequest),
            },
            None => write_bridge_error(stream, bridge::BridgeError::InvalidRequest),
        },
        ("POST", path) if path_matches_count_tokens(path) => handle_count_tokens(stream, request),
        ("POST", path) => match bridge::protocol_for_path(path) {
            Some(protocol) => handle_protocol_request(stream, home, request, protocol, providers),
            None => write_bridge_error(stream, bridge::BridgeError::InvalidRequest),
        },
        _ => write_bridge_error(stream, bridge::BridgeError::InvalidRequest),
    }
}

#[cfg(test)]
pub fn http_request_for_test(method: &str, target: &str, body: Vec<u8>, hop: u8) -> HttpRequest {
    let (path, query) = http::split_request_target(target)
        .unwrap_or_else(|_| (target.to_string(), std::collections::BTreeMap::new()));
    HttpRequest {
        method: method.to_string(),
        path,
        query,
        body,
        hop,
    }
}

#[cfg(test)]
pub type TestHttpResponse = (u16, Vec<(String, String)>, Vec<u8>);

#[cfg(test)]
pub fn handle_request_for_test(
    request: HttpRequest,
    home: &Path,
    providers: &[ProviderProfile],
) -> Result<TestHttpResponse, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let mut client = TcpStream::connect(address).map_err(|e| e.to_string())?;
    let (mut server, _) = listener.accept().map_err(|e| e.to_string())?;

    let result = handle_request(&mut server, home, &request, providers, &RuntimeStats::new());
    let _ = server.shutdown(std::net::Shutdown::Write);
    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .map_err(|e| e.to_string())?;
    result?;

    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "response missing header terminator".to_string())?;
    let header_text = String::from_utf8(response[..header_end].to_vec())
        .map_err(|_| "response headers are not UTF-8".to_string())?;
    let mut lines = header_text.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| "response missing status".to_string())?
        .parse::<u16>()
        .map_err(|_| "response status is invalid".to_string())?;
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_string(), value.trim().to_string()))
        .collect();
    Ok((status, headers, response[header_end + 4..].to_vec()))
}

fn write_bridge_error(stream: &mut TcpStream, error: bridge::BridgeError) -> Result<(), String> {
    write_json(
        stream,
        error.http_status(),
        serde_json::to_value(error.public_envelope()).map_err(|_| "internal bridge error")?,
    )
}

/// Writes an upstream failure envelope: status/code from `failure.kind`, but
/// with the generic `invalid upstream response` message replaced by the
/// relayed upstream error text when one was extracted. An explicit
/// `failure.error_type` is emitted as `error.type` only when present.
fn write_upstream_error(stream: &mut TcpStream, failure: &UpstreamFailure) -> Result<(), String> {
    let message = failure
        .relay
        .clone()
        .unwrap_or_else(|| failure.kind.public_message().to_string());
    let mut error = json!({
        "code": failure.kind.log_category(),
        "message": message,
    });
    if let Some(error_type) = failure.error_type {
        error["type"] = json!(error_type);
    }
    write_json(
        stream,
        failure.status.unwrap_or_else(|| failure.kind.http_status()),
        json!({ "error": error }),
    )
}

fn handle_protocol_request(
    stream: &mut TcpStream,
    home: &Path,
    request: &HttpRequest,
    source: bridge::WireProtocol,
    providers: &[ProviderProfile],
) -> Result<(), String> {
    let started = Instant::now();
    let result = process_protocol_request(stream, home, request, source, providers);
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            // 把拒绝原因（含 Unsupported 的具体字段）打到 stderr，便于
            // 排查兼容层拒绝（如 Claude Code 压缩请求的未知字段）。
            if let bridge::BridgeError::Unsupported { field } = &error.kind {
                eprintln!("runtime bridge rejected: {field} (path={})", request.path);
            }
            // Failures are recorded too (status/error, zero tokens) so the
            // request count and success rate stay truthful; the write is
            // best-effort and never blocks the error relay.
            let _ = crate::stats::record_usage(
                home,
                &failure_usage_record(request, source, providers, started, &error),
            );
            write_upstream_error(stream, &error)
        }
    }
}

fn process_protocol_request(
    stream: &mut TcpStream,
    home: &Path,
    request: &HttpRequest,
    source: bridge::WireProtocol,
    providers: &[ProviderProfile],
) -> Result<(), UpstreamFailure> {
    let started = Instant::now();
    let input: Value =
        serde_json::from_slice(&request.body).map_err(|_| bridge::BridgeError::InvalidRequest)?;
    let model_name = input
        .get("model")
        .and_then(Value::as_str)
        .ok_or(bridge::BridgeError::InvalidRequest)?;
    let (provider, upstream_model) = resolve_model(providers, model_name).map_err(|error| {
        // Fail closed BEFORE `is_runtime_upstream()` / `build_upstream_request()`
        // / any credential-bearing upstream request: the slug is echoed back
        // sanitized, never the provider key or authorization.
        let message = match &error {
            ModelResolutionError::NotConfigured(slug) => {
                format!("model not configured: {slug}")
            }
            ModelResolutionError::Ambiguous(slug) => {
                format!("model provider is ambiguous: {slug}")
            }
            ModelResolutionError::InvalidSuffix(slug) => {
                format!("model provider suffix is invalid: {slug}")
            }
        };
        UpstreamFailure {
            kind: bridge::BridgeError::InvalidRequest,
            relay: Some(sanitize_relay_text(&message)),
            status: Some(400),
            error_type: Some("invalid_request_error"),
        }
    })?;
    if is_runtime_upstream(&provider.base_url) {
        return Err(bridge::BridgeError::InvalidRequest.into());
    }
    let target = bridge::WireProtocol::from(provider.protocol);
    if source == target {
        // Same wire protocol on both sides: forward without an IR round-trip so
        // fields the bridge cannot represent (opaque reasoning, item ids,
        // provider extensions) pass through unchanged.
        return passthrough_protocol_request(
            stream,
            home,
            &request.path,
            provider,
            target,
            &input,
            &upstream_model,
        );
    }
    let mut ir = bridge::parse_request(source, &input)?;
    // DeepSeek 缓存优化：显式 DeepSeek 模式，或 Auto 下按 vendor/模型名自动识别。
    let effective_cache_mode = provider
        .cache_mode
        .effective(provider.vendor.clone(), &upstream_model);
    if effective_cache_mode == CacheMode::DeepSeek && target == bridge::WireProtocol::OpenAiChat {
        ir.extensions.insert(
            bridge::EXT_PROVIDER_CACHE_MODE.to_string(),
            json!("deepseek"),
        );
    }
    let mut body = bridge::encode_upstream_request(&ir, target, &upstream_model)?;
    normalize_provider_request(provider, target, &mut body);

    let rectifier = DefaultRectifier;
    if ir.stream {
        return stream_protocol_request(
            stream,
            home,
            &request.path,
            provider,
            source,
            target,
            body,
            &rectifier,
        );
    }

    let response = send_upstream_request(provider, target, body, &rectifier)?;
    let response_body = read_upstream_json(response)?;
    // A 2xx body carrying an error object is a semantic failure: relay its
    // error text the same way non-2xx statuses relay theirs.
    let response_ir = bridge::decode_response(target, response_body.clone()).map_err(|error| {
        UpstreamFailure {
            kind: map_upstream_bridge_error(error),
            relay: relay_from_error_json(&response_body),
            status: None,
            error_type: None,
        }
    })?;
    let usage = response_ir.usage.clone();
    let output = bridge::encode_response(response_ir, source).map_err(map_upstream_bridge_error)?;
    write_json(stream, 200, output)
        .map_err(|_| UpstreamFailure::from(bridge::BridgeError::Internal))?;
    let _ = crate::stats::record_usage(
        home,
        &usage_record(
            chrono::Utc::now().timestamp(),
            &request.path,
            &upstream_model,
            usage.as_ref(),
            crate::stats::SOURCE_CONVERTED,
            started,
        ),
    );
    Ok(())
}

fn normalize_provider_request(
    provider: &ProviderProfile,
    target: bridge::WireProtocol,
    body: &mut Value,
) {
    if provider.id != "zen" || target != bridge::WireProtocol::OpenAiChat {
        return;
    }

    // OpenCode Zen's Chat endpoint accepts system/user/assistant/tool, but not
    // the developer role supported by the OpenAI Chat wire format.
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if message.get("role").and_then(Value::as_str) == Some("developer") {
                message["role"] = Value::String("system".to_string());
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stream_protocol_request(
    stream: &mut TcpStream,
    home: &Path,
    path: &str,
    provider: &ProviderProfile,
    source: bridge::WireProtocol,
    target: bridge::WireProtocol,
    body: Value,
    rectifier: &dyn RequestRectifier,
) -> Result<(), UpstreamFailure> {
    let started = Instant::now();
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut response = send_upstream_request(provider, target, body, rectifier)?;
    let mut decoder = bridge::SseDecoder::default();
    let mut upstream_state = bridge::StreamState::new();
    let mut output_state = bridge::StreamState::new();
    let mut headers_written = false;
    let mut downstream_write_failed = false;
    let mut buffer = [0u8; 8192];

    let process = (|| -> Result<(), UpstreamFailure> {
        if !response.status().is_success() {
            return Err(upstream_failure_from_response(&mut response));
        }
        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|error| map_reqwest_io_error(&error))?;
            if read == 0 {
                break;
            }
            let frames = decoder
                .feed(&buffer[..read])
                .map_err(map_upstream_bridge_error)?;
            for frame in frames {
                let events = bridge::decode_stream_frame(target, &frame, &mut upstream_state)
                    .map_err(map_upstream_bridge_error)?;
                let output_frames =
                    bridge::encode_stream_events(source, &events, &mut output_state)
                        .map_err(map_upstream_bridge_error)?;
                if !output_frames.is_empty() && !headers_written {
                    write_raw_headers(stream, 200, "OK", "text/event-stream")
                        .map_err(|_| bridge::BridgeError::Internal)?;
                    headers_written = true;
                }
                for output in output_frames {
                    write_sse_frame(stream, output.event.as_deref(), &output.data).map_err(
                        |_| {
                            downstream_write_failed = true;
                            bridge::BridgeError::Internal
                        },
                    )?;
                }
            }
        }
        decoder.finish().map_err(map_upstream_bridge_error)?;
        upstream_state
            .finish_eof()
            .map_err(map_upstream_bridge_error)?;
        if !headers_written {
            return Err(UpstreamFailure::from(bridge::BridgeError::InvalidUpstream));
        }
        Ok(())
    })();

    let result = match process {
        Ok(()) => Ok(()),
        Err(error) if headers_written && !downstream_write_failed => {
            if output_state.is_terminal() {
                eprintln!(
                    "runtime bridge error after terminal event: {}",
                    error.kind.log_category()
                );
                Ok(())
            } else {
                let failure = bridge::StreamEventIr::Failed(error.kind.clone());
                let frames = bridge::encode_stream_events(source, &[failure], &mut output_state)
                    .map_err(|_| bridge::BridgeError::Internal)?;
                for frame in frames {
                    write_sse_frame(stream, frame.event.as_deref(), &frame.data)
                        .map_err(|_| bridge::BridgeError::Internal)?;
                }
                Ok(())
            }
        }
        Err(error) => Err(error),
    };
    if result.is_ok() {
        // Success records carry the merged usage; failures are recorded by
        // the caller (handle_protocol_request) with status/error and zero
        // tokens, so the request count still lands exactly once.
        let _ = crate::stats::record_usage(
            home,
            &usage_record(
                chrono::Utc::now().timestamp(),
                path,
                &model,
                upstream_state.usage(),
                crate::stats::SOURCE_CONVERTED_STREAM,
                started,
            ),
        );
    }
    result
}

fn send_upstream_request(
    provider: &ProviderProfile,
    protocol: bridge::WireProtocol,
    body: Value,
    rectifier: &dyn RequestRectifier,
) -> Result<reqwest::blocking::Response, UpstreamFailure> {
    let mut last_error = UpstreamFailure::from(bridge::BridgeError::InvalidUpstream);
    for attempt in 0..=provider.max_retries.min(3) {
        let request = build_upstream_request(provider, protocol, &body)?;

        match request.send() {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let status = response.status();
                let retryable = status.as_u16() == 429 || status.is_server_error();
                // Read the (bounded) error body once: it drives both the
                // thinking-rejection rectification check and the relayed
                // client error text.
                let mut error_body = Vec::new();
                response
                    .take(MAX_RECTIFY_ERROR_BODY_BYTES as u64)
                    .read_to_end(&mut error_body)
                    .ok();
                let relay = relay_from_error_body(&error_body);
                if !retryable {
                    if bridge::thinking_rejection(&String::from_utf8_lossy(&error_body)) {
                        if let Some(retried) =
                            try_rectified_retry(provider, protocol, &body, rectifier)
                        {
                            return retried;
                        }
                    }
                    return Err(UpstreamFailure {
                        kind: bridge::BridgeError::InvalidUpstream,
                        relay,
                        status: Some(status.as_u16()),
                        error_type: None,
                    });
                }
                last_error = UpstreamFailure {
                    kind: bridge::BridgeError::InvalidUpstream,
                    relay,
                    status: Some(status.as_u16()),
                    error_type: None,
                };
            }
            Err(error) => {
                last_error = if error.is_timeout() {
                    UpstreamFailure::from(bridge::BridgeError::Timeout)
                } else {
                    UpstreamFailure::from(bridge::BridgeError::InvalidUpstream)
                };
            }
        }
        if attempt < provider.max_retries.min(3) {
            std::thread::sleep(Duration::from_millis(100 * (attempt as u64 + 1)));
        }
    }
    Err(last_error)
}

/// Attempts exactly one rectified re-send after a `thinking`-related
/// rejection (the rejection body was already consumed by the caller).
/// Returns `Some` only when a re-send was actually attempted; the outcome of
/// that single re-send is surfaced unchanged (success, or the failure with
/// its own upstream error text relayed).
fn try_rectified_retry(
    provider: &ProviderProfile,
    protocol: bridge::WireProtocol,
    body: &Value,
    rectifier: &dyn RequestRectifier,
) -> Option<Result<reqwest::blocking::Response, UpstreamFailure>> {
    let rewritten = rectifier.rectify(protocol, body, &bridge::BridgeError::InvalidUpstream)?;
    let request = build_upstream_request(provider, protocol, &rewritten).ok()?;
    Some(match request.send() {
        Ok(response) if response.status().is_success() => Ok(response),
        Ok(mut response) => Err(upstream_failure_from_response(&mut response)),
        Err(_) => Err(UpstreamFailure::from(bridge::BridgeError::InvalidUpstream)),
    })
}

/// Rejects malformed or empty base URLs at the local boundary before any
/// upstream connection is attempted. Only http/https schemes with a host are
/// accepted; everything else is a local 400 (`BridgeError::InvalidRequest`)
/// whose public body never contains the URL or any credential.
fn validate_upstream_base_url(base_url: &str) -> Result<(), bridge::BridgeError> {
    let url =
        reqwest::Url::parse(base_url.trim()).map_err(|_| bridge::BridgeError::InvalidRequest)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(bridge::BridgeError::InvalidRequest);
    }
    Ok(())
}

fn build_upstream_request(
    provider: &ProviderProfile,
    protocol: bridge::WireProtocol,
    body: &Value,
) -> Result<reqwest::blocking::RequestBuilder, bridge::BridgeError> {
    validate_upstream_base_url(&provider.base_url)?;
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(provider.timeout_ms.max(1)))
        .build()
        .map_err(|_| bridge::BridgeError::Internal)?;
    let mut request = client
        .post(join_url(
            &provider.base_url,
            bridge::upstream_path(protocol),
        ))
        .json(body);
    request = match protocol {
        bridge::WireProtocol::AnthropicMessages if !provider.api_key.trim().is_empty() => request
            .header("x-api-key", &provider.api_key)
            .header("anthropic-version", "2023-06-01"),
        bridge::WireProtocol::AnthropicMessages => {
            request.header("anthropic-version", "2023-06-01")
        }
        bridge::WireProtocol::OpenAiChat | bridge::WireProtocol::OpenAiResponses
            if !provider.api_key.trim().is_empty() =>
        {
            request.bearer_auth(&provider.api_key)
        }
        bridge::WireProtocol::OpenAiChat | bridge::WireProtocol::OpenAiResponses => request,
    };
    for (key, value) in &provider.extra_headers {
        request = request.header(key, value);
    }
    request = request.header(RUNTIME_HOP_HEADER, MAX_RUNTIME_HOPS.to_string());
    Ok(request)
}

/// Same-protocol fast path: forward the request body (only the model slug is
/// rewritten and provider normalization/auth/hop logic still apply) and relay
/// the upstream response bytes untouched, including streaming and error
/// responses, so a native client never sees an IR round-trip.
fn passthrough_protocol_request(
    stream: &mut TcpStream,
    home: &Path,
    path: &str,
    provider: &ProviderProfile,
    protocol: bridge::WireProtocol,
    input: &Value,
    upstream_model: &str,
) -> Result<(), UpstreamFailure> {
    let started = Instant::now();
    let mut body = input.clone();
    if let Some(object) = body.as_object_mut() {
        object.insert("model".to_string(), json!(upstream_model));
    }
    normalize_provider_request(provider, protocol, &mut body);

    let streaming = input
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut response = passthrough_send_upstream(provider, protocol, body)?;
    let status = response.status();

    if streaming && status.is_success() {
        write_raw_headers(stream, 200, "OK", "text/event-stream")
            .map_err(|_| bridge::BridgeError::Internal)?;
        let usage = forward_passthrough_stream(&mut response, stream)?;
        if let Some(usage) = usage {
            let (input, output, cached, cache_creation) =
                crate::stats::usage_from_json(&usage).unwrap_or((0, 0, 0, 0));
            let _ = crate::stats::record_usage(
                home,
                &crate::stats::UsageRecord {
                    ts: chrono::Utc::now().timestamp(),
                    path: path.to_string(),
                    model: upstream_model.to_string(),
                    input,
                    output,
                    cached,
                    cache_creation,
                    semantics: crate::stats::semantics_for_usage(&usage),
                    latency_ms: Some(started.elapsed().as_millis() as u64),
                    status_code: Some(200),
                    is_streaming: true,
                    error: None,
                    source: crate::stats::SOURCE_PASSTHROUGH_STREAM,
                },
            );
        }
        return Ok(());
    }

    let bytes = read_limited_response(&mut response)?;
    let status_text = status.canonical_reason().unwrap_or("OK").to_owned();
    write_raw_headers(stream, status.as_u16(), &status_text, "application/json")
        .map_err(|_| bridge::BridgeError::Internal)?;
    stream
        .write_all(&bytes)
        .map_err(|_| bridge::BridgeError::Internal)?;
    if status.is_success() {
        let response_json = serde_json::from_slice::<Value>(&bytes).ok();
        let usage = response_json
            .as_ref()
            .and_then(crate::stats::usage_from_frame)
            .unwrap_or((0, 0, 0, 0));
        let semantics = response_json
            .as_ref()
            .and_then(|value| value.get("usage"))
            .map(crate::stats::semantics_for_usage)
            .unwrap_or(crate::stats::SEMANTICS_FRESH);
        let _ = crate::stats::record_usage(
            home,
            &crate::stats::UsageRecord {
                ts: chrono::Utc::now().timestamp(),
                path: path.to_string(),
                model: upstream_model.to_string(),
                input: usage.0,
                output: usage.1,
                cached: usage.2,
                cache_creation: usage.3,
                semantics,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                status_code: Some(status.as_u16()),
                is_streaming: false,
                error: None,
                source: crate::stats::SOURCE_PASSTHROUGH,
            },
        );
    } else {
        // Non-2xx passthrough responses are relayed verbatim, but still
        // recorded as failures (status/error, zero tokens) so the request
        // count and success rate stay truthful.
        let error = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| relay_from_error_json(&value))
            .unwrap_or_else(|| status.canonical_reason().unwrap_or("error").to_string());
        let _ = crate::stats::record_usage(
            home,
            &crate::stats::UsageRecord {
                ts: chrono::Utc::now().timestamp(),
                path: path.to_string(),
                model: upstream_model.to_string(),
                input: 0,
                output: 0,
                cached: 0,
                cache_creation: 0,
                semantics: crate::stats::SEMANTICS_FRESH,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                status_code: Some(status.as_u16()),
                is_streaming: false,
                error: Some(error),
                source: crate::stats::SOURCE_PASSTHROUGH,
            },
        );
    }
    Ok(())
}

/// Maps a bridge usage object (or its absence) onto a stats record with the
/// given `source` marker. Missing usage numbers default to zero so request
/// counts are still recorded. Bridge IR input is the full count (fresh plus
/// cache read plus cache creation), so the record is marked TOTAL and the
/// aggregation layer normalizes it back to fresh.
fn usage_record(
    ts: i64,
    path: &str,
    model: &str,
    usage: Option<&bridge::UsageIr>,
    source: &'static str,
    started: Instant,
) -> crate::stats::UsageRecord {
    let (input, output, cached, cache_creation) = usage
        .map(|usage| {
            (
                usage.input_tokens.unwrap_or(0),
                usage.output_tokens.unwrap_or(0),
                usage.cache_read_tokens.unwrap_or(0),
                usage.cache_write_tokens.unwrap_or(0),
            )
        })
        .unwrap_or((0, 0, 0, 0));
    crate::stats::UsageRecord {
        ts,
        path: path.to_string(),
        model: model.to_string(),
        input,
        output,
        cached,
        cache_creation,
        semantics: crate::stats::SEMANTICS_TOTAL,
        latency_ms: Some(started.elapsed().as_millis() as u64),
        status_code: Some(200),
        is_streaming: source.ends_with("_stream"),
        error: None,
        source,
    }
}

/// Builds a failure record: zero tokens (the request still counts), the
/// failure HTTP status and relayed error text, and a source marker derived
/// from whether the request would have taken the passthrough fast path.
fn failure_usage_record(
    request: &HttpRequest,
    source: bridge::WireProtocol,
    providers: &[ProviderProfile],
    started: Instant,
    failure: &UpstreamFailure,
) -> crate::stats::UsageRecord {
    let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let passthrough = resolve_model(providers, &model)
        .ok()
        .is_some_and(|(provider, _)| bridge::WireProtocol::from(provider.protocol) == source);
    let marker = match (passthrough, streaming) {
        (true, true) => crate::stats::SOURCE_PASSTHROUGH_STREAM,
        (true, false) => crate::stats::SOURCE_PASSTHROUGH,
        (false, true) => crate::stats::SOURCE_CONVERTED_STREAM,
        (false, false) => crate::stats::SOURCE_CONVERTED,
    };
    crate::stats::UsageRecord {
        ts: chrono::Utc::now().timestamp(),
        path: request.path.clone(),
        model,
        input: 0,
        output: 0,
        cached: 0,
        cache_creation: 0,
        semantics: crate::stats::SEMANTICS_FRESH,
        latency_ms: Some(started.elapsed().as_millis() as u64),
        status_code: Some(failure.status.unwrap_or_else(|| failure.kind.http_status())),
        is_streaming: streaming,
        error: Some(upstream_failure_message(failure)),
        source: marker,
    }
}

fn upstream_failure_message(failure: &UpstreamFailure) -> String {
    failure
        .relay
        .clone()
        .unwrap_or_else(|| failure.kind.public_message().to_string())
}

/// Forwards a passthrough SSE response body to the client byte-for-byte
/// while scanning complete `data:` lines for JSON frames that carry a
/// `usage` object, merging usage fields across frames (dict-update on
/// present fields, with the CC Switch delta rule: a later frame reporting a
/// SMALLER positive input wins for the whole block, since some upstreams
/// correct the initial cache-inclusive input in the final usage frame).
/// Anthropic streams split the counts: input/cache live in message_start,
/// output in message_delta — taking only the last frame would lose half the
/// data. Returns None when the stream carried no usage at all. Forwarded
/// bytes are untouched: scanning only inspects a copy.
fn forward_passthrough_stream(
    response: &mut reqwest::blocking::Response,
    stream: &mut TcpStream,
) -> Result<Option<Value>, bridge::BridgeError> {
    let mut merged: Option<Value> = None;
    let mut pending = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|error| map_reqwest_io_error(&error))?;
        if read == 0 {
            break;
        }
        stream
            .write_all(&buffer[..read])
            .map_err(|_| bridge::BridgeError::Internal)?;
        pending.extend_from_slice(&buffer[..read]);
        for frame in scan_passthrough_usage_frames(&mut pending) {
            merge_usage_frame(&mut merged, &frame);
        }
    }
    Ok(merged)
}

/// Consumes complete newline-terminated lines from `pending`, yielding the
/// `usage` object of each line that is a `data:` JSON frame carrying one.
/// The unterminated tail stays buffered so frame boundaries split across
/// chunks are still found; if the tail outgrows 1 MiB the scan window is
/// dropped (forwarding is unaffected, only usage tracking degrades).
fn scan_passthrough_usage_frames(pending: &mut Vec<u8>) -> Vec<Value> {
    const MAX_PENDING_SCAN_BYTES: usize = 1024 * 1024;
    let mut frames = Vec::new();
    while let Some(relative_end) = pending.iter().position(|byte| *byte == b'\n') {
        let mut line: Vec<u8> = pending.drain(..=relative_end).collect();
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        let Ok(line) = std::str::from_utf8(&line) else {
            continue;
        };
        let Some(data) = line.strip_prefix("data:").map(str::trim_start) else {
            continue;
        };
        let Ok(frame) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let usage = frame.get("usage").cloned().or_else(|| {
            frame
                .get("message")
                .and_then(|message| message.get("usage"))
                .cloned()
        });
        if let Some(usage) = usage {
            frames.push(usage);
        }
    }
    if pending.len() > MAX_PENDING_SCAN_BYTES {
        pending.clear();
    }
    frames
}

/// Merges a per-frame `usage` object into `merged` by field (dict-update on
/// present fields), so counts split across frames (message_start carries
/// input/cache, message_delta carries output) all survive. CC Switch delta
/// rule: when the incoming frame reports a SMALLER positive input than what
/// is merged so far, the incoming frame replaces the whole block (its input
/// and cache counts are treated as the corrected figures).
fn merge_usage_frame(merged: &mut Option<Value>, frame: &Value) {
    let Some(object) = frame.as_object() else {
        return;
    };
    if let Some(frame_input) = usage_primary_input(object) {
        let existing_input = merged
            .as_ref()
            .and_then(Value::as_object)
            .and_then(usage_primary_input);
        if frame_input > 0 && existing_input.is_some_and(|input| frame_input < input) {
            *merged = Some(frame.clone());
            return;
        }
    }
    let target = merged.get_or_insert_with(|| json!({}));
    if let Some(target) = target.as_object_mut() {
        for (key, value) in object {
            if !value.is_null() {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Primary input field of a usage object: `input_tokens` (Anthropic family)
/// or `prompt_tokens` (OpenAI family).
fn usage_primary_input(usage: &serde_json::Map<String, Value>) -> Option<u64> {
    usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)
}

fn passthrough_send_upstream(
    provider: &ProviderProfile,
    protocol: bridge::WireProtocol,
    body: Value,
) -> Result<reqwest::blocking::Response, UpstreamFailure> {
    let mut last_error = UpstreamFailure::from(bridge::BridgeError::InvalidUpstream);
    let mut last_response: Option<reqwest::blocking::Response> = None;
    for attempt in 0..=provider.max_retries.min(3) {
        let request = build_upstream_request(provider, protocol, &body)?;

        match request.send() {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let retryable =
                    response.status().as_u16() == 429 || response.status().is_server_error();
                last_response = Some(response);
                last_error = UpstreamFailure::from(bridge::BridgeError::InvalidUpstream);
                if !retryable {
                    break;
                }
            }
            Err(error) => {
                last_error = if error.is_timeout() {
                    UpstreamFailure::from(bridge::BridgeError::Timeout)
                } else {
                    UpstreamFailure::from(bridge::BridgeError::InvalidUpstream)
                };
            }
        }
        if attempt < provider.max_retries.min(3) {
            std::thread::sleep(Duration::from_millis(100 * (attempt as u64 + 1)));
        }
    }
    match last_response {
        Some(response) => Ok(response),
        None => Err(last_error),
    }
}

fn read_upstream_json(
    mut response: reqwest::blocking::Response,
) -> Result<Value, bridge::BridgeError> {
    let body = read_limited_response(&mut response)?;
    serde_json::from_slice(&body).map_err(|_| bridge::BridgeError::InvalidUpstream)
}

fn read_limited_response(
    response: &mut reqwest::blocking::Response,
) -> Result<Vec<u8>, bridge::BridgeError> {
    let mut body = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|error| map_reqwest_io_error(&error))?;
        if read == 0 {
            return Ok(body);
        }
        if body.len().saturating_add(read) > MAX_BODY_BYTES {
            return Err(bridge::BridgeError::ResourceLimit);
        }
        body.extend_from_slice(&buffer[..read]);
    }
}

fn map_reqwest_io_error(error: &std::io::Error) -> bridge::BridgeError {
    if matches!(error.kind(), std::io::ErrorKind::TimedOut) {
        bridge::BridgeError::Timeout
    } else {
        bridge::BridgeError::InvalidUpstream
    }
}

fn map_upstream_bridge_error(error: bridge::BridgeError) -> bridge::BridgeError {
    match error {
        bridge::BridgeError::ResourceLimit => bridge::BridgeError::ResourceLimit,
        bridge::BridgeError::Timeout => bridge::BridgeError::Timeout,
        _ => bridge::BridgeError::InvalidUpstream,
    }
}

fn is_runtime_upstream(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    let Ok(listen) = listen_addr().parse::<SocketAddr>() else {
        return false;
    };
    let host_matches = matches!(
        url.host_str(),
        Some("127.0.0.1") | Some("localhost") | Some("::1")
    );
    host_matches && url.port_or_known_default() == Some(listen.port())
}

fn health_response(providers: &[ProviderProfile], stats: Option<&RuntimeStats>) -> Value {
    let (uptime_seconds, active_connections, success_count, failure_count) = match stats {
        Some(stats) => (
            stats.started_at.elapsed().as_secs(),
            stats.active_connections.load(Ordering::SeqCst),
            stats.success_count.load(Ordering::SeqCst),
            stats.failure_count.load(Ordering::SeqCst),
        ),
        None => (0, 0, 0, 0),
    };
    json!({
        "ok": true,
        "service": "spec-runtime",
        "listen_addr": listen_addr(),
        "uptime_seconds": uptime_seconds,
        "active_connections": active_connections,
        "success_count": success_count,
        "failure_count": failure_count,
        "provider_count": providers.len(),
        "capabilities": {
            "entry_points": {
                "anthropic_messages": true,
                "openai_chat": true,
                "openai_responses": true
            },
            "streaming": true,
            "tools": true,
            "tool_execution": false,
            "reasoning": "partial"
        },
        "limits": {
            "streaming": true,
            "streaming_mode": "protocol_conversion",
            "streaming_conversion": true,
            "tools": true,
            "tool_execution": false,
            "reasoning": "partial",
            "text_only": false
        }
    })
}

#[cfg(test)]
fn stream_route_converts(path: &str, protocol: ProtocolKind) -> bool {
    matches!(
        (path, protocol),
        ("/v1/messages", ProtocolKind::OpenAiChat) | ("/v1/responses", ProtocolKind::OpenAiChat)
    )
}

/// Typed resolution failure: the requested model cannot be mapped to a
/// configured provider/model without ambiguity or an empty suffix remnant.
/// All variants fail closed with the `invalid_request` 400 envelope before
/// any credential-bearing upstream request is built.
#[derive(Debug, PartialEq, Eq)]
enum ModelResolutionError {
    NotConfigured(String),
    Ambiguous(String),
    InvalidSuffix(String),
}

fn resolve_model<'a>(
    providers: &'a [ProviderProfile],
    requested_model: &str,
) -> Result<(&'a ProviderProfile, String), ModelResolutionError> {
    let requested_model = strip_context_suffix(requested_model);
    for provider in providers {
        let suffix = format!("_{}", provider.id);
        if let Some(model) = requested_model.strip_suffix(&suffix) {
            if model.is_empty() {
                return Err(ModelResolutionError::InvalidSuffix(
                    requested_model.to_string(),
                ));
            }
            let upstream = provider
                .configured_request_name_for(model)
                .ok_or_else(|| ModelResolutionError::NotConfigured(model.to_string()))?;
            return Ok((provider, upstream.to_string()));
        }
    }
    if providers.len() == 1 {
        let upstream = providers[0]
            .configured_request_name_for(requested_model)
            .ok_or_else(|| ModelResolutionError::NotConfigured(requested_model.to_string()))?;
        return Ok((&providers[0], upstream.to_string()));
    }
    Err(ModelResolutionError::Ambiguous(requested_model.to_string()))
}

fn strip_context_suffix(model: &str) -> &str {
    match model.get(model.len().saturating_sub(4)..) {
        Some(suffix) if suffix.eq_ignore_ascii_case("[1m]") && model.len() > 4 => {
            &model[..model.len() - 4]
        }
        _ => model,
    }
}

fn join_url(base_url: &str, route: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        route.trim_start_matches('/')
    )
}

#[allow(dead_code)]
mod legacy {
    use super::*;

    pub(super) fn anthropic_to_chat_messages(input: &Value) -> Result<Vec<Value>, String> {
        let mut messages = Vec::new();
        if let Some(system) = input.get("system") {
            messages.push(json!({ "role": "system", "content": value_text(system)? }));
        }
        let history = input
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| "Anthropic request missing messages".to_string())?;
        for message in history {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let content = message.get("content").unwrap_or(&Value::Null);
            match role {
                "assistant" => messages.extend(anthropic_assistant_to_chat(role, content)?),
                _ => messages.extend(anthropic_user_to_chat(role, content)?),
            }
        }
        Ok(messages)
    }

    fn anthropic_assistant_to_chat(role: &str, content: &Value) -> Result<Vec<Value>, String> {
        if let Some(text) = content.as_str() {
            return Ok(vec![json!({ "role": role, "content": text })]);
        }
        let blocks = content
            .as_array()
            .ok_or_else(|| "assistant content must be text or blocks".to_string())?;
        let mut chunks = Vec::new();
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        text_parts.push(text.to_string());
                    }
                }
                Some("tool_use") => {
                    let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let args = serde_json::to_string(&block.get("input").unwrap_or(&Value::Null))
                        .unwrap_or_else(|_| "{}".to_string());
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": args }
                    }));
                }
                other => {
                    return Err(format!("unsupported assistant block type: {other:?}"));
                }
            }
        }
        if tool_calls.is_empty() {
            chunks.push(json!({
                "role": role,
                "content": text_parts.join("\n"),
            }));
        } else {
            chunks.push(json!({
                "role": role,
                "content": text_parts.join("\n"),
                "tool_calls": tool_calls
            }));
        }
        Ok(chunks)
    }

    fn anthropic_user_to_chat(role: &str, content: &Value) -> Result<Vec<Value>, String> {
        if let Some(text) = content.as_str() {
            return Ok(vec![json!({ "role": role, "content": text })]);
        }
        let blocks = content
            .as_array()
            .ok_or_else(|| "user content must be text or blocks".to_string())?;
        let mut chunks = Vec::new();
        let mut text_parts = Vec::new();
        let mut tool_results = Vec::new();
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") | None => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        text_parts.push(text.to_string());
                    }
                }
                Some("tool_result") => {
                    let tool_use_id = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let result_text = value_text(block.get("content").unwrap_or(&Value::Null))?;
                    tool_results.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "content": result_text
                    }));
                }
                Some("image") => {
                    return Err("image content is not supported in chat adapter yet".to_string());
                }
                other => {
                    return Err(format!("unsupported user block type: {other:?}"));
                }
            }
        }
        chunks.extend(tool_results);
        if !text_parts.is_empty() {
            chunks.push(json!({ "role": role, "content": text_parts.join("\n") }));
        }
        Ok(chunks)
    }

    pub(super) fn anthropic_tools_to_openai(input: &Value) -> Option<Vec<Value>> {
        let tools = input.get("tools")?.as_array()?;
        let converted: Vec<Value> = tools
            .iter()
            .map(|tool| {
                let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
                let description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let parameters = tool
                    .get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": description,
                        "parameters": parameters
                    }
                })
            })
            .collect();
        Some(converted)
    }

    fn anthropic_tool_choice_to_openai(input: &Value) -> Option<Value> {
        let choice = input.get("tool_choice")?;
        match choice.get("type").and_then(Value::as_str) {
            Some("any") => Some(json!("required")),
            Some("tool") => {
                let name = choice.get("name").and_then(Value::as_str)?;
                Some(json!({
                    "type": "function",
                    "function": { "name": name }
                }))
            }
            Some("auto") | None => Some(json!("auto")),
            Some("none") => Some(json!("none")),
            _ => Some(json!("auto")),
        }
    }

    pub(super) fn responses_to_chat_messages(input: &Value) -> Result<Vec<Value>, String> {
        let input_value = input
            .get("input")
            .ok_or_else(|| "Responses request missing input".to_string())?;
        let mut messages = Vec::new();
        if let Some(instructions) = input.get("instructions") {
            messages.push(json!({ "role": "system", "content": value_text(instructions)? }));
        }
        if let Some(text) = input_value.as_str() {
            messages.push(json!({ "role": "user", "content": text }));
            return Ok(messages);
        }
        let items = input_value
            .as_array()
            .ok_or_else(|| "Responses input must be a string or array".to_string())?;
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("function_call_output") => {
                    let tool_call_id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let output = value_text(item.get("output").unwrap_or(&Value::Null))?;
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": output
                    }));
                    continue;
                }
                Some("function_call") => {
                    let call_id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
                    let arguments = item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}");
                    messages.push(json!({
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [{
                            "id": call_id,
                            "type": "function",
                            "function": { "name": name, "arguments": arguments }
                        }]
                    }));
                    continue;
                }
                _ => {}
            }
            let role = match item.get("role").and_then(Value::as_str).unwrap_or("user") {
                "developer" => "system",
                other => other,
            };
            messages.push(json!({
                "role": role,
                "content": value_text(item.get("content").unwrap_or(item))?,
            }));
        }
        Ok(messages)
    }

    pub(super) fn responses_to_anthropic(input: &Value) -> Result<Value, String> {
        let input_value = input
            .get("input")
            .ok_or_else(|| "Responses request missing input".to_string())?;
        let mut system = Vec::new();
        if let Some(instructions) = input.get("instructions") {
            system.push(value_text(instructions)?);
        }
        let mut messages = Vec::new();
        if let Some(text) = input_value.as_str() {
            messages.push(json!({ "role": "user", "content": text }));
        } else {
            let items = input_value
                .as_array()
                .ok_or_else(|| "Responses input must be a string or array".to_string())?;
            for item in items {
                let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                let text = value_text(item.get("content").unwrap_or(item))?;
                match role {
                    "system" | "developer" => system.push(text),
                    "assistant" => messages.push(json!({ "role": "assistant", "content": text })),
                    _ => messages.push(json!({ "role": "user", "content": text })),
                }
            }
        }
        let mut body = json!({
            "model": input.get("model").and_then(Value::as_str).unwrap_or(""),
            "max_tokens": input.get("max_output_tokens").and_then(Value::as_u64).unwrap_or(4096),
            "messages": messages,
        });
        if !system.is_empty() {
            body["system"] = json!(system.join("\n\n"));
        }
        copy_generation_params(input, &mut body, ParamTarget::Anthropic);
        Ok(body)
    }

    fn value_text(value: &Value) -> Result<String, String> {
        if let Some(text) = value.as_str() {
            return Ok(text.to_string());
        }
        if let Some(items) = value.as_array() {
            let mut out = String::new();
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") | Some("input_text") | Some("output_text") | None => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            if !out.is_empty() {
                                out.push('\n');
                            }
                            out.push_str(text);
                        }
                    }
                    Some(other) => return Err(format!("unsupported content block type: {other}")),
                }
            }
            return Ok(out);
        }
        Err("content must be text or text blocks".to_string())
    }

    fn post_openai_chat(
        provider: &ProviderProfile,
        model: &str,
        messages: Vec<Value>,
        source: &Value,
    ) -> Result<Value, String> {
        let mut body = json!({ "model": model, "messages": messages, "stream": false });
        copy_generation_params(source, &mut body, ParamTarget::OpenAiChat);
        post_json(provider, "chat/completions", body, HeaderMode::OpenAi)
    }

    fn post_openai_responses(
        provider: &ProviderProfile,
        model: &str,
        input: &Value,
    ) -> Result<Value, String> {
        let mut body = input.clone();
        body["model"] = json!(model);
        post_json(provider, "responses", body, HeaderMode::OpenAi)
    }

    fn post_anthropic(
        provider: &ProviderProfile,
        model: &str,
        input: &Value,
    ) -> Result<Value, String> {
        let mut body = input.clone();
        body["model"] = json!(model);
        if body.get("max_tokens").is_none() {
            body["max_tokens"] = json!(provider.max_output_tokens.max(4096));
        }
        copy_generation_params(input, &mut body, ParamTarget::Anthropic);
        post_json(provider, "messages", body, HeaderMode::Anthropic)
    }

    #[derive(Clone, Copy)]
    enum HeaderMode {
        OpenAi,
        Anthropic,
    }

    #[derive(Clone, Copy)]
    enum ParamTarget {
        OpenAiChat,
        Anthropic,
    }

    fn copy_generation_params(source: &Value, body: &mut Value, target: ParamTarget) {
        for key in [
            "temperature",
            "top_p",
            "presence_penalty",
            "frequency_penalty",
        ] {
            if let Some(value) = source.get(key) {
                body[key] = value.clone();
            }
        }
        match target {
            ParamTarget::OpenAiChat => {
                if let Some(value) = source
                    .get("max_tokens")
                    .or_else(|| source.get("max_output_tokens"))
                {
                    body["max_tokens"] = value.clone();
                }
                if let Some(value) = source.get("stop").or_else(|| source.get("stop_sequences")) {
                    body["stop"] = value.clone();
                }
            }
            ParamTarget::Anthropic => {
                if let Some(value) = source
                    .get("max_tokens")
                    .or_else(|| source.get("max_output_tokens"))
                {
                    body["max_tokens"] = value.clone();
                }
                if let Some(value) = source.get("stop_sequences").or_else(|| source.get("stop")) {
                    body["stop_sequences"] = value.clone();
                }
            }
        }
    }

    fn post_json(
        provider: &ProviderProfile,
        route: &str,
        body: Value,
        header_mode: HeaderMode,
    ) -> Result<Value, String> {
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(provider.timeout_ms.max(1_000)))
            .build()
            .map_err(|e| e.to_string())?;
        let mut last_error = String::new();
        for attempt in 0..=provider.max_retries.min(3) {
            let mut request = client.post(join_url(&provider.base_url, route)).json(&body);
            match header_mode {
                HeaderMode::OpenAi => {
                    request = request.bearer_auth(&provider.api_key);
                }
                HeaderMode::Anthropic => {
                    request = request
                        .header("x-api-key", &provider.api_key)
                        .header("anthropic-version", "2023-06-01");
                }
            }
            for (key, value) in &provider.extra_headers {
                request = request.header(key, value);
            }
            match request.send() {
                Ok(response) => {
                    let status = response.status();
                    let value: Value = response.json().map_err(|e| e.to_string())?;
                    if status.is_success() {
                        return Ok(value);
                    }
                    last_error = format!("upstream returned {status}: {value}");
                    if !(status.as_u16() == 429 || status.is_server_error()) {
                        break;
                    }
                }
                Err(error) => {
                    last_error = error.to_string();
                }
            }
            if attempt < provider.max_retries.min(3) {
                std::thread::sleep(Duration::from_millis(100 * (attempt as u64 + 1)));
            }
        }
        Err(last_error)
    }

    fn post_json_stream(
        provider: &ProviderProfile,
        route: &str,
        body: Value,
        header_mode: HeaderMode,
        stream: &mut TcpStream,
    ) -> Result<(), String> {
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(provider.timeout_ms.max(1_000)))
            .build()
            .map_err(|e| e.to_string())?;
        let mut request = client.post(join_url(&provider.base_url, route)).json(&body);
        match header_mode {
            HeaderMode::OpenAi => {
                request = request.bearer_auth(&provider.api_key);
            }
            HeaderMode::Anthropic => {
                request = request
                    .header("x-api-key", &provider.api_key)
                    .header("anthropic-version", "2023-06-01");
            }
        }
        for (key, value) in &provider.extra_headers {
            request = request.header(key, value);
        }
        let mut response = request.send().map_err(|e| e.to_string())?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("text/event-stream")
            .to_string();
        write_raw_headers(
            stream,
            status.as_u16(),
            status.canonical_reason().unwrap_or("OK"),
            &content_type,
        )?;
        let mut buffer = [0; 8192];
        loop {
            let read = response.read(&mut buffer).map_err(|e| e.to_string())?;
            if read == 0 {
                break;
            }
            stream
                .write_all(&buffer[..read])
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn stream_anthropic_to_chat(
        stream: &mut TcpStream,
        provider: &ProviderProfile,
        model: &str,
        anthropic_body: &Value,
    ) -> Result<(), String> {
        let messages = anthropic_to_chat_messages(anthropic_body)?;
        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true }
        });
        if let Some(tools) = anthropic_tools_to_openai(anthropic_body) {
            body["tools"] = json!(tools);
        }
        if let Some(tool_choice) = anthropic_tool_choice_to_openai(anthropic_body) {
            body["tool_choice"] = tool_choice;
        }
        if anthropic_body.get("reasoning").is_none() && anthropic_body.get("thinking").is_none() {
            body["reasoning_effort"] = json!("none");
        }
        copy_generation_params(anthropic_body, &mut body, ParamTarget::OpenAiChat);

        let mut response =
            send_stream_request(provider, "chat/completions", &body, HeaderMode::OpenAi)?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("text/event-stream")
            .to_string();
        write_raw_headers(
            stream,
            status.as_u16(),
            status.canonical_reason().unwrap_or("OK"),
            &content_type,
        )?;
        if !status.is_success() {
            let mut buffer = [0; 8192];
            loop {
                let read = response.read(&mut buffer).map_err(|e| e.to_string())?;
                if read == 0 {
                    break;
                }
                stream
                    .write_all(&buffer[..read])
                    .map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        stream_anthropic_events(stream, &mut response, model)
    }

    fn stream_responses_to_chat(
        stream: &mut TcpStream,
        provider: &ProviderProfile,
        model: &str,
        responses_body: &Value,
        requested_model: &str,
    ) -> Result<(), String> {
        let body = responses_to_chat_request(responses_body, model)?;
        let mut response =
            send_stream_request(provider, "chat/completions", &body, HeaderMode::OpenAi)?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("text/event-stream")
            .to_string();
        write_raw_headers(
            stream,
            status.as_u16(),
            status.canonical_reason().unwrap_or("OK"),
            &content_type,
        )?;
        if !status.is_success() {
            let mut buffer = [0; 8192];
            loop {
                let read = response.read(&mut buffer).map_err(|e| e.to_string())?;
                if read == 0 {
                    break;
                }
                stream
                    .write_all(&buffer[..read])
                    .map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        stream_chat_to_responses_events(stream, &mut response, requested_model)
    }

    pub(super) fn responses_to_chat_request(input: &Value, model: &str) -> Result<Value, String> {
        let messages = responses_to_chat_messages(input)?;
        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
            "reasoning_effort": "none"
        });
        if let Some(tools) = responses_tools_to_chat(input) {
            body["tools"] = json!(tools);
        }
        if let Some(tool_choice) = input.get("tool_choice") {
            body["tool_choice"] = responses_tool_choice_to_chat(tool_choice);
        }
        copy_generation_params(input, &mut body, ParamTarget::OpenAiChat);
        Ok(body)
    }

    fn responses_tool_choice_to_chat(choice: &Value) -> Value {
        if choice
            .as_object()
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
            == Some("function")
        {
            if let Some(name) = choice.get("name").and_then(Value::as_str) {
                return json!({ "type": "function", "function": { "name": name } });
            }
        }
        choice.clone()
    }

    fn responses_tools_to_chat(input: &Value) -> Option<Vec<Value>> {
        let tools = input.get("tools")?.as_array()?;
        let converted: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                if tool.get("type").and_then(Value::as_str) != Some("function") {
                    return None;
                }
                let name = tool
                    .get("name")
                    .or_else(|| tool.pointer("/function/name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let description = tool
                    .get("description")
                    .or_else(|| tool.pointer("/function/description"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let parameters = tool
                    .get("parameters")
                    .or_else(|| tool.get("input_schema"))
                    .or_else(|| tool.pointer("/function/parameters"))
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": description,
                        "parameters": parameters
                    }
                }))
            })
            .collect();
        (!converted.is_empty()).then_some(converted)
    }

    fn send_stream_request(
        provider: &ProviderProfile,
        route: &str,
        body: &Value,
        header_mode: HeaderMode,
    ) -> Result<reqwest::blocking::Response, String> {
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(provider.timeout_ms.max(1_000)))
            .build()
            .map_err(|e| e.to_string())?;
        let mut last_error = String::new();
        for attempt in 0..=provider.max_retries.min(3) {
            let mut request = client.post(join_url(&provider.base_url, route)).json(body);
            match header_mode {
                HeaderMode::OpenAi => {
                    request = request.bearer_auth(&provider.api_key);
                }
                HeaderMode::Anthropic => {
                    request = request
                        .header("x-api-key", &provider.api_key)
                        .header("anthropic-version", "2023-06-01");
                }
            }
            for (key, value) in &provider.extra_headers {
                request = request.header(key, value);
            }
            match request.send() {
                Ok(response) => return Ok(response),
                Err(error) => {
                    last_error = error.to_string();
                    if attempt < provider.max_retries.min(3) {
                        std::thread::sleep(Duration::from_millis(100 * (attempt as u64 + 1)));
                    }
                }
            }
        }
        Err(last_error)
    }

    struct ResponsesStreamTool {
        output_index: usize,
        item_id: String,
        call_id: String,
        name: String,
        arguments: String,
        started: bool,
    }

    fn stream_chat_to_responses_events(
        stream: &mut TcpStream,
        response: &mut reqwest::blocking::Response,
        model: &str,
    ) -> Result<(), String> {
        let response_id = generated_id("resp");
        let mut state = json!({
            "id": response_id,
            "object": "response",
            "created_at": now_secs(),
            "model": model,
            "status": "in_progress",
            "output": []
        });
        write_sse(
            stream,
            "response.created",
            &json!({ "type": "response.created", "response": state }),
        )?;

        let message_id = generated_id("msg");
        let mut text_started = false;
        let mut text = String::new();
        let mut tools: Vec<ResponsesStreamTool> = Vec::new();
        let mut finish_reason = None;
        let mut usage = json!({});
        let mut finished = false;
        let mut buffer = [0; 8192];
        let mut acc = String::new();

        while !finished {
            let read = response.read(&mut buffer).map_err(|e| e.to_string())?;
            if read == 0 {
                break;
            }
            acc.push_str(&String::from_utf8_lossy(&buffer[..read]));
            while let Some(pos) = acc.find('\n') {
                let line = acc[..pos].trim().to_string();
                acc = acc[pos + 1..].to_string();
                if line == "data: [DONE]" {
                    finished = true;
                    break;
                }
                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                let chunk: Value = match serde_json::from_str(payload.trim()) {
                    Ok(chunk) => chunk,
                    Err(_) => continue,
                };
                let choices = chunk
                    .get("choices")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for choice in choices {
                    let delta = choice.get("delta").unwrap_or(&Value::Null);
                    if let Some(value) = delta.get("content").and_then(Value::as_str) {
                        if !value.is_empty() {
                            if !text_started {
                                write_sse(
                                    stream,
                                    "response.output_item.added",
                                    &json!({
                                        "type": "response.output_item.added",
                                        "response_id": response_id,
                                        "output_index": 0,
                                        "item": {
                                            "id": message_id,
                                            "type": "message",
                                            "role": "assistant",
                                            "content": [],
                                            "status": "in_progress"
                                        }
                                    }),
                                )?;
                                write_sse(
                                    stream,
                                    "response.content_part.added",
                                    &json!({
                                        "type": "response.content_part.added",
                                        "item_id": message_id,
                                        "output_index": 0,
                                        "content_index": 0,
                                        "part": { "type": "output_text", "text": "", "annotations": [] }
                                    }),
                                )?;
                                text_started = true;
                            }
                            text.push_str(value);
                            write_sse(
                                stream,
                                "response.output_text.delta",
                                &json!({
                                    "type": "response.output_text.delta",
                                    "item_id": message_id,
                                    "output_index": 0,
                                    "content_index": 0,
                                    "delta": value
                                }),
                            )?;
                        }
                    }
                    if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                        for tool_call in tool_calls {
                            let tool_index =
                                tool_call.get("index").and_then(Value::as_u64).unwrap_or(0)
                                    as usize;
                            while tools.len() <= tool_index {
                                let index = tools.len();
                                tools.push(ResponsesStreamTool {
                                    output_index: usize::from(text_started) + index,
                                    item_id: generated_id("fc"),
                                    call_id: String::new(),
                                    name: String::new(),
                                    arguments: String::new(),
                                    started: false,
                                });
                            }
                            let tool = &mut tools[tool_index];
                            if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
                                if !id.is_empty() {
                                    tool.call_id = id.to_string();
                                }
                            }
                            if let Some(name) =
                                tool_call.pointer("/function/name").and_then(Value::as_str)
                            {
                                if !name.is_empty() {
                                    tool.name = name.to_string();
                                }
                            }
                            let start = !tool.started;
                            tool.started = true;
                            let output_index = tool.output_index;
                            let item_id = tool.item_id.clone();
                            let call_id = if tool.call_id.is_empty() {
                                item_id.clone()
                            } else {
                                tool.call_id.clone()
                            };
                            let name = tool.name.clone();
                            if start {
                                write_sse(
                                    stream,
                                    "response.output_item.added",
                                    &json!({
                                        "type": "response.output_item.added",
                                        "response_id": response_id,
                                        "output_index": output_index,
                                        "item": {
                                            "id": item_id,
                                            "type": "function_call",
                                            "call_id": call_id,
                                            "name": name,
                                            "arguments": "",
                                            "status": "in_progress"
                                        }
                                    }),
                                )?;
                            }
                            if let Some(arguments) = tool_call
                                .pointer("/function/arguments")
                                .and_then(Value::as_str)
                            {
                                if !arguments.is_empty() {
                                    tool.arguments.push_str(arguments);
                                    write_sse(
                                        stream,
                                        "response.function_call_arguments.delta",
                                        &json!({
                                            "type": "response.function_call_arguments.delta",
                                            "item_id": item_id,
                                            "output_index": output_index,
                                            "delta": arguments
                                        }),
                                    )?;
                                }
                            }
                        }
                    }
                    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                        if !reason.is_empty() && reason != "null" {
                            finish_reason = Some(reason.to_string());
                        }
                    }
                }
                if let Some(value) = chunk.get("usage") {
                    if !value.is_null() {
                        usage = value.clone();
                    }
                }
            }
        }

        let mut output = Vec::new();
        if text_started {
            write_sse(
                stream,
                "response.output_text.done",
                &json!({
                    "type": "response.output_text.done",
                    "item_id": message_id,
                    "output_index": 0,
                    "content_index": 0,
                    "text": text
                }),
            )?;
            write_sse(
                stream,
                "response.content_part.done",
                &json!({
                    "type": "response.content_part.done",
                    "item_id": message_id,
                    "output_index": 0,
                    "content_index": 0,
                    "part": { "type": "output_text", "text": text, "annotations": [] }
                }),
            )?;
            let item = json!({
                "id": message_id,
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": text, "annotations": [] }],
                "status": "completed"
            });
            write_sse(
                stream,
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "response_id": response_id,
                    "output_index": 0,
                    "item": item.clone()
                }),
            )?;
            output.push(item);
        }
        for tool in tools {
            let call_id = if tool.call_id.is_empty() {
                tool.item_id.clone()
            } else {
                tool.call_id.clone()
            };
            let item = json!({
                "id": tool.item_id,
                "type": "function_call",
                "call_id": call_id,
                "name": tool.name,
                "arguments": tool.arguments,
                "status": "completed"
            });
            write_sse(
                stream,
                "response.function_call_arguments.done",
                &json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": item["id"],
                    "output_index": tool.output_index,
                    "arguments": item["arguments"]
                }),
            )?;
            write_sse(
                stream,
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "response_id": response_id,
                    "output_index": tool.output_index,
                    "item": item.clone()
                }),
            )?;
            output.push(item);
        }
        state["output"] = json!(output);
        state["status"] = json!(if finish_reason.as_deref() == Some("length") {
            "incomplete"
        } else {
            "completed"
        });
        state["usage"] = chat_usage_to_responses(&usage);
        state["incomplete_details"] = Value::Null;
        state["error"] = Value::Null;
        write_sse(
            stream,
            "response.completed",
            &json!({ "type": "response.completed", "response": state }),
        )?;
        write_sse_done(stream)
    }

    pub(super) fn anthropic_stop_reason(reason: Option<&str>) -> &'static str {
        match reason {
            Some("tool_calls") => "tool_use",
            Some("stop") => "end_turn",
            Some("length") => "max_tokens",
            Some("content_filter") => "refusal",
            _ => "end_turn",
        }
    }

    pub(super) fn anthropic_usage(usage: &Value) -> Value {
        let input_tokens = usage
            .get("input_tokens")
            .or_else(|| usage.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output_tokens = usage
            .get("output_tokens")
            .or_else(|| usage.get("completion_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let mut out = json!({
            "input_tokens": input_tokens,
            "output_tokens": output_tokens
        });
        if let Some(cached) = usage
            .get("cached_tokens")
            .or_else(|| usage.pointer("/prompt_tokens_details/cached_tokens"))
            .and_then(Value::as_u64)
        {
            out["cache_creation_input_tokens"] = json!(cached);
        }
        out
    }

    pub(super) struct AnthropicToolTracker {
        started: Vec<i64>,
    }

    impl AnthropicToolTracker {
        pub(super) fn new() -> Self {
            Self {
                started: Vec::new(),
            }
        }

        pub(super) fn start(&mut self, tool_index: i64, id: &str, name: &str) -> Option<Value> {
            if self.started.contains(&tool_index) {
                return None;
            }
            self.started.push(tool_index);
            Some(json!({
                "type": "content_block_start",
                "index": tool_index,
                "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} }
            }))
        }

        pub(super) fn stop(&mut self) -> Vec<Value> {
            let mut out = Vec::new();
            let mut indexes: Vec<i64> = self.started.clone();
            indexes.sort_unstable();
            for index in indexes {
                out.push(json!({ "type": "content_block_stop", "index": index }));
            }
            out
        }

        fn next_index(&self, text_started: bool) -> i64 {
            let base = if text_started { 1 } else { 0 };
            let used = self.started.iter().max().copied().unwrap_or(-1);
            used.max(base - 1) + 1
        }
    }

    fn stream_anthropic_events(
        stream: &mut TcpStream,
        response: &mut reqwest::blocking::Response,
        model: &str,
    ) -> Result<(), String> {
        let message_id = generated_id("msg");
        write_sse(
            stream,
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 }
                }
            }),
        )?;

        let mut text_started = false;
        let text_index: i64 = 0;
        let mut tools = AnthropicToolTracker::new();
        let mut stop_reason: Option<String> = None;
        let mut finished = false;

        let reader = response;
        let mut buffer = [0; 8192];
        let mut acc = String::new();
        while !finished {
            let read = reader.read(&mut buffer).map_err(|e| e.to_string())?;
            if read == 0 {
                break;
            }
            acc.push_str(&String::from_utf8_lossy(&buffer[..read]));
            while let Some(pos) = acc.find('\n') {
                let line = acc[..pos].trim().to_string();
                acc = acc[pos + 1..].to_string();
                if line == "data: [DONE]" {
                    finished = true;
                    break;
                }
                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                let chunk: Value = match serde_json::from_str(payload.trim()) {
                    Ok(chunk) => chunk,
                    Err(_) => continue,
                };
                let choices: Vec<Value> = chunk
                    .get("choices")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for choice in choices {
                    let delta = choice.get("delta").unwrap_or(&Value::Null);
                    if let Some(text) = delta.get("content").and_then(Value::as_str) {
                        if !text.is_empty() {
                            if !text_started {
                                write_sse(
                                    stream,
                                    "content_block_start",
                                    &json!({
                                        "type": "content_block_start",
                                        "index": text_index,
                                        "content_block": { "type": "text", "text": "" }
                                    }),
                                )?;
                                text_started = true;
                            }
                            write_sse(
                                stream,
                                "content_block_delta",
                                &json!({
                                    "type": "content_block_delta",
                                    "index": text_index,
                                    "delta": { "type": "text_delta", "text": text }
                                }),
                            )?;
                        }
                    }
                    if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                        for tool_call in tool_calls {
                            let tool_index = tool_call
                                .get("index")
                                .and_then(Value::as_i64)
                                .unwrap_or(tools.next_index(text_started));
                            let id = tool_call
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            let name = tool_call
                                .pointer("/function/name")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            if !name.is_empty() || !id.is_empty() {
                                if let Some(event) = tools.start(tool_index, id, name) {
                                    write_sse(stream, "content_block_start", &event)?;
                                }
                            }
                            if let Some(arguments) = tool_call
                                .pointer("/function/arguments")
                                .and_then(Value::as_str)
                            {
                                if !arguments.is_empty() {
                                    write_sse(
                                        stream,
                                        "content_block_delta",
                                        &json!({
                                            "type": "content_block_delta",
                                            "index": tool_index,
                                            "delta": { "type": "input_json_delta", "partial_json": arguments }
                                        }),
                                    )?;
                                }
                            }
                        }
                    }
                    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                        if !reason.is_empty() && reason != "null" {
                            stop_reason = Some(reason.to_string());
                        }
                    }
                }
                if let Some(usage) = chunk.get("usage") {
                    if !usage.is_null() {
                        write_sse(
                            stream,
                            "message_delta",
                            &json!({
                                "type": "message_delta",
                                "delta": { "stop_reason": anthropic_stop_reason(stop_reason.as_deref()), "stop_sequence": null },
                                "usage": anthropic_usage(usage)
                            }),
                        )?;
                    }
                }
            }
        }

        for stop in tools.stop() {
            write_sse(stream, "content_block_stop", &stop)?;
        }
        if text_started {
            write_sse(
                stream,
                "content_block_stop",
                &json!({
                    "type": "content_block_stop",
                    "index": text_index
                }),
            )?;
        }
        if stop_reason.is_none() {
            write_sse(
                stream,
                "message_delta",
                &json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": "end_turn", "stop_sequence": null },
                    "usage": {}
                }),
            )?;
        }
        write_sse(stream, "message_stop", &json!({ "type": "message_stop" }))?;
        Ok(())
    }

    fn chat_to_anthropic(response: &Value, requested_model: &str) -> Value {
        let message = response
            .pointer("/choices/0/message")
            .cloned()
            .unwrap_or_default();
        let text = message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut content: Vec<Value> = Vec::new();
        let mut stop_reason = "end_turn";
        if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
            if !tool_calls.is_empty() {
                stop_reason = "tool_use";
            }
            for tool_call in tool_calls {
                let id = tool_call
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let name = tool_call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let arguments = tool_call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let input: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
                content.push(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input
                }));
            }
        } else if !text.is_empty() {
            content.push(json!({ "type": "text", "text": text }));
        } else if content.is_empty() {
            content.push(json!({ "type": "text", "text": "" }));
        }
        json!({
            "id": generated_id("msg"),
            "type": "message",
            "role": "assistant",
            "model": requested_model,
            "content": content,
            "stop_reason": stop_reason,
            "stop_sequence": null,
            "usage": response.get("usage").cloned().unwrap_or_else(|| json!({}))
        })
    }

    fn chat_to_responses(response: &Value, requested_model: &str) -> Value {
        let text = response
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        responses_text(requested_model, text)
    }

    pub(super) fn chat_usage_to_responses(usage: &Value) -> Value {
        let input_tokens = usage
            .get("input_tokens")
            .or_else(|| usage.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output_tokens = usage
            .get("output_tokens")
            .or_else(|| usage.get("completion_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let total_tokens = usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(input_tokens + output_tokens);
        json!({
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": total_tokens,
            "input_tokens_details": usage
                .get("input_tokens_details")
                .or_else(|| usage.get("prompt_tokens_details"))
                .cloned()
                .unwrap_or_else(|| json!({ "cached_tokens": 0 }))
        })
    }

    fn anthropic_to_responses(response: &Value, requested_model: &str) -> Value {
        let text = response
            .get("content")
            .and_then(|v| value_text(v).ok())
            .unwrap_or_default();
        responses_text(requested_model, &text)
    }

    fn responses_text(model: &str, text: &str) -> Value {
        json!({
            "id": generated_id("resp"),
            "object": "response",
            "created_at": now_secs(),
            "model": model,
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": text }]
            }],
            "output_text": text,
        })
    }

    fn join_url(base_url: &str, route: &str) -> String {
        format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            route.trim_start_matches('/')
        )
    }

    fn generated_id(prefix: &str) -> String {
        format!("{prefix}_{}", now_secs())
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

#[cfg(test)]
use legacy::*;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::thread;

    use super::*;
    use crate::domain::{CacheMode, ProviderVendor};

    fn round_trip_request(path: &str, body: Value, provider: ProviderProfile) -> Value {
        let upstream = TcpListener::bind("127.0.0.1:0").expect("bind upstream test listener");
        let upstream_addr = upstream.local_addr().expect("upstream address");
        let upstream_thread = thread::spawn(move || {
            upstream
                .set_nonblocking(true)
                .expect("configure upstream test listener");
            let mut accepted = None;
            for _ in 0..100 {
                match upstream.accept() {
                    Ok(pair) => {
                        accepted = Some(pair);
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept upstream request: {error}"),
                }
            }
            let Some((mut socket, _)) = accepted else {
                return;
            };
            let mut request = [0; 8192];
            let _ = socket.read(&mut request).expect("read upstream request");
            let body = br#"{"id":"chat-1","object":"chat.completion","model":"upstream-model","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
            let headers = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            socket
                .write_all(headers.as_bytes())
                .and_then(|_| socket.write_all(body))
                .expect("write upstream response");
            socket
                .shutdown(Shutdown::Write)
                .expect("finish upstream response");
        });

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind runtime test listener");
        let address = listener.local_addr().expect("runtime address");
        let client = TcpStream::connect(address).expect("connect runtime test client");
        let (mut server, _) = listener.accept().expect("accept runtime test client");
        let mut client = client;
        let body = serde_json::to_vec(&body).expect("serialize request body");
        let request = format!(
            "POST {path} HTTP/1.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        client
            .write_all(request.as_bytes())
            .and_then(|_| client.write_all(&body))
            .expect("write runtime request");
        client
            .shutdown(Shutdown::Write)
            .expect("finish runtime request");

        let mut provider = provider;
        provider.base_url = format!("http://{upstream_addr}");
        let home = tempfile::tempdir().expect("temp home");
        handle_connection(&mut server, home.path(), &[provider], &RuntimeStats::new())
            .expect("runtime request handled");
        server
            .shutdown(Shutdown::Write)
            .expect("finish runtime response");
        let mut output = Vec::new();
        client
            .read_to_end(&mut output)
            .expect("read runtime response");
        upstream_thread.join().expect("upstream thread completed");

        let body_start = output
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("runtime response has headers")
            + 4;
        serde_json::from_slice(&output[body_start..]).expect("runtime response body is JSON")
    }

    fn response_status(body: Value) -> (u16, Value) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind runtime test listener");
        let address = listener.local_addr().expect("runtime address");
        let client = TcpStream::connect(address).expect("connect runtime test client");
        let (mut server, _) = listener.accept().expect("accept runtime test client");
        let mut client = client;
        let body = serde_json::to_vec(&body).expect("serialize request body");
        let request = format!(
            "GET /v1/unknown HTTP/1.1\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        client
            .write_all(request.as_bytes())
            .and_then(|_| client.write_all(&body))
            .expect("write runtime request");
        client
            .shutdown(Shutdown::Write)
            .expect("finish runtime request");
        let home = tempfile::tempdir().expect("temp home");
        handle_connection(&mut server, home.path(), &[], &RuntimeStats::new())
            .expect("runtime request handled");
        server
            .shutdown(Shutdown::Write)
            .expect("finish runtime response");
        let mut output = Vec::new();
        client
            .read_to_end(&mut output)
            .expect("read runtime response");

        let headers_end = output
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("runtime response has headers");
        let status = String::from_utf8_lossy(&output[..headers_end])
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|status| status.parse().ok())
            .expect("runtime response status");
        let value = serde_json::from_slice(&output[headers_end + 4..])
            .expect("runtime response body is JSON");
        (status, value)
    }

    fn bridged_round_trip(
        path: &str,
        body: Value,
        mut provider: ProviderProfile,
        upstream_content_type: &str,
        upstream_body: &[u8],
    ) -> (u16, String, String) {
        let upstream = TcpListener::bind("127.0.0.1:0").expect("bind upstream test listener");
        let upstream_addr = upstream.local_addr().expect("upstream address");
        let upstream_body = upstream_body.to_vec();
        let upstream_content_type = upstream_content_type.to_string();
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        let upstream_thread = thread::spawn(move || {
            upstream
                .set_nonblocking(true)
                .expect("configure upstream test listener");
            let mut accepted = None;
            for _ in 0..100 {
                match upstream.accept() {
                    Ok(pair) => {
                        accepted = Some(pair);
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept upstream request: {error}"),
                }
            }
            let Some((mut socket, _)) = accepted else {
                return;
            };
            let mut request = [0; 16 * 1024];
            let read = socket.read(&mut request).expect("read upstream request");
            request_tx
                .send(String::from_utf8_lossy(&request[..read]).into_owned())
                .expect("send captured upstream request");
            let headers = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {upstream_content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                upstream_body.len()
            );
            socket
                .write_all(headers.as_bytes())
                .and_then(|_| socket.write_all(&upstream_body))
                .expect("write upstream response");
            socket
                .shutdown(Shutdown::Write)
                .expect("finish upstream response");
        });

        provider.base_url = format!("http://{upstream_addr}");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind runtime test listener");
        let address = listener.local_addr().expect("runtime address");
        let client = TcpStream::connect(address).expect("connect runtime test client");
        let (mut server, _) = listener.accept().expect("accept runtime test client");
        let mut client = client;
        let body = serde_json::to_vec(&body).expect("serialize request body");
        let request = format!(
            "POST {path} HTTP/1.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        client
            .write_all(request.as_bytes())
            .and_then(|_| client.write_all(&body))
            .expect("write runtime request");
        client
            .shutdown(Shutdown::Write)
            .expect("finish runtime request");

        let home = tempfile::tempdir().expect("temp home");
        handle_connection(&mut server, home.path(), &[provider], &RuntimeStats::new())
            .expect("runtime request handled");
        server
            .shutdown(Shutdown::Write)
            .expect("finish runtime response");
        let mut output = Vec::new();
        client
            .read_to_end(&mut output)
            .expect("read runtime response");
        upstream_thread.join().expect("upstream thread completed");
        let request = request_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("captured upstream request");

        let headers_end = output
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("runtime response has headers");
        let status = String::from_utf8_lossy(&output[..headers_end])
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|status| status.parse().ok())
            .expect("runtime response status");
        (
            status,
            String::from_utf8_lossy(&output[headers_end + 4..]).into_owned(),
            request,
        )
    }

    fn raw_runtime_request(request: &[u8], providers: &[ProviderProfile]) -> (u16, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind runtime test listener");
        let address = listener.local_addr().expect("runtime address");
        let client = TcpStream::connect(address).expect("connect runtime test client");
        let (mut server, _) = listener.accept().expect("accept runtime test client");
        let mut client = client;
        client
            .write_all(request)
            .expect("write raw runtime request");
        client
            .shutdown(Shutdown::Write)
            .expect("finish raw runtime request");
        let home = tempfile::tempdir().expect("temp home");
        handle_connection(&mut server, home.path(), providers, &RuntimeStats::new())
            .expect("runtime request handled");
        server
            .shutdown(Shutdown::Write)
            .expect("finish runtime response");
        let mut output = Vec::new();
        client
            .read_to_end(&mut output)
            .expect("read runtime response");

        let headers_end = output
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("runtime response has headers");
        let status = String::from_utf8_lossy(&output[..headers_end])
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|status| status.parse().ok())
            .expect("runtime response status");
        (
            status,
            String::from_utf8_lossy(&output[headers_end + 4..]).into_owned(),
        )
    }

    #[test]
    fn route_dispatches_all_public_protocol_endpoints() {
        let cases = [
            (
                "/v1/messages",
                json!({
                    "model": "fixture_provider",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                "type",
                "message",
            ),
            (
                "/v1/chat/completions",
                json!({
                    "model": "fixture_provider",
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                "object",
                "chat.completion",
            ),
            (
                "/v1/responses",
                json!({"model": "fixture_provider", "input": "hello"}),
                "object",
                "response",
            ),
        ];

        for (path, body, response_key, expected_object) in cases {
            let response = round_trip_request(
                path,
                body,
                provider("fixture_provider", ProtocolKind::OpenAiChat),
            );
            assert_eq!(response[response_key], expected_object, "route {path}");
        }
    }

    #[test]
    fn unsupported_route_uses_safe_invalid_request_envelope() {
        let (status, response) = response_status(json!({}));

        assert_eq!(status, 400);
        assert_eq!(response["error"]["code"], "invalid_request");
        assert_eq!(response["error"]["message"], "invalid request");
    }

    #[test]
    fn models_list_entries_are_dual_protocol_compatible() {
        let (status, body) = raw_runtime_request(
            b"GET /v1/models HTTP/1.1\r\n\r\n",
            &[provider("zen", ProtocolKind::OpenAiChat)],
        );
        assert_eq!(status, 200);
        let value: Value = serde_json::from_str(&body).expect("models response is JSON");
        assert_eq!(value["object"], json!("list"));
        let entry = &value["data"][0];
        assert_eq!(entry["id"], json!("deepseek-chat_zen"));
        assert_eq!(entry["object"], json!("model"));
        assert_eq!(entry["type"], json!("model"));
        assert_eq!(entry["owned_by"], json!("zen"));
        assert!(
            entry.get("display_name").is_some(),
            "Anthropic clients read display_name"
        );
        assert!(
            entry.get("created_at").is_some(),
            "Anthropic clients read created_at"
        );
    }

    #[test]
    fn models_retrieve_returns_single_dual_protocol_entry() {
        let (status, body) = raw_runtime_request(
            b"GET /v1/models/deepseek-chat_zen HTTP/1.1\r\n\r\n",
            &[provider("zen", ProtocolKind::OpenAiChat)],
        );
        assert_eq!(status, 200);
        let value: Value = serde_json::from_str(&body).expect("models retrieve response is JSON");
        assert_eq!(value["id"], json!("deepseek-chat_zen"));
        assert_eq!(value["type"], json!("model"));
        assert!(value.get("display_name").is_some());
        assert!(value.get("created_at").is_some());
    }

    #[test]
    fn stream_survives_total_duration_beyond_provider_timeout_when_chunks_trickle() {
        let upstream = TcpListener::bind("127.0.0.1:0").expect("bind upstream test listener");
        let upstream_addr = upstream.local_addr().expect("upstream address");
        let upstream_thread = thread::spawn(move || {
            upstream
                .set_nonblocking(true)
                .expect("configure upstream test listener");
            let mut accepted = None;
            for _ in 0..100 {
                match upstream.accept() {
                    Ok(pair) => {
                        accepted = Some(pair);
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept upstream request: {error}"),
                }
            }
            let Some((mut socket, _)) = accepted else {
                return;
            };
            let mut request = [0; 8192];
            let _ = socket.read(&mut request).expect("read upstream request");
            let headers = concat!(
                "HTTP/1.1 200 OK\r\n",
                "content-type: text/event-stream\r\n",
                "connection: close\r\n\r\n"
            );
            socket
                .write_all(headers.as_bytes())
                .expect("write upstream headers");
            for index in 0..8 {
                let chunk = if index == 7 {
                    "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"upstream-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
                } else {
                    "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"upstream-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n"
                };
                socket
                    .write_all(chunk.as_bytes())
                    .expect("write upstream chunk");
                socket.flush().expect("flush upstream chunk");
                thread::sleep(Duration::from_millis(100));
            }
            socket
                .shutdown(Shutdown::Write)
                .expect("finish upstream response");
        });

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind runtime test listener");
        let address = listener.local_addr().expect("runtime address");
        let client = TcpStream::connect(address).expect("connect runtime test client");
        let (mut server, _) = listener.accept().expect("accept runtime test client");
        let mut client = client;
        let body = serde_json::to_vec(&json!({
            "model": "fixture_provider",
            "max_tokens": 16,
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .expect("serialize request body");
        let request = format!(
            "POST /v1/messages HTTP/1.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        client
            .write_all(request.as_bytes())
            .and_then(|_| client.write_all(&body))
            .expect("write runtime request");
        client
            .shutdown(Shutdown::Write)
            .expect("finish runtime request");

        let mut provider = provider("fixture_provider", ProtocolKind::OpenAiChat);
        provider.base_url = format!("http://{upstream_addr}");
        provider.timeout_ms = 300;
        let home = tempfile::tempdir().expect("temp home");
        handle_connection(&mut server, home.path(), &[provider], &RuntimeStats::new())
            .expect("runtime request handled");
        server
            .shutdown(Shutdown::Write)
            .expect("finish runtime response");
        let mut output = Vec::new();
        client
            .read_to_end(&mut output)
            .expect("read runtime response");
        upstream_thread.join().expect("upstream thread completed");

        let headers_end = output
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("runtime response has headers");
        let status: u16 = String::from_utf8_lossy(&output[..headers_end])
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|status| status.parse().ok())
            .expect("runtime response status");
        let response = String::from_utf8_lossy(&output[headers_end + 4..]).into_owned();

        assert_eq!(status, 200);
        assert!(
            response.contains("event: message_stop"),
            "slow-but-trickling stream must survive past the 300ms provider timeout:\
             reqwest blocking wraps each read() in a fresh timeout window rather than\
             capping the whole stream, so total duration may exceed provider.timeout_ms"
        );
    }

    #[test]
    fn runtime_converts_chat_sse_to_anthropic_sse_and_sets_upstream_hop() {
        let upstream = concat!(
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"upstream-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"upstream-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let (status, response, request) = bridged_round_trip(
            "/v1/messages",
            json!({
                "model": "fixture_provider",
                "max_tokens": 16,
                "stream": true,
                "messages": [{"role": "user", "content": "hello"}]
            }),
            provider("fixture_provider", ProtocolKind::OpenAiChat),
            "text/event-stream",
            upstream.as_bytes(),
        );

        assert_eq!(status, 200);
        assert!(response.contains("event: message_start"));
        assert!(response.contains("text_delta"));
        assert!(response.contains("message_stop"));
        assert!(request.starts_with("POST /chat/completions HTTP/1.1"));
        assert!(request.contains("x-spec-runtime-hop: 1"));
    }

    #[test]
    fn runtime_terminates_post_header_incomplete_stream_with_source_error_event() {
        let upstream = concat!(
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"upstream-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\""
        );
        let (status, response, _) = bridged_round_trip(
            "/v1/messages",
            json!({
                "model": "fixture_provider",
                "max_tokens": 16,
                "stream": true,
                "messages": [{"role": "user", "content": "hello"}]
            }),
            provider("fixture_provider", ProtocolKind::OpenAiChat),
            "text/event-stream",
            upstream.as_bytes(),
        );

        assert_eq!(status, 200);
        assert!(response.contains("event: error"));
        assert!(response.contains("api_error"));
        assert!(response.contains("event: message_stop"));
    }

    #[test]
    fn runtime_maps_semantically_invalid_success_response_to_502() {
        let (status, response, _) = bridged_round_trip(
            "/v1/messages",
            json!({
                "model": "fixture_provider",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hello"}]
            }),
            provider("fixture_provider", ProtocolKind::OpenAiChat),
            "application/json",
            br#"{"error":{"message":"provider failed"}}"#,
        );

        assert_eq!(status, 502);
        assert!(response.contains(r#""code":"invalid_upstream""#));
        assert!(
            response.contains(r#""message":"provider failed""#),
            "semantic upstream error text must be relayed to the client: {response}"
        );
    }

    /// Sends a converted request (chat -> anthropic with `thinking` enabled,
    /// so the encoded body carries a top-level `thinking` object) against a
    /// mock upstream that answers with the given status/body per attempt and
    /// captures every received request.
    fn rectifying_round_trip(
        client_body: Value,
        upstream_answers: &[(u16, &[u8])],
    ) -> (u16, String, Vec<String>) {
        let (status, response, requests, _home) =
            rectifying_round_trip_with_home(client_body, upstream_answers);
        (status, response, requests)
    }

    fn rectifying_round_trip_with_home(
        client_body: Value,
        upstream_answers: &[(u16, &[u8])],
    ) -> (u16, String, Vec<String>, std::path::PathBuf) {
        let home = tempfile::tempdir().expect("temp home").keep();
        let upstream = TcpListener::bind("127.0.0.1:0").expect("bind upstream test listener");
        let upstream_addr = upstream.local_addr().expect("upstream address");
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        let answers: Vec<(u16, Vec<u8>)> = upstream_answers
            .iter()
            .map(|(status, body)| (*status, body.to_vec()))
            .collect();
        let upstream_thread = thread::spawn(move || {
            upstream
                .set_nonblocking(true)
                .expect("configure upstream test listener");
            for (status, body) in answers {
                let mut accepted = None;
                for _ in 0..100 {
                    match upstream.accept() {
                        Ok(pair) => {
                            accepted = Some(pair);
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept upstream request: {error}"),
                    }
                }
                let Some((mut socket, _)) = accepted else {
                    return;
                };
                let mut request = [0; 16 * 1024];
                let read = socket.read(&mut request).expect("read upstream request");
                request_tx
                    .send(String::from_utf8_lossy(&request[..read]).into_owned())
                    .expect("send captured upstream request");
                let reason = if status == 200 { "OK" } else { "Bad Request" };
                let headers = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                socket
                    .write_all(headers.as_bytes())
                    .and_then(|_| socket.write_all(&body))
                    .expect("write upstream response");
                socket
                    .shutdown(Shutdown::Write)
                    .expect("finish upstream response");
            }
        });

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind runtime test listener");
        let address = listener.local_addr().expect("runtime address");
        let client = TcpStream::connect(address).expect("connect runtime test client");
        let (mut server, _) = listener.accept().expect("accept runtime test client");
        let mut client = client;
        let body = serde_json::to_vec(&client_body).expect("serialize request body");
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        client
            .write_all(request.as_bytes())
            .and_then(|_| client.write_all(&body))
            .expect("write runtime request");
        client
            .shutdown(Shutdown::Write)
            .expect("finish runtime request");

        let mut provider = provider("fixture_provider", ProtocolKind::AnthropicMessages);
        provider.base_url = format!("http://{upstream_addr}");
        handle_connection(&mut server, &home, &[provider], &RuntimeStats::new())
            .expect("runtime request handled");
        server
            .shutdown(Shutdown::Write)
            .expect("finish runtime response");
        let mut output = Vec::new();
        client
            .read_to_end(&mut output)
            .expect("read runtime response");
        upstream_thread.join().expect("upstream thread completed");

        let mut requests = Vec::new();
        while let Ok(request) = request_rx.recv_timeout(Duration::from_secs(1)) {
            requests.push(request);
        }
        let headers_end = output
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("runtime response has headers");
        let status = String::from_utf8_lossy(&output[..headers_end])
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|status| status.parse().ok())
            .expect("runtime response status");
        let response = String::from_utf8_lossy(&output[headers_end + 4..]).into_owned();
        (status, response, requests, home)
    }

    fn thinking_chat_request(stream: bool) -> Value {
        json!({
            "model": "fixture_provider",
            "thinking": {"type": "enabled"},
            "max_tokens": 32,
            "stream": stream,
            "messages": [{"role": "user", "content": "hello"}]
        })
    }

    #[test]
    fn upstream_thinking_rejection_rectifies_and_retries_once() {
        let ok = include_bytes!("../../tests/fixtures/runtime_bridge/anthropic_text_response.json");
        let (status, response, requests) = rectifying_round_trip(
            thinking_chat_request(false),
            &[
                (
                    400,
                    br#"{"error":{"message":"unsupported field: thinking"}}"#,
                ),
                (200, ok),
            ],
        );

        assert_eq!(status, 200);
        assert!(response.contains("fixture response text"));
        assert_eq!(
            requests.len(),
            2,
            "rejection must trigger exactly one rectified retry"
        );
        assert!(
            requests[0].contains("\"thinking\""),
            "first request must carry the rejected thinking field"
        );
        assert!(
            !requests[1].contains("\"thinking\""),
            "rectified retry must drop the thinking field"
        );
    }

    #[test]
    fn upstream_thinking_rejection_relays_rectified_retry_error() {
        let (status, response, requests) = rectifying_round_trip(
            thinking_chat_request(false),
            &[
                (400, br#"{"error":{"message":"thinking not supported"}}"#),
                (400, br#"{"error":{"message":"still rejected"}}"#),
            ],
        );

        assert_eq!(status, 400);
        assert!(
            response.contains("still rejected"),
            "rectified retry failure must relay its own upstream error text: {response}"
        );
        assert_eq!(
            requests.len(),
            2,
            "rejection must trigger exactly one rectified retry"
        );
        assert!(
            !requests[1].contains("\"thinking\""),
            "rectified retry must drop the thinking field"
        );
    }

    #[test]
    fn upstream_rejection_without_thinking_keyword_is_not_rectified() {
        let (status, response, requests) = rectifying_round_trip(
            thinking_chat_request(false),
            &[(
                400,
                br#"{"error":{"message":"unsupported parameter: temperature"}}"#,
            )],
        );

        assert_eq!(status, 400, "upstream 400 is relayed with its real status");
        assert!(response.contains(r#""code":"invalid_upstream""#));
        assert!(
            response.contains("unsupported parameter: temperature"),
            "non-2xx upstream error text must be relayed to the client: {response}"
        );
        assert_eq!(
            requests.len(),
            1,
            "unrelated rejections must not be retried"
        );
        assert!(requests[0].contains("\"thinking\""));
    }

    #[test]
    fn upstream_thinking_rejection_rectifies_stream_requests() {
        let stream = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"m\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let (status, response, requests) = rectifying_round_trip(
            thinking_chat_request(true),
            &[
                (400, br#"{"error":{"message":"thinking is not supported"}}"#),
                (200, stream.as_bytes()),
            ],
        );

        assert_eq!(status, 200);
        assert!(response.contains(r#""delta":{"content":"ok"}"#));
        assert_eq!(requests.len(), 2);
        assert!(
            !requests[1].contains("\"thinking\""),
            "rectified stream retry must drop the thinking field"
        );
    }

    #[test]
    fn runtime_rejects_inbound_hop_without_dispatching() {
        let request = b"GET /health HTTP/1.1\r\nx-spec-runtime-hop: 1\r\ncontent-length: 0\r\n\r\n";
        let (status, response) = raw_runtime_request(request, &[]);

        assert_eq!(status, 400);
        assert!(response.contains(r#""code":"invalid_request""#));
    }

    #[test]
    fn runtime_maps_oversized_declared_body_to_413() {
        let request = format!(
            "POST /v1/messages HTTP/1.1\r\ncontent-length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        let (status, response) = raw_runtime_request(request.as_bytes(), &[]);

        assert_eq!(status, 413);
        assert!(response.contains(r#""code":"resource_limit""#));
    }

    #[test]
    fn resolves_provider_slug_to_upstream_model() {
        let providers = vec![provider("deepseek", ProtocolKind::OpenAiChat)];

        let (resolved, model) = resolve_model(&providers, "deepseek-chat_deepseek").unwrap();

        assert_eq!(resolved.id, "deepseek");
        assert_eq!(model, "deepseek-chat");
    }

    #[test]
    fn uses_explicit_request_name_over_suffix_stripping() {
        let mut p = provider("zen", ProtocolKind::OpenAiChat);
        p.models = vec!["ds-v4".to_string()];
        p.model_entries.insert(
            "ds-v4".to_string(),
            crate::domain::ModelEntry {
                client_name: "ds-v4".to_string(),
                display_name: "DeepSeek V4".to_string(),
                request_name: "deepseek-v4-flash-free".to_string(),
            },
        );
        let providers = vec![p];

        let (resolved, model) = resolve_model(&providers, "ds-v4_zen").unwrap();

        assert_eq!(resolved.id, "zen");
        assert_eq!(model, "deepseek-v4-flash-free");
    }

    #[test]
    fn uses_explicit_request_name_after_1m_context_suffix() {
        let mut p = provider("zen", ProtocolKind::OpenAiChat);
        p.models = vec!["ds-v4".to_string()];
        p.model_entries.insert(
            "ds-v4".to_string(),
            crate::domain::ModelEntry {
                client_name: "ds-v4".to_string(),
                display_name: "DeepSeek V4".to_string(),
                request_name: "deepseek-v4-flash-free".to_string(),
            },
        );
        let providers = vec![p];

        let (resolved, model) = resolve_model(&providers, "ds-v4_zen[1m]").unwrap();

        assert_eq!(resolved.id, "zen");
        assert_eq!(model, "deepseek-v4-flash-free");
    }

    #[test]
    fn entry_without_request_name_keeps_suffix_stripped_model() {
        let mut p = provider("zen", ProtocolKind::OpenAiChat);
        p.models = vec!["ds-v4".to_string()];
        p.model_entries.insert(
            "ds-v4".to_string(),
            crate::domain::ModelEntry {
                client_name: "ds-v4".to_string(),
                display_name: "DeepSeek V4 (free)".to_string(),
                request_name: "ds-v4".to_string(),
            },
        );
        let providers = vec![p];

        let (resolved, model) = resolve_model(&providers, "ds-v4_zen").unwrap();

        assert_eq!(resolved.id, "zen");
        assert_eq!(model, "ds-v4");
    }

    #[test]
    fn strips_1m_context_suffix_before_provider_slug() {
        let providers = vec![provider("zen", ProtocolKind::OpenAiChat)];

        let (resolved, model) = resolve_model(&providers, "deepseek-chat_zen[1m]").unwrap();

        assert_eq!(resolved.id, "zen");
        assert_eq!(model, "deepseek-chat");
    }

    #[test]
    fn bare_1m_suffix_is_left_untouched() {
        let providers = [provider("zen_provider", ProtocolKind::OpenAiChat)];
        assert_eq!(strip_context_suffix("[1m]"), "[1m]");
        assert_eq!(strip_context_suffix("[1M]"), "[1M]");
        let _ = providers;
    }

    #[test]
    fn strips_uppercase_1m_context_suffix_without_provider_slug() {
        let providers = vec![provider("zen", ProtocolKind::OpenAiChat)];

        let (resolved, model) = resolve_model(&providers, "deepseek-chat[1M]").unwrap();

        assert_eq!(resolved.id, "zen");
        assert_eq!(model, "deepseek-chat");
    }

    #[test]
    fn resolve_model_rejects_unknown_model_before_credentials() {
        let mut provider = provider("zen", ProtocolKind::OpenAiChat);
        provider.api_key = "provider-secret".into();
        provider.models = vec!["configured".into()];
        let error = resolve_model(&[provider], "not-configured_fixture").unwrap_err();
        assert_eq!(
            error,
            ModelResolutionError::NotConfigured("not-configured_fixture".into())
        );
    }

    #[test]
    fn resolve_model_accepts_client_and_request_names() {
        let mut provider = provider("zen", ProtocolKind::OpenAiChat);
        provider.models = vec!["client-key".into()];
        provider.model_entries.insert(
            "client-key".into(),
            crate::domain::ModelEntry {
                client_name: "client-key".into(),
                display_name: "Client key".into(),
                request_name: "upstream-name".into(),
            },
        );
        let providers = vec![provider];
        assert_eq!(
            resolve_model(&providers, "client-key_zen").unwrap().1,
            "upstream-name"
        );
        assert_eq!(
            resolve_model(&providers, "upstream-name_zen").unwrap().1,
            "upstream-name"
        );
    }

    #[test]
    fn resolve_model_requires_provider_suffix_when_multiple_providers_exist() {
        let providers = vec![
            provider("zen", ProtocolKind::OpenAiChat),
            provider("other", ProtocolKind::OpenAiChat),
        ];
        assert!(matches!(
            resolve_model(&providers, "deepseek-chat"),
            Err(ModelResolutionError::Ambiguous(slug)) if slug == "deepseek-chat"
        ));
    }

    #[test]
    fn count_tokens_returns_input_token_estimate() {
        let providers = vec![provider("zen", ProtocolKind::AnthropicMessages)];
        let body = serde_json::to_vec(&json!({
            "model": "deepseek-v4-flash-free",
            "messages": [{ "role": "user", "content": "hello world" }]
        }))
        .expect("serialize count_tokens body");
        let request = http_request_for_test("POST", "/v1/messages/count_tokens?beta=true", body, 0);

        let dir = tempfile::tempdir().expect("temp dir");
        let (status, _, response_body) = handle_request_for_test(request, dir.path(), &providers)
            .expect("count_tokens request handled");

        assert_eq!(status, 200);
        let response: Value =
            serde_json::from_slice(&response_body).expect("count_tokens response is JSON");
        let keys = response.as_object().map(|object| object.len());
        assert_eq!(keys, Some(1), "response must contain only input_tokens");
        assert!(
            response
                .get("input_tokens")
                .and_then(Value::as_u64)
                .expect("input_tokens is a number")
                > 0
        );
    }

    #[test]
    fn count_tokens_rejects_missing_messages() {
        let providers = vec![provider("zen", ProtocolKind::AnthropicMessages)];
        let body =
            serde_json::to_vec(&json!({ "model": "x" })).expect("serialize count_tokens body");
        let request = http_request_for_test("POST", "/v1/messages/count_tokens", body, 0);

        let dir = tempfile::tempdir().expect("temp dir");
        let (status, _, _) = handle_request_for_test(request, dir.path(), &providers)
            .expect("count_tokens request handled");

        assert_eq!(status, 400);
    }

    #[test]
    fn bridge_accepts_streaming_requests_for_runtime_dispatch() {
        let request = bridge::parse_request(
            bridge::WireProtocol::OpenAiChat,
            &json!({
                "model": "fixture",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": true
            }),
        )
        .expect("streaming request parses");

        assert!(request.stream);
    }

    #[test]
    fn health_reports_uptime_and_connection_stats() {
        let providers = vec![provider("test", ProtocolKind::OpenAiChat)];
        let health = health_response(&providers, None);
        assert!(health.get("uptime_seconds").is_some());
        assert!(health.get("active_connections").is_some());
        assert!(health.get("success_count").is_some());
        assert!(health.get("failure_count").is_some());
    }

    #[test]
    fn health_reports_shared_runtime_counters_when_stats_are_provided() {
        let stats = RuntimeStats::new();
        stats.success_count.store(3, Ordering::SeqCst);
        stats.failure_count.store(1, Ordering::SeqCst);
        stats.active_connections.store(1, Ordering::SeqCst);

        let started = stats.started_at;
        let before = Instant::now();
        let health = health_response(&[provider("test", ProtocolKind::OpenAiChat)], Some(&stats));
        let after = Instant::now();

        let uptime = health["uptime_seconds"]
            .as_u64()
            .expect("uptime_seconds is a number");
        assert!(
            uptime >= before.duration_since(started).as_secs()
                && uptime <= after.duration_since(started).as_secs(),
            "uptime_seconds {uptime} must be within the elapsed window"
        );
        assert_eq!(health["active_connections"], json!(1));
        assert_eq!(health["success_count"], json!(3));
        assert_eq!(health["failure_count"], json!(1));
    }

    #[test]
    fn panicked_worker_returns_counters_to_zero_and_unblocks_drain() {
        let stats = Arc::new(RuntimeStats::new());
        let worker = std::thread::spawn({
            let stats = Arc::clone(&stats);
            move || {
                let mut count_guard = ConnectionCountGuard::new(Arc::clone(&stats));
                count_guard.mark_active();
                panic!("simulated worker panic after counters incremented");
            }
        });
        assert!(
            worker.join().is_err(),
            "simulated panic must propagate through join"
        );

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn({
            let stats = Arc::clone(&stats);
            move || {
                wait_for_drain(&stats);
                let _ = done_tx.send(());
            }
        });
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("drain must complete after worker panic");
        assert_eq!(stats.pending_connections.load(Ordering::SeqCst), 0);
        assert_eq!(stats.active_connections.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn health_response_reports_runtime_limits() {
        let value = health_response(&[provider("deepseek", ProtocolKind::OpenAiChat)], None);

        assert_eq!(value["ok"], true);
        assert_eq!(value["provider_count"], 1);
        assert_eq!(
            value["capabilities"]["entry_points"]["anthropic_messages"],
            true
        );
        assert_eq!(value["capabilities"]["entry_points"]["openai_chat"], true);
        assert_eq!(
            value["capabilities"]["entry_points"]["openai_responses"],
            true
        );
        assert_eq!(value["capabilities"]["streaming"], true);
        assert_eq!(value["capabilities"]["tools"], true);
        assert_eq!(value["capabilities"]["tool_execution"], false);
        assert_eq!(value["capabilities"]["reasoning"], "partial");
        assert_eq!(value["limits"]["streaming"], true);
        assert_eq!(value["limits"]["streaming_conversion"], true);
        assert_eq!(value["limits"]["streaming_mode"], "protocol_conversion");
        assert_eq!(value["limits"]["tools"], true);
        assert_eq!(value["limits"]["text_only"], false);
    }

    #[test]
    fn streaming_routes_convert_chat_and_responses_clients_when_upstream_is_chat() {
        assert!(stream_route_converts(
            "/v1/messages",
            ProtocolKind::OpenAiChat
        ));
        assert!(stream_route_converts(
            "/v1/responses",
            ProtocolKind::OpenAiChat
        ));
        assert!(!stream_route_converts(
            "/v1/messages",
            ProtocolKind::AnthropicMessages
        ));
    }

    #[test]
    fn converts_responses_string_input_to_chat_messages() {
        let messages = responses_to_chat_messages(&json!({ "input": "hello" })).unwrap();

        assert_eq!(
            messages,
            vec![json!({ "role": "user", "content": "hello" })]
        );
    }

    #[test]
    fn tool_result_precedes_follow_up_text_in_converted_messages() {
        let messages = anthropic_to_chat_messages(&json!({
            "model": "deepseek-v4-flash-free_zen",
            "messages": [
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "call_00_o4dFLFuIIqw34wLGEZHE4621",
                        "name": "Skill",
                        "input": { "skill": "run" }
                    }]
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "call_00_o4dFLFuIIqw34wLGEZHE4621",
                            "content": "Launching skill: run"
                        },
                        {
                            "type": "text",
                            "text": "Base directory for this skill: /tmp/skills/run"
                        }
                    ]
                }
            ]
        }))
        .unwrap();

        assert_eq!(messages[0]["role"], json!("assistant"));
        assert_eq!(
            messages[0]["tool_calls"][0]["id"],
            json!("call_00_o4dFLFuIIqw34wLGEZHE4621")
        );
        assert_eq!(
            messages[1],
            json!({
                "role": "tool",
                "tool_call_id": "call_00_o4dFLFuIIqw34wLGEZHE4621",
                "content": "Launching skill: run"
            })
        );
        assert_eq!(
            messages[2],
            json!({
                "role": "user",
                "content": "Base directory for this skill: /tmp/skills/run"
            })
        );
    }

    #[test]
    fn claude_code_style_body_keeps_user_instruction() {
        let input = json!({
            "model": "deepseek-v4-flash-free_zen",
            "max_tokens": 8000,
            "stream": true,
            "system": [{
                "type": "text",
                "text": "You are Claude Code, a coding assistant.",
                "cache_control": { "type": "ephemeral" }
            }],
            "tools": [{
                "name": "Bash",
                "description": "Run a bash command",
                "input_schema": { "type": "object", "properties": { "command": { "type": "string" } } }
            }],
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "Reply with exactly: SUCCESS-19316",
                    "cache_control": { "type": "ephemeral" }
                }]
            }]
        });

        let messages = anthropic_to_chat_messages(&input).unwrap();

        assert_eq!(
            messages[0],
            json!({ "role": "system", "content": "You are Claude Code, a coding assistant." })
        );
        assert_eq!(
            messages[1],
            json!({ "role": "user", "content": "Reply with exactly: SUCCESS-19316" })
        );
        let tools = anthropic_tools_to_openai(&input).unwrap();
        assert_eq!(tools[0]["function"]["name"], json!("Bash"));
    }

    #[test]
    fn responses_to_chat_preserves_instructions_and_developer_role() {
        let messages = responses_to_chat_messages(&json!({
            "instructions": "be concise",
            "input": [{ "role": "developer", "content": [{ "type": "input_text", "text": "prefer bullets" }] }, { "role": "user", "content": "hello" }]
        }))
        .unwrap();

        assert_eq!(
            messages[0],
            json!({ "role": "system", "content": "be concise" })
        );
        assert_eq!(
            messages[1],
            json!({ "role": "system", "content": "prefer bullets" })
        );
        assert_eq!(messages[2], json!({ "role": "user", "content": "hello" }));
    }

    #[test]
    fn responses_to_chat_preserves_function_call_turns() {
        let messages = responses_to_chat_messages(&json!({
            "input": [
                {
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "check weather" }]
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "22C sunny"
                }
            ]
        }))
        .unwrap();

        assert_eq!(messages[1]["role"], json!("assistant"));
        assert_eq!(messages[1]["tool_calls"][0]["id"], json!("call_1"));
        assert_eq!(
            messages[2],
            json!({
                "role": "tool",
                "tool_call_id": "call_1",
                "content": "22C sunny"
            })
        );
    }

    #[test]
    fn responses_chat_request_maps_tools_and_disables_reasoning() {
        let body = responses_to_chat_request(
            &json!({
                "model": "deepseek-v4-flash-free_zen",
                "input": "hello",
                "tools": [{
                    "type": "function",
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": { "type": "object", "properties": {} }
                }],
                "tool_choice": { "type": "function", "name": "get_weather" }
            }),
            "deepseek-v4-flash-free",
        )
        .unwrap();

        assert_eq!(body["model"], json!("deepseek-v4-flash-free"));
        assert_eq!(body["messages"][0]["content"], json!("hello"));
        assert_eq!(body["reasoning_effort"], json!("none"));
        assert_eq!(body["tools"][0]["function"]["name"], json!("get_weather"));
        assert_eq!(
            body["tool_choice"],
            json!({ "type": "function", "function": { "name": "get_weather" } })
        );
    }

    #[test]
    fn anthropic_stop_reason_maps_openai_to_anthropic_values() {
        assert_eq!(anthropic_stop_reason(Some("tool_calls")), "tool_use");
        assert_eq!(anthropic_stop_reason(Some("stop")), "end_turn");
        assert_eq!(anthropic_stop_reason(Some("length")), "max_tokens");
        assert_eq!(anthropic_stop_reason(Some("content_filter")), "refusal");
        assert_eq!(anthropic_stop_reason(None), "end_turn");
        assert_eq!(anthropic_stop_reason(Some("unknown_x")), "end_turn");
    }

    #[test]
    fn anthropic_usage_normalizes_openai_token_fields() {
        let usage = anthropic_usage(&json!({
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "total_tokens": 120
        }));

        assert_eq!(usage["input_tokens"], json!(100));
        assert_eq!(usage["output_tokens"], json!(20));
    }

    #[test]
    fn tool_block_tracker_starts_each_tool_once_by_index() {
        let mut tracker = AnthropicToolTracker::new();

        let first = tracker.start(0, "call_1", "get_weather");
        assert!(first.is_some());
        let second = tracker.start(0, "call_1", "get_weather");
        assert!(second.is_none(), "same tool_index must not re-start");
        let next = tracker.start(1, "call_2", "Bash");
        assert!(next.is_some());

        let stopped = tracker.stop();
        assert_eq!(
            stopped,
            vec![
                json!({ "type": "content_block_stop", "index": 0 }),
                json!({ "type": "content_block_stop", "index": 1 })
            ]
        );
    }

    #[test]
    fn chat_usage_maps_to_responses_usage_schema() {
        let usage = chat_usage_to_responses(&json!({
            "prompt_tokens": 18,
            "completion_tokens": 11,
            "total_tokens": 29,
            "prompt_tokens_details": { "cached_tokens": 3 }
        }));

        assert_eq!(usage["input_tokens"], json!(18));
        assert_eq!(usage["output_tokens"], json!(11));
        assert_eq!(usage["total_tokens"], json!(29));
        assert_eq!(usage["input_tokens_details"]["cached_tokens"], json!(3));
    }

    #[test]
    fn responses_to_anthropic_moves_system_roles_to_system_field() {
        let body = responses_to_anthropic(&json!({
            "model": "claude_foo",
            "instructions": "be concise",
            "max_output_tokens": 123,
            "temperature": 0.2,
            "input": [{ "role": "system", "content": "system note" }, { "role": "user", "content": "hello" }]
        }))
        .unwrap();

        assert_eq!(body["system"], json!("be concise\n\nsystem note"));
        assert_eq!(body["max_tokens"], json!(123));
        assert_eq!(body["temperature"], json!(0.2));
        assert_eq!(
            body["messages"],
            json!([{ "role": "user", "content": "hello" }])
        );
    }

    #[test]
    fn zen_chat_requests_normalize_developer_roles_only_for_zen() {
        let mut body = json!({
            "messages": [
                {"role": "developer", "content": "fixture"},
                {"role": "user", "content": "hello"}
            ]
        });
        normalize_provider_request(
            &provider("zen", ProtocolKind::OpenAiChat),
            bridge::WireProtocol::OpenAiChat,
            &mut body,
        );
        assert_eq!(body["messages"][0]["role"], json!("system"));

        let mut body = json!({
            "messages": [{"role": "developer", "content": "fixture"}]
        });
        normalize_provider_request(
            &provider("other", ProtocolKind::OpenAiChat),
            bridge::WireProtocol::OpenAiChat,
            &mut body,
        );
        assert_eq!(body["messages"][0]["role"], json!("developer"));
    }

    const MAX_CONCURRENT_CONNECTIONS_TEST: usize = 2;

    #[test]
    fn serve_processes_second_connection_while_first_is_stalled() {
        let providers = vec![provider("test", ProtocolKind::OpenAiChat)];
        let home = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let slots = std::sync::Arc::new(ConnectionSlots::new(MAX_CONCURRENT_CONNECTIONS_TEST));
            for stream in listener.incoming().take(2) {
                let Ok(mut stream) = stream else { continue };
                let providers = providers.clone();
                let home = home.path().to_path_buf();
                let slots = std::sync::Arc::clone(&slots);
                std::thread::spawn(move || {
                    let _guard = slots.acquire();
                    let _ = handle_connection(&mut stream, &home, &providers, &RuntimeStats::new());
                });
            }
        });
        let mut stalled = TcpStream::connect(address).unwrap();
        stalled.write_all(b"GET /health HTTP/1.1\r\n").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let mut fast = TcpStream::connect(address).unwrap();
        fast.write_all(b"GET /health HTTP/1.1\r\nhost: x\r\n\r\n")
            .unwrap();
        let mut buf = Vec::new();
        fast.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        fast.read_to_end(&mut buf).unwrap();
        assert!(
            String::from_utf8_lossy(&buf).contains("200 OK"),
            "second connection must be served while first stalls"
        );
        server.join().unwrap();
    }

    #[test]
    fn connection_slots_blocks_acquire_over_capacity_until_release() {
        let slots = std::sync::Arc::new(ConnectionSlots::new(1));

        let slots_holder = std::sync::Arc::clone(&slots);
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let _guard = slots_holder.acquire();
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        held_rx.recv().unwrap();

        let slots_waiter = std::sync::Arc::clone(&slots);
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel::<()>();
        let waiter = std::thread::spawn(move || {
            let _guard = slots_waiter.acquire();
            acquired_tx.send(()).unwrap();
        });

        assert!(
            acquired_rx
                .recv_timeout(Duration::from_millis(300))
                .is_err(),
            "acquire() must block (not return, not reject) while the single slot is held"
        );

        release_tx.send(()).unwrap();
        holder.join().unwrap();
        acquired_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("queued acquire() must succeed once the slot is released");
        waiter.join().unwrap();
    }

    #[test]
    fn slow_client_read_times_out() {
        let providers = vec![provider("test", ProtocolKind::OpenAiChat)];
        let home = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            handle_connection_with_timeout(
                &mut stream,
                home.path(),
                &providers,
                Duration::from_millis(200),
                &RuntimeStats::new(),
            )
        });
        let mut client = TcpStream::connect(address).unwrap();
        let _ = client.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = [0u8; 512];
        // EINTR 偶发（并行测试环境）；重试一次。
        let read = match client.read(&mut buf) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                client.read(&mut buf).unwrap()
            }
            Err(error) => panic!("read failed: {error}"),
        };
        let server_result = server.join().unwrap();
        // 超时分支已写出 504 信封并返回 Ok（防外层补写第二响应）。
        assert!(server_result.is_ok(), "timeout must not double-respond");
        assert!(
            read == 0 || String::from_utf8_lossy(&buf[..read]).contains("504"),
            "client must see timeout response"
        );
    }

    fn provider(id: &str, protocol: ProtocolKind) -> ProviderProfile {
        ProviderProfile {
            id: id.to_string(),
            name: id.to_string(),
            notes: None,
            website: None,
            vendor: ProviderVendor::CustomOpenAiCompatible,
            protocol,
            base_url: "https://example.com/v1".to_string(),
            api_key: "secret".to_string(),
            models: vec!["deepseek-chat".to_string(), "fixture_provider".to_string()],
            model_entries: BTreeMap::new(),
            model_metadata: BTreeMap::new(),
            claude_slots: BTreeMap::new(),
            default_model: "deepseek-chat".to_string(),
            extra_headers: BTreeMap::new(),
            request_url_mode: None,
            header_mode: None,
            timeout_ms: 10_000,
            max_retries: 0,
            context_window: 128_000,
            max_output_tokens: 4096,
            reasoning_effort: None,
            cache_mode: CacheMode::Auto,
        }
    }

    #[test]
    fn empty_api_key_omits_authorization() {
        let mut provider = provider("fixture", ProtocolKind::OpenAiChat);
        provider.base_url = "https://example.test/v1".into();
        provider.api_key.clear();
        provider.extra_headers.clear();
        let request =
            build_upstream_request(&provider, bridge::WireProtocol::OpenAiChat, &json!({}))
                .unwrap()
                .build()
                .unwrap();
        assert!(request.headers().get("authorization").is_none());
        assert!(request.headers().get("x-api-key").is_none());
    }

    #[test]
    fn relay_from_error_json_extracts_error_message_shapes() {
        assert_eq!(
            super::relay_from_error_json(&json!({"error": {"message": "rate limit exceeded"}})),
            Some("rate limit exceeded".to_string())
        );
        assert_eq!(
            super::relay_from_error_json(&json!({"message": "quota exhausted"})),
            Some("quota exhausted".to_string())
        );
        assert_eq!(
            super::relay_from_error_json(&json!({"error": "no object"})),
            None
        );
        assert_eq!(
            super::relay_from_error_json(&json!({"error": {"message": "   "}})),
            None,
            "blank messages are not relayed"
        );
        assert_eq!(super::relay_from_error_json(&json!({"ok": true})), None);
    }

    #[test]
    fn relay_from_error_json_truncates_long_messages_to_512_chars() {
        let long = format!("limit: {}", "x".repeat(600));
        let relay = super::relay_from_error_json(&json!({"error": {"message": long}})).unwrap();
        assert_eq!(relay.len(), 512);
        assert!(relay.starts_with("limit: "));
        assert!(relay.ends_with('x'));
    }

    #[test]
    fn relay_from_error_json_sanitizes_ansi_and_control_characters() {
        let relay = super::relay_from_error_json(&json!({
            "error": {"message": "\u{1b}[31mred\u{1b}[0m\u{7}done\u{85}"}
        }))
        .unwrap();
        assert_eq!(
            relay, "reddone",
            "ESC/C0/C1 must be stripped from relayed text"
        );
        assert!(
            !relay.contains('\u{1b}'),
            "relayed text must not carry ESC bytes"
        );
    }

    #[test]
    fn relay_from_error_json_preserves_tab_newline_and_unicode() {
        let relay = super::relay_from_error_json(&json!({
            "error": {"message": "first\tline\nsecond 中文"}
        }))
        .unwrap();
        assert_eq!(relay, "first\tline\nsecond 中文");
    }

    #[test]
    fn upstream_error_relay_strips_ansi_escape_sequences() {
        let body = r#"{"error":{"message":"\u001b[31mdenied\u001b[0m \tbye"}}"#;
        let (status, response, requests) =
            rectifying_round_trip(thinking_chat_request(false), &[(400, body.as_bytes())]);

        assert_eq!(status, 400);
        assert!(
            response.contains("denied"),
            "relayed message must keep the visible text: {response}"
        );
        assert!(
            response.contains("\\tbye"),
            "relayed message must keep tab characters (JSON-escaped): {response}"
        );
        assert!(
            !response.contains('\u{1b}'),
            "relayed message must not leak ESC escape sequences: {response}"
        );
        assert_eq!(requests.len(), 1);
    }

    /// Sends a passthrough request (client wire protocol == provider wire
    /// protocol) through the runtime against a raw TCP mock upstream that
    /// answers with the given body. Returns the client-visible status, the
    /// response body after the header terminator and the home dir used for
    /// stats recording.
    fn passthrough_round_trip(
        client_body: Value,
        upstream_body: &[u8],
    ) -> (u16, Vec<u8>, std::path::PathBuf) {
        let home = tempfile::tempdir().expect("temp home").keep();
        let upstream = TcpListener::bind("127.0.0.1:0").expect("bind upstream test listener");
        let upstream_addr = upstream.local_addr().expect("upstream address");
        let upstream_body = upstream_body.to_vec();
        let upstream_thread = thread::spawn(move || {
            let (mut socket, _) = upstream.accept().expect("accept upstream request");
            let mut request = [0; 16 * 1024];
            let _ = socket.read(&mut request).expect("read upstream request");
            let headers = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                upstream_body.len()
            );
            socket
                .write_all(headers.as_bytes())
                .and_then(|_| socket.write_all(&upstream_body))
                .expect("write upstream response");
            socket
                .shutdown(Shutdown::Write)
                .expect("finish upstream response");
        });

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind runtime test listener");
        let address = listener.local_addr().expect("runtime address");
        let client = TcpStream::connect(address).expect("connect runtime test client");
        let (mut server, _) = listener.accept().expect("accept runtime test client");
        let mut client = client;
        let body = serde_json::to_vec(&client_body).expect("serialize request body");
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        client
            .write_all(request.as_bytes())
            .and_then(|_| client.write_all(&body))
            .expect("write runtime request");
        client
            .shutdown(Shutdown::Write)
            .expect("finish runtime request");

        let mut provider = provider("fixture_provider", ProtocolKind::OpenAiChat);
        provider.base_url = format!("http://{upstream_addr}");
        handle_connection(&mut server, &home, &[provider], &RuntimeStats::new())
            .expect("runtime request handled");
        server
            .shutdown(Shutdown::Write)
            .expect("finish runtime response");
        let mut output = Vec::new();
        client
            .read_to_end(&mut output)
            .expect("read runtime response");
        upstream_thread.join().expect("upstream thread completed");

        let headers_end = output
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("runtime response has headers");
        let status = String::from_utf8_lossy(&output[..headers_end])
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|status| status.parse().ok())
            .expect("runtime response status");
        (status, output[headers_end + 4..].to_vec(), home)
    }

    fn read_stats_records(home: &Path) -> Vec<crate::stats::UsageRecord> {
        let dir = home.join(".codex").join("stats");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut records = Vec::new();
        for entry in entries.flatten() {
            let Ok(content) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for line in content.lines() {
                if let Some(record) = crate::stats::parse_record(line) {
                    records.push(record);
                }
            }
        }
        records
    }

    #[test]
    fn passthrough_stream_records_merged_usage_and_forwards_bytes_exactly() {
        let stream = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"cache_read_input_tokens\":1}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"usage of tokens\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":10,\"output_tokens\":9,\"cache_read_input_tokens\":3}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let body = json!({
            "model": "fixture_provider",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        });
        let (status, forwarded, home) = passthrough_round_trip(body, stream.as_bytes());

        assert_eq!(status, 200);
        assert_eq!(
            forwarded,
            stream.as_bytes(),
            "passthrough streaming must forward upstream bytes byte-for-byte"
        );
        let records = read_stats_records(&home);
        assert_eq!(
            records.len(),
            1,
            "exactly one record for one passthrough stream"
        );
        let record = &records[0];
        assert_eq!(record.source, crate::stats::SOURCE_PASSTHROUGH_STREAM);
        assert_eq!(record.input, 10);
        assert_eq!(record.output, 9);
        assert_eq!(record.cached, 3);
        assert_eq!(record.cache_creation, 0);
        assert_eq!(
            record.semantics,
            crate::stats::SEMANTICS_FRESH,
            "Anthropic-shaped usage reports the billed fresh input"
        );
        assert_eq!(record.status_code, Some(200));
        assert!(record.is_streaming);
        assert!(record.latency_ms.is_some());
        assert_eq!(record.error, None);
        assert_eq!(record.path, "/v1/chat/completions");
    }

    #[test]
    fn passthrough_stream_without_usage_frame_records_nothing() {
        let stream = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"content\":[]}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let body = json!({
            "model": "fixture_provider",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        });
        let (status, forwarded, home) = passthrough_round_trip(body, stream.as_bytes());

        assert_eq!(status, 200);
        assert_eq!(forwarded, stream.as_bytes());
        assert!(
            read_stats_records(&home).is_empty(),
            "no usage frame means no stats record for the passthrough stream"
        );
    }

    #[test]
    fn passthrough_stream_records_cache_only_usage_with_cache_creation() {
        // A cache-only completion (input 0 but cache read > 0) must not be
        // dropped, and the cache-creation bucket is captured separately.
        let stream = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":0,\"output_tokens\":0,\"cache_read_input_tokens\":50,\"cache_creation_input_tokens\":5}}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":0,\"output_tokens\":10}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let body = json!({
            "model": "fixture_provider",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        });
        let (status, _forwarded, home) = passthrough_round_trip(body, stream.as_bytes());
        assert_eq!(status, 200);

        let records = read_stats_records(&home);
        assert_eq!(records.len(), 1, "cache-only request must be recorded");
        assert_eq!(records[0].input, 0);
        assert_eq!(records[0].cached, 50);
        assert_eq!(records[0].cache_creation, 5);
    }

    #[test]
    fn passthrough_stream_prefers_smaller_delta_input() {
        // CC Switch rule: some upstreams report the cache-inclusive input in
        // message_start and a corrected (smaller) fresh input in
        // message_delta; the smaller positive delta input wins, together
        // with its cache counts.
        let stream = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1200,\"output_tokens\":0,\"cache_read_input_tokens\":1100}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":100,\"output_tokens\":45,\"cache_read_input_tokens\":0}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let body = json!({
            "model": "fixture_provider",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        });
        let (status, forwarded, home) = passthrough_round_trip(body, stream.as_bytes());
        assert_eq!(status, 200);
        assert_eq!(forwarded, stream.as_bytes());

        let records = read_stats_records(&home);
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].input, 100,
            "the smaller positive delta input must replace the start input"
        );
        assert_eq!(
            records[0].cached, 0,
            "delta cache counts win with its input"
        );
        assert_eq!(records[0].output, 45);
    }

    #[test]
    fn passthrough_usage_frame_split_across_chunks_is_still_found() {
        let frame = "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":7,\"output_tokens\":3}}\n\n";
        let cut = frame.len() / 2;
        let (first, rest) = frame.split_at(cut);
        let mut pending = Vec::new();
        assert!(
            scan_passthrough_usage_frames(&mut pending).is_empty(),
            "nothing to scan before any bytes arrive"
        );
        pending.extend_from_slice(first.as_bytes());
        pending.extend_from_slice(rest.as_bytes());
        let frames = scan_passthrough_usage_frames(&mut pending);
        assert_eq!(
            frames.len(),
            1,
            "a usage frame split across chunks must be found"
        );
        assert_eq!(frames[0]["input_tokens"], 7);
        assert_eq!(frames[0]["output_tokens"], 3);
        assert!(
            pending.is_empty(),
            "consumed lines are dropped from pending"
        );
    }

    #[test]
    fn passthrough_stream_merges_usage_across_frames_fieldwise() {
        // Anthropic-style stream: input/cache live in message_start, output in
        // message_delta. Merging must keep both (taking only the last frame
        // would lose the input counts).
        let start = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1200,\"cache_read_input_tokens\":1100}}}\n\n";
        let delta = "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":45}}\n\n";
        let mut pending = Vec::new();
        pending.extend_from_slice(start.as_bytes());
        pending.extend_from_slice(delta.as_bytes());
        let frames = scan_passthrough_usage_frames(&mut pending);
        assert_eq!(frames.len(), 2, "both frames carry a usage object");

        let mut merged: Option<Value> = None;
        for frame in &frames {
            merge_usage_frame(&mut merged, frame);
        }
        let merged = merged.expect("merged usage");
        assert_eq!(merged["input_tokens"], 1200);
        assert_eq!(merged["cache_read_input_tokens"], 1100);
        assert_eq!(
            merged["output_tokens"], 45,
            "output from message_delta must survive the merge"
        );
    }

    #[test]
    fn passthrough_nonstream_records_usage_from_response_json() {
        let upstream = br#"{
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "model": "fixture-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}}],
            "usage": {"prompt_tokens": 21, "completion_tokens": 9, "total_tokens": 30,
                      "prompt_tokens_details": {"cached_tokens": 4}}
        }"#;
        let body = json!({
            "model": "fixture_provider",
            "messages": [{"role": "user", "content": "hello"}]
        });
        let (status, forwarded, home) = passthrough_round_trip(body, upstream);

        assert_eq!(status, 200);
        assert_eq!(
            forwarded, upstream,
            "passthrough must relay bytes untouched"
        );
        let records = read_stats_records(&home);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, crate::stats::SOURCE_PASSTHROUGH);
        assert_eq!(records[0].input, 21);
        assert_eq!(records[0].output, 9);
        assert_eq!(records[0].cached, 4);
        assert_eq!(records[0].cache_creation, 0);
        assert_eq!(
            records[0].semantics,
            crate::stats::SEMANTICS_LEGACY,
            "OpenAI prompt_tokens includes the cached prefix; no write figure is reported"
        );
        assert_eq!(records[0].status_code, Some(200));
        assert!(!records[0].is_streaming);
        assert!(records[0].latency_ms.is_some());
    }

    #[test]
    fn converted_nonstream_records_usage_from_decoded_response() {
        let ok = include_bytes!("../../tests/fixtures/runtime_bridge/anthropic_text_response.json");
        let (status, response, requests, home) =
            rectifying_round_trip_with_home(thinking_chat_request(false), &[(200, ok)]);

        assert_eq!(status, 200);
        assert!(response.contains("fixture response text"));
        assert_eq!(requests.len(), 1);
        let records = read_stats_records(&home);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, crate::stats::SOURCE_CONVERTED);
        assert_eq!(
            records[0].input, 14,
            "UsageIr input_tokens is the full input including cache-read tokens (12+2)"
        );
        assert_eq!(records[0].output, 4);
        assert_eq!(records[0].cached, 2);
        assert_eq!(records[0].cache_creation, 0);
        assert_eq!(
            records[0].semantics,
            crate::stats::SEMANTICS_TOTAL,
            "bridge IR input is the full count (fresh + cache read + cache write)"
        );
        assert_eq!(records[0].status_code, Some(200));
        assert!(!records[0].is_streaming);
        assert!(records[0].latency_ms.is_some());
        assert_eq!(records[0].error, None);
    }

    #[test]
    fn converted_stream_records_merged_usage_from_stream_state() {
        let stream =
            include_bytes!("../../tests/fixtures/runtime_bridge/anthropic_text_stream.sse");
        let (status, response, _requests, home) =
            rectifying_round_trip_with_home(thinking_chat_request(true), &[(200, stream)]);

        assert_eq!(status, 200);
        assert!(
            response.contains("[DONE]"),
            "chat-encoded stream must end with the [DONE] marker"
        );
        let records = read_stats_records(&home);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, crate::stats::SOURCE_CONVERTED_STREAM);
        assert_eq!(records[0].input, 12);
        assert_eq!(records[0].output, 4);
        assert_eq!(records[0].semantics, crate::stats::SEMANTICS_TOTAL);
        assert_eq!(records[0].status_code, Some(200));
        assert!(records[0].is_streaming);
        assert!(records[0].latency_ms.is_some());
    }

    #[test]
    fn converted_stream_still_records_zero_usage_request_without_usage_frames() {
        let stream = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"m\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let (status, response, _requests, home) = rectifying_round_trip_with_home(
            thinking_chat_request(true),
            &[(200, stream.as_bytes())],
        );

        assert_eq!(status, 200);
        assert!(
            response.contains("[DONE]"),
            "chat-encoded stream must end with the [DONE] marker"
        );
        let records = read_stats_records(&home);
        assert_eq!(
            records.len(),
            1,
            "request count is recorded even without usage"
        );
        assert_eq!(records[0].source, crate::stats::SOURCE_CONVERTED_STREAM);
        assert_eq!(records[0].input, 0);
        assert_eq!(records[0].output, 0);
        assert_eq!(records[0].status_code, Some(200));
    }

    #[test]
    fn instrumentation_does_not_block_when_upstream_fails() {
        let body = json!({
            "model": "fixture_provider",
            "messages": [{"role": "user", "content": "hello"}]
        });
        let home = tempfile::tempdir().expect("temp home");
        let upstream = TcpListener::bind("127.0.0.1:0").expect("bind upstream test listener");
        let upstream_addr = upstream.local_addr().expect("upstream address");
        let upstream_thread = thread::spawn(move || {
            let (mut socket, _) = upstream.accept().expect("accept upstream request");
            let mut request = [0; 16 * 1024];
            let _ = socket.read(&mut request).expect("read upstream request");
            let error_body = br#"{"error":{"message":"upstream exploded"}}"#;
            let headers = format!(
                "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                error_body.len()
            );
            socket
                .write_all(headers.as_bytes())
                .and_then(|_| socket.write_all(error_body))
                .expect("write upstream error");
            socket
                .shutdown(Shutdown::Write)
                .expect("finish upstream response");
        });

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind runtime test listener");
        let address = listener.local_addr().expect("runtime address");
        let client = TcpStream::connect(address).expect("connect runtime test client");
        let (mut server, _) = listener.accept().expect("accept runtime test client");
        let mut client = client;
        let body = serde_json::to_vec(&body).expect("serialize request body");
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        client
            .write_all(request.as_bytes())
            .and_then(|_| client.write_all(&body))
            .expect("write runtime request");
        client
            .shutdown(Shutdown::Write)
            .expect("finish runtime request");

        let mut provider = provider("fixture_provider", ProtocolKind::OpenAiChat);
        provider.base_url = format!("http://{upstream_addr}");
        handle_connection(&mut server, home.path(), &[provider], &RuntimeStats::new())
            .expect("runtime request handled");
        server
            .shutdown(Shutdown::Write)
            .expect("finish runtime response");
        let mut output = Vec::new();
        client
            .read_to_end(&mut output)
            .expect("read runtime response");
        upstream_thread.join().expect("upstream thread completed");

        let text = String::from_utf8_lossy(&output);
        assert!(
            text.contains("500") && text.contains("upstream exploded"),
            "upstream failure must surface as a normal error response: {text}"
        );
        let records = read_stats_records(home.path());
        assert_eq!(
            records.len(),
            1,
            "failed requests are recorded too (zero tokens, status + error)"
        );
        assert_eq!(records[0].input, 0);
        assert_eq!(records[0].output, 0);
        assert_eq!(records[0].cached, 0);
        assert_eq!(records[0].status_code, Some(500));
        assert_eq!(
            records[0].error.as_deref(),
            Some("upstream exploded"),
            "the relayed upstream error text lands in the record"
        );
        assert!(!records[0].is_streaming);
        assert_eq!(records[0].source, crate::stats::SOURCE_PASSTHROUGH);
    }

    #[test]
    fn converted_failure_is_recorded_with_status_and_error() {
        let upstream_error = br#"{"error":{"message":"provider failed"}}"#;
        let (status, response, _requests, home) =
            rectifying_round_trip_with_home(thinking_chat_request(false), &[(500, upstream_error)]);

        assert_eq!(
            status, 500,
            "a retryable 500 is surfaced with its real status"
        );
        assert!(
            response.contains(r#""message":"provider failed""#),
            "semantic upstream error text must be relayed to the client: {response}"
        );
        let records = read_stats_records(&home);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, crate::stats::SOURCE_CONVERTED);
        assert_eq!(
            records[0].status_code,
            Some(500),
            "failure rows carry the same status the client receives"
        );
        assert_eq!(records[0].error.as_deref(), Some("provider failed"));
        assert_eq!(records[0].input, 0, "failure rows carry zero tokens");
        assert!(!records[0].is_streaming);
        assert!(records[0].latency_ms.is_some());
    }

    #[test]
    fn converted_stream_failure_is_recorded_with_status_and_error() {
        let upstream_error = br#"{"error":{"message":"stream exploded"}}"#;
        let (status, response, _requests, home) =
            rectifying_round_trip_with_home(thinking_chat_request(true), &[(500, upstream_error)]);

        assert_eq!(
            status, 500,
            "a retryable 500 is surfaced with its real status"
        );
        assert!(response.contains("stream exploded"));
        let records = read_stats_records(&home);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, crate::stats::SOURCE_CONVERTED_STREAM);
        assert_eq!(
            records[0].status_code,
            Some(500),
            "failure rows carry the same status the client receives"
        );
        assert_eq!(records[0].error.as_deref(), Some("stream exploded"));
        assert_eq!(records[0].input, 0);
        assert!(records[0].is_streaming);
    }

    #[test]
    fn passthrough_nonstream_failure_is_recorded_with_status_and_error() {
        let upstream_error = br#"{"error":{"message":"boom"}}"#;
        let home = tempfile::tempdir().expect("temp home").keep();
        let upstream = TcpListener::bind("127.0.0.1:0").expect("bind upstream test listener");
        let upstream_addr = upstream.local_addr().expect("upstream address");
        let upstream_error = upstream_error.to_vec();
        let upstream_thread = thread::spawn(move || {
            let (mut socket, _) = upstream.accept().expect("accept upstream request");
            let mut request = [0; 16 * 1024];
            let _ = socket.read(&mut request).expect("read upstream request");
            let headers = format!(
                "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                upstream_error.len()
            );
            socket
                .write_all(headers.as_bytes())
                .and_then(|_| socket.write_all(&upstream_error))
                .expect("write upstream response");
            socket
                .shutdown(Shutdown::Write)
                .expect("finish upstream response");
        });

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind runtime test listener");
        let address = listener.local_addr().expect("runtime address");
        let client = TcpStream::connect(address).expect("connect runtime test client");
        let (mut server, _) = listener.accept().expect("accept runtime test client");
        let mut client = client;
        let body = serde_json::to_vec(&json!({
            "model": "fixture_provider",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .expect("serialize request body");
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        client
            .write_all(request.as_bytes())
            .and_then(|_| client.write_all(&body))
            .expect("write runtime request");
        client
            .shutdown(Shutdown::Write)
            .expect("finish runtime request");

        let mut provider = provider("fixture_provider", ProtocolKind::OpenAiChat);
        provider.base_url = format!("http://{upstream_addr}");
        handle_connection(&mut server, &home, &[provider], &RuntimeStats::new())
            .expect("runtime request handled");
        server
            .shutdown(Shutdown::Write)
            .expect("finish runtime response");
        let mut output = Vec::new();
        client
            .read_to_end(&mut output)
            .expect("read runtime response");
        upstream_thread.join().expect("upstream thread completed");

        let text = String::from_utf8_lossy(&output);
        assert!(
            text.contains("429") && text.contains("boom"),
            "non-2xx passthrough responses are relayed verbatim: {text}"
        );
        let records = read_stats_records(&home);
        assert_eq!(
            records.len(),
            1,
            "non-2xx passthrough is recorded as a failure"
        );
        assert_eq!(records[0].source, crate::stats::SOURCE_PASSTHROUGH);
        assert_eq!(records[0].status_code, Some(429));
        assert_eq!(records[0].error.as_deref(), Some("boom"));
        assert_eq!(records[0].input, 0);
        assert!(!records[0].is_streaming);
    }

    #[test]
    fn stats_api_reports_all_periods_and_filters_by_period() {
        let home = tempfile::tempdir().expect("temp home");
        let record = crate::stats::UsageRecord {
            ts: chrono::Utc::now().timestamp(),
            path: "/v1/messages".to_string(),
            model: "m".to_string(),
            input: 10,
            output: 2,
            cached: 3,
            cache_creation: 0,
            semantics: crate::stats::SEMANTICS_LEGACY,
            latency_ms: Some(5),
            status_code: Some(200),
            is_streaming: false,
            error: None,
            source: crate::stats::SOURCE_CONVERTED,
        };
        crate::stats::record_usage(home.path(), &record).expect("record usage");

        let providers = vec![provider("zen", ProtocolKind::OpenAiChat)];
        let request = http_request_for_test("GET", "/api/stats/tokens", Vec::new(), 0);
        let (status, _, response_body) =
            handle_request_for_test(request, home.path(), &providers).expect("stats handled");
        assert_eq!(status, 200);
        let response: Value =
            serde_json::from_slice(&response_body).expect("stats response is JSON");
        let periods = response["periods"].as_object().expect("periods object");
        assert_eq!(periods.len(), 4, "no period filter returns all windows");
        assert_eq!(periods["24h"]["input"], json!(10));
        assert_eq!(periods["24h"]["output"], json!(2));
        assert_eq!(periods["24h"]["cached"], json!(3));
        assert_eq!(periods["24h"]["requests"], json!(1));
        assert_eq!(periods["24h"]["passthrough_approx"], json!(0));
        assert!(periods["24h"]["cache_hit_rate"].is_f64());

        let request = http_request_for_test("GET", "/api/stats/tokens?period=24h", Vec::new(), 0);
        let (status, _, response_body) =
            handle_request_for_test(request, home.path(), &providers).expect("stats handled");
        assert_eq!(status, 200);
        let response: Value =
            serde_json::from_slice(&response_body).expect("stats response is JSON");
        let periods = response["periods"].as_object().expect("periods object");
        assert_eq!(periods.len(), 1);
        assert!(periods.contains_key("24h"));

        let request = http_request_for_test("GET", "/api/stats/tokens?period=bogus", Vec::new(), 0);
        let (status, _, response_body) =
            handle_request_for_test(request, home.path(), &providers).expect("stats handled");
        assert_eq!(status, 200);
        let response: Value =
            serde_json::from_slice(&response_body).expect("stats response is JSON");
        assert_eq!(
            response["periods"].as_object().map(|object| object.len()),
            Some(4),
            "unknown period values fall back to all windows"
        );
    }

    #[test]
    fn stats_period_query_limits_response() {
        let home = tempfile::tempdir().expect("temp home");
        let record = crate::stats::UsageRecord {
            ts: chrono::Utc::now().timestamp(),
            path: "/v1/messages".to_string(),
            model: "m".to_string(),
            input: 10,
            output: 2,
            cached: 3,
            cache_creation: 0,
            semantics: crate::stats::SEMANTICS_LEGACY,
            latency_ms: Some(5),
            status_code: Some(200),
            is_streaming: false,
            error: None,
            source: crate::stats::SOURCE_CONVERTED,
        };
        crate::stats::record_usage(home.path(), &record).expect("record usage");

        let providers = vec![provider("zen", ProtocolKind::OpenAiChat)];
        let request = http_request_for_test("GET", "/api/stats/tokens?period=24h", Vec::new(), 0);
        let (_, _, body) =
            handle_request_for_test(request, home.path(), &providers).expect("stats handled");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("24h"));
        assert!(!body.contains("7d"));
    }

    #[test]
    fn raw_socket_stats_query_limits_response() {
        let providers = vec![provider("zen", ProtocolKind::OpenAiChat)];
        let (status, body) = raw_runtime_request(
            b"GET /api/stats/tokens?period=24h HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &providers,
        );
        assert_eq!(status, 200);
        assert!(
            body.contains("24h"),
            "period filter must select the 24h window"
        );
        assert!(
            !body.contains("7d"),
            "period filter must not leak other windows"
        );
    }

    #[test]
    fn raw_socket_malformed_query_uses_invalid_request_envelope() {
        let providers = vec![provider("zen", ProtocolKind::OpenAiChat)];
        let (status, body) = raw_runtime_request(
            b"GET /api/stats/tokens?period=%zz HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &providers,
        );
        assert_eq!(status, 400);
        assert!(body.contains(r#""code":"invalid_request""#));
    }
}

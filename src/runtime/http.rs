use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;

use serde_json::Value;

pub(super) const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

#[cfg(test)]
#[derive(Debug)]
pub struct HttpRequest {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) query: BTreeMap<String, String>,
    pub(super) body: Vec<u8>,
    pub(super) hop: u8,
}

#[cfg(not(test))]
#[derive(Debug)]
pub(super) struct HttpRequest {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) query: BTreeMap<String, String>,
    pub(super) body: Vec<u8>,
    pub(super) hop: u8,
}

/// Split a raw request target into an exact-match path and a decoded query
/// map (first value wins per key). Shared by socket parsing and
/// `http_request_for_test()` so both construct identical `HttpRequest`s.
pub(super) fn split_request_target(
    raw_target: &str,
) -> Result<(String, BTreeMap<String, String>), String> {
    let (path, query_text) = raw_target.split_once('?').unwrap_or((raw_target, ""));
    Ok((path.to_string(), parse_query(query_text)?))
}

/// `application/x-www-form-urlencoded`-style pairs: `&`-separated, `key`
/// without `=` counts as an empty value, `%XX` percent-decoded, `+` as space.
fn parse_query(query_text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut query = BTreeMap::new();
    for pair in query_text.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (raw_key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(raw_key)?;
        if query.contains_key(&key) {
            continue;
        }
        query.insert(key, percent_decode(value)?);
    }
    Ok(query)
}

fn percent_decode(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' => {
                let Some(high) = bytes.get(index + 1).and_then(|byte| hex_value(*byte)) else {
                    return Err("malformed percent escape in query".to_string());
                };
                let Some(low) = bytes.get(index + 2).and_then(|byte| hex_value(*byte)) else {
                    return Err("malformed percent escape in query".to_string());
                };
                decoded.push((high << 4) | low);
                index += 3;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    Ok(String::from_utf8_lossy(&decoded).into_owned())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 读错误归类：按 ErrorKind 而非 strerror 文本（中文 locale 下字符串匹配会失效）。
fn classify_read_error(error: std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            "read timed out".to_string()
        }
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe => {
            "connection reset by client".to_string()
        }
        _ => error.to_string(),
    }
}

pub(super) fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, String> {
    let mut temp = [0; 1024];
    read_http_request_with_buffer(stream, &mut temp)
}

fn read_http_request_with_buffer(
    stream: &mut impl Read,
    temp: &mut [u8],
) -> Result<HttpRequest, String> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut scanned = 0usize; // 已扫描窗口起点（含 3 字节重叠），避免 O(n²) 重扫。
    let header_end = loop {
        let read = stream.read(&mut *temp).map_err(classify_read_error)?;
        if read == 0 {
            return Err("connection closed before headers".to_string());
        }
        buffer.extend_from_slice(&temp[..read]);
        if buffer.len() > MAX_BODY_BYTES {
            return Err("request too large".to_string());
        }
        let search_start = scanned.saturating_sub(3);
        match find_header_end(&buffer[search_start..]) {
            Some(found) => break search_start + found,
            None => {
                scanned = buffer.len();
            }
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
    let (path, query) = split_request_target(raw_target)?;
    let mut content_length = 0;
    let mut hop = 0;
    let mut seen_content_length = false;
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.eq_ignore_ascii_case("transfer-encoding") {
            return Err("transfer encoding is not supported; send content-length".to_string());
        }
        if key.eq_ignore_ascii_case("content-length") {
            if seen_content_length {
                return Err("conflicting duplicate content-length headers".to_string());
            }
            seen_content_length = true;
            content_length = value.trim().parse::<usize>().unwrap_or(0);
        } else if key.eq_ignore_ascii_case("x-spec-runtime-hop") {
            hop = value
                .trim()
                .parse::<u8>()
                .map_err(|_| "invalid runtime hop header".to_string())?;
        }
    }
    if content_length > MAX_BODY_BYTES {
        return Err("request body too large".to_string());
    }

    let body_start = header_end + 4;
    let mut body = buffer.get(body_start..).unwrap_or_default().to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut *temp).map_err(classify_read_error)?;
        if read == 0 {
            return Err("connection closed before body".to_string());
        }
        body.extend_from_slice(&temp[..read]);
    }
    body.truncate(content_length);

    Ok(HttpRequest {
        method,
        path,
        query,
        body,
        hop,
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// 非阻塞排空 socket 中尚未读取的请求字节，避免 close 触发 RST 吞掉已写响应。
/// 关键约束：运行在唯一 accept 循环里，绝不能等待——空闲对端立即 WouldBlock
/// 返回；已到达内核缓冲的请求字节（典型：客户端已发完在等响应）一次读尽。
pub(super) fn drain_incoming(stream: &mut std::net::TcpStream) {
    let _ = stream.set_nonblocking(true);
    let mut sink = [0u8; 8192];
    // 上限 MAX_BODY_BYTES：防恶意端无限流式发送拖住 accept 循环。
    let mut drained = 0usize;
    while drained <= MAX_BODY_BYTES {
        match stream.read(&mut sink) {
            Ok(0) => break,
            Ok(n) => drained += n,
            Err(_) => break, // WouldBlock / 其它错误都立即收手
        }
    }
    let _ = stream.set_nonblocking(false);
}

/// RFC 9110 §6.6.1：源站响应 MUST 带 Date。
pub(super) fn http_date() -> String {
    chrono::Utc::now()
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

pub(super) fn write_json(stream: &mut TcpStream, status: u16, body: Value) -> Result<(), String> {
    let status_text = match status {
        200 => "OK",
        503 => "Service Unavailable",
        400 => "Bad Request",
        413 => "Payload Too Large",
        502 => "Bad Gateway",
        504 => "Gateway Timeout",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let body = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
    let headers = format!(
        "HTTP/1.1 {status} {status_text}\r\ndate: {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        http_date(),
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .and_then(|_| stream.write_all(&body))
        .map_err(|e| e.to_string())
}

pub(super) fn write_sse(stream: &mut TcpStream, event: &str, data: &Value) -> Result<(), String> {
    let payload = format!(
        "event: {event}\ndata: {}\n\n",
        serde_json::to_string(data).map_err(|e| e.to_string())?
    );
    stream
        .write_all(payload.as_bytes())
        .map_err(|e| e.to_string())
}

pub(super) fn write_sse_done(stream: &mut TcpStream) -> Result<(), String> {
    stream
        .write_all(b"data: [DONE]\n\n")
        .map_err(|e| e.to_string())
}

pub(super) fn write_sse_frame(
    stream: &mut TcpStream,
    event: Option<&str>,
    data: &str,
) -> Result<(), String> {
    if let Some(event) = event {
        return stream
            .write_all(format!("event: {event}\ndata: {data}\n\n").as_bytes())
            .map_err(|e| e.to_string());
    }
    stream
        .write_all(format!("data: {data}\n\n").as_bytes())
        .map_err(|e| e.to_string())
}

pub(super) fn write_raw_headers(
    stream: &mut TcpStream,
    status: u16,
    status_text: &str,
    content_type: &str,
) -> Result<(), String> {
    let headers = format!(
        "HTTP/1.1 {status} {status_text}\r\ndate: {}\r\ncontent-type: {content_type}\r\nconnection: close\r\n\r\n",
        http_date()
    );
    stream
        .write_all(headers.as_bytes())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::thread;

    use serde_json::json;

    use super::*;

    fn connected_streams() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        (server, client)
    }

    fn read_request_from_bytes(request: Vec<u8>) -> Result<HttpRequest, String> {
        let mut reader = std::io::Cursor::new(request);
        let mut buffer = vec![0; MAX_BODY_BYTES + 1];
        super::read_http_request_with_buffer(&mut reader, &mut buffer)
    }

    #[test]
    fn reads_request_path_without_query_and_exact_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let sender = thread::spawn(move || {
            let mut client = TcpStream::connect(address).unwrap();
            client
                .write_all(
                    b"POST /v1/messages?stream=true HTTP/1.1\r\nHost: localhost\r\nContent-Length: 6\r\n\r\n{\"ok\"}",
                )
                .unwrap();
            client.shutdown(Shutdown::Write).unwrap();
        });
        let (mut server, _) = listener.accept().unwrap();

        let request = read_http_request(&mut server).unwrap();

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/messages");
        assert_eq!(request.body, br#"{"ok"}"#);
        sender.join().unwrap();
    }

    #[test]
    fn writes_json_and_sse_payloads_with_expected_wire_format() {
        let (mut server, mut client) = connected_streams();

        write_json(&mut server, 200, json!({ "ok": true })).unwrap();
        write_sse(
            &mut server,
            "message_start",
            &json!({ "type": "message_start" }),
        )
        .unwrap();
        write_sse_done(&mut server).unwrap();
        server.shutdown(Shutdown::Write).unwrap();

        let mut output = Vec::new();
        client.read_to_end(&mut output).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("HTTP/1.1 200 OK\r\n"));
        assert!(output.contains("content-type: application/json\r\n"));
        assert!(output.contains("{\"ok\":true}"));
        assert!(output.contains("event: message_start\ndata: {\"type\":\"message_start\"}\n\n"));
        assert!(output.ends_with("data: [DONE]\n\n"));
    }

    #[test]
    fn closed_connection_before_headers_reports_header_error() {
        let error = match read_request_from_bytes(Vec::new()) {
            Ok(_) => panic!("closed headers must be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("connection closed before headers"));
    }

    #[test]
    fn oversized_headers_report_request_limit_error() {
        let mut request = b"GET /health HTTP/1.1\r\nX-Fill: ".to_vec();
        request.resize(MAX_BODY_BYTES + 1, b'x');

        let error = match read_request_from_bytes(request) {
            Ok(_) => panic!("oversized headers must be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("request too large"));
    }

    #[test]
    fn oversized_declared_body_reports_body_limit_error() {
        let request = format!(
            "POST /v1/messages HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        )
        .into_bytes();

        let error = match read_request_from_bytes(request) {
            Ok(_) => panic!("oversized bodies must be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("request body too large"));
    }

    #[test]
    fn body_eof_before_content_length_reports_body_error() {
        let request = b"POST /v1/messages HTTP/1.1\r\nContent-Length: 10\r\n\r\nshort";

        let error = match read_request_from_bytes(request.to_vec()) {
            Ok(_) => panic!("short bodies must be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("connection closed before body"));
    }

    #[test]
    fn missing_content_length_keeps_body_empty() {
        let request = b"POST /v1/messages HTTP/1.1\r\nHost: localhost\r\n\r\nignored";

        let request = read_request_from_bytes(request.to_vec()).unwrap();

        assert!(request.body.is_empty());
    }

    #[test]
    fn malformed_content_length_keeps_body_empty() {
        let request = b"POST /v1/messages HTTP/1.1\r\nContent-Length: invalid\r\n\r\nignored";

        let request = read_request_from_bytes(request.to_vec()).unwrap();

        assert!(request.body.is_empty());
    }

    #[test]
    fn request_line_preserves_query_separately() {
        let request = read_request_from_bytes(
            b"GET /api/stats/tokens?period=24h HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
        )
        .unwrap();
        assert_eq!(request.path, "/api/stats/tokens");
        assert_eq!(request.query["period"], "24h");
    }

    #[test]
    fn split_request_target_decodes_query_values() {
        let (path, query) = super::split_request_target("/v1/models?beta=true&q=a%20b+c").unwrap();
        assert_eq!(path, "/v1/models");
        assert_eq!(query["beta"], "true");
        assert_eq!(query["q"], "a b c");
    }

    #[test]
    fn split_request_target_keeps_first_value_for_duplicate_keys() {
        let (_, query) =
            super::split_request_target("/api/stats/tokens?period=24h&period=7d").unwrap();
        assert_eq!(query["period"], "24h");
        assert_eq!(query.len(), 1);
    }

    #[test]
    fn split_request_target_accepts_flag_pairs_as_empty_values() {
        let (_, query) = super::split_request_target("/v1/messages?stream").unwrap();
        assert_eq!(query["stream"], "");
    }

    #[test]
    fn split_request_target_without_query_returns_empty_map() {
        let (path, query) = super::split_request_target("/health").unwrap();
        assert_eq!(path, "/health");
        assert!(query.is_empty());
    }

    #[test]
    fn split_request_target_rejects_malformed_percent_escapes() {
        assert!(super::split_request_target("/api/stats/tokens?period=%zz").is_err());
        assert!(super::split_request_target("/api/stats/tokens?period=%2").is_err());
    }

    #[test]
    fn duplicate_content_length_is_rejected() {
        let request =
            b"POST /v1/messages HTTP/1.1\r\nContent-Length: 2\r\nContent-Length: 4\r\n\r\nbody";
        let Err(error) = read_request_from_bytes(request.to_vec()) else {
            panic!("duplicate content-length must be rejected");
        };
        assert!(error.contains("conflicting duplicate content-length"));
    }

    #[test]
    fn transfer_encoding_is_rejected() {
        let request = b"POST /v1/messages HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";
        let Err(error) = read_request_from_bytes(request.to_vec()) else {
            panic!("transfer encoding must be rejected");
        };
        assert!(error.contains("transfer encoding"));
    }

    #[test]
    fn split_request_target_dedupes_decoded_keys() {
        let (_, query) = super::split_request_target("/api/stats?period=24h&%70eriod=7d").unwrap();
        assert_eq!(query["period"], "24h");
        assert_eq!(query.len(), 1);
    }
}

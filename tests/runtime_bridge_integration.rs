#![allow(dead_code, unused_imports)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

mod domain {
    pub use spec::domain::*;
}

mod agents {
    pub use spec::agents::*;
}

mod providers {
    pub use spec::providers::*;
}

mod stats {
    pub use spec::stats::*;
}

#[path = "../src/runtime/mod.rs"]
mod runtime;

use serde_json::{json, Value};
use spec::domain::{CacheMode, ProtocolKind, ProviderProfile, ProviderVendor};

#[derive(Clone, Copy, Debug)]
struct Direction {
    name: &'static str,
    source: ProtocolKind,
    source_path: &'static str,
    target: ProtocolKind,
}

const DIRECTIONS: [Direction; 6] = [
    Direction {
        name: "anthropic_to_chat",
        source: ProtocolKind::AnthropicMessages,
        source_path: "/v1/messages",
        target: ProtocolKind::OpenAiChat,
    },
    Direction {
        name: "anthropic_to_responses",
        source: ProtocolKind::AnthropicMessages,
        source_path: "/v1/messages",
        target: ProtocolKind::OpenAiResponses,
    },
    Direction {
        name: "chat_to_anthropic",
        source: ProtocolKind::OpenAiChat,
        source_path: "/v1/chat/completions",
        target: ProtocolKind::AnthropicMessages,
    },
    Direction {
        name: "chat_to_responses",
        source: ProtocolKind::OpenAiChat,
        source_path: "/v1/chat/completions",
        target: ProtocolKind::OpenAiResponses,
    },
    Direction {
        name: "responses_to_anthropic",
        source: ProtocolKind::OpenAiResponses,
        source_path: "/v1/responses",
        target: ProtocolKind::AnthropicMessages,
    },
    Direction {
        name: "responses_to_chat",
        source: ProtocolKind::OpenAiResponses,
        source_path: "/v1/responses",
        target: ProtocolKind::OpenAiChat,
    },
];

#[derive(Debug)]
struct CapturedRequest {
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
    raw_body: Vec<u8>,
}

struct MockReply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    delay: Duration,
    chunk_size: Option<usize>,
}

#[derive(Debug)]
struct MockWorkerResult {
    request: Option<CapturedRequest>,
    result: Result<(), String>,
}

impl MockReply {
    fn raw(status: u16, content_type: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type,
            body: body.into(),
            delay: Duration::ZERO,
            chunk_size: None,
        }
    }

    fn json(body: Value) -> Self {
        Self::raw(
            200,
            "application/json",
            serde_json::to_vec(&body).expect("mock JSON serializes"),
        )
    }

    fn sse(body: &str) -> Self {
        let mut body = body.as_bytes().to_vec();
        if !body.ends_with(b"\n\n") {
            body.push(b'\n');
        }
        Self {
            status: 200,
            content_type: "text/event-stream",
            body,
            delay: Duration::ZERO,
            chunk_size: Some(7),
        }
    }
}

struct MockUpstream {
    address: SocketAddr,
    result: Receiver<MockWorkerResult>,
    thread: JoinHandle<()>,
}

impl MockUpstream {
    fn start(reply: MockReply) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock upstream");
        let address = listener.local_addr().expect("mock upstream address");
        listener
            .set_nonblocking(true)
            .expect("configure mock upstream listener");
        let (sender, result) = mpsc::channel();
        let thread = thread::spawn(move || {
            let report = match accept_mock_stream(&listener, Duration::from_secs(2)) {
                Ok((mut stream, _)) => {
                    let stream_setup = stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(2))))
                        .map_err(|error| format!("mock stream setup failed: {error}"));
                    match stream_setup.and_then(|_| read_mock_request(&mut stream)) {
                        Ok(request) => {
                            if !reply.delay.is_zero() {
                                thread::sleep(reply.delay);
                            }
                            let result = write_mock_reply(&mut stream, &reply);
                            MockWorkerResult {
                                request: Some(request),
                                result,
                            }
                        }
                        Err(error) => MockWorkerResult {
                            request: None,
                            result: Err(error),
                        },
                    }
                }
                Err(error) => MockWorkerResult {
                    request: None,
                    result: Err(error),
                },
            };
            let _ = sender.send(report);
        });
        Self {
            address,
            result,
            thread,
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn finish(self) -> CapturedRequest {
        self.finish_result()
            .expect("mock upstream response I/O")
            .expect("mock captured an upstream request")
    }

    fn finish_result(self) -> Result<Option<CapturedRequest>, String> {
        let report = self
            .result
            .recv_timeout(Duration::from_secs(3))
            .map_err(|error| format!("mock upstream worker result unavailable: {error}"));
        let joined = self
            .thread
            .join()
            .map_err(|_| "mock upstream thread panicked".to_string());
        match (report, joined) {
            (Ok(report), Ok(())) => report.result.map(|()| report.request),
            (Ok(_), Err(error)) | (Err(_), Err(error)) => Err(error),
            (Err(error), Ok(())) => Err(error),
        }
    }

    fn finish_optional(self) -> Option<CapturedRequest> {
        self.finish_result().expect("mock upstream response I/O")
    }
}

fn accept_mock_stream(
    listener: &TcpListener,
    timeout: Duration,
) -> Result<(TcpStream, SocketAddr), String> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok(connection) => return Ok(connection),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("mock upstream accept timed out".to_string());
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(format!("mock upstream accept failed: {error}")),
        }
    }
}

fn write_mock_reply(stream: &mut TcpStream, reply: &MockReply) -> Result<(), String> {
    let reason = match reply.status {
        200 => "OK",
        400 => "Bad Request",
        500 => "Internal Server Error",
        _ => "Mock",
    };
    let headers = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reply.status,
        reason,
        reply.content_type,
        reply.body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .map_err(|error| format!("mock response headers write failed: {error}"))?;
    match reply.chunk_size {
        Some(chunk_size) => {
            for chunk in reply.body.chunks(chunk_size) {
                stream
                    .write_all(chunk)
                    .map_err(|error| format!("mock response body write failed: {error}"))?;
                stream
                    .flush()
                    .map_err(|error| format!("mock response flush failed: {error}"))?;
            }
        }
        None => {
            stream
                .write_all(&reply.body)
                .map_err(|error| format!("mock response body write failed: {error}"))?;
        }
    }
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| format!("mock response shutdown failed: {error}"))?;
    Ok(())
}

fn read_mock_request(stream: &mut TcpStream) -> Result<CapturedRequest, String> {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0u8; 4096];
        let read = stream
            .read(&mut chunk)
            .map_err(|error| format!("mock request header read failed: {error}"))?;
        if read == 0 {
            return Err("mock request closed before headers".to_string());
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index;
        }
    };
    let header_text = String::from_utf8(bytes[..header_end].to_vec())
        .map_err(|error| format!("mock request headers were not UTF-8: {error}"))?;
    let mut lines = header_text.lines();
    let path = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| "mock request path missing".to_string())?
        .to_string();
    let mut headers = BTreeMap::new();
    let mut content_length = 0;
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.to_ascii_lowercase();
            let value = value.trim().to_string();
            if key == "content-length" {
                content_length = value
                    .parse()
                    .map_err(|error| format!("mock request content length invalid: {error}"))?;
            }
            headers.insert(key, value);
        }
    }
    let body_start = header_end + 4;
    while bytes.len().saturating_sub(body_start) < content_length {
        let mut chunk = [0u8; 4096];
        let read = stream
            .read(&mut chunk)
            .map_err(|error| format!("mock request body read failed: {error}"))?;
        if read == 0 {
            return Err("mock request closed before body".to_string());
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    let raw_body = bytes[body_start..body_start + content_length].to_vec();
    let body = serde_json::from_slice(&raw_body)
        .map_err(|error| format!("mock request JSON invalid: {error}"))?;
    Ok(CapturedRequest {
        path,
        headers,
        body,
        raw_body,
    })
}

fn provider() -> ProviderProfile {
    provider_for(ProtocolKind::OpenAiChat, "http://127.0.0.1:1")
}

fn provider_for(protocol: ProtocolKind, base_url: &str) -> ProviderProfile {
    ProviderProfile {
        id: "fixture".to_string(),
        name: "Fixture provider".to_string(),
        notes: None,
        website: None,
        vendor: ProviderVendor::CustomOpenAiCompatible,
        protocol,
        base_url: base_url.to_string(),
        api_key: "fixture-key".to_string(),
        models: vec!["fixture-model".to_string()],
        model_entries: Default::default(),
        model_metadata: Default::default(),
        claude_slots: Default::default(),
        default_model: "fixture-model".to_string(),
        extra_headers: Default::default(),
        request_url_mode: None,
        header_mode: None,
        timeout_ms: 100,
        max_retries: 0,
        context_window: 128000,
        max_output_tokens: 4096,
        reasoning_effort: None,
        cache_mode: CacheMode::Auto,
    }
}

fn provider_with_key(protocol: ProtocolKind, base_url: &str, api_key: &str) -> ProviderProfile {
    let mut provider = provider_for(protocol, base_url);
    provider.api_key = api_key.to_string();
    provider
}

fn mock_provider_for(protocol: ProtocolKind, base_url: &str) -> ProviderProfile {
    let id = match protocol {
        ProtocolKind::AnthropicMessages => "mock-anthropic",
        ProtocolKind::OpenAiChat => "mock-chat",
        ProtocolKind::OpenAiResponses => "mock-responses",
    };
    let mut provider = provider_for(protocol, base_url);
    provider.id = id.to_string();
    provider
}

fn text_response_for_direction(direction: Direction) -> Value {
    response_for_target(direction.target, direction.source)
}

fn assert_reply_has_terminal(reply: &Value, target: ProtocolKind) {
    assert!(reply["id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(reply["model"]
        .as_str()
        .is_some_and(|model| !model.is_empty()));
    let terminal = match target {
        ProtocolKind::AnthropicMessages => reply["stop_reason"].as_str(),
        ProtocolKind::OpenAiChat => reply["choices"][0]["finish_reason"].as_str(),
        ProtocolKind::OpenAiResponses => reply["status"].as_str(),
    };
    assert!(
        terminal.is_some(),
        "mock reply for {target:?} missing terminal status: {reply}"
    );
}

fn fixture_json(name: &str) -> Value {
    let source = match name {
        "anthropic_text_request" => {
            include_str!("fixtures/runtime_bridge/anthropic_text_request.json")
        }
        "anthropic_stream_text_request" => {
            include_str!("fixtures/runtime_bridge/anthropic_stream_text_request.json")
        }
        "chat_text_request" => include_str!("fixtures/runtime_bridge/chat_text_request.json"),
        "responses_text_request" => {
            include_str!("fixtures/runtime_bridge/responses_text_request.json")
        }
        "anthropic_tool_request" => {
            include_str!("fixtures/runtime_bridge/anthropic_tool_request.json")
        }
        "anthropic_clean_request" => {
            include_str!("fixtures/runtime_bridge/anthropic_clean_request.json")
        }
        "anthropic_parallel_tool_request" => {
            include_str!("fixtures/runtime_bridge/anthropic_parallel_tool_request.json")
        }
        "chat_clean_request" => include_str!("fixtures/runtime_bridge/chat_clean_request.json"),
        "chat_tool_request" => include_str!("fixtures/runtime_bridge/chat_tool_request.json"),
        "chat_parallel_anthropic_request" => {
            include_str!("fixtures/runtime_bridge/chat_parallel_anthropic_request.json")
        }
        "responses_compatible_request" => {
            include_str!("fixtures/runtime_bridge/responses_compatible_request.json")
        }
        "responses_parallel_tool_request" => {
            include_str!("fixtures/runtime_bridge/responses_parallel_tool_request.json")
        }
        "responses_parallel_anthropic_request" => {
            include_str!("fixtures/runtime_bridge/responses_parallel_anthropic_request.json")
        }
        "responses_reasoning_request" => {
            include_str!("fixtures/runtime_bridge/responses_reasoning_request.json")
        }
        "responses_custom_tool_request" => {
            include_str!("fixtures/runtime_bridge/responses_custom_tool_request.json")
        }
        "ccswitch_anthropic_system_tool_choice_request" => include_str!(
            "fixtures/runtime_bridge/ccswitch/anthropic_system_tool_choice_request.json"
        ),
        "ccswitch_chat_parallel_tool_calls_request" => {
            include_str!("fixtures/runtime_bridge/ccswitch/chat_parallel_tool_calls_request.json")
        }
        "ccswitch_responses_reasoning_request" => {
            include_str!("fixtures/runtime_bridge/ccswitch/responses_reasoning_request.json")
        }
        "ccswitch_anthropic_malformed_messages_request" => include_str!(
            "fixtures/runtime_bridge/ccswitch/anthropic_malformed_messages_request.json"
        ),
        "ccswitch_anthropic_document_request" => {
            include_str!("fixtures/runtime_bridge/ccswitch/anthropic_document_request.json")
        }
        "ccswitch_anthropic_cache_write_response" => {
            include_str!("fixtures/runtime_bridge/ccswitch/anthropic_cache_write_response.json")
        }
        "ccswitch_anthropic_zero_cache_write_response" => {
            include_str!(
                "fixtures/runtime_bridge/ccswitch/anthropic_zero_cache_write_response.json"
            )
        }
        "ccswitch_anthropic_cache_read_response" => {
            include_str!("fixtures/runtime_bridge/ccswitch/anthropic_cache_read_response.json")
        }
        "ccswitch_chat_length_response" => {
            include_str!("fixtures/runtime_bridge/ccswitch/chat_length_response.json")
        }
        "anthropic_text_response" => {
            include_str!("fixtures/runtime_bridge/anthropic_text_response.json")
        }
        "chat_text_response" => include_str!("fixtures/runtime_bridge/chat_text_response.json"),
        "responses_text_response" => {
            include_str!("fixtures/runtime_bridge/responses_text_response.json")
        }
        "anthropic_tool_response" => {
            include_str!("fixtures/runtime_bridge/anthropic_tool_response.json")
        }
        "chat_tool_response" => include_str!("fixtures/runtime_bridge/chat_tool_response.json"),
        "responses_tool_response" => {
            include_str!("fixtures/runtime_bridge/responses_tool_response.json")
        }
        _ => panic!("unknown runtime bridge fixture: {name}"),
    };
    serde_json::from_str(source).expect("runtime bridge fixture JSON")
}

fn fixture_for_direction(name: &str) -> Value {
    let fixture = match name {
        "anthropic_to_chat" | "anthropic_to_responses" => "anthropic_clean_request",
        "chat_to_anthropic" | "chat_to_responses" => "chat_clean_request",
        "responses_to_anthropic" | "responses_to_chat" => "responses_compatible_request",
        _ => panic!("unknown direction fixture: {name}"),
    };
    fixture_json(fixture)
}

fn set_fixture_request_defaults(mut request: Value, stream: bool) -> Value {
    request["model"] = json!("fixture-model");
    request["stream"] = json!(stream);
    request
}

fn task10_manifest() -> Value {
    serde_json::from_str(include_str!(
        "fixtures/runtime_bridge/ccswitch/task10_differential_manifest.json"
    ))
    .expect("Task 10 differential manifest JSON")
}

fn task10_protocol(name: &str) -> runtime::bridge::WireProtocol {
    match name {
        "anthropic_messages" => runtime::bridge::WireProtocol::AnthropicMessages,
        "open_ai_chat" => runtime::bridge::WireProtocol::OpenAiChat,
        "open_ai_responses" => runtime::bridge::WireProtocol::OpenAiResponses,
        _ => panic!("unknown Task 10 protocol: {name}"),
    }
}

fn assert_task10_fields(case_id: &str, body: &Value, fields: &Value) {
    for (pointer, expected) in fields.as_object().expect("Task 10 fields object") {
        if let Some(prefix) = expected
            .as_object()
            .and_then(|matcher| matcher.get("starts_with"))
            .and_then(Value::as_str)
        {
            let actual = body
                .pointer(pointer)
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("Task 10 case {case_id} field {pointer} is not a string; body={body:?}")
                });
            assert!(
                actual.starts_with(prefix),
                "Task 10 case {case_id} field {pointer} did not start with {prefix:?}; actual={actual:?}"
            );
            continue;
        }
        assert_eq!(
            body.pointer(pointer).unwrap_or(&Value::Null),
            expected,
            "Task 10 case {case_id} field {pointer}; body={body:?}"
        );
    }
}

fn task10_case<'a>(cases: &'a [Value], case_id: &str) -> &'a Value {
    cases
        .iter()
        .find(|case| case["id"].as_str() == Some(case_id))
        .unwrap_or_else(|| panic!("missing Task 10 case {case_id}"))
}

fn assert_task10_rejection(case_id: &str, error: &runtime::bridge::BridgeError, expected: &Value) {
    if error.log_category() == "unsupported" {
        let expected_field = expected["error_field"]
            .as_str()
            .unwrap_or_else(|| panic!("Task 10 case {case_id} missing structured error_field"));
        let actual_field = match error {
            runtime::bridge::BridgeError::Unsupported { field } => field,
            other => panic!("Task 10 case {case_id} expected unsupported error, got {other:?}"),
        };
        assert_eq!(
            actual_field, expected_field,
            "Task 10 case {case_id} structured error field"
        );
        let reason_code = match expected_field {
            "messages.content" | "response.content.media" | "usage.cache_write_tokens" => {
                "unsupported request field"
            }
            other => panic!("unknown Task 10 structured error_field {other}"),
        };
        assert_eq!(
            expected["reason_code"], reason_code,
            "Task 10 case {case_id} structured field reason mapping"
        );
    }
    assert_eq!(
        error.log_category(),
        expected["error_category"]
            .as_str()
            .expect("Task 10 error category"),
        "Task 10 case {case_id} rejection category"
    );
    assert_eq!(
        error.http_status(),
        expected["http_status"]
            .as_u64()
            .expect("Task 10 HTTP status") as u16,
        "Task 10 case {case_id} rejection HTTP status"
    );
    assert_eq!(
        error.public_message(),
        expected["reason_code"]
            .as_str()
            .expect("Task 10 reason code"),
        "Task 10 case {case_id} rejection reason"
    );
}

fn assert_task10_case_schema(case_id: &str, case: &Value) {
    let comparison = case["comparison"]
        .as_str()
        .unwrap_or_else(|| panic!("Task 10 case {case_id} missing comparison outcome"));
    assert!(
        matches!(comparison, "match" | "mismatch"),
        "Task 10 case {case_id} has invalid comparison outcome {comparison:?}"
    );

    let cc_switch = &case["cc_switch"];
    let xu = &case["xu"];
    let cc_result = cc_switch["result"]
        .as_str()
        .unwrap_or_else(|| panic!("Task 10 case {case_id} missing CC Switch result"));
    let xu_result = xu["result"]
        .as_str()
        .unwrap_or_else(|| panic!("Task 10 case {case_id} missing Xu result"));
    assert!(
        matches!(cc_result, "accept" | "reject") && matches!(xu_result, "accept" | "reject"),
        "Task 10 case {case_id} has invalid result contract"
    );
    assert_eq!(
        comparison == "match",
        cc_result == xu_result,
        "Task 10 case {case_id} comparison outcome does not match result divergence"
    );

    for (name, contract) in [("cc_switch", cc_switch), ("xu", xu)] {
        assert!(
            contract.get("error_field").is_some(),
            "Task 10 case {case_id} {name} contract is missing error_field"
        );
        for field in ["reason_code", "reason_detail"] {
            assert!(
                contract[field]
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty()),
                "Task 10 case {case_id} {name} contract is missing non-empty {field}"
            );
        }
        if contract["result"] == "accept" {
            assert!(
                contract["fields"]
                    .as_object()
                    .is_some_and(|fields| !fields.is_empty()),
                "Task 10 case {case_id} {name} accepted contract is missing fields"
            );
        } else {
            assert!(contract["error_category"].as_str().is_some());
            assert!(contract["http_status"].as_u64().is_some());
        }
    }
}

#[test]
fn task10_cc_switch_differential_manifest_executes_xu_contract() {
    let manifest = task10_manifest();
    assert_eq!(
        manifest["reference"]["commit"],
        "413c09e0790c304506888ae24b9be72820aca126"
    );
    let cases = manifest["cases"].as_array().expect("Task 10 cases array");
    assert_eq!(cases.len(), 9, "Task 10 must contain exactly nine cases");
    let case_ids = cases
        .iter()
        .map(|case| case["id"].as_str().expect("Task 10 case id"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        case_ids.len(),
        cases.len(),
        "Task 10 case ids must be unique"
    );

    for case in cases {
        assert_task10_case_schema(case["id"].as_str().expect("Task 10 case id"), case);
    }

    let malformed = task10_case(cases, "anthropic_malformed_messages_to_chat");
    assert_eq!(malformed["kind"], "request");
    assert_eq!(malformed["source_protocol"], "anthropic_messages");
    assert_eq!(malformed["target_protocol"], "open_ai_chat");
    assert_eq!(malformed["stage"], "parse_request");
    assert_eq!(malformed["input_problem"]["path"], "/messages");
    assert_eq!(malformed["input_problem"]["kind"], "wrong_type");
    assert_eq!(malformed["xu"]["result"], "reject");
    assert_eq!(malformed["xu"]["error_category"], "invalid_request");
    assert_eq!(malformed["xu"]["http_status"], 400);
    assert_eq!(malformed["xu"]["reason_code"], "invalid request");
    assert_eq!(malformed["comparison"], "mismatch");

    let reasoning = task10_case(cases, "responses_reasoning_to_anthropic");
    for pointer in [
        "/system",
        "/messages/1/content/0/type",
        "/messages/1/content/0/thinking",
        "/messages/1/content/0/signature",
    ] {
        assert!(
            reasoning["xu"]["fields"].get(pointer).is_some(),
            "Task 10 reasoning case is missing opaque field contract {pointer}"
        );
    }

    for case_id in ["anthropic_document_to_chat_is_fail_closed"]
        .iter()
        .copied()
    {
        let case = task10_case(cases, case_id);
        assert!(
            case["xu"]["error_field"]
                .as_str()
                .is_some_and(|field| !field.is_empty()),
            "Task 10 case {case_id} is missing structured error_field"
        );
        assert!(
            case["xu"]["reason_code"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "Task 10 case {case_id} is missing executable reason_code"
        );
    }

    for case in cases {
        let case_id = case["id"].as_str().expect("Task 10 case id");
        let source = task10_protocol(
            case["source_protocol"]
                .as_str()
                .expect("Task 10 source protocol"),
        );
        let target = task10_protocol(
            case["target_protocol"]
                .as_str()
                .expect("Task 10 target protocol"),
        );
        let fixture = fixture_json(case["fixture"].as_str().expect("Task 10 fixture"));
        if let Some(problem) = case.get("input_problem") {
            let path = problem["path"]
                .as_str()
                .expect("Task 10 malformed input path");
            let value = fixture
                .pointer(path)
                .unwrap_or_else(|| panic!("Task 10 malformed input path missing: {path}"));
            match problem["actual_type"].as_str().expect("Task 10 input type") {
                "string" => assert!(value.is_string(), "Task 10 malformed input at {path}"),
                "array" => assert!(value.is_array(), "Task 10 malformed input at {path}"),
                "object" => assert!(value.is_object(), "Task 10 malformed input at {path}"),
                expected => panic!("unknown Task 10 malformed input type {expected}"),
            }
        }
        let result = match case["kind"].as_str().expect("Task 10 case kind") {
            "request" => runtime::bridge::parse_request(source, &fixture).and_then(|ir| {
                runtime::bridge::encode_upstream_request(&ir, target, "fixture-target-model")
            }),
            "response" => runtime::bridge::decode_response(source, fixture)
                .and_then(|ir| runtime::bridge::encode_response(ir, target)),
            other => panic!("unknown Task 10 case kind: {other}"),
        };

        if let Some(problem) = case.get("input_problem") {
            if case["xu"]["result"] == "reject" {
                assert_eq!(
                    case["xu"]["error_field"], problem["path"],
                    "Task 10 case {case_id} malformed input error field"
                );
            }
        }

        match case["xu"]["result"].as_str().expect("Task 10 Xu result") {
            "accept" => {
                let body = result.unwrap_or_else(|error| {
                    panic!("Task 10 case {case_id} unexpectedly rejected: {error}")
                });
                assert_task10_fields(case_id, &body, &case["xu"]["fields"]);
                if case["comparison"] == "match" {
                    assert_task10_fields(case_id, &body, &case["cc_switch"]["fields"]);
                }
            }
            "reject" => {
                let error = result.expect_err("Task 10 case unexpectedly accepted");
                assert_task10_rejection(case_id, &error, &case["xu"]);
            }
            other => panic!("unknown Task 10 Xu result: {other}"),
        }
    }
}

fn request_for_source(source: ProtocolKind) -> Value {
    let fixture = match source {
        ProtocolKind::AnthropicMessages => "anthropic_text_request",
        ProtocolKind::OpenAiChat => "chat_text_request",
        ProtocolKind::OpenAiResponses => "responses_text_request",
    };
    set_fixture_request_defaults(fixture_json(fixture), false)
}

fn streaming_request_for_source(source: ProtocolKind) -> Value {
    let fixture = match source {
        ProtocolKind::AnthropicMessages => "anthropic_stream_text_request",
        ProtocolKind::OpenAiChat => "chat_text_request",
        ProtocolKind::OpenAiResponses => "responses_text_request",
    };
    set_fixture_request_defaults(fixture_json(fixture), true)
}

fn tool_request_for_source(source: ProtocolKind) -> Value {
    let fixture = match source {
        ProtocolKind::AnthropicMessages => "anthropic_tool_request",
        ProtocolKind::OpenAiChat => "chat_clean_request",
        ProtocolKind::OpenAiResponses => "responses_compatible_request",
    };
    set_fixture_request_defaults(fixture_json(fixture), false)
}

fn tool_request_for_direction(direction: Direction) -> Value {
    parallel_tool_request_for_direction(direction)
}

fn parallel_tool_request_for_direction(direction: Direction) -> Value {
    let fixture = match (direction.source, direction.target) {
        (ProtocolKind::OpenAiChat, ProtocolKind::AnthropicMessages) => {
            "chat_parallel_anthropic_request"
        }
        (ProtocolKind::OpenAiResponses, ProtocolKind::AnthropicMessages) => {
            "responses_parallel_anthropic_request"
        }
        (ProtocolKind::AnthropicMessages, _) => "anthropic_parallel_tool_request",
        (ProtocolKind::OpenAiChat, _) => "chat_tool_request",
        (ProtocolKind::OpenAiResponses, _) => "responses_parallel_tool_request",
    };
    set_fixture_request_defaults(fixture_json(fixture), false)
}

fn tool_streaming_request_for_source(source: ProtocolKind) -> Value {
    let mut request = tool_request_for_source(source);
    request["stream"] = json!(true);
    request
}

fn stream_for_target(target: ProtocolKind) -> &'static str {
    match target {
        ProtocolKind::AnthropicMessages => {
            include_str!("fixtures/runtime_bridge/anthropic_tool_stream_runtime.sse")
        }
        ProtocolKind::OpenAiChat => include_str!("fixtures/runtime_bridge/chat_tool_stream.sse"),
        ProtocolKind::OpenAiResponses => {
            include_str!("fixtures/runtime_bridge/responses_tool_stream.sse")
        }
    }
}

fn text_stream_for_target(target: ProtocolKind) -> &'static str {
    match target {
        ProtocolKind::AnthropicMessages => {
            include_str!("fixtures/runtime_bridge/anthropic_text_stream.sse")
        }
        ProtocolKind::OpenAiChat => include_str!("fixtures/runtime_bridge/chat_text_stream.sse"),
        ProtocolKind::OpenAiResponses => {
            include_str!("fixtures/runtime_bridge/responses_text_stream.sse")
        }
    }
}

fn single_tool_stream_for_target(target: ProtocolKind) -> &'static str {
    match target {
        ProtocolKind::AnthropicMessages => {
            include_str!("fixtures/runtime_bridge/anthropic_single_tool_stream.sse")
        }
        ProtocolKind::OpenAiChat => {
            include_str!("fixtures/runtime_bridge/chat_single_tool_stream.sse")
        }
        ProtocolKind::OpenAiResponses => {
            include_str!("fixtures/runtime_bridge/responses_single_tool_stream.sse")
        }
    }
}

fn reasoning_stream_for_target(target: ProtocolKind) -> &'static str {
    match target {
        ProtocolKind::AnthropicMessages => {
            include_str!("fixtures/runtime_bridge/anthropic_reasoning_stream.sse")
        }
        ProtocolKind::OpenAiChat => panic!("Chat cannot carry native reasoning deltas"),
        ProtocolKind::OpenAiResponses => {
            include_str!("fixtures/runtime_bridge/responses_reasoning_stream.sse")
        }
    }
}

fn reasoning_request_for_source(source: ProtocolKind) -> Value {
    if source == ProtocolKind::OpenAiResponses {
        return set_fixture_request_defaults(fixture_json("responses_reasoning_request"), true);
    }
    streaming_request_for_source(source)
}

fn reasoning_request_for_direction(direction: Direction) -> Value {
    let mut request = reasoning_request_for_source(direction.source);
    if direction.target == ProtocolKind::AnthropicMessages
        && direction.source == ProtocolKind::OpenAiResponses
    {
        let object = request
            .as_object_mut()
            .expect("Responses reasoning request object");
        object.remove("parallel_tool_calls");
        if let Some(input) = object.get_mut("input").and_then(Value::as_array_mut) {
            for item in input {
                if item["type"] == "function_call" {
                    item.as_object_mut()
                        .expect("Responses function call object")
                        .remove("id");
                }
            }
        }
    }
    request
}

fn response_for_target(target: ProtocolKind, source: ProtocolKind) -> Value {
    let fixture = match target {
        ProtocolKind::AnthropicMessages => "anthropic_text_response",
        ProtocolKind::OpenAiChat => "chat_text_response",
        ProtocolKind::OpenAiResponses => "responses_text_response",
    };
    let mut body = fixture_json(fixture);
    if target == ProtocolKind::OpenAiResponses && source != ProtocolKind::OpenAiResponses {
        body["output"][0]
            .as_object_mut()
            .expect("Responses text output item object")
            .remove("id");
        body["usage"]
            .as_object_mut()
            .expect("Responses text usage object")
            .remove("input_tokens_details");
        body["usage"]
            .as_object_mut()
            .expect("Responses text usage object")
            .remove("output_tokens_details");
    }
    body
}

fn rich_response_is_representable(direction: Direction) -> bool {
    direction.target != ProtocolKind::OpenAiResponses
}

fn tool_response_for_target(target: ProtocolKind) -> Value {
    let fixture = match target {
        ProtocolKind::AnthropicMessages => "anthropic_tool_response",
        ProtocolKind::OpenAiChat => "chat_tool_response",
        ProtocolKind::OpenAiResponses => "responses_tool_response",
    };
    fixture_json(fixture)
}

fn reasoning_response_from_anthropic() -> Value {
    json!({
        "id": "msg_fixture_reasoning",
        "type": "message",
        "role": "assistant",
        "model": "fixture-model",
        "content": [
            {"type": "thinking", "thinking": "fixture reasoning", "signature": "fixture-signature"},
            {"type": "text", "text": "fixture response text"},
        ],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 12, "output_tokens": 8},
    })
}

fn public_path(protocol: ProtocolKind) -> &'static str {
    match protocol {
        ProtocolKind::AnthropicMessages => "/v1/messages",
        ProtocolKind::OpenAiChat => "/v1/chat/completions",
        ProtocolKind::OpenAiResponses => "/v1/responses",
    }
}

fn upstream_path(protocol: ProtocolKind) -> &'static str {
    match protocol {
        ProtocolKind::AnthropicMessages => "/messages",
        ProtocolKind::OpenAiChat => "/chat/completions",
        ProtocolKind::OpenAiResponses => "/responses",
    }
}

fn assert_text_response(direction: Direction, body: &[u8]) {
    let body: Value = serde_json::from_slice(body).expect("downstream response JSON");
    // The anthropic text fixture bills 12 (cache excluded) so its normalized
    // full input is 14 (12 + 2 cache_read); chat/responses fixtures report a
    // full input of 12 directly.
    let expected_full_input = if direction.target == ProtocolKind::AnthropicMessages {
        14
    } else {
        12
    };
    match direction.source {
        ProtocolKind::AnthropicMessages => {
            assert_eq!(body["content"].as_array().map(Vec::len), Some(1));
            assert_eq!(body["content"][0]["text"], "fixture response text");
            let input_tokens = body["usage"]["input_tokens"].as_u64().unwrap();
            let cache_read = body["usage"]["cache_read_input_tokens"]
                .as_u64()
                .unwrap_or(0);
            assert_eq!(
                input_tokens + cache_read,
                expected_full_input,
                "input_tokens must exclude the cached prefix while cache_read reports it"
            );
            assert_eq!(body["usage"]["output_tokens"], 4);
        }
        ProtocolKind::OpenAiChat => {
            assert_eq!(body["choices"].as_array().map(Vec::len), Some(1));
            assert_eq!(
                body["choices"][0]["message"]["content"],
                "fixture response text"
            );
            assert_eq!(body["usage"]["prompt_tokens"], expected_full_input);
            assert_eq!(body["usage"]["completion_tokens"], 4);
            assert_eq!(body["usage"]["total_tokens"], expected_full_input + 4);
        }
        ProtocolKind::OpenAiResponses => {
            assert_eq!(body["output"].as_array().map(Vec::len), Some(1));
            assert_eq!(body["output"][0]["type"], "message");
            assert_eq!(
                body["output"][0]["content"].as_array().map(Vec::len),
                Some(1)
            );
            assert_eq!(body["output_text"], "fixture response text");
            let input_tokens = body["usage"]["input_tokens"].as_u64().unwrap();
            let cache_read = body["usage"]["cache_read_input_tokens"]
                .as_u64()
                .unwrap_or(0);
            assert_eq!(
                input_tokens + cache_read,
                expected_full_input,
                "input_tokens must exclude the cached prefix while cache_read reports it"
            );
            assert_eq!(body["usage"]["output_tokens"], 4);
            assert_eq!(body["usage"]["total_tokens"], expected_full_input + 4);
        }
    }
}

fn expected_tool_names(direction: Direction) -> &'static [&'static str] {
    if direction.source == direction.target {
        &["get_fixture_weather"]
    } else {
        &["get_fixture_weather", "get_fixture_time"]
    }
}

fn expected_tool_parameters(name: &str) -> Value {
    match name {
        "get_fixture_weather" => json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"],
            "additionalProperties": false,
        }),
        "get_fixture_time" => json!({
            "type": "object",
            "properties": {"zone": {"type": "string"}},
            "required": ["zone"],
            "additionalProperties": false,
        }),
        _ => panic!("unknown fixture tool: {name}"),
    }
}

fn expected_tool_description(name: &str) -> &'static str {
    match name {
        "get_fixture_weather" => "Return fixture weather data",
        "get_fixture_time" => "Return fixture time data",
        _ => panic!("unknown fixture tool: {name}"),
    }
}

fn expected_tool_arguments(name: &str) -> &'static str {
    match name {
        "get_fixture_weather" => "{\"city\":\"fixture-city\"}",
        "get_fixture_time" => "{\"zone\":\"fixture-zone\"}",
        _ => panic!("unknown fixture tool: {name}"),
    }
}

fn assert_tool_request(direction: Direction, body: &Value) {
    assert_tool_request_with_names(direction, body, expected_tool_names(direction));
}

fn assert_tool_request_with_names(direction: Direction, body: &Value, expected_names: &[&str]) {
    match direction.target {
        ProtocolKind::AnthropicMessages => {
            let tools = body["tools"].as_array().expect("Anthropic tools array");
            assert_eq!(tools.len(), expected_names.len());
            for (tool, name) in tools.iter().zip(expected_names) {
                assert_eq!(tool["name"], *name);
                assert_eq!(tool["description"], expected_tool_description(name));
                assert_eq!(tool["input_schema"], expected_tool_parameters(name));
            }
            let messages = body["messages"]
                .as_array()
                .expect("Anthropic messages array");
            let mut expected_roles = vec!["user", "assistant"];
            expected_roles.extend(std::iter::repeat_n("user", expected_names.len()));
            expected_roles.push("user");
            assert_eq!(
                messages
                    .iter()
                    .map(|message| message["role"].as_str().unwrap_or_default())
                    .collect::<Vec<_>>(),
                expected_roles
            );
            let assistant = messages
                .iter()
                .find(|message| message["role"] == "assistant")
                .expect("Anthropic assistant tool message");
            let tool_uses: Vec<&Value> = assistant["content"]
                .as_array()
                .expect("Anthropic assistant content")
                .iter()
                .filter(|item| item["type"] == "tool_use")
                .collect();
            assert_eq!(tool_uses.len(), expected_names.len());
            for (tool_use, name) in tool_uses.iter().zip(expected_names) {
                assert_eq!(
                    tool_use["id"],
                    format!(
                        "call_fixture_{}",
                        name.strip_prefix("get_fixture_").unwrap()
                    )
                );
                assert_eq!(tool_use["name"], *name);
                let arguments: Value = serde_json::from_str(expected_tool_arguments(name))
                    .expect("Anthropic tool input JSON");
                assert_eq!(tool_use["input"], arguments);
            }
            let tool_results: Vec<&Value> = messages
                .iter()
                .flat_map(|message| message["content"].as_array().into_iter().flatten())
                .filter(|item| item["type"] == "tool_result")
                .collect();
            assert_eq!(tool_results.len(), expected_names.len());
            for (tool_result, name) in tool_results.iter().zip(expected_names) {
                assert_eq!(
                    tool_result["tool_use_id"],
                    format!(
                        "call_fixture_{}",
                        name.strip_prefix("get_fixture_").unwrap()
                    )
                );
                assert_eq!(
                    tool_result["content"],
                    match *name {
                        "get_fixture_weather" => "fixture weather result",
                        "get_fixture_time" => "fixture time result",
                        _ => unreachable!(),
                    }
                );
            }
        }
        ProtocolKind::OpenAiChat => {
            let tools = body["tools"].as_array().expect("Chat tools array");
            assert_eq!(tools.len(), expected_names.len());
            for (tool, name) in tools.iter().zip(expected_names) {
                assert_eq!(tool["type"], "function");
                assert_eq!(tool["function"]["name"], *name);
                assert_eq!(
                    tool["function"]["description"],
                    expected_tool_description(name)
                );
                assert_eq!(
                    tool["function"]["parameters"],
                    expected_tool_parameters(name)
                );
            }
            let messages = body["messages"].as_array().expect("Chat messages array");
            let mut expected_roles = match direction.source {
                ProtocolKind::AnthropicMessages => vec!["system", "user", "assistant"],
                ProtocolKind::OpenAiResponses => {
                    vec!["system", "developer", "user", "assistant"]
                }
                ProtocolKind::OpenAiChat => vec!["assistant"],
            };
            if direction.source != ProtocolKind::OpenAiChat {
                expected_roles.extend(std::iter::repeat_n("tool", expected_names.len()));
                expected_roles.push("user");
            }
            assert_eq!(
                messages
                    .iter()
                    .map(|message| message["role"].as_str().unwrap_or_default())
                    .collect::<Vec<_>>(),
                expected_roles
            );
            let assistant = messages
                .iter()
                .find(|message| message["role"] == "assistant")
                .expect("Chat assistant tool message");
            let calls = assistant["tool_calls"].as_array().expect("Chat tool calls");
            assert_eq!(calls.len(), expected_names.len());
            for (call, name) in calls.iter().zip(expected_names) {
                assert_eq!(call["type"], "function");
                assert_eq!(
                    call["id"],
                    format!(
                        "call_fixture_{}",
                        name.strip_prefix("get_fixture_").unwrap()
                    )
                );
                assert_eq!(call["function"]["name"], *name);
                assert_eq!(call["function"]["arguments"], expected_tool_arguments(name));
            }
            let tool_results: Vec<&Value> = messages
                .iter()
                .filter(|message| message["role"] == "tool")
                .collect();
            assert_eq!(tool_results.len(), expected_names.len());
            assert_eq!(tool_results[0]["tool_call_id"], "call_fixture_weather");
            assert_eq!(tool_results[0]["content"], "fixture weather result");
            if expected_names.len() == 2 {
                assert_eq!(tool_results[1]["tool_call_id"], "call_fixture_time");
                assert_eq!(tool_results[1]["content"], "fixture time result");
            }
            assert_chat_parallel_semantics(direction, body, expected_names);
        }
        ProtocolKind::OpenAiResponses => {
            let tools = body["tools"].as_array().expect("Responses tools array");
            assert_eq!(tools.len(), expected_names.len());
            for (tool, name) in tools.iter().zip(expected_names) {
                assert_eq!(tool["type"], "function");
                assert_eq!(tool["name"], *name);
                assert_eq!(tool["description"], expected_tool_description(name));
                assert_eq!(tool["parameters"], expected_tool_parameters(name));
            }
            let input = body["input"].as_array().expect("Responses input array");
            let message_count = match direction.source {
                ProtocolKind::AnthropicMessages => 1,
                ProtocolKind::OpenAiChat | ProtocolKind::OpenAiResponses => 2,
            };
            let mut expected_input_types =
                std::iter::repeat_n("message", message_count).collect::<Vec<_>>();
            expected_input_types.extend(std::iter::repeat_n("function_call", expected_names.len()));
            expected_input_types.extend(std::iter::repeat_n(
                "function_call_output",
                expected_names.len(),
            ));
            expected_input_types.push("message");
            assert_eq!(
                input
                    .iter()
                    .map(|item| item["type"].as_str().unwrap_or_default())
                    .collect::<Vec<_>>(),
                expected_input_types,
                "{} Responses input order",
                direction.name
            );
            let calls: Vec<&Value> = input
                .iter()
                .filter(|item| item["type"] == "function_call")
                .collect();
            let outputs: Vec<&Value> = input
                .iter()
                .filter(|item| item["type"] == "function_call_output")
                .collect();
            assert_eq!(calls.len(), expected_names.len());
            assert_eq!(outputs.len(), expected_names.len());
            for ((call, output), name) in calls.iter().zip(outputs.iter()).zip(expected_names) {
                assert_eq!(
                    call["call_id"],
                    format!(
                        "call_fixture_{}",
                        name.strip_prefix("get_fixture_").unwrap()
                    )
                );
                assert_eq!(call["name"], *name);
                assert_eq!(call["arguments"], expected_tool_arguments(name));
                assert_eq!(output["call_id"], call["call_id"]);
            }
            let follow_up = if direction.source == ProtocolKind::OpenAiResponses {
                "fixture compatible follow-up"
            } else {
                "fixture follow-up after tool results"
            };
            assert!(input.iter().any(|item| {
                item["type"] == "message"
                    && item["role"] == "user"
                    && item["content"].as_array().is_some_and(|content| {
                        content
                            .iter()
                            .any(|part| part["type"] == "input_text" && part["text"] == follow_up)
                    })
            }));
        }
    }
}

fn assert_chat_parallel_semantics(direction: Direction, body: &Value, expected_names: &[&str]) {
    let actual = body.get("parallel_tool_calls");
    match (direction.source, expected_names.len()) {
        (ProtocolKind::AnthropicMessages, 2) => assert!(
            actual.is_none(),
            "{} Chat parallel_tool_calls must use the tested Anthropic protocol default",
            direction.name
        ),
        (_, 2) => assert_eq!(
            actual,
            Some(&json!(true)),
            "{} Chat parallel_tool_calls",
            direction.name
        ),
        (_, 1) => assert!(
            actual.is_none(),
            "{} Chat single-tool parallel_tool_calls must be absent",
            direction.name
        ),
        _ => unreachable!("tool request must contain one or two fixture tools"),
    }
}

#[test]
fn chat_tool_request_assertions_reject_wrong_call_id() {
    let direction = DIRECTIONS[0];
    let mut body = fixture_json("chat_parallel_anthropic_request");
    body["messages"].as_array_mut().unwrap().remove(1);
    body["messages"][2]["tool_calls"][0]["id"] = json!("wrong-call-id");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_tool_request_with_names(
            direction,
            &body,
            &["get_fixture_weather", "get_fixture_time"],
        );
    }));

    assert!(result.is_err(), "Chat tool call ID drift was not rejected");
}

#[test]
fn chat_tool_request_assertions_reject_wrong_parallel_semantics() {
    let direction = DIRECTIONS[0];
    let mut body = fixture_json("chat_parallel_anthropic_request");
    body["messages"].as_array_mut().unwrap().remove(1);
    body["parallel_tool_calls"] = json!(false);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_tool_request_with_names(
            direction,
            &body,
            &["get_fixture_weather", "get_fixture_time"],
        );
    }));

    assert!(
        result.is_err(),
        "Chat parallel_tool_calls drift was not rejected"
    );
}

#[test]
fn anthropic_text_stream_assertions_reject_reordered_lifecycle() {
    let reordered = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_fixture_text_stream\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"fixture-anthropic-model\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":12,\"output_tokens\":0}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"fixture response text\"}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":12,\"output_tokens\":4}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_text_stream_response(ProtocolKind::AnthropicMessages, reordered.as_bytes());
    }));

    assert!(
        result.is_err(),
        "reordered Anthropic lifecycle was not rejected"
    );
}

#[test]
fn responses_tool_lifecycle_rejects_missing_argument_done() {
    let mut frames = parse_sse_frames(include_str!(
        "fixtures/runtime_bridge/responses_single_tool_stream.sse"
    ));
    for frame in &mut frames {
        if frame.event == "response.output_item.added" || frame.event == "response.output_item.done"
        {
            frame.data["item"]["id"] = json!("fc_call_fixture_weather");
        }
        if frame.data.get("item_id").is_some() {
            frame.data["item_id"] = json!("fc_call_fixture_weather");
        }
    }
    frames.retain(|frame| frame.event != "response.function_call_arguments.done");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_responses_tool_lifecycle(&frames, &["get_fixture_weather"]);
    }));

    assert!(
        result.is_err(),
        "missing Responses argument finalization was not rejected"
    );
}

#[test]
fn mock_upstream_reports_pre_capture_read_failure() {
    let upstream = MockUpstream::start(MockReply::json(json!({"ok": true})));
    let stream = TcpStream::connect(upstream.address).expect("connect mock upstream");
    drop(stream);

    let result = upstream.finish_result();
    assert!(
        matches!(result, Err(ref error) if error.contains("mock request")),
        "pre-capture mock failure was not propagated: {result:?}"
    );
}

fn assert_tool_response(direction: Direction, body: &[u8]) {
    let body: Value = serde_json::from_slice(body).expect("tool response JSON");
    match direction.source {
        ProtocolKind::AnthropicMessages => {
            assert_eq!(body["content"].as_array().map(Vec::len), Some(1));
            assert_eq!(body["stop_reason"], "tool_use");
            assert_eq!(body["content"][0]["type"], "tool_use");
            assert_eq!(body["content"][0]["id"], "call_fixture_weather");
            assert_eq!(body["content"][0]["name"], "get_fixture_weather");
            assert_eq!(body["content"][0]["input"], json!({"city": "fixture-city"}));
            assert_eq!(body["usage"]["input_tokens"], 12);
            assert_eq!(body["usage"]["output_tokens"], 8);
        }
        ProtocolKind::OpenAiChat => {
            let message = &body["choices"][0]["message"];
            assert_eq!(body["choices"].as_array().map(Vec::len), Some(1));
            assert_eq!(message["tool_calls"].as_array().map(Vec::len), Some(1));
            assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
            assert_eq!(message["tool_calls"][0]["id"], "call_fixture_weather");
            assert_eq!(
                message["tool_calls"][0]["function"]["name"],
                "get_fixture_weather"
            );
            assert_eq!(
                message["tool_calls"][0]["function"]["arguments"],
                "{\"city\":\"fixture-city\"}"
            );
            assert_eq!(body["usage"]["prompt_tokens"], 12);
            assert_eq!(body["usage"]["completion_tokens"], 8);
            assert_eq!(body["usage"]["total_tokens"], 20);
        }
        ProtocolKind::OpenAiResponses => {
            let item = &body["output"][0];
            assert_eq!(body["output"].as_array().map(Vec::len), Some(1));
            assert_eq!(body["status"], "completed");
            let expected_item_id = if direction.target == ProtocolKind::OpenAiResponses {
                "fc_fixture_weather"
            } else {
                "fc_call_fixture_weather"
            };
            assert_eq!(item["id"], expected_item_id);
            assert_eq!(item["type"], "function_call");
            assert_eq!(item["call_id"], "call_fixture_weather");
            assert_eq!(item["name"], "get_fixture_weather");
            assert_eq!(item["arguments"], "{\"city\":\"fixture-city\"}");
            assert_eq!(body["usage"]["input_tokens"], 12);
            assert_eq!(body["usage"]["output_tokens"], 8);
            assert_eq!(body["usage"]["total_tokens"], 20);
            if direction.target == ProtocolKind::OpenAiResponses {
                assert_eq!(body["usage"]["input_tokens_details"]["cached_tokens"], 0);
                assert_eq!(
                    body["usage"]["input_tokens_details"]["cache_write_tokens"],
                    0
                );
                assert_eq!(
                    body["usage"]["output_tokens_details"]["reasoning_tokens"],
                    0
                );
            } else {
                assert!(body["usage"]["input_tokens_details"].is_null());
                assert!(body["usage"]["output_tokens_details"].is_null());
            }
        }
    }
}

fn assert_upstream_auth(target: ProtocolKind, headers: &BTreeMap<String, String>) {
    match target {
        ProtocolKind::AnthropicMessages => {
            assert_eq!(headers.get("x-api-key"), Some(&"fixture-key".to_string()));
            assert_eq!(
                headers.get("anthropic-version"),
                Some(&"2023-06-01".to_string())
            );
        }
        ProtocolKind::OpenAiChat | ProtocolKind::OpenAiResponses => assert_eq!(
            headers.get("authorization"),
            Some(&"Bearer fixture-key".to_string())
        ),
    }
    assert_eq!(headers.get("x-spec-runtime-hop"), Some(&"1".to_string()));
}

fn assert_stream_response(source: ProtocolKind, body: &[u8]) {
    assert_tool_stream_response(source, body, &["get_fixture_weather", "get_fixture_time"]);
}

fn assert_text_stream_response(source: ProtocolKind, body: &[u8]) {
    let body = String::from_utf8(body.to_vec()).expect("downstream text SSE is UTF-8");
    let frames = parse_sse_frames(&body);
    assert!(!frames.is_empty(), "empty downstream SSE");
    match source {
        ProtocolKind::AnthropicMessages => {
            assert_anthropic_text_lifecycle(&frames);
            let events: Vec<&str> = frames.iter().map(|frame| frame.event.as_str()).collect();
            assert_eq!(events.first(), Some(&"message_start"));
            assert_eq!(events.last(), Some(&"message_stop"));
            assert_eq!(
                events
                    .iter()
                    .filter(|event| **event == "message_start")
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| **event == "content_block_start")
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| **event == "content_block_stop")
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| **event == "message_stop")
                    .count(),
                1
            );
            let text: String = frames
                .iter()
                .filter(|frame| frame.event == "content_block_delta")
                .map(|frame| {
                    frame.data["delta"]["text"]
                        .as_str()
                        .expect("Anthropic text delta")
                })
                .collect();
            assert_eq!(text, "fixture response text");
            assert!(frames.iter().any(|frame| {
                frame.event == "message_delta" && frame.data["delta"]["stop_reason"] == "end_turn"
            }));
        }
        ProtocolKind::OpenAiChat => {
            assert_chat_text_lifecycle(&frames);
            assert_eq!(
                frames.last().expect("Chat terminal frame").data_raw,
                "[DONE]"
            );
            let json_frames = &frames[..frames.len() - 1];
            assert!(!json_frames.is_empty());
            assert!(json_frames.iter().all(|frame| frame.event.is_empty()));
            let text: String = json_frames
                .iter()
                .filter_map(|frame| frame.data["choices"].as_array())
                .flat_map(|choices| choices.iter())
                .filter_map(|choice| choice["delta"]["content"].as_str())
                .collect();
            assert_eq!(text, "fixture response text");
            let finish_count = json_frames
                .iter()
                .filter(|frame| {
                    frame.data["choices"].as_array().is_some_and(|choices| {
                        choices
                            .iter()
                            .any(|choice| choice["finish_reason"] == "stop")
                    })
                })
                .count();
            assert_eq!(finish_count, 1, "Chat text frames: {json_frames:?}");
            assert!(json_frames.iter().all(|frame| {
                frame.data["choices"].as_array().is_some_and(|choices| {
                    choices
                        .iter()
                        .all(|choice| choice["delta"].get("tool_calls").is_none())
                })
            }));
        }
        ProtocolKind::OpenAiResponses => {
            assert_responses_text_lifecycle(&frames);
            let events: Vec<&str> = frames.iter().map(|frame| frame.event.as_str()).collect();
            assert_eq!(events.first(), Some(&"response.created"));
            assert_eq!(events.last(), Some(&"response.completed"));
            for event in [
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ] {
                assert_eq!(
                    events
                        .iter()
                        .filter(|candidate| **candidate == event)
                        .count(),
                    1,
                    "unexpected {event} lifecycle count: {events:?}"
                );
            }
            assert_eq!(
                events
                    .iter()
                    .filter(|candidate| **candidate == "response.content_part.added")
                    .count(),
                1,
                "unexpected content part lifecycle count: {events:?}"
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|candidate| **candidate == "response.output_text.delta")
                    .count(),
                2,
                "unexpected text delta lifecycle count: {events:?}"
            );
            let text: String = frames
                .iter()
                .filter(|frame| frame.event == "response.output_text.delta")
                .map(|frame| frame.data["delta"].as_str().expect("Responses text delta"))
                .collect();
            assert_eq!(text, "fixture response text");
        }
    }
}

#[derive(Debug)]
struct ParsedSseFrame {
    event: String,
    data_raw: String,
    data: Value,
}

fn parse_sse_frames(body: &str) -> Vec<ParsedSseFrame> {
    body.split("\n\n")
        .filter_map(|block| {
            let mut event = String::new();
            let mut data_raw = String::new();
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("event: ") {
                    event = value.to_string();
                } else if let Some(value) = line.strip_prefix("data: ") {
                    if !data_raw.is_empty() {
                        data_raw.push('\n');
                    }
                    data_raw.push_str(value);
                }
            }
            if data_raw.is_empty() {
                return None;
            }
            let data = if data_raw == "[DONE]" {
                Value::Null
            } else {
                serde_json::from_str(&data_raw).expect("SSE frame JSON")
            };
            Some(ParsedSseFrame {
                event,
                data_raw,
                data,
            })
        })
        .collect()
}

fn assert_anthropic_text_lifecycle(frames: &[ParsedSseFrame]) {
    let events: Vec<&str> = frames.iter().map(|frame| frame.event.as_str()).collect();
    assert_eq!(events.first(), Some(&"message_start"));
    assert_eq!(events.last(), Some(&"message_stop"));
    assert!(events.iter().all(|event| {
        matches!(
            *event,
            "message_start"
                | "content_block_start"
                | "content_block_delta"
                | "content_block_stop"
                | "message_delta"
                | "message_stop"
        )
    }));
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "message_start")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "content_block_start")
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .filter(|event| **event == "content_block_delta")
            .count()
            > 0
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "content_block_stop")
            .count(),
        1
    );
    let terminal_message_deltas: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| {
            frame.event == "message_delta" && !frame.data["delta"]["stop_reason"].is_null()
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        terminal_message_deltas.len(),
        1,
        "Anthropic text lifecycle: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "message_stop")
            .count(),
        1
    );

    let start = events
        .iter()
        .position(|event| *event == "content_block_start")
        .unwrap();
    let first_delta = events
        .iter()
        .position(|event| *event == "content_block_delta")
        .unwrap();
    let stop = events
        .iter()
        .position(|event| *event == "content_block_stop")
        .unwrap();
    let message_delta = terminal_message_deltas[0];
    assert!(start < first_delta && first_delta < stop && stop < message_delta);
    assert_eq!(
        frames[message_delta].data["delta"]["stop_reason"],
        "end_turn"
    );
    assert!(matches!(
        frames[0].data["message"]["usage"]["input_tokens"].as_u64(),
        Some(0 | 12)
    ));
    let input_usage = frames
        .iter()
        .find(|frame| frame.data["usage"]["input_tokens"].is_number())
        .expect("Anthropic text input usage");
    assert_eq!(input_usage.data["usage"]["input_tokens"], 12);
    assert_eq!(frames[message_delta].data["usage"]["output_tokens"], 4);
}

fn assert_chat_text_lifecycle(frames: &[ParsedSseFrame]) {
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame.data_raw == "[DONE]")
            .count(),
        1
    );
    assert_eq!(
        frames.last().map(|frame| frame.data_raw.as_str()),
        Some("[DONE]")
    );
    let json_frames = &frames[..frames.len() - 1];
    assert!(!json_frames.is_empty());
    assert!(json_frames.iter().all(|frame| frame.event.is_empty()));
    assert_eq!(
        json_frames[0].data["choices"][0]["delta"]["role"],
        "assistant"
    );

    let finish_positions: Vec<usize> = json_frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| {
            frame.data["choices"].as_array().is_some_and(|choices| {
                choices
                    .iter()
                    .any(|choice| choice["finish_reason"] == "stop")
            })
        })
        .map(|(index, _)| index)
        .collect();
    let usage_positions: Vec<usize> = json_frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| frame.data["usage"].is_object())
        .map(|(index, _)| index)
        .collect();
    let content_positions: Vec<usize> = json_frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| {
            frame.data["choices"].as_array().is_some_and(|choices| {
                choices
                    .iter()
                    .any(|choice| choice["delta"]["content"].is_string())
            })
        })
        .map(|(index, _)| index)
        .collect();
    assert!(!content_positions.is_empty());
    assert_eq!(finish_positions.len(), 1);
    assert!(!usage_positions.is_empty());
    assert!(content_positions
        .iter()
        .all(|index| *index < finish_positions[0]));
    assert!(usage_positions.iter().all(|index| {
        json_frames[*index].data["choices"]
            .as_array()
            .is_some_and(Vec::is_empty)
    }));
    let usage = &json_frames[*usage_positions.last().unwrap()].data["usage"];
    assert_eq!(usage["prompt_tokens"], 12);
    assert_eq!(usage["completion_tokens"], 4);
    assert_eq!(usage["total_tokens"], 16);
}

fn assert_anthropic_tool_lifecycle(frames: &[ParsedSseFrame], expected_names: &[&str]) {
    let events: Vec<&str> = frames.iter().map(|frame| frame.event.as_str()).collect();
    assert_eq!(events.first(), Some(&"message_start"));
    assert_eq!(events.last(), Some(&"message_stop"));
    assert!(events.iter().all(|event| {
        matches!(
            *event,
            "message_start"
                | "content_block_start"
                | "content_block_delta"
                | "content_block_stop"
                | "message_delta"
                | "message_stop"
        )
    }));
    let starts: Vec<(usize, &ParsedSseFrame)> = frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| frame.event == "content_block_start")
        .collect();
    let stops: Vec<(usize, &ParsedSseFrame)> = frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| frame.event == "content_block_stop")
        .collect();
    assert_eq!(starts.len(), expected_names.len());
    assert_eq!(stops.len(), expected_names.len());
    let terminal_message_deltas: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| {
            frame.event == "message_delta" && !frame.data["delta"]["stop_reason"].is_null()
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        terminal_message_deltas.len(),
        1,
        "Anthropic tool lifecycle: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "message_stop")
            .count(),
        1
    );

    let first_stop = stops[0].0;
    let first_delta = events
        .iter()
        .position(|event| *event == "content_block_delta")
        .expect("Anthropic tool argument delta");
    assert!(starts.iter().all(|(index, _)| *index < first_stop));
    assert!(first_delta > starts[0].0);
    assert!(frames.iter().enumerate().all(|(index, frame)| {
        frame.event != "content_block_delta" || (index > starts[0].0 && index < first_stop)
    }));

    for (position, ((start_index, start), (stop_index, stop))) in
        starts.iter().zip(stops.iter()).enumerate()
    {
        assert_eq!(start.data["index"], position as u64);
        assert_eq!(stop.data["index"], position as u64);
        assert_eq!(start.data["content_block"]["type"], "tool_use");
        assert_eq!(
            start.data["content_block"]["id"],
            format!(
                "call_fixture_{}",
                expected_names[position]
                    .strip_prefix("get_fixture_")
                    .unwrap()
            )
        );
        assert_eq!(
            start.data["content_block"]["name"],
            expected_names[position]
        );
        assert!(*start_index < *stop_index);
    }
    let message_delta = terminal_message_deltas[0];
    assert!(stops.iter().all(|(index, _)| *index < message_delta));
    assert_eq!(
        frames[message_delta].data["delta"]["stop_reason"],
        "tool_use"
    );
    let usage = frames
        .iter()
        .find(|frame| {
            frame.event == "message_delta" && frame.data["usage"]["input_tokens"].is_number()
        })
        .expect("Anthropic tool usage");
    let input_tokens = usage.data["usage"]["input_tokens"].as_u64().unwrap();
    let cache_read = usage.data["usage"]["cache_read_input_tokens"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(
        input_tokens + cache_read,
        12,
        "streaming usage must not double count the cached prefix"
    );
    assert_eq!(
        usage.data["usage"]["output_tokens"],
        if expected_names.len() > 1 { 8 } else { 4 }
    );
}

fn assert_chat_tool_lifecycle(frames: &[ParsedSseFrame], expected_output_tokens: u64) {
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame.data_raw == "[DONE]")
            .count(),
        1
    );
    assert_eq!(
        frames.last().map(|frame| frame.data_raw.as_str()),
        Some("[DONE]")
    );
    let json_frames = &frames[..frames.len() - 1];
    assert_eq!(
        json_frames[0].data["choices"][0]["delta"]["role"],
        "assistant"
    );
    let finish = json_frames
        .iter()
        .enumerate()
        .find(|(_, frame)| {
            frame.data["choices"].as_array().is_some_and(|choices| {
                choices
                    .iter()
                    .any(|choice| choice["finish_reason"] == "tool_calls")
            })
        })
        .map(|(index, _)| index)
        .expect("Chat tool terminal");
    assert_eq!(
        json_frames
            .iter()
            .filter(
                |frame| frame.data["choices"].as_array().is_some_and(|choices| {
                    choices
                        .iter()
                        .any(|choice| choice["finish_reason"] == "tool_calls")
                })
            )
            .count(),
        1
    );
    let usage_positions: Vec<usize> = json_frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| frame.data["usage"].is_object())
        .map(|(index, _)| index)
        .collect();
    assert!(!usage_positions.is_empty());
    assert!(usage_positions.iter().all(|index| {
        json_frames[*index].data["choices"]
            .as_array()
            .is_some_and(Vec::is_empty)
    }));
    let final_usage = &json_frames[*usage_positions.last().unwrap()].data["usage"];
    assert_eq!(final_usage["prompt_tokens"], 12);
    assert_eq!(final_usage["completion_tokens"], expected_output_tokens);
    assert_eq!(final_usage["total_tokens"], 12 + expected_output_tokens);
    assert!(json_frames.iter().enumerate().all(|(index, frame)| {
        frame.data["choices"].as_array().is_none_or(|choices| {
            index <= finish
                || choices
                    .iter()
                    .all(|choice| choice["delta"].get("tool_calls").is_none())
        })
    }));
}

fn assert_responses_text_lifecycle(frames: &[ParsedSseFrame]) {
    let events: Vec<&str> = frames.iter().map(|frame| frame.event.as_str()).collect();
    assert_eq!(events.first(), Some(&"response.created"));
    assert_eq!(events.get(1), Some(&"response.in_progress"));
    assert_eq!(events.last(), Some(&"response.completed"));
    let required = [
        "response.created",
        "response.in_progress",
        "response.output_item.added",
        "response.content_part.added",
        "response.output_text.done",
        "response.content_part.done",
        "response.output_item.done",
        "response.completed",
    ];
    for event in required {
        assert_eq!(
            events
                .iter()
                .filter(|candidate| **candidate == event)
                .count(),
            1
        );
    }
    let delta_positions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| **event == "response.output_text.delta")
        .map(|(index, _)| index)
        .collect();
    assert!(!delta_positions.is_empty());
    let positions = |event: &str| {
        events
            .iter()
            .position(|candidate| *candidate == event)
            .unwrap()
    };
    assert!(
        positions("response.output_item.added") < positions("response.content_part.added")
            && positions("response.content_part.added") < delta_positions[0]
            && delta_positions.last().unwrap() < &positions("response.output_text.done")
            && positions("response.output_text.done") < positions("response.content_part.done")
            && positions("response.content_part.done") < positions("response.output_item.done")
            && positions("response.output_item.done") < positions("response.completed")
    );
}

fn assert_responses_tool_lifecycle(frames: &[ParsedSseFrame], expected_names: &[&str]) {
    let events: Vec<&str> = frames.iter().map(|frame| frame.event.as_str()).collect();
    assert_eq!(events.first(), Some(&"response.created"));
    assert_eq!(events.get(1), Some(&"response.in_progress"));
    assert_eq!(events.last(), Some(&"response.completed"));
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "response.created")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "response.in_progress")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "response.completed")
            .count(),
        1
    );

    let starts: Vec<(usize, &ParsedSseFrame)> = frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| frame.event == "response.output_item.added")
        .collect();
    assert_eq!(starts.len(), expected_names.len());
    for (position, (start_index, start)) in starts.iter().enumerate() {
        let output_index = position as u64;
        assert_eq!(start.data["output_index"], output_index);
        assert_eq!(start.data["item"]["type"], "function_call");
        assert_eq!(
            start.data["item"]["id"],
            format!(
                "fc_call_fixture_{}",
                expected_names[position]
                    .strip_prefix("get_fixture_")
                    .unwrap()
            )
        );
        assert_eq!(
            start.data["item"]["call_id"],
            format!(
                "call_fixture_{}",
                expected_names[position]
                    .strip_prefix("get_fixture_")
                    .unwrap()
            )
        );
        assert_eq!(start.data["item"]["name"], expected_names[position]);

        let delta_positions: Vec<usize> = frames
            .iter()
            .enumerate()
            .filter(|(_, frame)| {
                frame.event == "response.function_call_arguments.delta"
                    && frame.data["output_index"] == output_index
            })
            .map(|(index, _)| index)
            .collect();
        assert!(!delta_positions.is_empty());
        assert!(delta_positions.iter().all(|index| *index > *start_index));
        let argument_done_positions: Vec<(usize, &ParsedSseFrame)> = frames
            .iter()
            .enumerate()
            .filter(|(_, frame)| {
                frame.event == "response.function_call_arguments.done"
                    && frame.data["output_index"] == output_index
            })
            .collect();
        assert_eq!(
            argument_done_positions.len(),
            1,
            "Responses argument finalization count for output index {output_index}"
        );
        let (argument_done_index, argument_done) = argument_done_positions[0];
        let item_done = frames
            .iter()
            .enumerate()
            .find(|(_, frame)| {
                frame.event == "response.output_item.done"
                    && frame.data["output_index"] == output_index
            })
            .map(|(index, _)| index)
            .expect("Responses output item terminal");
        assert!(delta_positions.iter().all(|index| *index < item_done));
        assert!(delta_positions
            .iter()
            .all(|index| *index < argument_done_index));
        assert!(argument_done_index < item_done);
        assert_eq!(argument_done.data["item_id"], start.data["item"]["id"]);
        assert_eq!(
            argument_done.data["arguments"],
            expected_tool_arguments(expected_names[position])
        );
    }
}

fn assert_stream_terminal(source: ProtocolKind, body: &str) {
    let frames = parse_sse_frames(body);
    let events: Vec<&str> = frames.iter().map(|frame| frame.event.as_str()).collect();
    match source {
        ProtocolKind::AnthropicMessages => {
            assert_eq!(events.first(), Some(&"message_start"));
            assert_eq!(events.last(), Some(&"message_stop"));
            assert_eq!(
                events
                    .iter()
                    .filter(|event| **event == "message_start")
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| **event == "message_stop")
                    .count(),
                1
            );
            assert_eq!(
                frames
                    .iter()
                    .filter(|frame| {
                        frame.event == "message_delta"
                            && !frame.data["delta"]["stop_reason"].is_null()
                    })
                    .count(),
                1
            );
        }
        ProtocolKind::OpenAiChat => {
            assert_eq!(
                frames.last().map(|frame| frame.data_raw.as_str()),
                Some("[DONE]")
            );
            assert_eq!(
                frames
                    .iter()
                    .filter(|frame| frame.data_raw == "[DONE]")
                    .count(),
                1
            );
            assert_eq!(
                frames
                    .iter()
                    .filter(
                        |frame| frame.data["choices"].as_array().is_some_and(|choices| {
                            choices
                                .iter()
                                .any(|choice| choice["finish_reason"] == "tool_calls")
                        })
                    )
                    .count(),
                1,
                "Chat tool stream frames: {frames:?}"
            );
        }
        ProtocolKind::OpenAiResponses => {
            assert_eq!(events.first(), Some(&"response.created"));
            assert_eq!(events.last(), Some(&"response.completed"));
            assert_eq!(
                events
                    .iter()
                    .filter(|event| **event == "response.created")
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| **event == "response.completed")
                    .count(),
                1
            );
        }
    }
}

fn assert_single_tool_stream_response(source: ProtocolKind, body: &[u8]) {
    assert_tool_stream_response(source, body, &["get_fixture_weather"]);
}

fn assert_tool_stream_response(source: ProtocolKind, body: &[u8], expected_names: &[&str]) {
    let body = String::from_utf8(body.to_vec()).expect("downstream tool SSE is UTF-8");
    let frames = parse_sse_frames(&body);
    assert_stream_terminal(source, &body);
    match source {
        ProtocolKind::AnthropicMessages => assert_anthropic_tool_lifecycle(&frames, expected_names),
        ProtocolKind::OpenAiChat => {
            assert_chat_tool_lifecycle(&frames, if expected_names.len() == 2 { 8 } else { 4 })
        }
        ProtocolKind::OpenAiResponses => assert_responses_tool_lifecycle(&frames, expected_names),
    }

    match source {
        ProtocolKind::AnthropicMessages => {
            let starts: Vec<&ParsedSseFrame> = frames
                .iter()
                .filter(|frame| frame.event == "content_block_start")
                .collect();
            assert_eq!(starts.len(), expected_names.len());
            for (start, name) in starts.iter().zip(expected_names) {
                let block = &start.data["content_block"];
                assert_eq!(block["type"], "tool_use");
                assert_eq!(
                    block["id"],
                    format!(
                        "call_fixture_{}",
                        name.strip_prefix("get_fixture_").unwrap()
                    )
                );
                assert_eq!(block["name"], *name);
                assert_eq!(block["input"], json!({}));
                let index = start.data["index"].as_u64().expect("Anthropic block index");
                let arguments: String = frames
                    .iter()
                    .filter(|frame| {
                        frame.event == "content_block_delta" && frame.data["index"] == index
                    })
                    .map(|frame| {
                        frame.data["delta"]["partial_json"]
                            .as_str()
                            .expect("Anthropic tool argument delta")
                    })
                    .collect();
                assert_eq!(arguments, expected_tool_arguments(name));
                assert!(serde_json::from_str::<Value>(&arguments).is_ok());
            }
            assert_eq!(
                frames
                    .iter()
                    .filter(|frame| frame.event == "content_block_stop")
                    .count(),
                expected_names.len()
            );
        }
        ProtocolKind::OpenAiChat => {
            let mut calls: Vec<(usize, String, String, String)> = Vec::new();
            for frame in frames.iter().filter(|frame| frame.data_raw != "[DONE]") {
                for choice in frame.data["choices"].as_array().into_iter().flatten() {
                    for call in choice["delta"]["tool_calls"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        let index = call["index"].as_u64().expect("Chat tool index") as usize;
                        let position = calls.iter().position(|call| call.0 == index);
                        if let Some(position) = position {
                            if let Some(id) = call["id"].as_str() {
                                calls[position].1 = id.to_string();
                            }
                            if let Some(name) = call["function"]["name"].as_str() {
                                calls[position].2 = name.to_string();
                            }
                            if let Some(arguments) = call["function"]["arguments"].as_str() {
                                calls[position].3.push_str(arguments);
                            }
                        } else {
                            calls.push((
                                index,
                                call["id"].as_str().unwrap_or_default().to_string(),
                                call["function"]["name"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                                call["function"]["arguments"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                            ));
                        }
                    }
                }
            }
            calls.sort_by_key(|call| call.0);
            assert_eq!(calls.len(), expected_names.len());
            for (position, ((index, id, name, arguments), expected_name)) in
                calls.iter().zip(expected_names).enumerate()
            {
                assert_eq!(*index, position, "Chat tool call wire index");
                assert_eq!(
                    id,
                    &format!(
                        "call_fixture_{}",
                        expected_name.strip_prefix("get_fixture_").unwrap()
                    )
                );
                assert_eq!(name, expected_name);
                assert_eq!(arguments, expected_tool_arguments(expected_name));
                assert!(serde_json::from_str::<Value>(arguments).is_ok());
            }
        }
        ProtocolKind::OpenAiResponses => {
            let starts: Vec<&ParsedSseFrame> = frames
                .iter()
                .filter(|frame| frame.event == "response.output_item.added")
                .collect();
            assert_eq!(starts.len(), expected_names.len());
            for (position, (start, name)) in starts.iter().zip(expected_names).enumerate() {
                let item = &start.data["item"];
                assert_eq!(start.data["output_index"], position as u64);
                let call_id = format!(
                    "call_fixture_{}",
                    name.strip_prefix("get_fixture_").unwrap()
                );
                assert_eq!(item["type"], "function_call");
                assert_eq!(item["id"], format!("fc_{call_id}"));
                assert_eq!(item["call_id"], call_id);
                assert_eq!(item["name"], *name);
                let index = start.data["output_index"]
                    .as_u64()
                    .expect("Responses output index");
                let arguments: String = frames
                    .iter()
                    .filter(|frame| {
                        frame.event == "response.function_call_arguments.delta"
                            && frame.data["output_index"] == index
                    })
                    .map(|frame| {
                        frame.data["delta"]
                            .as_str()
                            .expect("Responses tool argument delta")
                    })
                    .collect();
                assert_eq!(arguments, expected_tool_arguments(name));
                assert!(serde_json::from_str::<Value>(&arguments).is_ok());
            }
            let done_indices: Vec<u64> = frames
                .iter()
                .filter(|frame| frame.event == "response.output_item.done")
                .map(|frame| {
                    frame.data["output_index"]
                        .as_u64()
                        .expect("Responses tool output index")
                })
                .collect();
            assert_eq!(
                done_indices,
                (0..expected_names.len() as u64).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn test_helper_dispatches_health_without_a_runtime_daemon() {
    let request = runtime::http_request_for_test("GET", "/health", Vec::new(), 0);

    let (status, headers, body) = runtime::handle_request_for_test(
        request,
        &tempfile::tempdir().unwrap().keep(),
        &[provider()],
    )
    .expect("health request works");

    assert_eq!(status, 200);
    assert!(headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case("content-type") && value == "application/json"
    }));
    let body = serde_json::from_slice::<Value>(&body).expect("health response JSON");
    assert_eq!(
        body["capabilities"]["entry_points"]["anthropic_messages"],
        true
    );
    assert_eq!(body["capabilities"]["entry_points"]["openai_chat"], true);
    assert_eq!(
        body["capabilities"]["entry_points"]["openai_responses"],
        true
    );
    assert_eq!(body["capabilities"]["streaming"], true);
    assert_eq!(body["capabilities"]["tools"], true);
    assert_eq!(body["capabilities"]["tool_execution"], false);
    assert_eq!(body["capabilities"]["reasoning"], "partial");
}

#[test]
fn claude_code_probe_dispatches_without_a_runtime_daemon() {
    let request = runtime::http_request_for_test("HEAD", "/api/hello", Vec::new(), 0);

    let (status, _, _) = runtime::handle_request_for_test(
        request,
        &tempfile::tempdir().unwrap().keep(),
        &[provider()],
    )
    .expect("Claude Code probe should be handled");

    assert_eq!(status, 200);
}

#[test]
fn non_streaming_routes_cover_all_protocol_directions() {
    for direction in DIRECTIONS {
        let response = if rich_response_is_representable(direction) {
            tool_response_for_target(direction.target)
        } else {
            response_for_target(direction.target, direction.source)
        };
        let upstream = MockUpstream::start(MockReply::json(response));
        let request = runtime::http_request_for_test(
            "POST",
            direction.source_path,
            serde_json::to_vec(&tool_request_for_direction(direction))
                .expect("source request serializes"),
            0,
        );
        let providers = [provider_for(direction.target, &upstream.base_url())];
        let (status, headers, body) = runtime::handle_request_for_test(
            request,
            &tempfile::tempdir().unwrap().keep(),
            &providers,
        )
        .unwrap_or_else(|error| panic!("{} route failed: {error}", direction.name));
        let captured = upstream.finish_optional();

        assert_eq!(
            status,
            200,
            "{} downstream status body={:?} upstream={:?}",
            direction.name,
            String::from_utf8_lossy(&body),
            captured
        );
        let captured = captured.expect("successful route dispatched upstream");
        if rich_response_is_representable(direction) {
            assert_tool_response(direction, &body);
        } else {
            assert_text_response(direction, &body);
        }
        assert_eq!(captured.path, upstream_path(direction.target));
        assert_tool_request(direction, &captured.body);
        assert_eq!(captured.body["stream"], false);
        assert_upstream_auth(direction.target, &captured.headers);
        assert!(headers.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("content-type") && value == "application/json"
        }));
    }
}

#[test]
fn converted_route_accepts_upstream_null_tool_calls() {
    let reply = json!({
        "id": "chatcmpl_null_tools",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "fixture-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "plain reply",
                "tool_calls": null
            },
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
    });
    let upstream = MockUpstream::start(MockReply::json(reply));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&request_for_source(ProtocolKind::AnthropicMessages)).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("null tool_calls converted route handled");
    let _ = upstream.finish();

    assert_eq!(status, 200, "body={:?}", String::from_utf8_lossy(&body));
    let body: Value = serde_json::from_slice(&body).expect("anthropic response JSON");
    assert_eq!(body["type"], "message");
    assert_eq!(body["content"][0]["type"], "text");
    assert_eq!(body["content"][0]["text"], "plain reply");
}

#[test]
fn converted_route_accepts_upstream_tool_calls_with_index_field() {
    let reply = json!({
        "id": "chatcmpl_tool_index",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "fixture-model",
        "choices": [{
            "index": 0,
            "finish_reason": "tool_calls",
            "logprobs": null,
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\": \"/tmp/x\"}"}
                }]
            }
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    });
    let upstream = MockUpstream::start(MockReply::json(reply));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&tool_request_for_source(ProtocolKind::AnthropicMessages)).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("tool reply with index handled");
    let _ = upstream.finish();

    assert_eq!(status, 200, "body={:?}", String::from_utf8_lossy(&body));
    let body: Value = serde_json::from_slice(&body).expect("anthropic response JSON");
    assert!(body["content"].as_array().unwrap().iter().any(|block| {
        block["type"] == "tool_use"
            && block["name"] == "read_file"
            && block["input"] == json!({"path": "/tmp/x"})
    }));
}

#[test]
fn text_routes_cover_all_protocol_directions() {
    for direction in DIRECTIONS {
        let reply = text_response_for_direction(direction);
        assert_reply_has_terminal(&reply, direction.target);
        let upstream = MockUpstream::start(MockReply::json(reply));
        let request = runtime::http_request_for_test(
            "POST",
            direction.source_path,
            serde_json::to_vec(&request_for_source(direction.source))
                .expect("source request serializes"),
            0,
        );
        let providers = [mock_provider_for(direction.target, &upstream.base_url())];
        let (status, headers, body) = runtime::handle_request_for_test(
            request,
            &tempfile::tempdir().unwrap().keep(),
            &providers,
        )
        .unwrap_or_else(|error| panic!("{} text route failed: {error}", direction.name));
        let captured = upstream.finish_optional();

        assert_eq!(
            status,
            200,
            "{} downstream status body={:?} upstream={:?}",
            direction.name,
            String::from_utf8_lossy(&body),
            captured
        );
        let captured = captured.expect("successful text route dispatched upstream");
        assert_text_response(direction, &body);
        assert_eq!(captured.path, upstream_path(direction.target));
        assert_eq!(captured.body["model"], "fixture-model");
        assert_eq!(captured.body["stream"], false);
        assert_upstream_auth(direction.target, &captured.headers);
        assert!(headers.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("content-type") && value == "application/json"
        }));
    }
}

#[test]
fn empty_key_openai_provider_sends_no_auth_headers() {
    let upstream = MockUpstream::start(MockReply::json(response_for_target(
        ProtocolKind::OpenAiChat,
        ProtocolKind::OpenAiChat,
    )));
    let request = runtime::http_request_for_test(
        "POST",
        public_path(ProtocolKind::OpenAiChat),
        serde_json::to_vec(&request_for_source(ProtocolKind::OpenAiChat))
            .expect("OpenAI source request serializes"),
        0,
    );
    let providers = [provider_with_key(
        ProtocolKind::OpenAiChat,
        &upstream.base_url(),
        "",
    )];
    let (status, _, _) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("empty-key OpenAI route handled");
    let captured = upstream.finish();

    assert_eq!(status, 200);
    let headers = &captured.headers;
    assert!(
        !headers.contains_key("authorization"),
        "empty-key OpenAI provider must not send authorization: {captured:?}"
    );
    assert!(
        !headers.contains_key("x-api-key"),
        "empty-key OpenAI provider must not send x-api-key: {captured:?}"
    );
}

#[test]
fn empty_key_openai_provider_keeps_bearer_when_keyed() {
    let upstream = MockUpstream::start(MockReply::json(response_for_target(
        ProtocolKind::OpenAiChat,
        ProtocolKind::OpenAiChat,
    )));
    let request = runtime::http_request_for_test(
        "POST",
        public_path(ProtocolKind::OpenAiChat),
        serde_json::to_vec(&request_for_source(ProtocolKind::OpenAiChat))
            .expect("OpenAI source request serializes"),
        0,
    );
    let providers = [provider_with_key(
        ProtocolKind::OpenAiChat,
        &upstream.base_url(),
        "test-key",
    )];
    let (status, _, _) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("keyed OpenAI route handled");
    let captured = upstream.finish();

    assert_eq!(status, 200);
    assert_eq!(
        captured.headers.get("authorization"),
        Some(&"Bearer test-key".to_string()),
        "keyed OpenAI provider must send Bearer auth: {captured:?}"
    );
    assert!(
        !captured.headers.contains_key("x-api-key"),
        "keyed OpenAI provider must not send x-api-key: {captured:?}"
    );
}

#[test]
fn empty_key_anthropic_provider_sends_no_auth_headers() {
    let upstream = MockUpstream::start(MockReply::json(response_for_target(
        ProtocolKind::AnthropicMessages,
        ProtocolKind::AnthropicMessages,
    )));
    let request = runtime::http_request_for_test(
        "POST",
        public_path(ProtocolKind::AnthropicMessages),
        serde_json::to_vec(&request_for_source(ProtocolKind::AnthropicMessages))
            .expect("Anthropic source request serializes"),
        0,
    );
    let providers = [provider_with_key(
        ProtocolKind::AnthropicMessages,
        &upstream.base_url(),
        "",
    )];
    let (status, _, _) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("empty-key Anthropic route handled");
    let captured = upstream.finish();

    assert_eq!(status, 200);
    let headers = &captured.headers;
    assert!(
        !headers.contains_key("authorization"),
        "empty-key Anthropic provider must not send authorization: {captured:?}"
    );
    assert!(
        !headers.contains_key("x-api-key"),
        "empty-key Anthropic provider must not send x-api-key: {captured:?}"
    );
    assert_eq!(
        headers.get("anthropic-version"),
        Some(&"2023-06-01".to_string()),
        "anthropic-version must be sent even without a key: {captured:?}"
    );
}

#[test]
fn empty_key_anthropic_provider_keeps_x_api_key_when_keyed() {
    let upstream = MockUpstream::start(MockReply::json(response_for_target(
        ProtocolKind::AnthropicMessages,
        ProtocolKind::AnthropicMessages,
    )));
    let request = runtime::http_request_for_test(
        "POST",
        public_path(ProtocolKind::AnthropicMessages),
        serde_json::to_vec(&request_for_source(ProtocolKind::AnthropicMessages))
            .expect("Anthropic source request serializes"),
        0,
    );
    let providers = [provider_with_key(
        ProtocolKind::AnthropicMessages,
        &upstream.base_url(),
        "test-key",
    )];
    let (status, _, _) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("keyed Anthropic route handled");
    let captured = upstream.finish();

    assert_eq!(status, 200);
    assert_eq!(
        captured.headers.get("x-api-key"),
        Some(&"test-key".to_string()),
        "keyed Anthropic provider must send x-api-key: {captured:?}"
    );
    assert!(
        !captured.headers.contains_key("authorization"),
        "keyed Anthropic provider must not send authorization: {captured:?}"
    );
    assert_eq!(
        captured.headers.get("anthropic-version"),
        Some(&"2023-06-01".to_string())
    );
}

#[test]
fn anthropic_clean_request_fixture_routes_with_preserved_fields() {
    let direction = DIRECTIONS[0];
    let upstream = MockUpstream::start(MockReply::json(response_for_target(
        direction.target,
        direction.source,
    )));
    let request = runtime::http_request_for_test(
        "POST",
        direction.source_path,
        serde_json::to_vec(&set_fixture_request_defaults(
            fixture_json("anthropic_clean_request"),
            false,
        ))
        .expect("clean Anthropic request serializes"),
        0,
    );
    let providers = [provider_for(direction.target, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("clean Anthropic route handled");
    let captured = upstream.finish();

    assert_eq!(status, 200);
    assert_text_response(direction, &body);
    assert_eq!(captured.path, "/chat/completions");
    assert_eq!(captured.body["model"], "fixture-model");
    assert_eq!(captured.body["stream"], false);
    assert_eq!(captured.body["messages"][0]["role"], "system");
    assert_eq!(captured.body["messages"][1]["role"], "user");
    assert_eq!(
        captured.body["tools"][0]["function"]["name"],
        "get_fixture_weather"
    );
    assert_eq!(captured.body["temperature"], 0.2);
    assert_eq!(captured.body["top_p"], 0.8);
    assert_eq!(captured.body["stop"], json!(["STOP"]));
    assert_eq!(
        captured.body["metadata"]["fixture_id"],
        "anthropic_clean_text_request"
    );
    assert_upstream_auth(direction.target, &captured.headers);
}

#[test]
fn six_direction_request_contract_names_every_route() {
    for direction in DIRECTIONS {
        assert_eq!(
            runtime::bridge::protocol_for_path(direction.source_path),
            Some(direction.source.into()),
            "{} source path routes to the source protocol",
            direction.name
        );
        let input = fixture_for_direction(direction.name);
        let ir = runtime::bridge::parse_request(direction.source.into(), &input)
            .expect("fixture request parses");
        let body = runtime::bridge::encode_upstream_request(
            &ir,
            direction.target.into(),
            "fixture-target-model",
        )
        .expect("representable direction encodes");
        assert_eq!(body["model"], "fixture-target-model");
        assert_eq!(body["stream"], json!(ir.stream));
    }
}

fn assert_unsupported_field(error: runtime::bridge::BridgeError, field: &str) {
    match error {
        runtime::bridge::BridgeError::Unsupported { field: actual } => {
            assert_eq!(actual, field)
        }
        other => panic!("expected unsupported field {field:?}, got {other:?}"),
    }
}

fn assert_unsupported_envelope(status: u16, body: &[u8]) {
    assert_eq!(status, 400, "unsupported must surface as a 400: {body:?}");
    let body: Value = serde_json::from_slice(body).expect("unsupported error response JSON");
    assert_eq!(body["error"]["code"], "unsupported");
    assert_eq!(body["error"]["message"], "unsupported request field");
}

#[test]
fn anthropic_to_chat_reasoning_content_is_visible_text() {
    let upstream_reply = json!({
        "id": "chatcmpl_reasoning_fixture",
        "object": "chat.completion",
        "created": 1,
        "model": "fixture-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "visible fixture",
                "reasoning_content": "provider reasoning fixture"
            },
            "finish_reason": "stop",
            "logprobs": null
        }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 2,
            "total_tokens": 3
        }
    });
    let upstream = MockUpstream::start(MockReply::json(upstream_reply));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&request_for_source(ProtocolKind::AnthropicMessages)).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, headers, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("anthropic to chat reasoning route handled");
    let captured = upstream.finish();

    assert_eq!(status, 200);
    let body: Value = serde_json::from_slice(&body).expect("downstream Anthropic response JSON");
    let blocks = body["content"]
        .as_array()
        .expect("downstream content blocks");
    assert_eq!(
        body["content"],
        json!([
            { "type": "text", "text": "provider reasoning fixture" },
            { "type": "text", "text": "visible fixture" }
        ]),
        "Chat reasoning must reach Anthropic clients as two ordered text blocks"
    );
    assert!(
        blocks.iter().all(|block| block["type"] != "thinking"),
        "Chat reasoning must never be relayed as unsigned thinking"
    );
    assert_eq!(captured.path, "/chat/completions");
    assert_upstream_auth(ProtocolKind::OpenAiChat, &captured.headers);
    assert!(headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case("content-type") && value == "application/json"
    }));
}

#[test]
fn signed_thinking_request_to_chat_fails_closed_with_unsupported() {
    let mut request_body = request_for_source(ProtocolKind::AnthropicMessages);
    request_body["messages"]
        .as_array_mut()
        .expect("Anthropic messages array")
        .push(json!({
            "role": "assistant",
            "content": [
                { "type": "thinking", "thinking": "signed fixture", "signature": "fixture-signature" },
                { "type": "text", "text": "visible fixture" }
            ]
        }));
    let upstream = MockUpstream::start(MockReply::json(response_for_target(
        ProtocolKind::OpenAiChat,
        ProtocolKind::AnthropicMessages,
    )));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&request_body).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("signed thinking request handled");
    let captured = upstream.finish_result();

    match captured {
        Err(error) => assert!(
            error.contains("accept timed out"),
            "no connection may reach upstream for unrepresentable signed thinking: {error}"
        ),
        other => {
            panic!("unrepresentable signed thinking must never be relayed upstream: {other:?}")
        }
    }
    assert_unsupported_envelope(status, &body);
}

#[test]
fn redacted_thinking_response_to_chat_fails_closed_with_unsupported() {
    let upstream_reply = json!({
        "id": "msg_fixture_redacted",
        "type": "message",
        "role": "assistant",
        "model": "fixture-model",
        "content": [
            { "type": "redacted_thinking", "data": "opaque fixture" },
            { "type": "text", "text": "visible fixture" }
        ],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": { "input_tokens": 12, "output_tokens": 8 }
    });
    let upstream = MockUpstream::start(MockReply::json(upstream_reply));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/chat/completions",
        serde_json::to_vec(&request_for_source(ProtocolKind::OpenAiChat)).unwrap(),
        0,
    );
    let providers = [provider_for(
        ProtocolKind::AnthropicMessages,
        &upstream.base_url(),
    )];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("redacted thinking response handled");
    let captured = upstream.finish();

    assert_eq!(
        captured.path, "/messages",
        "upstream dispatch preceded fail-closed relay"
    );
    // 响应侧不可表达内容按运行时既有契约视为上游失败（502 invalid_upstream）：
    // map_upstream_bridge_error 将响应编解码阶段的 Unsupported 折叠为
    // InvalidUpstream（请求侧的 400 unsupported 信封仅适用于请求编码拒绝）。
    // 关键契约：绝不静默剥离后 200 回放。
    let body: Value = serde_json::from_slice(&body).expect("error response JSON");
    assert_eq!(
        status, 502,
        "no successful relay for unrepresentable upstream content"
    );
    assert_eq!(body["error"]["code"], "invalid_upstream");
    assert_eq!(body["error"]["message"], "invalid upstream response");
}

#[test]
fn responses_to_chat_rejects_opaque_reasoning() {
    let input = set_fixture_request_defaults(fixture_json("responses_reasoning_request"), false);
    let ir = runtime::bridge::parse_request(ProtocolKind::OpenAiResponses.into(), &input)
        .expect("opaque Responses reasoning request parses");
    let error = runtime::bridge::encode_upstream_request(
        &ir,
        ProtocolKind::OpenAiChat.into(),
        "fixture-target-model",
    )
    .expect_err("Chat cannot represent opaque Responses reasoning");
    assert_unsupported_field(error, "responses.reasoning");
}

#[test]
fn chat_to_anthropic_rejects_parallel_tool_calls() {
    let mut input = set_fixture_request_defaults(fixture_json("chat_clean_request"), false);
    input["parallel_tool_calls"] = json!(true);
    let ir = runtime::bridge::parse_request(ProtocolKind::OpenAiChat.into(), &input)
        .expect("Chat parallel_tool_calls request parses");
    let error = runtime::bridge::encode_upstream_request(
        &ir,
        ProtocolKind::AnthropicMessages.into(),
        "fixture-target-model",
    )
    .expect_err("parallel_tool_calls has no Anthropic mapping");
    assert_unsupported_field(error, "parallel_tool_calls");
}

#[test]
fn unknown_anthropic_cache_control_is_rejected() {
    let mut input = fixture_json("anthropic_clean_request");
    input["system"] = json!([{
        "type": "text",
        "text": "fixture system instruction",
        "cache_control": {"type": "unknown"},
    }]);
    let error = runtime::bridge::parse_request(ProtocolKind::AnthropicMessages.into(), &input)
        .expect_err("unknown cache_control type is rejected");
    assert_unsupported_field(error, "cache_control.type");
}

#[test]
fn text_streaming_routes_cover_all_protocol_directions() {
    for direction in DIRECTIONS {
        let upstream =
            MockUpstream::start(MockReply::sse(text_stream_for_target(direction.target)));
        let request = runtime::http_request_for_test(
            "POST",
            direction.source_path,
            serde_json::to_vec(&streaming_request_for_source(direction.source))
                .expect("text streaming source request serializes"),
            0,
        );
        let providers = [provider_for(direction.target, &upstream.base_url())];
        let (status, headers, body) = runtime::handle_request_for_test(
            request,
            &tempfile::tempdir().unwrap().keep(),
            &providers,
        )
        .unwrap_or_else(|error| panic!("{} text stream route failed: {error}", direction.name));
        let captured = upstream.finish_optional();

        assert_eq!(
            status,
            200,
            "{} text streaming status body={:?} upstream={:?}",
            direction.name,
            String::from_utf8_lossy(&body),
            captured
        );
        let captured = captured.expect("successful text route dispatched upstream");
        assert_text_stream_response(direction.source, &body);
        assert_eq!(captured.path, upstream_path(direction.target));
        assert_eq!(captured.body["model"], "fixture-model");
        assert_eq!(captured.body["stream"], true);
        assert_upstream_auth(direction.target, &captured.headers);
        assert!(headers.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("content-type") && value == "text/event-stream"
        }));
    }
}

#[test]
fn responses_tool_response_fixture_round_trips_without_item_loss() {
    let direction = Direction {
        name: "responses_to_responses",
        source: ProtocolKind::OpenAiResponses,
        source_path: "/v1/responses",
        target: ProtocolKind::OpenAiResponses,
    };
    let upstream = MockUpstream::start(MockReply::json(tool_response_for_target(direction.target)));
    let request = runtime::http_request_for_test(
        "POST",
        direction.source_path,
        serde_json::to_vec(&tool_request_for_source(direction.source))
            .expect("Responses tool request serializes"),
        0,
    );
    let providers = [provider_for(direction.target, &upstream.base_url())];
    let (status, headers, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("Responses tool fixture route handled");
    let captured = upstream.finish();

    assert_eq!(status, 200);
    assert_tool_response(direction, &body);
    assert_eq!(captured.path, "/responses");
    assert_tool_request(direction, &captured.body);
    assert_eq!(captured.body["stream"], false);
    assert_upstream_auth(direction.target, &captured.headers);
    assert!(headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case("content-type") && value == "application/json"
    }));
}

#[test]
fn responses_custom_tool_request_round_trips_through_chat_without_format_residue() {
    let direction = Direction {
        name: "responses_custom_tool_to_chat",
        source: ProtocolKind::OpenAiResponses,
        source_path: "/v1/responses",
        target: ProtocolKind::OpenAiChat,
    };
    let upstream = MockUpstream::start(MockReply::json(tool_response_for_target(direction.target)));
    let request = runtime::http_request_for_test(
        "POST",
        direction.source_path,
        serde_json::to_vec(&set_fixture_request_defaults(
            fixture_json("responses_custom_tool_request"),
            false,
        ))
        .expect("Responses custom tool request serializes"),
        0,
    );
    let providers = [provider_for(direction.target, &upstream.base_url())];
    let (status, headers, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("Responses custom tool route handled");
    let captured = upstream.finish();

    assert_eq!(status, 200);
    assert_tool_response(direction, &body);
    assert_eq!(captured.path, "/chat/completions");
    assert_eq!(captured.body["stream"], false);
    assert_upstream_auth(direction.target, &captured.headers);
    assert!(headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case("content-type") && value == "application/json"
    }));

    let tools = captured.body["tools"].as_array().expect("Chat tools array");
    assert_eq!(tools.len(), 2, "custom and function tools both forward");
    assert!(
        tools
            .iter()
            .all(|tool| tool["type"] == json!("function") && tool.get("format").is_none()),
        "Chat tools must be function-shaped with no custom type or format residue: {tools:?}"
    );
    assert_eq!(tools[0]["function"]["name"], "apply_patch");
    assert_eq!(tools[0]["function"]["description"], "Apply a diff to files");
    assert_eq!(
        tools[0]["function"]["parameters"],
        json!({"type": "object"}),
        "custom tool parameters default to an empty object schema"
    );
    assert_eq!(tools[1]["function"]["name"], "get_fixture_weather");
    assert_eq!(
        tools[1]["function"]["parameters"],
        expected_tool_parameters("get_fixture_weather")
    );

    let messages = captured.body["messages"]
        .as_array()
        .expect("Chat messages array");
    assert_eq!(
        messages
            .iter()
            .map(|message| message["role"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["system", "developer", "user", "assistant", "tool", "user"]
    );
    let assistant = messages
        .iter()
        .find(|message| message["role"] == "assistant")
        .expect("Chat assistant tool message");
    assert_eq!(
        assistant["tool_calls"][0]["function"]["name"],
        "get_fixture_weather"
    );
    assert_eq!(
        assistant["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"fixture-city\"}"
    );
    assert_eq!(messages[4]["tool_call_id"], "call_fixture_weather");
    assert_eq!(messages[4]["content"], "fixture weather result");
}

#[test]
fn reasoning_response_preserves_content_for_responses_clients() {
    let upstream = MockUpstream::start(MockReply::json(reasoning_response_from_anthropic()));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/responses",
        serde_json::to_vec(&request_for_source(ProtocolKind::OpenAiResponses))
            .expect("reasoning request serializes"),
        0,
    );
    let providers = [provider_for(
        ProtocolKind::AnthropicMessages,
        &upstream.base_url(),
    )];
    let (status, headers, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("reasoning route handled");
    let captured = upstream.finish();

    assert_eq!(status, 200);
    let body = serde_json::from_slice::<Value>(&body).expect("reasoning response JSON");
    assert_eq!(body["output_text"], "fixture response text");
    assert!(body["output"].as_array().is_some_and(|items| {
        items.iter().any(|item| {
            item["type"] == "reasoning" && item["encrypted_content"] == "fixture-signature"
        })
    }));
    assert_eq!(captured.path, "/messages");
    assert_upstream_auth(ProtocolKind::AnthropicMessages, &captured.headers);
    assert!(headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case("content-type") && value == "application/json"
    }));
}

#[test]
fn streaming_routes_cover_all_protocol_directions() {
    for direction in DIRECTIONS {
        let upstream = MockUpstream::start(MockReply::sse(stream_for_target(direction.target)));
        let mut source_request = tool_request_for_direction(direction);
        source_request["stream"] = json!(true);
        let request = runtime::http_request_for_test(
            "POST",
            direction.source_path,
            serde_json::to_vec(&source_request).expect("streaming source request serializes"),
            0,
        );
        let providers = [provider_for(direction.target, &upstream.base_url())];
        let (status, headers, body) = runtime::handle_request_for_test(
            request,
            &tempfile::tempdir().unwrap().keep(),
            &providers,
        )
        .unwrap_or_else(|error| panic!("{} stream route failed: {error}", direction.name));
        let captured = upstream.finish();

        assert_eq!(
            status,
            200,
            "{} streaming status body={:?} upstream={:?}",
            direction.name,
            String::from_utf8_lossy(&body),
            captured
        );
        assert_stream_response(direction.source, &body);
        assert_eq!(captured.path, upstream_path(direction.target));
        assert_eq!(captured.body["model"], "fixture-model");
        assert_eq!(captured.body["stream"], true);
        assert_tool_request(direction, &captured.body);
        assert_upstream_auth(direction.target, &captured.headers);
        assert!(headers.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("content-type") && value == "text/event-stream"
        }));
    }
}

#[test]
fn malformed_anthropic_tool_stream_is_contained_after_headers() {
    let direction = Direction {
        name: "chat_to_anthropic_malformed_stream",
        source: ProtocolKind::OpenAiChat,
        source_path: "/v1/chat/completions",
        target: ProtocolKind::AnthropicMessages,
    };
    let upstream = MockUpstream::start(MockReply::sse(include_str!(
        "fixtures/runtime_bridge/anthropic_tool_stream.sse"
    )));
    let mut source_request = tool_request_for_direction(direction);
    source_request["stream"] = json!(true);
    let request = runtime::http_request_for_test(
        "POST",
        direction.source_path,
        serde_json::to_vec(&source_request).expect("malformed stream request serializes"),
        0,
    );
    let providers = [provider_for(direction.target, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("malformed Anthropic stream handled after headers");
    let captured = upstream.finish();
    let body = String::from_utf8(body).expect("malformed stream response UTF-8");
    let frames = parse_sse_frames(&body);

    assert_eq!(status, 200);
    let error_frames: Vec<&ParsedSseFrame> = frames
        .iter()
        .filter(|frame| frame.data["error"].is_object())
        .collect();
    assert_eq!(
        error_frames.len(),
        1,
        "malformed stream error frames: {frames:?}"
    );
    assert_eq!(error_frames[0].data["error"]["type"], "invalid_upstream");
    assert_eq!(error_frames[0].data["error"]["code"], "invalid_upstream");
    assert_eq!(
        error_frames[0].data["error"]["message"],
        "invalid upstream response"
    );
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame.data_raw == "[DONE]")
            .count(),
        1
    );
    assert_eq!(
        frames.last().map(|frame| frame.data_raw.as_str()),
        Some("[DONE]")
    );
    let terminal_index = frames
        .iter()
        .position(|frame| frame.data_raw == "[DONE]")
        .expect("Chat malformed stream terminal");
    let error_index = frames
        .iter()
        .position(|frame| frame.data["error"].is_object())
        .expect("Chat malformed stream error");
    assert!(error_index < terminal_index);
    assert_eq!(captured.path, "/messages");
    assert_tool_request(direction, &captured.body);
    assert_upstream_auth(direction.target, &captured.headers);
}

#[test]
fn single_tool_streaming_routes_cover_all_protocol_directions() {
    for direction in DIRECTIONS {
        let upstream = MockUpstream::start(MockReply::sse(single_tool_stream_for_target(
            direction.target,
        )));
        let request = runtime::http_request_for_test(
            "POST",
            direction.source_path,
            serde_json::to_vec(&tool_streaming_request_for_source(direction.source))
                .expect("single tool request serializes"),
            0,
        );
        let providers = [provider_for(direction.target, &upstream.base_url())];
        let (status, _, body) = runtime::handle_request_for_test(
            request,
            &tempfile::tempdir().unwrap().keep(),
            &providers,
        )
        .unwrap_or_else(|error| panic!("{} single tool route failed: {error}", direction.name));
        let captured = upstream.finish_optional();
        assert_eq!(
            status,
            200,
            "{} single tool status body={:?} upstream={:?}",
            direction.name,
            String::from_utf8_lossy(&body),
            captured
        );
        assert_single_tool_stream_response(direction.source, &body);
        let captured = captured.expect("successful single-tool route dispatched upstream");
        assert_eq!(captured.path, upstream_path(direction.target));
        assert_eq!(captured.body["stream"], true);
        assert_tool_request_with_names(direction, &captured.body, &["get_fixture_weather"]);
        assert_upstream_auth(direction.target, &captured.headers);
    }
}

#[test]
fn reasoning_streaming_routes_cover_representable_directions() {
    for direction in DIRECTIONS {
        if direction.target == ProtocolKind::OpenAiChat {
            continue;
        }
        let upstream = MockUpstream::start(MockReply::sse(reasoning_stream_for_target(
            direction.target,
        )));
        let request = runtime::http_request_for_test(
            "POST",
            public_path(direction.source),
            serde_json::to_vec(&reasoning_request_for_direction(direction)).unwrap(),
            0,
        );
        let providers = [provider_for(direction.target, &upstream.base_url())];
        let (status, headers, body) = runtime::handle_request_for_test(
            request,
            &tempfile::tempdir().unwrap().keep(),
            &providers,
        )
        .expect("reasoning stream route handled");
        let captured = upstream.finish_optional();
        let body = String::from_utf8(body).expect("reasoning stream response UTF-8");

        assert_eq!(
            status, 200,
            "{} reasoning stream status body={body:?} upstream={captured:?}",
            direction.name
        );
        let captured = captured.expect("successful reasoning route dispatched upstream");

        if direction.source == ProtocolKind::OpenAiResponses {
            let has_reasoning = match direction.target {
                ProtocolKind::AnthropicMessages => captured.body["messages"]
                    .as_array()
                    .is_some_and(|messages| {
                        messages.iter().any(|message| {
                            message["content"].as_array().is_some_and(|content| {
                                content.iter().any(|item| {
                                    item["type"] == "thinking"
                                        && item["thinking"] == "fixture reasoning"
                                })
                            })
                        })
                    }),
                ProtocolKind::OpenAiResponses => {
                    captured.body["input"].as_array().is_some_and(|input| {
                        input.iter().any(|item| {
                            item["type"] == "reasoning"
                                && item["summary"][0]["text"] == "fixture reasoning"
                        })
                    })
                }
                ProtocolKind::OpenAiChat => false,
            };
            assert!(
                has_reasoning,
                "{} did not forward Responses reasoning request: {:?}",
                direction.name, captured.body
            );
        }

        if direction.source == ProtocolKind::OpenAiChat {
            // Chat 流式推理增量折叠为正文（对齐非流式降级），不再 safe-error。
            assert!(
                body.contains("fixture reasoning"),
                "chat stream must fold reasoning delta into content: {body}"
            );
            assert!(body.contains("[DONE]"), "missing Chat terminal: {body}");
        } else {
            assert!(
                body.contains("fixture reasoning"),
                "missing reasoning: {body}"
            );
            assert_stream_terminal(direction.source, &body);
        }
        assert_eq!(captured.path, upstream_path(direction.target));
        assert_upstream_auth(direction.target, &captured.headers);
        assert!(!headers.is_empty());
    }
}

#[test]
fn orphaned_tool_result_is_rejected_before_upstream_dispatch() {
    let request = json!({
        "model": "fixture-model",
        "messages": [{
            "role": "tool",
            "tool_call_id": "orphan_call",
            "content": "orphan result",
        }],
    });
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/chat/completions",
        serde_json::to_vec(&request).unwrap(),
        0,
    );
    let providers = [provider_for(
        ProtocolKind::AnthropicMessages,
        "http://127.0.0.1:1",
    )];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("orphaned tool result handled");

    assert_json_error(status, &body, "tool_state");
}

#[test]
fn oversized_tool_arguments_emit_safe_stream_error() {
    let arguments = "x".repeat(64 * 1024 + 1);
    let mut reply = String::from(
        "data: {\"id\":\"chatcmpl_large_tool\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
    );
    reply.push_str(
        "data: {\"id\":\"chatcmpl_large_tool\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_large\",\"type\":\"function\",\"function\":{\"name\":\"get_fixture_weather\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
    );
    reply.push_str(&format!(
        "data: {{\"id\":\"chatcmpl_large_tool\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"fixture-model\",\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"arguments\":\"{}\"}}}}]}},\"finish_reason\":null}}]}}\n\n",
        arguments
    ));
    let upstream = MockUpstream::start(MockReply::sse(&reply));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&tool_streaming_request_for_source(
            ProtocolKind::AnthropicMessages,
        ))
        .unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("oversized tool arguments handled");
    let _ = upstream.finish();
    let body = String::from_utf8(body).expect("tool error stream UTF-8");

    assert_eq!(status, 200);
    assert!(
        body.contains("invalid upstream response"),
        "missing safe error: {body}"
    );
    assert!(
        body.contains("message_stop"),
        "missing Anthropic terminal: {body}"
    );
}

#[test]
fn oversized_sse_frame_returns_safe_502() {
    let payload = "x".repeat(256 * 1024);
    let reply = format!("data: {{\"payload\":\"{}\"}}\n\n", payload);
    let upstream =
        MockUpstream::start(MockReply::raw(200, "text/event-stream", reply.into_bytes()));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&streaming_request_for_source(
            ProtocolKind::AnthropicMessages,
        ))
        .unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("oversized SSE frame handled");
    let _ = upstream.finish();

    assert_json_error(status, &body, "resource_limit");
}

fn assert_json_error(status: u16, body: &[u8], code: &str) {
    let body: Value = serde_json::from_slice(body).expect("error response JSON");
    let expected_status = match code {
        "invalid_request" | "tool_state" => 400,
        "resource_limit" => 413,
        "timeout" => 504,
        "invalid_upstream" => {
            // 上游 HTTP 失败现在透传真实状态码（400/429/500...）；
            // 语义失败（2xx 解析错误）仍为 502。只断言是错误码。
            assert!(
                status >= 400,
                "invalid_upstream must be an error status: {status}"
            );
            assert_eq!(body["error"]["code"], code);
            return;
        }
        _ => 502,
    };
    assert_eq!(status, expected_status);
    assert_eq!(body["error"]["code"], code);
    if code != "invalid_upstream" {
        // invalid_upstream messages are no longer a constant: the runtime
        // relays upstream error text when it can, so relayed messages are
        // asserted per-test instead.
        assert_eq!(
            body["error"]["message"],
            match code {
                "invalid_request" => "invalid request",
                "tool_state" => "invalid tool state",
                "resource_limit" => "request exceeds resource limit",
                "timeout" => "upstream request timed out",
                _ => "invalid upstream response",
            }
        );
    }
}

fn assert_expected_timeout_cleanup(result: Result<Option<CapturedRequest>, String>) {
    match result {
        Ok(Some(_)) => {}
        Ok(None) => panic!("timeout mock did not capture an upstream request"),
        Err(error) => {
            let expected_write_phase = error.starts_with("mock response body write failed:")
                || error.starts_with("mock response flush failed:");
            let expected_disconnect = is_expected_client_disconnect(&error);
            assert!(
                expected_write_phase && expected_disconnect,
                "unexpected timeout cleanup error: {error}"
            );
        }
    }
}

fn is_expected_client_disconnect(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("broken pipe")
        || error.contains("connection reset by peer")
        || error.contains("connection aborted")
        || error.contains("not connected")
}

fn assert_expected_http_failure_cleanup(result: Result<Option<CapturedRequest>, String>) {
    match result {
        Ok(Some(_)) => {}
        Ok(None) => panic!("HTTP failure mock did not capture an upstream request"),
        Err(error) => assert!(
            error.starts_with("mock response shutdown failed:")
                && is_expected_client_disconnect(&error),
            "unexpected HTTP failure cleanup error: {error}"
        ),
    }
}

#[test]
fn malformed_request_json_returns_safe_400_without_upstream_dispatch() {
    let request =
        runtime::http_request_for_test("POST", "/v1/chat/completions", b"{not-json".to_vec(), 0);
    let (status, _, body) = runtime::handle_request_for_test(
        request,
        &tempfile::tempdir().unwrap().keep(),
        &[provider()],
    )
    .expect("malformed request handled");

    assert_json_error(status, &body, "invalid_request");
}

#[test]
fn malformed_upstream_json_returns_502_and_captures_request() {
    let upstream = MockUpstream::start(MockReply::raw(
        200,
        "application/json",
        b"{not-json".to_vec(),
    ));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&request_for_source(ProtocolKind::AnthropicMessages)).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("malformed upstream handled");
    let captured = upstream.finish();

    assert_json_error(status, &body, "invalid_upstream");
    assert_eq!(captured.path, "/chat/completions");
}

#[test]
fn semantic_upstream_error_returns_502_and_relays_upstream_error_text() {
    let upstream = MockUpstream::start(MockReply::json(json!({
        "error": {"message": "provider secret failure", "type": "server_error"}
    })));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&request_for_source(ProtocolKind::AnthropicMessages)).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("semantic upstream error handled");
    let _ = upstream.finish();

    // Contract change: upstream error text is now relayed to the client
    // (truncated to 512 chars), replacing the generic "invalid upstream
    // response" message; the code stays invalid_upstream.
    assert_json_error(status, &body, "invalid_upstream");
    assert!(
        String::from_utf8_lossy(&body).contains("provider secret failure"),
        "upstream error text must be relayed: {}",
        String::from_utf8_lossy(&body)
    );
}

#[test]
fn upstream_http_failure_relays_real_status() {
    let upstream = MockUpstream::start(MockReply::raw(
        500,
        "application/json",
        br#"{"error":{"message":"server failure"}}"#.to_vec(),
    ));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&request_for_source(ProtocolKind::AnthropicMessages)).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("upstream HTTP failure handled");
    assert_expected_http_failure_cleanup(upstream.finish_result());

    assert_json_error(status, &body, "invalid_upstream");
    assert!(
        String::from_utf8_lossy(&body).contains("server failure"),
        "upstream error text must be relayed: {}",
        String::from_utf8_lossy(&body)
    );
}

#[test]
fn upstream_rejection_relays_error_message_to_client() {
    let upstream = MockUpstream::start(MockReply::raw(
        400,
        "application/json",
        br#"{"error":{"message":"rate limit exceeded"}}"#.to_vec(),
    ));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&request_for_source(ProtocolKind::AnthropicMessages)).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("upstream rejection handled");
    let _ = upstream.finish();

    assert_json_error(status, &body, "invalid_upstream");
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("rate limit exceeded"),
        "upstream 4xx error text must be relayed: {body}"
    );
}

#[test]
fn upstream_error_message_is_truncated_to_512_chars() {
    let long_message = format!("limit: {}", "x".repeat(600));
    let upstream = MockUpstream::start(MockReply::raw(
        400,
        "application/json",
        format!(r#"{{"error":{{"message":"{long_message}"}}}}"#).into_bytes(),
    ));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&request_for_source(ProtocolKind::AnthropicMessages)).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("truncated upstream error handled");
    let _ = upstream.finish();

    assert_eq!(status, 400, "upstream 400 relayed with real status");
    let body: Value = serde_json::from_slice(&body).expect("error response JSON");
    let message = body["error"]["message"].as_str().expect("relayed message");
    assert_eq!(
        message.len(),
        512,
        "relayed text must be truncated to 512 chars"
    );
    assert!(message.starts_with("limit: "));
}

#[test]
fn non_json_upstream_error_body_keeps_generic_envelope() {
    let upstream = MockUpstream::start(MockReply::raw(
        400,
        "text/plain",
        b"rate limited, try later".to_vec(),
    ));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&request_for_source(ProtocolKind::AnthropicMessages)).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("non-JSON upstream error handled");
    let _ = upstream.finish();

    assert_json_error(status, &body, "invalid_upstream");
    let body: Value = serde_json::from_slice(&body).expect("error response JSON");
    assert_eq!(
        body["error"]["message"], "invalid upstream response",
        "non-JSON error bodies keep the generic envelope message"
    );
}

#[test]
fn upstream_timeout_returns_504() {
    let mut reply = MockReply::json(response_for_target(
        ProtocolKind::OpenAiChat,
        ProtocolKind::OpenAiChat,
    ));
    reply.delay = Duration::from_millis(200);
    let upstream = MockUpstream::start(reply);
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&request_for_source(ProtocolKind::AnthropicMessages)).unwrap(),
        0,
    );
    let mut provider = provider_for(ProtocolKind::OpenAiChat, &upstream.base_url());
    provider.timeout_ms = 50;
    let (status, _, body) = runtime::handle_request_for_test(
        request,
        &tempfile::tempdir().unwrap().keep(),
        &[provider],
    )
    .expect("upstream timeout handled");
    assert_expected_timeout_cleanup(upstream.finish_result());

    assert_json_error(status, &body, "timeout");
}

#[test]
fn oversized_upstream_body_returns_413() {
    let large_text = "x".repeat(2 * 1024 * 1024);
    let upstream = MockUpstream::start(MockReply::json(json!({
        "id": "chatcmpl_fixture_large",
        "object": "chat.completion",
        "model": "fixture-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": large_text},
            "finish_reason": "stop",
        }],
    })));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&request_for_source(ProtocolKind::AnthropicMessages)).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("oversized upstream handled");
    let _ = upstream.finish();

    assert_json_error(status, &body, "resource_limit");
}

#[test]
fn malformed_upstream_sse_returns_502_before_downstream_headers() {
    let upstream = MockUpstream::start(MockReply::raw(
        200,
        "text/event-stream",
        b"data: {not-json}\n\n".to_vec(),
    ));
    let mut request = streaming_request_for_source(ProtocolKind::AnthropicMessages);
    request["stream"] = json!(true);
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&request).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("malformed upstream SSE handled");
    let _ = upstream.finish();

    assert_json_error(status, &body, "invalid_upstream");
}

#[test]
fn truncated_upstream_sse_emits_source_error_event_after_headers() {
    let upstream = MockUpstream::start(MockReply::sse(
        "data: {\"id\":\"chatcmpl_partial\",\"object\":\"chat.completion.chunk\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n",
    ));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&streaming_request_for_source(
            ProtocolKind::AnthropicMessages,
        ))
        .unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, headers, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("truncated upstream SSE handled");
    let _ = upstream.finish();
    let body = String::from_utf8(body).expect("truncated stream response UTF-8");

    assert_eq!(status, 200);
    assert!(headers.iter().any(
        |(key, value)| key.eq_ignore_ascii_case("content-type") && value == "text/event-stream"
    ));
    assert!(body.contains("event: error"));
    assert!(body.contains("event: message_stop"));
}

fn decode_chunked_sse(
    sse: &str,
    protocol: runtime::bridge::WireProtocol,
) -> (
    Vec<runtime::bridge::SseFrame>,
    Vec<runtime::bridge::StreamEventIr>,
    runtime::bridge::StreamState,
) {
    let mut decoder = runtime::bridge::SseDecoder::default();
    let mut frames = Vec::new();
    let mut bytes = sse.as_bytes().to_vec();
    if !bytes.ends_with(b"\n\n") {
        bytes.push(b'\n');
    }
    for chunk in bytes.chunks(7) {
        frames.extend(decoder.feed(chunk).expect("chunked SSE decodes"));
    }
    frames.extend(decoder.finish().expect("SSE ends on a frame boundary"));
    let mut state = runtime::bridge::StreamState::new();
    let mut events = Vec::new();
    for frame in &frames {
        events.extend(
            runtime::bridge::decode_stream_frame(protocol, frame, &mut state).unwrap_or_else(
                |error| panic!("{protocol:?} frame {:?} failed: {error:?}", frame.event),
            ),
        );
    }
    (frames, events, state)
}

fn assert_single_terminal_contract(
    label: &str,
    events: &[runtime::bridge::StreamEventIr],
    state: &runtime::bridge::StreamState,
) {
    let terminal_count = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                runtime::bridge::StreamEventIr::Completed(_)
                    | runtime::bridge::StreamEventIr::Failed(_)
            )
        })
        .count();
    assert_eq!(
        terminal_count, 1,
        "{label} stream has exactly one terminal event"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, runtime::bridge::StreamEventIr::Completed(_))),
        "{label} terminal event is Completed"
    );
    assert!(state.is_terminal(), "{label} state is terminal");
    assert_eq!(
        state.finish_eof(),
        Ok(()),
        "{label} EOF after terminal is valid"
    );
}

#[test]
fn chunked_text_streams_decode_through_real_decoder() {
    for (source, label) in [
        (ProtocolKind::AnthropicMessages, "anthropic"),
        (ProtocolKind::OpenAiChat, "chat"),
        (ProtocolKind::OpenAiResponses, "responses"),
    ] {
        let (_, events, state) = decode_chunked_sse(text_stream_for_target(source), source.into());
        assert!(
            matches!(
                events.first(),
                Some(runtime::bridge::StreamEventIr::Started(_))
            ),
            "{label} text stream starts with Started"
        );
        let text: String = events
            .iter()
            .filter_map(|event| match event {
                runtime::bridge::StreamEventIr::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            text, "fixture response text",
            "{label} text deltas concatenate"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, runtime::bridge::StreamEventIr::ToolCallStarted(_))),
            "{label} text stream has no tool calls"
        );
        assert_single_terminal_contract(label, &events, &state);
    }
}

#[test]
fn chunked_tool_streams_decode_through_real_decoder() {
    for (source, tail_marker, label) in [
        (
            ProtocolKind::AnthropicMessages,
            "event: malformed",
            "anthropic",
        ),
        (ProtocolKind::OpenAiChat, "event: malformed", "chat"),
        (
            ProtocolKind::OpenAiResponses,
            "event: response.malformed",
            "responses",
        ),
    ] {
        let valid = stream_for_target(source)
            .split_once(tail_marker)
            .map(|(valid, _)| valid)
            .expect("tool fixture has a malformed tail to strip");
        let (_, events, state) = decode_chunked_sse(valid, source.into());
        assert!(
            matches!(
                events.first(),
                Some(runtime::bridge::StreamEventIr::Started(_))
            ),
            "{label} tool stream starts with Started"
        );
        let mut names: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                runtime::bridge::StreamEventIr::ToolCallStarted(call) => Some(call.name.as_str()),
                _ => None,
            })
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["get_fixture_time", "get_fixture_weather"],
            "{label} tool call starts"
        );
        let finished = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    runtime::bridge::StreamEventIr::ToolCallFinished { .. }
                )
            })
            .count();
        assert_eq!(finished, 2, "{label} tool calls finish");
        assert!(
            !events
                .iter()
                .any(|event| { matches!(event, runtime::bridge::StreamEventIr::TextDelta { .. }) }),
            "{label} tool stream has no text deltas"
        );
        assert_single_terminal_contract(label, &events, &state);
    }
}

#[test]
fn zen_tool_stream_tail_relays_through_same_protocol_passthrough() {
    let fixture = include_str!("fixtures/runtime_bridge/zen_tool_stream_with_tail.sse");
    let upstream = MockUpstream::start(MockReply::sse(fixture));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/chat/completions",
        serde_json::to_vec(&tool_streaming_request_for_source(ProtocolKind::OpenAiChat))
            .expect("Zen stream request serializes"),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, headers, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("Zen tail stream handled");
    let captured = upstream.finish();
    let body = String::from_utf8(body).expect("Zen stream response UTF-8");
    let frames = parse_sse_frames(&body);

    assert_eq!(status, 200, "Zen stream status body={body:?}");
    assert!(headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case("content-type") && value == "text/event-stream"
    }));
    assert_eq!(captured.path, "/chat/completions");
    assert_eq!(captured.body["stream"], true);

    assert_eq!(
        body, fixture,
        "same-protocol passthrough must relay the real Zen traffic byte-for-byte"
    );
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame.data_raw == "[DONE]")
            .count(),
        1,
        "the relayed stream has exactly one Chat terminal: {frames:?}"
    );
    assert_eq!(
        frames.last().map(|frame| frame.data_raw.as_str()),
        Some(r#"{"choices":[],"cost":"0"}"#),
        "the provider tail frame follows the terminal, unchanged: {frames:?}"
    );
    assert!(
        !frames.iter().any(|frame| frame.data["error"].is_object()),
        "no error frame in the relayed stream: {frames:?}"
    );
    assert!(
        frames.iter().any(|frame| {
            frame.data["choices"].as_array().is_some_and(|choices| {
                choices
                    .iter()
                    .any(|choice| choice["finish_reason"] == "tool_calls")
            })
        }),
        "the relayed stream contains the tool-call finish chunk: {frames:?}"
    );
}

#[test]
fn malformed_terminal_stream_contract() {
    let mut state = runtime::bridge::StreamState::new();
    assert_eq!(
        state.finish_eof(),
        Err(runtime::bridge::BridgeError::InvalidUpstream)
    );
    state
        .apply_event(&runtime::bridge::StreamEventIr::Started(
            runtime::bridge::ResponseMetaIr {
                id: Some("stream-1".to_string()),
                model: Some("model".to_string()),
            },
        ))
        .expect("stream starts before the terminal");
    let completed = runtime::bridge::StreamEventIr::Completed(runtime::bridge::CompletionIr {
        status: Some("completed".to_string()),
        ..runtime::bridge::CompletionIr::default()
    });
    state.apply_event(&completed).expect("first terminal event");
    assert_eq!(
        state.apply_event(&completed),
        Err(runtime::bridge::BridgeError::ToolState)
    );
    let mut limited_decoder = runtime::bridge::SseDecoder::new(4);
    assert_eq!(
        limited_decoder.feed(b"12345"),
        Err(runtime::bridge::BridgeError::ResourceLimit)
    );
}

#[test]
fn chat_to_chat_passthrough_forwards_request_and_response_raw() {
    let reply = json!({
        "id": "chatcmpl_passthrough",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "fixture-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "raw reply"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8, "x_custom_usage": {"keep": true}}
    });
    let upstream = MockUpstream::start(MockReply::json(reply.clone()));
    let mut request = request_for_source(ProtocolKind::OpenAiChat);
    request["x_runtime_extra"] = json!({"keep": true});
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/chat/completions",
        serde_json::to_vec(&request).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("chat passthrough");
    let captured = upstream.finish();

    assert_eq!(status, 200);
    assert_eq!(
        body,
        serde_json::to_vec(&reply).unwrap(),
        "same-protocol response bytes must be forwarded unchanged"
    );
    assert_eq!(captured.path, "/chat/completions");
    assert_eq!(captured.body["model"], "fixture-model");
    assert_eq!(captured.body["x_runtime_extra"], json!({"keep": true}));
    assert_eq!(captured.headers["x-spec-runtime-hop"], "1");
}

#[test]
fn responses_to_responses_passthrough_preserves_opaque_reasoning() {
    let mut request = request_for_source(ProtocolKind::OpenAiResponses);
    request["input"] = json!([
        {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
        {"type": "reasoning", "id": "rs_item_opaque", "summary": [],
         "content": [{"type": "encrypted_content", "data": "abc123"}]}
    ]);
    let reply = json!({
        "id": "resp_passthrough",
        "object": "response",
        "created_at": 1700000000,
        "status": "completed",
        "model": "fixture-model",
        "output": [
            {"id": "out_item_keep", "type": "reasoning",
             "content": [{"type": "encrypted_content", "data": "keep-me"}]},
            {"id": "msg_1", "type": "message", "role": "assistant", "status": "completed",
             "content": [{"type": "output_text", "text": "ok"}]}
        ]
    });
    let upstream = MockUpstream::start(MockReply::json(reply.clone()));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/responses",
        serde_json::to_vec(&request).unwrap(),
        0,
    );
    let providers = [provider_for(
        ProtocolKind::OpenAiResponses,
        &upstream.base_url(),
    )];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("responses passthrough");
    let captured = upstream.finish();

    assert_eq!(status, 200);
    assert_eq!(
        body,
        serde_json::to_vec(&reply).unwrap(),
        "responses passthrough must not reshape output items"
    );
    assert_eq!(captured.body["input"][1]["id"], "rs_item_opaque");
    assert_eq!(captured.body["input"][1]["content"][0]["data"], "abc123");
    assert_eq!(captured.body["model"], "fixture-model");
}

#[test]
fn anthropic_to_anthropic_passthrough_forwards_raw() {
    let reply = json!({
        "id": "msg_passthrough",
        "type": "message",
        "role": "assistant",
        "model": "fixture-model",
        "content": [{"type": "text", "text": "raw"}],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 5, "output_tokens": 3, "cache_creation_input_tokens": 5}
    });
    let upstream = MockUpstream::start(MockReply::json(reply.clone()));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/messages",
        serde_json::to_vec(&request_for_source(ProtocolKind::AnthropicMessages)).unwrap(),
        0,
    );
    let providers = [provider_for(
        ProtocolKind::AnthropicMessages,
        &upstream.base_url(),
    )];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("anthropic passthrough");
    let captured = upstream.finish();

    assert_eq!(status, 200);
    assert_eq!(
        body,
        serde_json::to_vec(&reply).unwrap(),
        "cache-write usage and raw bytes must pass through unchanged"
    );
    assert_eq!(captured.path, "/messages");
    assert_eq!(captured.headers["x-api-key"], "fixture-key");
    assert_eq!(captured.headers["anthropic-version"], "2023-06-01");
    assert_eq!(captured.body["model"], "fixture-model");
}

#[test]
fn passthrough_forwards_upstream_error_status_and_body_raw() {
    let error_body =
        br#"{"error":{"message":"invalid api key","type":"authentication_error"}}"#.to_vec();
    let upstream = MockUpstream::start(MockReply::raw(401, "application/json", error_body.clone()));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/chat/completions",
        serde_json::to_vec(&request_for_source(ProtocolKind::OpenAiChat)).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, _, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("passthrough error handled");
    let _ = upstream.finish();

    assert_eq!(
        status, 401,
        "same-protocol errors must keep the upstream status"
    );
    assert_eq!(body, error_body, "error body must be forwarded unchanged");
}

#[test]
fn passthrough_stream_forwards_upstream_bytes_exactly() {
    let reply = "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"},\"finish_reason\":null}]}\n\n\
         data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
         data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"fixture-model\",\"choices\":[]}\n\n\
         data: [DONE]\n\n\
         data: {\"choices\":[],\"cost\":\"0\",\"x_weird\":true}\n\n"
        .to_string();
    let upstream = MockUpstream::start(MockReply::sse(&reply));
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/chat/completions",
        serde_json::to_vec(&streaming_request_for_source(ProtocolKind::OpenAiChat)).unwrap(),
        0,
    );
    let providers = [provider_for(ProtocolKind::OpenAiChat, &upstream.base_url())];
    let (status, headers, body) =
        runtime::handle_request_for_test(request, &tempfile::tempdir().unwrap().keep(), &providers)
            .expect("passthrough stream");
    let _ = upstream.finish();

    assert_eq!(status, 200);
    assert!(headers.iter().any(
        |(key, value)| key.eq_ignore_ascii_case("content-type") && value == "text/event-stream"
    ));
    assert_eq!(
        String::from_utf8(body).expect("passthrough stream UTF-8"),
        reply,
        "streaming passthrough must relay provider bytes exactly"
    );
}

#[test]
fn passthrough_still_applies_zen_developer_role_normalization() {
    let upstream = MockUpstream::start(MockReply::json(json!({
        "id": "chatcmpl_zen",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "fixture-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}]
    })));
    let mut request = request_for_source(ProtocolKind::OpenAiChat);
    request["messages"][0]["role"] = json!("developer");
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/chat/completions",
        serde_json::to_vec(&request).unwrap(),
        0,
    );
    let mut provider = provider_for(ProtocolKind::OpenAiChat, &upstream.base_url());
    provider.id = "zen".to_string();
    let (status, _, _) = runtime::handle_request_for_test(
        request,
        &tempfile::tempdir().unwrap().keep(),
        &[provider],
    )
    .expect("zen passthrough");
    let captured = upstream.finish();

    assert_eq!(status, 200);
    assert_eq!(
        captured.body["messages"][0]["role"], "system",
        "zen developer-role normalization must survive passthrough"
    );
}

#[test]
fn unknown_model_fails_closed_with_400_before_any_upstream_request() {
    let upstream = MockUpstream::start(MockReply::json(json!({"ok": true})));
    let mut provider = provider_for(ProtocolKind::OpenAiChat, &upstream.base_url());
    provider.id = "zen".to_string();
    provider.api_key = "provider-secret".to_string();
    let request = runtime::http_request_for_test(
        "POST",
        "/v1/chat/completions",
        serde_json::to_vec(&json!({
            "model": "not-configured_fixture_zen",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .expect("unknown-model request serializes"),
        0,
    );
    let (status, _, body) = runtime::handle_request_for_test(
        request,
        &tempfile::tempdir().unwrap().keep(),
        &[provider],
    )
    .expect("unknown-model request handled");
    let text = String::from_utf8_lossy(&body);

    assert_eq!(status, 400);
    assert!(
        text.contains(r#""code":"invalid_request""#),
        "envelope must carry invalid_request code: {text}"
    );
    assert!(
        text.contains(r#""type":"invalid_request_error""#),
        "envelope must carry invalid_request_error type: {text}"
    );
    assert!(
        text.contains("model not configured"),
        "envelope must explain the unconfigured model: {text}"
    );
    assert!(
        !text.contains("provider-secret"),
        "sanitized error body must not leak the provider key: {text}"
    );
    assert!(
        !text.contains("Authorization"),
        "sanitized error body must not carry Authorization: {text}"
    );
    let captured = upstream.finish_result();
    assert!(
        !matches!(captured, Ok(Some(_))),
        "unknown model must never reach an upstream request: {captured:?}"
    );
}

// temp repro appended via test

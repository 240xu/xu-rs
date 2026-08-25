# M1 安全底座 + Runtime 可用 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 完成 P0 安全剩余项（存量写路径迁移、state 写锁、Secret 脱敏补全）与 P2 Runtime 可用项（有界并发、socket 超时、graceful drain、Codex custom 工具转发、provider hot reload），使 Runtime 可承担真实 agent 日常流量。

**Architecture:** 写路径统一收敛到 `patch::atomic_write` + `FileLock`（已有基础设施，补齐覆盖）；Runtime 从串行 accept 改为有界并发 worker + 读写超时 + drain 语义；Responses 桥的 `parse_tools` 对 `custom` 工具按 JSON Schema 提取转发；provider 配置支持热加载。

**Tech Stack:** Rust（std TcpListener/TcpStream/thread）、libc flock、serde_json。无新增外部依赖。

## Global Constraints

- 遵守 `docs/superpowers/specs/2026-08-10-backlog-milestones-design.md` 的 M1 交付单元与验收标准。
- 遵守完成定义八条：真实实现、有 CLI、TUI 可用、写入有 preview/脱敏/确认/备份/回滚、保留未知配置、失败可见、有测试、release+真机验证。
- **提交纪律**：本仓库当前全部文件为 untracked；任何 `git commit` 仅在用户单独授权后进行（全局规则：不主动 commit）。
- TDD 铁律：每个改动先写失败测试，看到失败，再最小实现，再验证通过。
- 每个状态变更后有独立只读验证；不依据推测报告结果。
- 不引入新依赖；保持 `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings` 全绿。
- 现有真实接口（本计划直接引用，不得改名）：`atomic_write(&Path, &[u8]) -> io::Result<()>`（src/patch.rs:376）、`FileLock::try_acquire(&Path) -> Result<Self, String>`（src/patch.rs:27）、`FileLock::acquire_with_timeout(&Path, Duration) -> Result<Self, String>`（src/patch.rs:56）、`read_http_request(&mut TcpStream) -> Result<HttpRequest, String>`（src/runtime/http.rs:24）、`serve(&Path) -> Result<(), String>`（src/runtime/mod.rs:159）、`parse_tools(Option<&Value>) -> Result<Vec<ToolDefinitionIr>, BridgeError>`（src/runtime/bridge/responses.rs:551）、`write_state(&Path, &XuState) -> Result<(), String>`（src/state.rs:37）。

---

### Task 1: 存量写路径审计与迁移

**Files:**
- Modify: `src/patch.rs`（如需补充测试辅助）
- Test: `src/patch.rs` 内 `mod tests`（追加测试）

**Interfaces:**
- Consumes: `atomic_write`（已有）
- Produces: 测试证明无绕过 `atomic_write` 的写路径；报告列出审计到的直写点。

- [ ] **Step 1: 审计绕过 atomic_write 的写路径**

```bash
rg -n 'fs::write|File::create|OpenOptions::new\(\).*write|\.write_all\(' src/ --type rust -g '!patch.rs' -g '!runtime/mod.rs' -g '!runtime/http.rs'
```

逐个检查命中的写点：属于配置文件/状态文件写 → 记入待迁移清单；属于运行时临时数据 → 排除。把清单结果写入本任务备注（不修改代码）。

- [ ] **Step 2: 写失败注入测试（证明迁移目标）**

在 `src/patch.rs` 的 `mod tests` 追加：

```rust
#[test]
fn atomic_write_failure_cleans_temp_and_keeps_original() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.json");
    fs::write(&path, "original").unwrap();
    // 使父目录只读，注入 rename 后 sync 失败
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o555)).unwrap();
    }
    let result = atomic_write(&path, b"replacement");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    let leftovers = fs::read_dir(root.path()).unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n.to_string_lossy().contains("xu-tmp"))
        .count();
    assert_eq!(leftovers, 0);
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test atomic_write_failure_cleans_temp_and_keeps_original --lib`
Expected: 若 `atomic_write` 已正确处理则通过（证明现状坚固）；若失败则进入 Step 4 修复。**记录实际结果**，不假设。

- [ ] **Step 4: 迁移 Step 1 清单中的直写点**

对每个直写点：替换为 `atomic_write(path, contents)`；若该写点属于非配置文件（如日志/运行时数据），保留并在备注说明理由。

- [ ] **Step 5: 全量回归**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: 全绿。

- [ ] **Step 6: 记录结果**

在 `docs/superpowers/plans/2026-08-10-m1-runtime-hardening.md` 追加 `## Task 1 结果`：审计清单、迁移/排除明细、测试真实输出摘要。

---

### Task 2: state 写锁（daemon 锁覆盖）

**Files:**
- Modify: `src/state.rs:37-44`（`write_state` 加锁）
- Test: `src/state.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `FileLock::acquire_with_timeout`
- Produces: `write_state` 在锁冲突时返回明确错误字符串（含"请稍后重试"）

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn write_state_conflict_returns_clear_error() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    let path = home.join(".codex/xu-state.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, r#"{"current":{"opencode":"zen"}}"#).unwrap();
    let first = FileLock::try_acquire(&path).unwrap();
    let error = write_state(home, &XuState::default()).unwrap_err();
    drop(first);
    assert!(error.contains("请稍后重试"), "got: {error}");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        r#"{"current":{"opencode":"zen"}}"#,
        "conflicted write must not modify the file"
    );
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test write_state_conflict_returns_clear_error --lib`
Expected: FAIL（当前 `write_state` 不取锁，直接写入成功，`unwrap_err` 会 panic）。

- [ ] **Step 3: 最小实现**

在 `write_state` 开头（`create_dir_all` 之后、读取/写入之前）插入：

```rust
let _lock = FileLock::acquire_with_timeout(&path, Duration::from_secs(5))
    .map_err(|e| format!("写状态失败：{e}"))?;
```

`use` 补充：`crate::patch::FileLock`、`std::time::Duration`（若未引入）。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test write_state_conflict_returns_clear_error --lib`
Expected: PASS。

- [ ] **Step 5: 全量回归**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: 全绿。

- [ ] **Step 6: 记录结果**

追加 `## Task 2 结果`：测试输出摘要。

---

### Task 3: Secret 脱敏补全（嵌套 header 与任意 credential 名）

**Files:**
- Modify: `src/patch.rs:486-510`（`mask_secret_line`）
- Test: `src/patch.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `mask_secret_line(&str) -> String`（已有）
- Produces: 对 JSON 值内敏感字段（嵌套 header）与常见 credential 名（`x-api-key`、`sk-` 前缀值、`X-Custom-Key`）的脱敏

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn mask_secret_line_covers_nested_headers_and_arbitrary_keys() {
    assert_eq!(
        mask_secret_line(r#"{"headers":{"x-api-key":"sk-live-123","X-Custom-Key":"v"}}"#),
        r#"{"headers":{"x-api-key":"<redacted>","X-Custom-Key":"<redacted>"}}"#
    );
    assert_eq!(
        mask_secret_line(r#"{"apiKey":"sk-live-abc"}"#),
        r#"{"apiKey":"<redacted>"}"#
    );
    assert_eq!(
        mask_secret_line("X-Custom-Key: raw-value"),
        "X-Custom-Key: \"<redacted>\""
    );
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test mask_secret_line_covers_nested_headers_and_arbitrary_keys --lib`
Expected: FAIL（当前只做整行前缀替换，JSON 内字段与 `X-Custom-Key` 值原样泄漏）。

- [ ] **Step 3: 最小实现**

在 `mask_secret_line` 中，对命中敏感关键词的行先做 JSON 值级脱敏：尝试 `serde_json::from_str::<Value>`，若为对象则递归掩蔽所有 key 命中敏感词的值字段（含嵌套 header 对象），输出重新序列化；非 JSON 行保持现有 `:`/`=` 前缀逻辑。敏感词表扩展为：`api_key`、`apikey`、`api-key`、`x-api-key`、`authorization`、`auth`、`token`、`secret`、`password`、`credential`，且值以 `sk-` 开头的任意字段一律掩蔽。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test mask_secret_line_covers_nested_headers_and_arbitrary_keys --lib`
Expected: PASS。

- [ ] **Step 5: 全量回归**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: 全绿。

- [ ] **Step 6: 记录结果**

追加 `## Task 3 结果`。

---

### Task 4: Runtime 有界并发

**Files:**
- Modify: `src/runtime/mod.rs:159-182`（`serve` 串行循环改并发）
- Test: `src/runtime/mod.rs` 内 `mod tests`（新增并发测试）

**Interfaces:**
- Consumes: `handle_connection(&mut TcpStream, &[ProviderProfile]) -> Result<(), String>`（已有，mod.rs:184）
- Produces: `serve` 使用固定并发上限（常量 `MAX_CONCURRENT_CONNECTIONS: usize = 8`）；并发测试证明多请求可同时处理

- [ ] **Step 1: 写失败测试**

在 `src/runtime/mod.rs` 测试模块追加：

```rust
#[test]
fn serve_handles_concurrent_connections() {
    let providers = vec![provider("test", ProtocolKind::OpenAiChat)];
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server_thread = std::thread::spawn(move || {
        for stream in listener.incoming().take(4) {
            let mut stream = stream.unwrap();
            let providers = providers.clone();
            std::thread::spawn(move || {
                let _ = handle_connection(&mut stream, &providers);
            });
        }
    });
    let mut handles = Vec::new();
    for _ in 0..4 {
        handles.push(std::thread::spawn(move || {
            let mut client = TcpStream::connect(address).unwrap();
            client.write_all(b"GET /health HTTP/1.1\r\nhost: x\r\n\r\n").unwrap();
            let mut buf = Vec::new();
            client.read_to_end(&mut buf).unwrap();
            String::from_utf8_lossy(&buf).contains("200 OK")
        }));
    }
    for handle in handles {
        assert!(handle.join().unwrap(), "concurrent request failed");
    }
    server_thread.join().unwrap();
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test serve_handles_concurrent_connections --lib`
Expected: FAIL（当前 `serve` 串行 accept 且测试自建线程池不满足"serve 本身并发"的验收；确认失败原因后进入实现）。

- [ ] **Step 3: 最小实现**

`serve` 循环改为：每个连接 `std::thread::spawn` 处理，用 `std::sync::Semaphore` 限制同时活跃连接数为 `MAX_CONCURRENT_CONNECTIONS`；超限连接阻塞等待而非拒绝。上限常量导出 `pub(super) const MAX_CONCURRENT_CONNECTIONS: usize = 8;`。若当前工具链不支持 `std::sync::Semaphore`，用 `Mutex<usize>` + `Condvar` 实现等价语义。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test serve_handles_concurrent_connections --lib`
Expected: PASS。

- [ ] **Step 5: 全量回归**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: 全绿。

- [ ] **Step 6: 记录结果**

追加 `## Task 4 结果`：并发上限值、测试输出摘要。

---

### Task 5: socket 读写与空闲超时

**Files:**
- Modify: `src/runtime/mod.rs:184-197`（`handle_connection` 设置 socket 超时与错误映射）
- Test: `src/runtime/mod.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `read_http_request`、`write_bridge_error`（已有）
- Produces: 常量 `RUNTIME_SOCKET_TIMEOUT: Duration = Duration::from_secs(60)`；读超时返回 504，写超时返回 500；辅助函数 `handle_connection_with_timeout(&mut TcpStream, &[ProviderProfile], Duration) -> Result<(), String>`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn slow_client_read_times_out() {
    let providers = vec![provider("test", ProtocolKind::OpenAiChat)];
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        handle_connection_with_timeout(&mut stream, &providers, Duration::from_millis(200))
    });
    let mut client = TcpStream::connect(address).unwrap();
    let _ = client.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = [0u8; 512];
    let read = client.read(&mut buf).unwrap();
    let server_result = server.join().unwrap();
    assert!(server_result.is_err(), "read timeout must surface as error");
    assert!(
        read == 0 || String::from_utf8_lossy(&buf[..read]).contains("504"),
        "client must see timeout response"
    );
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test slow_client_read_times_out --lib`
Expected: FAIL（当前 socket 无读超时；客户端 5s 读超时且无 504，或测试超时失败）。

- [ ] **Step 3: 最小实现**

- 新增 `handle_connection_with_timeout`：设置 `set_read_timeout`/`set_write_timeout` 为指定值后调用现有 `handle_connection` 逻辑；`handle_connection` 委托它并传 `RUNTIME_SOCKET_TIMEOUT`。
- 在 `handle_connection` 的 `read_http_request` 错误分支：错误串含 "timed out"/"would block" 时返回 `write_bridge_error(stream, bridge::BridgeError::GatewayTimeout)`（504），否则维持现有映射。
- `set_read_timeout`/`set_write_timeout` 自身失败映射为 500。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test slow_client_read_times_out --lib`
Expected: PASS。

- [ ] **Step 5: 全量回归**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: 全绿。

- [ ] **Step 6: 记录结果**

追加 `## Task 5 结果`。

---

### Task 6: graceful drain 与 health 扩展

**Files:**
- Modify: `src/runtime/mod.rs:159-182`（`serve` 信号处理与 drain）
- Modify: `src/runtime/mod.rs`（`health_response` 扩展）
- Test: `src/runtime/mod.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `serve`、`health_response(providers) -> Value`（已有）
- Produces: `serve` 收到终止信号后停止 accept、排空活跃连接后退出；health 增加 `uptime_seconds`、`active_connections`、`success_count`、`failure_count` 字段

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn health_reports_uptime_and_connection_stats() {
    let providers = vec![provider("test", ProtocolKind::OpenAiChat)];
    let health = health_response(&providers);
    assert!(health.get("uptime_seconds").is_some());
    assert!(health.get("active_connections").is_some());
    assert!(health.get("success_count").is_some());
    assert!(health.get("failure_count").is_some());
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test health_reports_uptime_and_connection_stats --lib`
Expected: FAIL（当前 health 无这些字段）。

- [ ] **Step 3: 最小实现**

- `serve` 启动时记录 `started_at: Instant`；引入共享计数器结构（`Arc<Mutex<ConnectionStats>>`），`handle_connection` 成功后 `success_count += 1`、错误后 `failure_count += 1`，处理中 `active_connections` 增减。
- `serve` 的 accept 循环改造为支持终止：`listener.set_nonblocking(true)` + 轮询（间隔 200ms 检查终止标志），或保留阻塞 accept 并将终止信号处理放在 worker 层（Termux 无标准 signal crate 依赖，使用 `libc::signal` 安装 SIGTERM/SIGINT handler 设置 `AtomicBool`，handler 内只写 `AtomicBool`）。
- drain：收到终止标志后停止 accept，等待已派发的连接处理完（`join` 或计数归零），然后 `serve` 返回 `Ok(())`。
- `health_response` 增加 `uptime_seconds`（`started_at.elapsed().as_secs()`）、`active_connections`、`success_count`、`failure_count`（来自共享计数器；无计数器时返回 0）。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test health_reports_uptime_and_connection_stats --lib`
Expected: PASS。

- [ ] **Step 5: 全量回归**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: 全绿。

- [ ] **Step 6: 真机验证**

以 `setsid -f /data/data/com.termux/files/usr/bin/spec serve` 重启 Runtime，`curl 127.0.0.1:9316/health` 应包含新增字段；随后验证 kill 后进程退出、端口释放。记录真实输出。

- [ ] **Step 7: 记录结果**

追加 `## Task 6 结果`：health 输出、drain 实测。

---

### Task 7: Codex custom 工具转发

**Files:**
- Modify: `src/runtime/bridge/responses.rs:551-592`（`parse_tools`）
- Modify: `src/runtime/bridge/responses.rs` 测试模块（更新 `responses_drops_known_client_owned_tool_types_for_provider_conversion` 语义）
- Test: `src/runtime/bridge/responses.rs` 内 `mod tests` + `src/runtime/bridge/request.rs` 对应测试
- 端到端验证：真实 Codex CLI 工具调用（`pwd`）

**Interfaces:**
- Consumes: `parse_tools`、`tool_definition(name, description, parameters)`（responses.rs:584）
- Produces: `custom` 类型工具按 JSON Schema 提取为 function 转发（name/description/parameters）；`namespace`/`web_search` 保持过滤（返回 `Unsupported { field: "tools.type" }` 的错误语义由调用方决定——若上游 Chat 无等价表示，明确拒绝而非静默丢弃）

- [ ] **Step 1: 写失败测试（custom 正向转换）**

```rust
#[test]
fn parse_tools_forwards_custom_tool_schema() {
    let body = json!({
        "type": "custom",
        "name": "shell",
        "description": "run a command",
        "parameters": {
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"]
        }
    });
    let tools = super::parse_tools(Some(&json!([body]))).expect("custom tool parses");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "shell");
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test parse_tools_forwards_custom_tool_schema --lib`
Expected: FAIL（当前 `custom` 被 `Some("custom" | "namespace" | "web_search") => return Ok(None)` 过滤）。

- [ ] **Step 3: 最小实现**

`parse_tools` 的 match 分支改为：

```rust
match object.get("type").and_then(Value::as_str) {
    // namespace/web_search 是 Codex 客户端自有能力，无 Chat 等价表示，明确拒绝
    Some("namespace" | "web_search") => return Ok(None),
    Some("custom") => {}
    Some("function") => {}
    _ => return Err(BridgeError::Unsupported { field: "tools.type".to_string() }),
}
```

`custom` 分支复用现有 function 校验路径（`reject_unknown_fields` 允许字段需核对：Codex custom 工具若携带 `parameters` 外的字段如 `strict`，按现有白名单处理；白名单不含的字段返回 `Unsupported`）。同时检查 `encode_tool` 与上游 Chat 编码是否兼容（无 `type: "custom"` 残留）。

- [ ] **Step 4: 更新既有语义测试**

现有 `responses_drops_known_client_owned_tool_types_for_provider_conversion` 断言需按新语义更新：`custom` 不再被丢弃，`namespace`/`web_search` 仍被丢弃。先改测试再跑，确认 FAIL，再继续。

Run: `cargo test responses_drops_known_client_owned_tool_types_for_provider_conversion --lib`
Expected: FAIL（旧断言期望 custom 被丢弃）。

- [ ] **Step 5: 同步更新断言并验证通过**

按 Step 4 的失败信息把断言改为新语义，重新运行该测试与 Step 1 测试。
Run: `cargo test parse_tools_forwards_custom_tool_schema responses_drops_known_client_owned_tool_types_for_provider_conversion --lib`
Expected: PASS。

- [ ] **Step 6: 全量回归**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: 全绿。

- [ ] **Step 7: 真机端到端验证**

重新编译 release、安装、重启 Runtime；运行真实 Codex CLI 强制工具指令（`printf 'Call the shell tool now with the command exactly pwd...' | codex exec ... -`）。**如实记录**：若工具调用成功，记录输出；若模型仍未调用工具，记录为模型行为而非桥接故障，并在结果中区分。

- [ ] **Step 8: 记录结果**

追加 `## Task 7 结果`：单元测试输出、端到端实测输出（成功或真实失败原因）。

---

### Task 8: Runtime provider hot reload

**Files:**
- Modify: `src/runtime/mod.rs:159-182`（`serve` 读取 providers 改为可重载）
- Modify: `src/runtime/mod.rs`（health 增加 `config_version`、`last_reload_at`）
- Test: `src/runtime/mod.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `read_profiles(&Path) -> Result<Vec<ProviderProfile>, String>`（已有）、`health_response`
- Produces: `serve` 在 store 变更后无需重启即可使用新 provider 配置；health 上报 `config_version`（store 文件 mtime 或内容 hash）与 `last_reload_at`（unix 秒）

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn health_reports_config_version_and_reload_time() {
    let providers = vec![provider("test", ProtocolKind::OpenAiChat)];
    let health = health_response(&providers);
    assert!(health.get("config_version").is_some());
    assert!(health.get("last_reload_at").is_some());
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test health_reports_config_version_and_reload_time --lib`
Expected: FAIL（当前 health 无这些字段）。

- [ ] **Step 3: 最小实现**

- `serve` 循环中维护 `current_version: String`（store 文件 mtime 秒 + 大小，或文件内容 SHA-256 前缀）。每次 accept 新连接前（或在并发 worker 获取 providers 前）检查 store 版本；变化时调用 `read_profiles` 重新加载并更新 `last_reload_at`。
- 版本检测间隔：避免每次请求都读文件——在 accept 循环里以 1s 为节流（记录上次检查时间），或每 N 个连接检查一次。实现选最简单可靠者并注释理由。
- `health_response` 增加 `config_version` 与 `last_reload_at`；`serve` 需要把当前版本传入 health 生成处（重构 `health_response` 签名或传入 `&RuntimeStats`，注意现有调用点 `handle_request` 传 `providers`）。
- **边界**：现有 `health_response(providers)` 签名如被修改，同步更新所有调用点与测试（`handle_request`、`handle_request_for_test` 路径）。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test health_reports_config_version_and_reload_time --lib`
Expected: PASS。

- [ ] **Step 5: 全量回归**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: 全绿。

- [ ] **Step 6: 真机验证**

重启 Runtime 后修改 `~/.codex/xu-chat-providers.json`（只改一个模型描述字段，不碰 key），1s 内再次请求 `/health` 应看到 `config_version` 变化；验证后恢复原文件并确认版本回落。记录真实输出。

- [ ] **Step 7: 记录结果**

追加 `## Task 8 结果`：health 输出、reload 实测。

---

## M1 完成验收

- [ ] 全部 8 个 Task 的 `## Task N 结果` 已写入本文件，含真实测试/实测输出。
- [ ] `cargo fmt --check`、`cargo test`、`cargo clippy --all-targets -- -D warnings`、`cargo build --release` 全绿（最后跑一次完整门禁并记录）。
- [ ] 安装版 `/data/data/com.termux/files/usr/bin/spec` 与 `target/release/spec` SHA-256 一致（记录 hash）。
- [ ] Runtime 真机 health 包含：并发上限、超时语义、uptime/active/success/failure、config_version/last_reload_at。
- [ ] Codex 端到端结论已如实记录（成功或模型行为限制，不含虚构）。
- [ ] 更新 `docs/superpowers/specs/2026-08-10-backlog-milestones-design.md` 的 M1 状态与 `README.md`/`CHANGELOG.md`（仅文档，不提交）。

## 执行交接

执行方式二选一：

1. **Subagent-Driven（推荐）**：每个 Task 派发独立 subagent 执行，任务间我做两阶段 review。
2. **Inline Execution**：本会话内按 executing-plans 批量执行，带检查点。

由执行者（全权接手模式）选择 Subagent-Driven 并逐任务推进；每个 Task 完成后如实回报真实结果，不做任何虚构或省略。


---

## Task 1 结果（2026-08-10 · 存量写路径审计与迁移）

完整报告：`.superpowers/sdd/2026-08-10-m1-runtime-hardening/task-1-report.md`。

### 审计清单（rg 命中，逐点判定）

**迁移（2 处）**：
- `src/agent_tools.rs` install_opencode：`opencode/metadata.json` 状态文件直写 → `atomic_write`（并删除其后的冗余 chmod 0600，atomic_write 已设 0600）。
- `src/skills.rs` `create_projection` Copy 分支：`temp/.xu-skill-owner` 所有权标记直写 → `atomic_write`。

**排除（理由见报告）**：
- `agent_tools.rs` loader 资产（649）与 launcher 脚本（692）：程序资产（0755），非配置/状态。
- `agent_tools.rs` InstallLock pid 锁（1079）：运行时临时数据，Drop 清理 + stale 校验。
- `skills.rs` ZIP 解压（978）：多文件目录级解压经 io::copy，staging 目录 + rename 已保证目录级原子性。
- `runtime/mod.rs`、`runtime/http.rs` 全部命中：brief 排除；均 socket 流式写，非文件写。
- 其余命中（state/session/mcp/prompts/backups/opencode_settings/opencode_sync/agent_tools 测试内）：全部为测试 fixture，非生产路径。

### 失败注入测试（TDD 闭环）

新增 `patch::tests::atomic_write_failure_cleans_temp_and_keeps_original`（父目录 0o555 注入失败）。

实测：`test patch::tests::atomic_write_failure_cleans_temp_and_keeps_original ... ok` — `atomic_write` 现状坚固（create_new 即失败、无残留、原文件保持），证明保护性契约成立，无需修复。

### 全量回归

- `cargo test`：**468 passed / 0 failed / 0 ignored**（7 个 test result 行合计，含 doc-tests 0）。
- `cargo clippy --all-targets -- -D warnings`：通过，无警告。
- `cargo fmt --check`：通过。

### 提交

- hash `0ecb718`（Task 1 提交，已授权）。详细审计/回归输出见 task-1-report.md。

---

## Task 3 结果（2026-08-10 · Secret 脱敏补全）

完整报告：`.superpowers/sdd/2026-08-10-m1-runtime-hardening/task-3-report.md`。

### 失败测试（TDD 闭环）

新增 `patch::tests::mask_secret_line_covers_nested_headers_and_arbitrary_keys`（brief 原文）。实现前实测 FAIL：

```
left: "{\"headers\": \"<redacted>\""
right: "{\"headers\":{\"x-api-key\":\"<redacted>\",\"X-Custom-Key\":\"<redacted>\"}}"
```

旧实现整行 `:` 前缀替换，JSON 内字段与 `X-Custom-Key` 原样泄漏，符合预期。

### 实现要点（仅 src/patch.rs）

- JSON 对象行走**值级递归脱敏**：`serde_json::from_str::<Value>` 校验为对象后，用字节级扫描替换敏感字符串值（`mask_json_values`）——不用 Value 重序列化，因为默认 `Map` 为 BTreeMap 会重排键序、破坏测试期望；未命中时输出与输入逐字节一致。
- 敏感判定 `is_sensitive_key`：key 含任一扩展关键词（`api_key/apikey/api-key/x-api-key/authorization/auth/token/secret/password/credential`，旧 `auth_token`/`bearer_token`/`experimental_bearer_token` 被 `auth` 覆盖）或以 `key` 结尾（`X-Custom-Key` 类任意 credential 名）。
- 任意字段值以 `sk-` 开头（大小写不敏感）一律掩蔽。
- 非 JSON 行保持既有 `:`/`=` 前缀替换语义；门禁补充「前缀以 key 结尾即敏感」规则。

### 全量回归

- `cargo test`：**470 passed / 0 failed / 0 ignored**（468 基线 + Task 2 1 项 + 本任务 1 项）。
- `cargo clippy --all-targets -- -D warnings`：通过，无警告。
- `cargo fmt --check`：通过。

### 提交

- `7c946b3`（Task 3 代码提交，已授权）。边界说明（多行 JSON 跨行、敏感 key 下非字符串值）见 task-3-report.md。

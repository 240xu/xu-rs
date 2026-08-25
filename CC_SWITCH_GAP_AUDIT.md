# Xu vs CC Switch：Production Gap Audit

基准：`farion1231/cc-switch@413c09e0790c304506888ae24b9be72820aca126`。本审计只比较协议桥行为语义，不复制 CC Switch 产品架构；Xu 当前 `0.1.0`。

状态：

- `DONE`：已有真实实现并经过测试。
- `PARTIAL`：有基础实现，但没有达到生产级或只覆盖一部分协议/客户端。
- `MISSING`：尚未实现。
- `N/A`：依赖桌面操作系统或 Tauri，不适合 Termux TUI。
- `intentional divergence`：Xu 有意采用更严格的 fail-closed 行为，不复制 CC Switch 的静默丢失语义。

## 结论

Xu 目前是一个可用的三端 Provider 与扩展管理器，还不是 CC Switch 级别的完整产品。Provider CRUD、三端非破坏配置写回、三端安装、基础会话浏览、统一 MCP、Prompt、Skills 和保守 Runtime 已经可用；Projects、Usage、Quota、远程 Skills、多 endpoint、真实 failover/circuit breaker、云同步和完整的多媒体/高兼容协议桥仍缺失。

在继续增加模块前，必须先解决原子写入、Claude/Codex 非破坏合并、Codex 危险默认权限、Runtime 虚假 capability、并发覆盖和无确认自动写入等 P0 风险。

## P0 生产阻断项

| 状态 | 缺口 | 生产要求 |
|---|---|---|
| PARTIAL | 配置写入直接截断文件 | 同目录临时文件、`0600`、`fsync`、原子 rename、父目录同步 |
| PARTIAL | 文件并发锁/代际校验 | 已加入 preview-to-apply 内容代际校验；跨进程锁和 daemon 锁仍待完成 |
| DONE (P0) | Claude 当前整文件替换 | 已改为只更新 Xu 管理的 env/model/marker，保留其他字段 |
| DONE (P0) | Codex 当前整文件替换 | 已改为结构化 TOML 局部更新，保留 MCP、features、projects、comments 和未知字段 |
| DONE (P0) | Codex 最小权限 | 不再自动写危险 sandbox、approval、trusted home 或隐藏警告字段 |
| DONE (P0) | Apply 事务 | Provider config 与 current state 已进入同一 patch 批次；失败统一回滚并报告 rollback 错误 |
| PARTIAL | Secret 安全 | 支持 stdin/env/file/secret reference；嵌套 header 和任意 credential 名称脱敏 |
| PARTIAL | Runtime capability 诚实性 | Runtime 未支持 tools 时 catalog 不得宣称 parallel tools/apply_patch |
| DONE (P0) | 自动 OpenCode sync | Provider 页面加载不再写 store；同步入口保留为显式操作 |
| MISSING | Runtime 并发和 timeout | 有界并发、socket read/write/idle timeout、body/header 限制、graceful shutdown |

## Provider 管理

### 已完成或部分完成

| 状态 | 功能 |
|---|---|
| DONE | Provider add/update/delete/show/duplicate/import/export |
| DONE | Chat/Responses/Anthropic 三协议分类 |
| DONE | Base URL、API key、模型、默认模型、headers、timeout、retry、context/output、reasoning、notes、website |
| DONE | OpenCode live provider 导入和模型元数据保留 |
| DONE | 显式 `/models` 获取和健康缓存 |
| DONE | 单供应商三端 apply 和显式多供应商模型后缀路由 |
| PARTIAL | Presets：Xu 只有少量模板，CC Switch 有 50+ 且按客户端维护 |
| PARTIAL | 排序：多供应商临时顺序可调，Provider 持久排序和拖动排序缺失 |
| PARTIAL | Provider category/vendor/capability 仅有少量推断，缺少完整元数据 |
| PARTIAL | Universal Provider：已有三端 adapter，但没有独立实体、联动编辑和原子三端同步 |

### 尚未实现

- 50+ 经过维护的 Provider presets、品牌、官网、取 key 地址和默认模型更新机制。
- Official Login provider 和一键返回官方登录。
- OAuth Auth Center、多账号、默认账号、provider 绑定和 token refresh。
- ChatGPT/Codex OAuth 官方会话代理接管。
- GitHub Copilot OAuth 管理。
- AWS Bedrock、Azure、Vertex 等专用认证表单和适配。
- Provider icon、颜色、分类、标签、搜索和收藏。
- Provider 持久排序、批量操作、隐藏客户端。
- Provider full URL endpoint 模式。
- Custom User-Agent。
- 每 Provider 成本倍率、日/月限额、定价来源。
- 每 Provider quota/balance script。
- Endpoint candidates、默认 endpoint 和自动最快选择。
- Shared/common config snippet 的抽取、过滤、合并和 switch-away backfill。
- Claude/Codex live config 双向 backfill。
- 从 CC Switch/Codex++ 数据库或配置直接迁移。
- Deep Link/二维码式 Provider 导入预览。

## 客户端覆盖

| 客户端 | CC Switch | Xu | 结论 |
|---|---|---|---|
| OpenCode | Provider/MCP/Prompt/Skills/Session | Provider/MCP/Prompt/Skills/permission/session | PARTIAL |
| Claude Code | 完整 | Provider/MCP/Prompt/Skills/install/session | PARTIAL |
| Codex | 完整 | Provider/MCP/Prompt/Skills/install/session/catalog | PARTIAL |
| Claude Desktop | Provider 和映射代理 | 无 | N/A，Termux 无官方桌面客户端 |
| Gemini CLI | 完整 | 无 | MISSING，可作为后续可选客户端 |
| OpenClaw | Provider/workspace/memory | 无 | MISSING，可选，不属于当前三端目标 |
| Hermes | Provider/MCP/Skills/session | 无 | MISSING，可选，不属于当前三端目标 |

## MCP

状态：`PARTIAL`。已完成 versioned SSOT、stdio/http/sse domain、CLI CRUD/三端启停、基础模板、三端非破坏 projection、三端 live import、import-all 部分失败汇总、冲突拒绝、dry-run/确认/备份/回滚及触屏列表/模板/启停。TUI 自定义新增/编辑和 Deep Link 仍待完成。

已完成的基础能力：versioned SSOT、stdio/http/sse、三端启停、live import、非破坏 projection、模板、冲突拒绝、Codex fail-closed、dry-run/确认/备份/回滚。

剩余需要实现：

- TUI 自定义 Add/edit/delete/duplicate/import/export 表单。
- tags 与更多经过维护的模板。
- 未安装客户端 existence gate。
- 逐客户端 projection 结果和部分失败汇总。
- Codex TOML parse fail-closed。
- 删除后不从 Provider snapshot 复活。
- 未归 Xu 管理的 MCP 保留。
- Deep Link/JSON import preview。
- dry-run、备份、原子写入、失败回滚和自愈重试。

## Prompts

状态：`PARTIAL`。已完成 versioned preset store、三端 live path、CLI add/import/activate/deactivate/delete、active 删除保护、smart backfill、原子 dry-run/确认/备份/回滚，以及触屏列表和三端 active 切换。Markdown 内置编辑器、搜索、描述编辑和 Deep Link 仍待完成。

已完成的基础能力：三端 preset store/live path、单端 active、首次导入、smart backfill、active 删除保护、diff/备份/回滚。

剩余需要实现：

- Markdown preset CRUD、描述、预览和搜索。
- active prompt 编辑后即时安全投影。
- diff、备份、恢复、导入导出和 Profile 引用。

## Skills

状态：`PARTIAL`。已完成 versioned SSOT、本地目录导入、根级 `SKILL.md` 校验、源 symlink 拒绝、确定性 SHA-256、symlink/copy 投影、三端启停、ownership-safe 删除、CLI verify 和触屏列表。GitHub/ZIP、repositories、更新检测、卸载备份/恢复和 public registry 仍待完成。

已完成的基础能力：versioned SSOT、本地目录导入、根级 `SKILL.md` 校验、确定性 SHA-256、copy/symlink 投影、三端启停和所有权安全删除。

剩余需要实现：

- GitHub owner/repo/branch/subdir 和 root/nested `SKILL.md` 发现。
- 自定义 Skill repositories。
- GitHub、ZIP、本地 unmanaged skill 导入。
- 搜索、installed filter 和公共 registry 搜索。
- SHA-256 更新检测、单项更新和 Update All。
- 安装、卸载、失败回滚。
- 卸载前备份、备份列表、恢复和 retention。
- 三端 projection、部分失败报告和 Profile 引用。

## Project Profiles

状态：`PARTIAL`，核心切换闭环已完成。已有 schema v1 Project store、全局实体/三端独立 scope、未捕获与捕获为空的语义区分、当前 live 状态捕获、自动保存离开项目、悬空引用诊断、逐文件脱敏 diff、dry-run-first CLI、触屏列表/目标选择/捕获/切换二次确认，以及 Provider/MCP/Prompt/Skills/route/current/Project store 的混合事务。Skill 删除使用隔离区，失败精确恢复，Project current 最后提交。

需要实现：

- Named profile create/list/show/rename/delete 已完成；rename 保持稳定 ID，delete 默认 dry-run 并保护所有 scope 的 current Project。
- 项目是全局实体，但 OpenCode、Claude、Codex 各自记录 current project 和独立快照槽位。
- 每 scope 独立记录 provider、MCP、Skills、Prompt、OpenCode permission 和 Runtime strategy。
- “未捕获”和“捕获为空”的语义区分。
- 离开 Profile 自动保存当前状态。
- 首页一级 Projects 工作区和三端 current Project header 摘要已完成。
- TUI Project create/rename 堆叠文本表单已完成，支持触屏选字段、键盘输入、只读稳定 ID、校验、dry-run 和确认。
- Apply 前状态差异预览已完成；逐文件 redacted diff 尚待完整计划生成。
- Provider/MCP/Skills/Prompt 最小差异 apply。
- 切换前自动保存离开项目的 live 状态，不提供容易遗忘的手动“更新快照”步骤。
- 应用顺序固定为 Provider、MCP 最小差异、Skills 最小差异、Prompt；引用已删除对象时明确告警。
- Xu 默认采用完整 dry-run 和事务回滚；如未来支持 best-effort，必须作为显式策略且不得错误更新 current project。
- 悬空引用诊断、导入导出和云同步。

## Proxy、协议转换与高可用

### 当前部分实现

- Loopback `127.0.0.1:9316`。
- 三协议非流式 request/response bridge，覆盖 Chat、Responses、Anthropic 之间的文本、工具调用和部分 reasoning 转换。
- 跨协议 streaming 状态机，覆盖文本、工具调用、usage、terminal 和 malformed/truncated stream 的安全错误。
- 原生 Anthropic/Responses 同协议 streaming passthrough。
- tools、tool_choice、parallel tool calls、tool arguments/results 和稳定 call id。
- system/developer 指令合并、Responses reasoning opaque envelope 和 cache-read usage 保持。
- 2xx semantic error、resource limit、timeout、orphan tool result 和 unsupported field fail-closed。
- 显式模型后缀路由。

### Task 10 Runtime 差异逐项状态

下表只记录协议桥行为证据，不把 CC Switch 行为自动变成 Xu 产品承诺。证据入口为 `tests/runtime_bridge_integration.rs::task10_cc_switch_differential_manifest_executes_xu_contract` 和 `tests/fixtures/runtime_bridge/ccswitch/task10_differential_manifest.json`。

| Case | CC Switch 参考行为 | Xu 状态 | Xu 测试证据 | 边界 |
|---|---|---|---|---|
| Anthropic system + named tool choice -> Chat | 保留 system、工具和命名 tool choice | DONE | manifest case `anthropic_system_and_named_tool_choice_to_chat` 字段断言 | 只承诺当前可表示字段 |
| Chat parallel tool calls -> Responses | 保留 parallel、调用顺序和稳定 call id | DONE | manifest case `chat_parallel_tool_calls_to_responses` 字段断言 | 仅覆盖当前 IR 可表达的工具生命周期 |
| Responses reasoning -> Anthropic | 保留 summary 和 opaque reasoning metadata | PARTIAL | manifest case `responses_reasoning_to_anthropic` 断言 system、thinking、signature | 只接受可恢复 envelope；不可还原 metadata 仍拒绝 |
| Anthropic malformed Messages -> Chat | JSON 解析后进入转换；错误类型的 `messages` 变成空 Chat `messages` 数组 | intentional divergence | manifest case `anthropic_malformed_messages_to_chat` 实际走 parser/encoder pipeline | Xu 对 `/messages` 非 array 返回 `invalid_request`/400，主动避免静默生成空请求 |
| Anthropic document -> Chat | Chat 转换忽略 `document` block；同一消息中的 text 仍保留。Responses 路径另行转换为 `input_file` | PARTIAL | manifest case `anthropic_document_to_chat_is_fail_closed` 断言 `unsupported`/400/reason code/`messages.content` | Xu 拒绝而不静默丢 document |
| Anthropic cache-write usage -> Chat | 转换 cache-write usage | PARTIAL | manifest case `anthropic_cache_write_usage_to_chat_is_fail_closed` 断言 `unsupported`/400/reason code | `usage.cache_write_tokens` 无等价字段，优先计费语义安全 |
| Anthropic cache-read usage -> Chat | 转换 cache-read usage | DONE | manifest case `anthropic_cache_read_usage_to_chat` 字段断言 | 仅覆盖当前 Chat usage details 表示 |
| Chat `finish_reason=length` -> Responses | 映射为 incomplete | DONE | manifest case `chat_length_to_responses_incomplete` 字段断言 | 只覆盖当前 terminal/status/output_text 语义 |

### 尚未实现

- Chat/Responses/Anthropic 的全部 provider-specific 字段和完整 response item metadata。
- Responses item id、reasoning item id、previous response id 等无法安全还原到 Chat/Anthropic 的字段。
- Chat 目标的 cache-write usage；当前转换明确返回 `unsupported`，不得静默丢失计费信息。
- Chat 目标的带签名 reasoning、redacted reasoning 和原生 reasoning streaming；当前安全拒绝。
- Anthropic document/PDF、音频及其他目标协议没有等价表示的多媒体结果；图片仅在目标支持时转换。
- custom tools、完整错误结果和 provider-specific tool metadata。
- 复杂 finish reason/incomplete metadata 的全量双向保持。
- SSE sniff、JSON-on-stream 恢复和 malformed history 分类。
- Prompt cache breakpoint injection 和 cache_control。
- Full endpoint、custom UA、header/body overrides。
- Request rectifier、thinking 修复、text-only media fallback。
- Provider/app takeover、backup ownership 和 abnormal shutdown 恢复。
- Runtime provider hot reload。
- 有界并发、连接池、keep-alive、chunked input 和请求大小限制。
- IPv6、自定义监听地址、可选 LAN 暴露。
- 出站 HTTP/HTTPS proxy。
- gzip/deflate/zstd。
- 活跃连接、uptime、成功率和 graceful drain。

## Failover 与 Circuit Breaker

状态：`MISSING`。当前多供应商只是显式模型路由。

需要实现：

- 每客户端 Provider queue。
- 固定优先级、request round-robin、session sticky、weighted routing。
- 跨 Provider retry 和 endpoint fallback。
- 网络、429、5xx、auth、invalid request、bridge failure 分类。
- 官方账号 auth error 永不跨账号 failover。
- Closed/Open/HalfOpen。
- consecutive failure、error rate、minimum sample。
- first-byte、stream idle、non-stream timeout。
- recovery wait、half-open probe 和 recovery threshold。
- circuit 状态持久化/恢复。
- failover event log、UI 状态、手动 reset。

## Usage、Cost、Pricing、Quota

状态：`MISSING`。

需要实现：

- SQLite request log 和 daily/hourly rollup。
- Runtime 请求日志与三端 session usage 增量导入。
- request/provider/model/session/app/status/stream 维度。
- input/output/cache-read/cache-write token。
- duration、TTFT、success rate、error。
- cache token 语义版本，避免历史记录重复计费。
- 模型 ID normalization 和 aliases。
- input/output/cache-read/cache-write 单价。
- Provider cost multiplier。
- built-in seed、用户自定义价格和 seed repair 不覆盖用户设置。
- 时间、Provider、模型、客户端、状态筛选。
- 趋势、Provider/模型统计、请求详情。
- JSON/CSV/Markdown 导出。
- 自动刷新间隔和 last-good cache。
- 官方 subscription quota、Codex OAuth、Copilot quota。
- 第三方 balance/token plan 模板。
- 自定义受限 usage script 或声明式 extractor。
- transient retry、429/5xx 分类和手动刷新。

## Sessions

### 已完成

- Claude/Codex/OpenCode 扫描。
- 搜索、来源筛选、基础分组、正文预览、滚动和恢复。
- 有界读取和隐藏内部/tool 原始数据。

### 尚未实现

- Session index/cache 和增量扫描。
- 标题重命名来源兼容。
- 完整 project/cwd/provider 分组。
- transcript 目录和 role styling。
- source path copy 和元数据详情。
- Markdown/JSON export。
- 单项删除、批量选择、filtered select-all 和安全删除。
- Provider metadata 修复和历史迁移。
- Codex official/custom unified history、迁移 ledger、备份和 restore。
- token usage 关联和 subagent 去重。
- archived session cursor 继承。
- CLI session list/show/export/delete。

## Import、Export、Migration、Cloud Sync

### 当前部分实现

- Provider JSON import/export。
- OpenCode Provider live sync。
- 相邻配置备份、恢复和 prune。
- release tarball 和 SHA-256。

### 尚未实现

- 带 schema version 的完整 Xu 数据导入导出。
- SQLite SSOT、DAO 和 migration framework。
- migration 前数据库备份、too-new schema recovery。
- 自动迁移旧 Provider/state 格式。
- CC Switch/Codex++ import report。
- 全量 config snapshot 和差异恢复。
- 定时 backup、retention 设置、rename 和 backup-now。
- WebDAV presets、test/upload/download/metadata/ETag/size guard。
- S3/R2/MinIO/OSS/COS/OBS 和 SigV4。
- 本地同步目录模式。
- 同步冲突检测和 revision 防盲覆盖。
- Secret 默认排除和可选独立加密包。
- Deep Link import parser、validation、preview 和 confirmation。

## 设置、首次启动和用户体验

### 当前部分实现

- Touch-first TUI。
- 自绘 3x3 点阵 `IconButton`。
- Provider 编辑分页。
- Agent doctor/install/update。
- OpenCode permission 设置。
- Help 页面。
- 首页已提升 Provider、Agents、MCP、Prompts、Skills、Sessions、Tools 七个真实工作区为一级入口。
- 首页按 64 列断点切换单双列，按高度只显示完整卡片行，并确保选中项进入视口。
- 首页绘制与触屏命中共享同一组响应式 `Rect`，方向键按当前网格移动。
- Agent 同步检测在执行前显示 Busy 页面，完成后显示耗时。

### 尚未实现或未达到生产级

- 首次启动 wizard：检测三端、导入现有配置、创建首个 Provider、验证并应用。
- 全局 Settings SSOT。
- 中文/英文等 i18n 和即时切换。
- dark/light/system 或至少高对比/单色主题。
- 除首页外各功能页的小高度、小宽度响应式布局。
- 自绘按钮自动换行和窄屏分页。
- 所有结果/diff/error 页面滚动。
- 多供应商超过 8 项分页。
- Session 窄屏单栏模式。
- 可配置按键、完整键盘导航、焦点指示和无颜色状态标记。
- destructive action 一致确认和 undo affordance。
- Toast/status history，不让短消息消失后无法追溯。
- 每个操作明确 loading/progress/cancel/retry/partial failure。
- 空状态直接提供主要动作。
- 设置搜索和帮助上下文。
- Runtime requirement 在 apply 前主动检查。
- Environment variable conflict 检测、来源显示、备份和清理。
- Runtime port、log level、backup retention、default target、update channel 设置。
- 崩溃/信号时 RAII 恢复 raw mode 和 alternate screen。
- 真实 Xu self-updater。

## 安装、发布和运维

### 当前部分实现

- Termux 三端安装/更新/诊断和 `--version` 验证。
- aarch64 OpenCode loader。
- release build、tarball 和 SHA-256。
- Rust format/test/clippy CI。

### 尚未实现或未达到生产级

- Android/Termux aarch64 CI 构建和真机 smoke test。
- 架构矩阵和非 aarch64 明确支持策略。
- tag -> GitHub Release 自动发布。
- artifact signing、provenance、SBOM 和 reproducible build。
- `cargo audit`、`cargo deny`、license 和 dependency review。
- Installer 下载校验和 signature 验证。
- 原子 installer、自身升级 rollback 和版本 channel。
- Loader asset runtime/CI checksum verification。
- 结构化日志、level、rotation、retention、request ID 和 audit log。
- Runtime crash recovery、Termux:Boot 和可选 wakelock。
- 健康/指标导出和诊断 bundle。
- 稳定 exit code、JSON CLI output、shell completion 和 man page。

## Secret 与安全

尚未达到生产级的项目：

- Provider key 仍明文存在 store、目标配置和备份。
- CLI `--api-key` 会进入 shell history/process args。
- 无 Android Keystore/keyring/encrypted store/command secret provider。
- custom header 任意命名的 credential 可能绕过脱敏。
- 无 symlink/path replacement 防护。
- preview 到 confirm 之间没有文件代际校验。
- 无跨进程锁。
- 无 dependency/security audit gate。
- 无 release signature。

## CC Switch 中不适合 Termux 的功能

这些不是 Xu 当前产品目标中的缺陷：

- Tauri/WebView 窗口、系统托盘和 lightweight window destruction。
- Windows MSI/portable、macOS DMG/notarization、Linux AppImage/RPM/DEB。
- Windows registry、LaunchAgent、XDG desktop autostart。
- 桌面 terminal chooser、clipboard fallback、AppleScript。
- Claude Desktop macOS/Windows profile 写入。
- 系统级 `ccswitch://` protocol registration。
- 桌面 window position/size 和 native close behavior。
- Codex 桌面 app 注入、签名绕过、桌面插件和界面增强。

Termux 对应方案是：TUI 快速切换、CLI deep-import 命令、Termux:Widget/Boot 可选集成、release tarball 和本地 daemon。

## 实施顺序

### P0：先保证不伤用户数据

1. 原子写入、权限、fsync、锁/代际校验。
2. Claude/Codex 非破坏结构化 merge。
3. 移除 Codex 危险默认权限。
4. capability 诚实化。
5. Apply/state/rollback 事务修复。
6. 自动同步改为 preview + confirm。
7. Secret 输入和脱敏强化。

### P1：形成完整用户闭环

1. 首次启动和 live config 导入 wizard。
2. 统一 MCP。
3. Prompt。
4. Skills。
5. Project Profiles。
6. 响应式/可滚动 TUI 和一致状态反馈。

### P2：让 Runtime 可用于真实 coding agent

1. 并发、timeout、reload、graceful shutdown。
2. Tools/tool results。
3. Cross-protocol streaming。
4. Reasoning/thinking/cache/multimodal。
5. Takeover 和 abnormal recovery。

### P3：可观测与高可用

1. Usage/request logs/pricing/quota。
2. Multi-endpoint。
3. Routing strategies。
4. Failover/circuit breaker。

### P4：同步与发布成熟度

1. SQLite schema/migrations/full export。
2. WebDAV/S3。
3. Self-update、signed release、Android CI 和诊断 bundle。

## 完成定义

功能只有同时满足以下条件才标为 `DONE`：

1. 有真实 domain/store/service，不是空按钮。
2. 有 CLI，便于自动化和恢复。
3. 有触屏 TUI，且窄屏可用。
4. 所有写入都有 preview、脱敏、确认、备份和失败回滚。
5. 保留用户未知配置。
6. 部分失败可见且可重试。
7. 有单元、集成和回归测试。
8. 已完成 release build、安装和真机验证。

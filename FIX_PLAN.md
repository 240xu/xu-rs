# Xu 修复与加固计划

> 日期：2026-07-19  
> 范围：全库审计后的可执行修复路线  
> 原则：不破现有 dry-run / 备份 / 原子写 / OpenCode 会话保护；先稳后快；每阶段可独立验证。

---

## 0. 现状摘要

| 模块 | 行数 | 判断 |
|------|------|------|
| `src/main.rs` | ~6000 | 状态机过重，改动风险高 |
| `src/menu.rs` | ~3200 | 绘制/hit-test/组件耦合 |
| `src/cli.rs` | ~2300 | 能力全，帮助中英混杂 |
| `src/agent_tools.rs` | ~1100 | 安装体系可用，可更智能 |
| `src/patch.rs` / stores | 中等 | 写入安全较好，缺跨进程锁 |
| `adapters.rs` 等 | 空壳 | 应清理 |

**已较强：** 写入安全、Skills 远程、MCP/Projects 事务、Agent 安装锁、OpenCode 不强更、触屏二次确认。  
**主要短板：** 架构拆分、统一 Busy、跨进程锁、Agent 增量更新、Prompt 编辑、CLI 中文化、Runtime 能力。

---

## 1. 目标

1. 代码可维护：大文件拆分，页面独立。  
2. 并发安全：SSOT 写入跨进程锁 + 代际校验。  
3. 交互一致：慢操作统一 Busy；危险操作二次确认。  
4. 更新更稳：Claude/Codex 优先增量，失败再重装；OpenCode 默认不动。  
5. 文案统一：TUI/CLI 中文，专有名词保留英文。  
6. 不扩大攻击面：不引入自动联网、不默认危险 sandbox。

---

## 2. 阶段划分

### 阶段 A — 架构拆分（不改功能）【优先】

**目的：** 降低回归成本，为后续改动腾空间。

| 步骤 | 动作 | 验收 |
|------|------|------|
| A1 | 抽出 `src/app/state.rs`：Provider/Agent/Project/Mcp/Skill 运行时状态 | `cargo test` 全过 |
| A2 | 抽出 `src/ui/components/{chip,list,confirm,busy,form}.rs` | 现有 hit-test 单测过 |
| A3 | 抽出 `src/ui/pages/{home,providers,agents,mcp,skills,projects,prompts,sessions,tools}.rs` | TUI 手测各页可进 |
| A4 | `main.rs` 仅保留事件循环与 mode 分发 | main 目标 < 1500 行 |
| A5 | 删除/合并空壳：`adapters.rs`、`models.rs`、`provider_store.rs` | `cargo check` 无死模块误导 |

**不做：** 行为变更、新功能、网络策略变更。

---

### 阶段 B — 写入与并发安全

**目的：** 两个 `xu` 同时写不会互相踩。

| 步骤 | 动作 | 验收 |
|------|------|------|
| B1 | 在 `patch.rs` 增加可复用 `FileLock`（同目录 `.lock`，超时） | 单测：互斥 |
| B2 | Provider / MCP / Prompt / Skills / Projects / state 写入统一走锁 | 第二进程明确报错 |
| B3 | 保留 preview-to-apply 内容代际校验；锁失败中文错误 | 错误可读 |
| B4 | 安装锁与 SSOT 锁职责写进 README | 文档一小节 |

**不做：** 分布式锁、网络锁服务。

---

### 阶段 C — 统一 Busy / 异步边界

**目的：** 任何慢操作都不“假死”。

| 步骤 | 动作 | 验收 |
|------|------|------|
| C1 | 定义统一 `BusyTask` + `run_with_busy` | 慢路径全覆盖 |
| C2 | 接入：查最新版、Provider 测试、Session 扫描、Skill 安装、MCP import | 手测有 Busy 页 |
| C3 | 超时提示（取消至少停在 dry-run 前） | 超时有文案 |
| C4 | 失败摘要 + 可选详情 `~/.codex/xu-logs/` | 安装失败可追溯 |

---

### 阶段 D — Agent 安装/更新体系完善

**目的：** 更快、更稳、更可预期。

| 步骤 | 动作 | 验收 |
|------|------|------|
| D1 | 策略：`noop(已最新)` → `增量装目标版` → `失败则卸载重装` → `再检测` | 已最新不重装 |
| D2 | 强制重装：UI 入口或 CLI `--force` | 可显式重装 |
| D3 | OpenCode UI 三态：可查版本 / 不自动更新 / 仅 env 强制 | 无误触 |
| D4 | 预览统一：当前→目标、步骤、是否跳过 OC | CLI/TUI 一致 |
| D5 | 批量：一项失败不回滚已成功；返回失败列表 | 部分成功可重试 |
| D6 | 测试：增量成功、失败转重装、OpenCode 跳过 | 单测覆盖 |

**硬约束：** 默认永不强更 OpenCode（保护当前会话）。

**已完成部分：** 重装流水线、二次点击确认、跳过 OpenCode、版本对照展示。

---

### 阶段 E — 交互与中文化收尾

| 步骤 | 动作 | 验收 |
|------|------|------|
| E1 | 危险操作统一二次确认 | 与 Agent 一致 |
| E2 | 只读操作一次完成：刷新/查版本/诊断 | 无多余确认 |
| E3 | CLI help 全中文 | `xu` 帮助可读 |
| E4 | UI 残留英文扫尾 | 专有名除外无夹杂 |
| E5 | Prompt：搜索 + 描述 + 简易 Markdown 编辑 | 可改正文/描述 |

---

### 阶段 F — Runtime / 高级能力（可选，后置）

仅在明确要「协议代理」时做：

1. tool-call 同协议透传  
2. SSE 同协议 passthrough 加固  
3. 明确拒绝跨协议 streaming 的产品文案  
4. 不做默认 danger sandbox / 自动 failover  

---

## 3. 每阶段验收门禁

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
git diff --check
```

安装：

```bash
cargo build --release
install -m 755 target/release/spec /data/data/com.termux/files/usr/bin/spec
```

手测最小集：首页 → 安装与更新 Agent → 自动查版本 → 二次确认（可不执行）。

---

## 4. 风险与回滚

| 风险 | 缓解 |
|------|------|
| 拆分回归 | A 阶段只搬家；每步全测 |
| 文件锁死锁 | 超时；锁与数据文件分离 |
| 增量漏修二进制 | 检测失败自动走重装 |
| 误更 OpenCode | 默认跳过 + env 门闩 + UI 文案 |

回滚：本地备份体系 + 建议每阶段一个 commit。

---

## 5. 默认执行顺序

```
A1 → A2 → A3 → A4 → A5
  → B1 → B2 → B3
  → C1 → C2 → C3
  → D1 → D2 → D3 → D4 → D5 → D6
  → E1 → E2 → E3 → E4 → E5
  → (可选 F)
```

| 优先级 | 内容 | 原因 |
|--------|------|------|
| 第一刀 | A1–A3 拆 Agent/Provider 页 | 后面改动都省事 |
| 第二刀 | B1–B2 SSOT 文件锁 | 防双开踩配置 |
| 第三刀 | D1 增量更新优先 | 用户直接体感更快 |

---

## 6. 明确不做

- 不复制 CC Switch / Codex++ 源码  
- 不默认 `sandbox_mode=danger-full-access`  
- 不在页面刷新时自动 clone GitHub  
- 不自动 commit / push（除非明确要求）  
- 本计划不做 WebDAV / S3 / 计费  

---

## 7. 进度

| 阶段 | 状态 |
|------|------|
| A 架构拆分 | main 6000→3610；app 含 client_page；menu/{chips,common,mod}；事件循环外提进行中 |
| B 并发安全 | 完成：FileLock + apply_patch 接入 |
| C Busy 统一 | run_with_busy 抽到 app；Agent/Skills/部分 MCP 已用；Provider 测试已是后台线程 |
| D Agent 更新 | 完成：增量优先/失败重装/已最新跳过/--force |
| E 中文/Prompt | 部分完成；待收尾 |
| F Runtime | 未开始 |

---

## 8. 会话管理（Session）— 进行中

| 步骤 | 动作 | 验收 |
|------|------|------|
| S1 | OpenCode 硬删（DELETE + CASCADE），Codex/Claude 删 jsonl | `/session` 与 DB 一致消失 |
| S2 | TUI 触屏：管理会话 / 勾选 / 全选 / 删除选中 / 确认 | 不误恢复会话 |
| S3 | CLI：`spec session list/delete --oldest N --source` | 实测删 10 条 DB 同步 |
| S4 | **会话标题完整展示**：列表/详情/CLI 都显示真实 title（OpenCode `session.title`、Claude/Codex 首条用户消息） | 标题可读、可搜索 |
| S5 | 列表标题宽度自适应（触屏列表不截断到看不清） | 小屏仍能辨认 |
| S6 | 按来源过滤 + 按标题搜索 | 过滤后批量删 |

**进度：** S1–S3 已完成；S4–S6 列入待做（标题已有基础字段，需加强展示与搜索）。

---

## 9. 下一步

1. 会话标题展示加强（S4–S5）  
2. 继续架构拆分（Provider/Skills/MCP 事件外提）  
3. Prompt 编辑器（E5）

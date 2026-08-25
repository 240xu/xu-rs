# 前端视觉打磨（v3 TUI + Web）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在不改业务逻辑的前提下，统一 TUI 与 Web 控制台的视觉层级：主题令牌收敛、选中卡片强调、Dock 次级降噪、Web 细节质感。

**Architecture:** 所有颜色经 `src/ui/theme.rs` 单一来源分发（方向 C 克制派）；渲染与命中几何继续同源；Web 端仅改 `style.css` 静态资源。

**Tech Stack:** Rust + ratatui/crossterm（TUI）；纯 CSS（Web）。

**Spec:** docs/superpowers/plans/2026-08-21-full-ui-ux-plan-v2.md（方向 C）+ 本轮用户指示「把前端画好看点」。

## Global Constraints

- 组件禁止裸写 `Color::`，一律用 theme 令牌（theme.rs 头注释铁律）
- 渲染↔命中必须同源几何（dock::DOCK_HEIGHT / list_window_rects 等）
- 不新增业务逻辑、不动事件分支
- 门禁：`cargo fmt`、`cargo test --all-targets`、`cargo clippy --all-targets -- -D warnings`、release 构建、安装哈希一致

---

### Task 1: theme 增补选中底色令牌

**Files:** Modify `src/ui/theme.rs`

- [ ] 新增 `SEL_BG = Rgb(36,41,60)`：选中卡片底色（比 PANEL_BG 略亮一档，与 Dock 主按钮区分层级）

### Task 2: 供应商列表卡令牌化 + 选中强化

**Files:** Modify `src/ui/screens/providers.rs`

- [ ] 健康徽章：Green→`theme::OK`、Red→`theme::BAD`、DarkGray→`theme::G2`
- [ ] 选中竖条 Cyan→`theme::ACCENT`、按压 Yellow→`theme::WARN`
- [ ] 名称 White/Gray/DarkGray→`INK/G1/G2`
- [ ] 生效端三色改用 `theme::IDENTITY[0..3]`（青/品红/黄单一来源）
- [ ] 选中/按压卡背景 `PANEL_BG`→`SEL_BG`

### Task 3: Dock 次级按钮降噪

**Files:** Modify `src/ui/widgets/dock.rs`

- [ ] 非主动作 fg `INK`→`G1`（灰阶降档），主动作保持 ACCENT+BOLD+SEL_BG

### Task 4: Web 静态页质感

**Files:** Modify `src/web/static/style.css`

- [ ] FAB 增加 hover 态（边框亮化+上浮 1px）
- [ ] 卡片 hover 边框微亮；tab-btn 增加 focus-visible 描边
- [ ] 细滚动条样式（webkit + firefox）
- [ ] topbar 底部渐变阴影替代硬边框（保留 border 兼容）

### Task 5: 门禁验证 + 安装

- [ ] `cargo fmt --all && cargo test --all-targets && cargo clippy --all-targets -- -D warnings && cargo build --release`
- [ ] `cp target/release/spec scripts/spec && ./scripts/install-termux.sh && rm -f scripts/spec`
- [ ] sha256 目标==安装一致

## Self-Review

- 覆盖：theme/providers/dock/web 四处视觉面 ✓；无占位符 ✓；令牌名前后一致（SEL_BG/OK/BAD/WARN/ACCENT/INK/G1/G2/IDENTITY）✓
- 用户已明示自主推进（「你改吧」「继续」），按 executing-plans 内联执行。

# 模型映射唯一入口整合（详情页 → 编辑表单）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把"模型映射编辑"从详情页模型表格移除，统一收敛到编辑表单「模型」tab 作为唯一编辑入口；详情页模型表格只保留"获取模型 + 勾选多选保存 models"。

**Architecture:** 详情页删除两处改名/写入路径（`EditModel` 行点击、`DetailPickMode::Model(row)` 拉取列表选择写入），保留拉取与勾选保存（`ToggleModelSel`/`SaveModels`）；模型表格区增加"改显示名/思考强度请到［编辑］→［模型］"引导提示。编辑表单「模型」tab（模型三列 + 增删行 + 获取模型）保持不动，成为唯一的映射编辑入口。

**Tech Stack:** Rust（spec CLI + ratatui TUI），serde_json，现有测试框架。

**Spec:** `docs/superpowers/specs/2026-08-16-web-console-design.md` 无关；本轮依据用户 2026-08-16 决策："编辑表单为唯一编辑入口（推荐）"（详情页模型表只留获取+勾选保存）。

## Global Constraints

- 协议转换层（`src/runtime/**`、`src/providers/**`、`src/domain/**`、`src/agents/**`）禁止修改。
- Web 控制台（`src/web/**`）禁止修改；前端 `/api/providers` 显示不受影响。
- 不新增任何 Cargo 依赖；不 git commit（除非用户明确要求）。
- `cargo clippy --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`git diff --check` 必须全绿。
- 现有 857 测试（含上次新增的 5 档推理循环）不得回归。

---

### Task 1: 移除详情页"行点击改名"入口（EditModel → 拉取选择写入）

**Files:**
- Modify: `src/main.rs:2350-2372`（`ProviderDetailAction::EditModel(row)` 分支）、`:2550-2600`（keyboard `PickModel` 分支，若有）、`:2901-2920`（`ProviderDetailAction::EditModel(row)` 分支）、`:2650` 附近（`ProviderDetailAction::PickModel(index)` 分支）
- Modify: `src/menu/mod.rs`：`ProviderDetailAction::EditModel(usize)` 变体（约 600 行）、模型表格点击命中（约 1710 行 `3 => Some(ProviderDetailAction::EditModel(index))`）、`DetailPickMode::Model(usize)` 相关 render（约 2333-2360）
- Test: `src/main.rs::tests`

**Interfaces:**
- Consumes: `menu::ProviderDetailAction` 枚举、`detail_fetch`/`detail_pick` 状态
- Produces: 移除 `ProviderDetailAction::EditModel` 与 `DetailPickMode::Model`；`ToggleModelSel`/`SaveModels`/`PickSlot` 保持

- [ ] **Step 1: 写失败测试**

把 `model_table_row_click_opens_name_edit` 现有断言改为期望"点击模型行不再进入改名模式"（若不存在此类测试，新建一个验证 `provider_detail_hit_test` 模型表格区返回新语义）：

```rust
#[test]
fn model_table_row_click_no_longer_enters_name_edit() {
    let area = Rect::new(0, 0, 80, 40);
    let model = model_with(&[("a", "a1")]);
    // 模型表格第一行（几何用 TALL 80x40 的 detail 布局，行号参考 detail_overlay_rects）
    let action = provider_detail_hit_test(area, 1, 1, &tap(30, 18), &model);
    assert!(!matches!(action, Some(ProviderDetailAction::EditModel(_))));
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --bin spec model_table_row_click_no_longer_enters_name_edit`
Expected: FAIL —— 当前点击模型行会返回 `Some(EditModel(_))`（若测试几何不准，先按 `detail_model_table_rects`/`detail_overlay_rects` 的实际矩形调整坐标，保证 RED 确实因"还进改名"而失败）。

- [ ] **Step 3: 移除点击进改名**

在 `src/menu/mod.rs` 模型表格命中处（约 1710 行），把表格区命中改为不产生 `EditModel`（返回 `None`，或新增语义化的 `DetailInfo` 动作仅用于提示）：

```rust
3 => None, // 模型表格行点击不再进入改名：编辑入口收敛到「编辑」→「模型」
```

同时删除 `ProviderDetailAction::EditModel(usize)` 变体及其它引用。

- [ ] **Step 4: 删除 main.rs 中 EditModel/PickModel 改名分支**

删除 `src/main.rs` 中：
- `ProviderDetailAction::EditModel(row)` 的三个分支（2350-2372、2901-2920）
- `ProviderDetailAction::PickModel(index)` 与 `DetailPickMode::Model(row)` 的写入逻辑（2665-2695）——保留 `PickSlot`/`PickSlotModel`（Claude 槽位展开，独立功能）

- [ ] **Step 5: 清理 DetailPickMode::Model 与 detail_row_* 改名辅助**

在 `src/main.rs` 删除 `detail_row_pick_result`（191-211）、`detail_row_sel_init`（214-223）及其编译相关引用；在 `src/app/provider_ops.rs` 删除 `detail_row_pick_args`（1417）；`menu::DetailPickMode::Model` 变体从 `src/menu/mod.rs:1605` 枚举中移除。

- [ ] **Step 6: 更新/删除受影响测试**

删除 `src/main.rs:4520 detail_row_sel_init_locates_aliased_rows_by_request_name` 测试（函数已删）；运行 `cargo test --bin spec`，修掉所有因删变体/函数导致的编译错误与断言失败（如 `detail_*` 流程测试）。

- [ ] **Step 7: 门禁与增量验证**

Run: `cargo test --quiet && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check && git diff --check`
Expected: 全绿；测试总数 = 857 - 删除数（≤858）。

---

### Task 2: 模型表格区引导提示 + 保留勾选/拉取交互

**Files:**
- Modify: `src/menu/mod.rs` 的 `render_provider_detail_v2` 模型表格标题区（约 2441 行 "模型表格"）
- Test: `src/menu/mod.rs::tests`

**Interfaces:**
- Consumes: Task 1 之后无 `EditModel` 的表格区
- Produces: 表格区标题下方一行 DarkGray 提示；`ToggleModelSel`/`SaveModels`/`Models` chip 行为不变

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn detail_model_table_shows_editor_hint() {
    // 渲染后表格标题行必须包含“编辑”，指向唯一编辑入口（渲染纯函数化前用 buffer 断言不可行，
    // 改为抽取 hint 常量并断言）
    assert!(DETAIL_MODEL_EDIT_HINT.contains("编辑"));
    assert!(DETAIL_MODEL_EDIT_HINT.contains("模型"));
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --bin spec detail_model_table_shows_editor_hint`
Expected: FAIL —— 常量 `DETAIL_MODEL_EDIT_HINT` 不存在。

- [ ] **Step 3: 实现提示常量与渲染**

在 `src/menu/mod.rs` 定义：

```rust
/// 模型表格区引导文案：改名/思考强度收敛到编辑表单「模型」tab。
pub const DETAIL_MODEL_EDIT_HINT: &str = "改显示名/思考强度请到 编辑 → 模型；此处仅拉取与勾选模型";
```

在 `render_provider_detail_v2` 模型表格标题下方渲染该行（`Style::default().fg(Color::DarkGray)`，`Alignment::Left`，占一行，位于 "模型表格" 标题与表头之间；若布局高度溢出，用 `Constraint::Length` 调整表格起始行）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --bin spec detail_model_table_shows_editor_hint`
Expected: PASS。

- [ ] **Step 5: 门禁**

Run: `cargo test --quiet && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check && git diff --check`
Expected: 全绿。

---

### Task 3: 帮助文本与详情页提示文案同步

**Files:**
- Modify: `src/menu/mod.rs` 帮助文本（详情页快捷键行，约 3370 附近 "m 获取模型 · s 模型多选保存" 处）
- Modify: `src/menu/mod.rs` `render_provider_detail_v2` 底部 hint 行（如存在旧提示"点行改名"）

**Interfaces:**
- Consumes: 无
- Produces: 帮助文本与渲染提示不再出现"点行改名/显示名=请求名"表述

- [ ] **Step 1: 更新帮助文本快捷键行**

把 "m 获取模型 · s 模型多选保存 · g 进入路由多选 · e 编辑 · t 调试 · d 删除" 之后，若存在"点模型行改显示名"类文案，改为 "模型改名/思考强度：编辑 → 模型"。

- [ ] **Step 2: 巡检残留文案**

Run: `rg -n "显示名=请求名|点.*行.*改|EditModel|DetailPickMode::Model" src/`
Expected: 除 Task 1 已清路径外零残留（若有遗漏返回 Task 1）。

- [ ] **Step 3: 门禁**

Run: `cargo test --quiet && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check && git diff --check`
Expected: 全绿。

---

### Task 4: 全量回归 + 构建安装

**Files:** 无源码修改

- [ ] **Step 1: 全量门禁**

Run: `cargo test --quiet`
Expected: 全部 target `test result: ok`，无 FAILED/error。

Run: `cargo clippy --all-targets -- -D warnings` → 0 error/warning
Run: `cargo fmt --all -- --check` → 0 diff
Run: `git diff --check` → 0

- [ ] **Step 2: 构建与安装**

Run: `cargo build --release && cp target/release/spec scripts/spec && bash scripts/install-termux.sh && rm scripts/spec`
Expected: `installed spec to /data/data/com.termux/files/usr/bin/spec`

- [ ] **Step 3: 人工冒烟（可选）**

Run: `spec` → 选某 provider → 详情页：确认模型表格点击不再进入改名、出现引导提示、`获取模型`/`多选保存` 仍可用；进入「编辑」→「模型」tab：确认三列（显示名/请求名/思考强度）+ 增删行 + 获取模型均正常。
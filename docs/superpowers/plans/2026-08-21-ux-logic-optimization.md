# spec TUI · 操作逻辑与体验优化计划（PM → 工程执行稿）

- 日期：2026-08-21 · 作者：PM（基于 PTY 走查 + 代码取证）
- 执行：工程师（本轮内完成）

## 一、现状走查结论

| 走查项 | 实测 |
|---|---|
| `q` 退出 | **只在 Menu 模式生效**（main.rs Menu arm）。Agent/MCP/Skills/详情等页按 q 无反应，必须 Esc 逐层退回再 q——手机上多打 2-4 次键 |
| 数字键 | 只绑 `1-4`；第 5 个 Tab「会话」无直达键 |
| 工具条 | IconButton 10×4 盒子 + gap=1 → **每行 chips 占 5 行**（30 行屏的 17%）。Providers/MCP/Agent 各 1 行、Skills 7 枚也 1 行。这是旧视觉最后一块大残留，且挤压内容区 |
| Esc 语义 | 各 Mode 独立定义（返回/清除/取消），层级正确但无全局兜底 |

## 二、问题清单与优先级

| # | 类别 | 问题 | 级别 |
|---|---|---|---|
| Q1 | 操作逻辑 | q 非全局：深层页面退出路径过长 | P0 |
| U1 | UI/空间 | 工具条 5 行带过高，内容区被挤 | P0 |
| Q2 | 操作逻辑 | 会话页无数字直达键 | P2 |
| Q3 | 反馈 | （已有 run_with_busy/connection_rx 异步模式，本轮不动） | — |

## 三、执行方案

### E1（Q1）全局 q 退出——白名单拦截
主循环 Key 分发**前置**一个拦截层：
```
q 且 当前无文本输入焦点 → Mode::Quit
```
文本输入焦点判定（不拦的白名单）：
- provider_search.focused
- Mode ∈ {ProviderAddForm, ProviderEditForm, McpForm, SkillForm, OpenCodeSettings}
- ProviderDetail 的输入框聚焦（view.field.is_some()）
实现点：main.rs 主循环 match ev 之前。Esc 层级返回语义保持不变。

### E2（U1）工具条压缩：10×4 盒 → 单行文字按钮
- 渲染：IconButton 改为单行 `[ + 新增 ]` 样式（方括号定界，激活色 glyph），高度 1
- 几何：`TOOL_CHIP_HEIGHT 4→2`（视觉 1 行 + 1 行呼吸），`toolbar_rects` 不变公式自动跟随
- 命中：矩形高 2，触屏可接受（相邻 gap=1 防误触）
- 收益：每页工具条带 5 行→3 行，**净省 2 行内容区**；Skills 页头部同步变矮
- 连锁：provider_card_area / agent_header_height / mcp_header_height / skill_header_height 公式全部引用常量自动适配；相关测试坐标（chips y=5→y=3 等）逐一更新

### E3（Q2）数字键 5 → Sessions tab
Menu arm `'1'..='4'` → `'1'..='5'`（TABS[4]=Sessions）。

### E4 回归
全量测试坐标修复 + fmt/clippy + release 安装 + PTY 走查（q 全局可达 / chips 新样式 / 内容区增高 / 5 直达会话）。

## 四、验收标准
1. Agent 页按 q 直接退出应用
2. 编辑表单/搜索聚焦时输入 q 是字符，不触发退出
3. 工具条视觉单行化，供应商列表一屏可见卡片数 ≥5（30 行终端）
4. 按 5 直达会话页
5. 837+ 测试全绿

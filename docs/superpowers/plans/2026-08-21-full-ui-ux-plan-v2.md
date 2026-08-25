# spec TUI · 全面优化计划 v2（交互逻辑简化 × UI 全量）

- 日期：2026-08-21 · PM 出稿 · 待分阶段执行
- 触发：用户反馈「太繁杂」——不止视觉，是**操作链路本身重**

## 一、现状诊断：为什么繁杂

### 1.1 模式爆炸
`Mode` 枚举 **37 个状态**，其中「三兄弟」模式泛滥：
每条 CRUD 流都是 `XxxForm → XxxPreview → XxxConfirm → XxxResult` 四跳：

| 流域 | 模式链 | 跳数 |
|---|---|---|
| MCP 增改 | Mcp → McpForm → McpFormPreview → McpFormConfirm → McpResult | **5 屏/次** |
| Skill 增改 | Skills → SkillForm → SkillFormPreview → SkillFormConfirm → SkillResult | 5 屏/次 |
| 供应商生效 | PlanPreview → ApplyConfirm → ApplyResult | 3 屏/次 |
| 删除类 | DeleteConfirm/McpConfirm/SkillConfirm 独立全屏 | 各 1 屏 |

**一次「加个 MCP 服务」要点 5 屏**——这就是繁杂感的根源。

### 1.2 双重/三重 Tab 心智
主 5 Tab → 详情内三端 Tab → 编辑内 5 分页。三层嵌套，且「连接」字段在详情与编辑两处重复出现。

### 1.3 UI 尾巴
15 处 IconButton 3 行盒、23 处边框面板、presets/multi/settings 页未过 C 语言。

## 二、方案

### Phase A · 交互链路减负（P0，砍半跳数）
- **A1 确认链三合一**：Preview 内容内嵌到 Confirm 弹窗（diff 直接看），Result 降级为返回后的一行状态消息。
  - 目标：Mcp/Skill 流 5 屏→**2 屏**（表单→带预览的确认）；Apply 流 3→1。
  - 模式数预期 37→**22 左右**（删 8 个 Preview/Result 模式 + 合并项）。
- **A2 危险操作分级**：删除保留独立确认（安全）；其余写入一律 A1 单屏确认。
- **A3 详情/编辑去重**：编辑表单砍掉「连接」分页（详情页字段已就地编辑），编辑只留 性能/能力/资料/模型 → 编辑 5 页变 4 页，心智少一层。

### Phase B · UI 全量收尾（P1）
- **B1** 15 处 IconButton 盒 → 已有紧凑分支，逐点裁 1 行渲染（零几何风险模板，本轮已在编辑页验证）。
- **B2** 23 处边框面板 triage：输入框/浮层对话框保留圆角盒（浮层语汇）；纯信息展示面板去框改 ▍标题+明度底。
- **B3** 未审计页过一遍：ProviderPresetList / McpPresets / MultiProvider / OpenCodeSettings / 各 Result 页。

### Phase C · 引导与容错（P2）
- 首次空态页给三条快捷路径（无供应商→模板/新增；无 MCP→导入）。
- 所有破坏性操作后提供 3 秒撤销窗口（status 行 + u 键）。

## 三、执行顺序建议
1. **A1-MCP**（最痛的流，打样三合一模式）
2. **A1-Skill / A1-Apply**（复制打样模式）
3. **B1/B2**（机械批量）
4. **A3**（动信息架构，单独一轮做）
5. C 项按需

## 四、验收基线
- 任一 CRUD ≤2 屏完成
- Mode 数 ≤24
- 全应用单一视觉词汇（已达成，保持不回退）
- 858 测试持续绿

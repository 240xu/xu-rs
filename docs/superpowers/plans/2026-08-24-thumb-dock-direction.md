# 触屏优先重设计 · 方向已选定

- 用户选择：**A · 拇指坞 Thumb-Dock**（原话："A"）
- 定义：每页底部常驻动作坞，当前页全部主操作=大按钮；键位功能 100% 获得按钮等价；内容区上移让位
- 前置审计结论：点击类（Tab/卡片/chips/开关/−/+ /模型表/▸）已触屏化；缺口=Enter/Esc/y/1·2·3/m/s 及列表翻页

## 实施规格（逐页）
1. **通用 Dock 组件** `ui/widgets/dock.rs`：`Dock { buttons: Vec<(label, action)> }`，占底部 2 行（大按钮 1 行 + 安全间距），渲染=均分文字钮（方向 C：无框、活动青底黑字、暗灰描边省略），命中=`dock_rects(area,count)` 与渲染同源。主循环把 Dock 命中翻译为对应 AppEvent/Mode 动作——**复用现有键盘分支**，零新业务逻辑。
2. **Providers 页**：`+ 新增 · # 模板 · ↻ 刷新 · ⇄ 多选 · 🔍 搜索`（搜索=聚焦 SearchBox）
3. **Detail 页**：`✎ 编辑 · ⚡ 测试 · ↓ 获取模型 · ☑ 多选保存 · 1 Claude · 2 Codex · 3 OpenCode · y 明文 · ← 返回(Esc)`
4. **Editor 页**：`‹ 页 › · ✓ 保存(Esc=取消) · ← 返回`
5. **MCP/Skills**：现有工具条 chips 已触屏，仅补 `← 返回`
6. **Sessions/Agents/Help**：补 `← 返回`；Agents 补 `↻ 刷新`
7. **列表翻页**：Dock 左端固定 `↑↓` 翻页钮（驱动现有 selection 移动，窗口跟随）
8. **Esc 等价**：所有 Dock「返回」直接走既有 Esc 分支

## 批次
- D1 widgets/dock.rs + Providers 试点（PTY 验收）
- D2 Detail 全键位按钮
- D3 其余页 + 翻页钮 + 回归

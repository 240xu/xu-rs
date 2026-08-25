# spec TUI · 前端层重写 v3（Frontend Rewrite）

- 目标：业务逻辑零改动，展示层整体重写
- 设计语言：方向 C（已选定，不重过门）
- 铁律：**功能无损迁移**——每迁一屏跑全量测试；模型映射（槽位+模型表+多选保存）为 P0 保护对象

## 一、旧前端的问题（为什么重写而不是继续补）
- `menu/mod.rs` 5150 行单文件：14 个 draw + 12 个 hit-test + 组件 + 几何常量混居
- 渲染↔命中靠约定同步（同源函数仅部分页面做到）
- `main.rs` 主循环 3300 行：37→30 个 Mode 臂内联全部键盘/鼠标分发
- 无 View 状态机：各屏各自维护零散局部变量，跨屏返回靠手抄

## 二、新架构

```
src/ui/
├── mod.rs          # 出口；Screen trait
├── theme.rs        # 设计令牌：色板/间距/字符（▍█░━）唯一来源
├── widgets/
│   ├── mod.rs      # re-export
│   ├── chip.rs     # ToolChip/PillChip/IconButton（含紧凑分支）
│   ├── card.rs     # Card 列表卡（▍条/面板底/右缘遥测）——主列表/MCP/Skills/Agent 共用
│   ├── field.rs    # 表单字段（▍标签/值/提示，−/+ 文字钮）
│   ├── toggle.rs   # ToggleRow（█░ + 开/关）
│   └── table.rs    # 模型映射表（显示名|请求名|强度 三列 + 勾选列）
├── layout.rs       # list_window_rects/list_window_start/clamp 等几何纯函数
└── screens/
    ├── providers.rs    # 列表页（render+hit 同文件同源）
    ├── detail.rs       # 详情页（三端Tab/协议/输入/开关/槽位/模型表/动作条）
    ├── editor.rs       # 编辑表单（4字段页+模型映射页）
    ├── agents.rs
    ├── mcp.rs          # 列表+表单+确认（内嵌diff）
    ├── skills.rs
    ├── sessions.rs
    └── help.rs
```

### 关键规则
1. **每个 screen 模块内 render_xxx 与 xxx_hit_test 相邻共置**，几何只准经 layout.rs 的纯函数派生
2. **组件只认 theme 令牌**，禁止裸 Color::
3. Screen trait（后续引入）：
   ```rust
   trait Screen { fn draw(&mut self, f: &mut Frame, area: Rect); fn handle(&mut self, ev: &AppEvent) -> Handled; }
   ```
   本轮先做模块化搬移，trait 化作为第二阶段（避免一次动主循环）

## 三、迁移策略（绞杀者模式，非大爆炸）
按依赖从低到高逐屏搬迁，旧 `menu/mod.rs` 每清空一块就从 `pub use` 桥接，测试全程绿：

| 批次 | 内容 | 风险 |
|---|---|---|
| M0 | theme.rs + layout.rs 抽取（纯函数平移） | 低 |
| M1 | widgets 四件套抽取（实现平移+紧凑分支收编） | 低 |
| M2 | screens/providers + agents（最熟的两屏） | 中 |
| M3 | screens/mcp + skills | 中 |
| M4 | screens/detail + editor（**模型映射 P0**：slot/model-table/多选保存逐项对照清单验收） | 高 |
| M5 | sessions/help + 旧 menu/mod.rs 清空为桥接壳 | 低 |
| M6 | main.rs 分发瘦身（match 臂调 screen.handle） | 高 |

## 四、P0 功能保护清单（M4 验收门）
- [ ] Claude 槽位映射：Fable/Opus/Sonnet/Haiku 四槽编辑 + ▸ 上游拉取选择
- [ ] 模型表格：显示名|请求名 两列编辑、思考强度五档、增删行、获取模型 overlay、多选勾选保存（variants 序列化）
- [ ] 生效链：单选/路由多选写入三端配置 + diff 确认
- [ ] 全部既有 858 测试绿（几何断言允许随新布局更新，行为断言不许删）

## 五、本轮执行
M0+M1 落地（theme/layout/widgets 抽取并桥接回旧路径），后续批次按「继续」推进。

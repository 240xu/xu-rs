# Xu: CC Switch + Codex++ 实施计划

## 目标

将 CC Switch 3.17.0 与 BigPizzaV3/CodexPlusPlus 1.2.36 中适合 Termux 的管理和路由能力，独立实现为 OpenCode、Claude Code、Codex 三端统一工具。

不复制 Tauri、桌面注入、系统托盘或 AGPL 源码。所有能力必须满足：

- Termux 原生可运行。
- 触屏优先，键盘备用。
- 操作按钮只执行操作，说明放底部灰色弱提示区。
- 写入前 dry-run 和脱敏 diff。
- 二次确认、备份、失败回滚。
- 不支持的协议明确拒绝，不提供空壳按钮。
- 网络探测只由用户显式触发。

## 视觉规范

### 信息架构

- 首页只展示有真实逻辑的一级工作区：Providers、Agents、MCP、Prompts、Skills、Sessions、Tools。
- Projects、Usage、Sync 等功能在 domain/CLI 可用前不显示空入口。
- MCP、Prompts、Skills 是平级工作区，不再通过 MCP 页互相跳转作为唯一入口。
- 宽度小于 64 列使用单列；宽屏使用双列；高度不足时滚动卡片窗口并保持焦点可见。
- 绘制与触屏命中必须使用同一个布局函数生成的 `Rect`，不得按固定终端列数猜测。

### 图形按钮

不依赖 Unicode 图标字体。使用 Ratatui `Buffer` 在固定 5x5 矩形中绘制边框、底色和 3x3 点阵图标，效果类似将小型 SVG 栅格化到终端单元格。

- 点阵由代码定义，不受 Android 字体中的图标字形影响。
- 新建、导入、导出、刷新、设置、编辑、测试、模型、三端、确认、取消等操作使用不同点阵。
- 绘制和触屏 hit-test 复用同一个 `Rect`，不手写另一套字符列范围。
- 激活状态使用青色反色填充；普通状态使用深灰边框和点阵。
- 白色用于正文，青色用于焦点/选中，绿色仅用于成功，红色仅用于危险/失败。
- 按钮含义显示在底部 `DarkGray` 说明区，不响应点击。

### 触屏规则

- 主要按钮高度至少 3 行或整张卡片可点击。
- 顶部工具栏左对齐，点击命中复用自绘控件的固定矩形。
- 卡片点击执行主要动作，不打开说明页。
- 高风险操作必须进入确认页。
- 小屏幕分页，不依赖横向滚动。

## 阶段 0：安全配置基础（已完成）

- Provider CRUD、模板、导入导出。
- 三端 adapter 与协议边界。
- OpenCode additive provider 合并。
- OpenCode live config 自动导入 Xu。
- OpenCode 非破坏写回。
- dry-run、脱敏 diff、备份、恢复、回滚。
- 三端 Agent 安装、更新、诊断。
- 三端会话浏览和恢复。

## 阶段 1：统一视觉和导航（基础已完成，持续迁移子页面）

### 功能

- 建立自绘点阵图标和 `IconButton` 控件。
- 替换首页、Provider、Agent、编辑器、确认页的纯中文按钮。
- 统一极简终端配色，移除普通导航中的彩虹语义色。
- 把说明移到底部灰色区域。
- 重新校准所有触屏 hit-test。

### 验收

- 所有可见按钮都有图形符号。
- 不再通过可点击按钮打开纯说明页。
- 触屏命中测试覆盖顶部按钮、卡片和分页。
- 40 列宽终端仍能看到主要动作。
- 40/60/80 列和小高度的布局、滚动、触屏命中有测试覆盖。

## 阶段 2：统一 MCP 管理（基础已完成）

### SSOT

`~/.codex/xu-mcp.json`

记录：

- ID、名称、transport (`stdio/http/sse`)。
- command、args、env、URL、headers。
- OpenCode/Claude/Codex 启用状态。
- 描述、主页、标签。

### 三端投影

- OpenCode：`opencode.json.mcp`，additive merge。
- Claude：`~/.claude.json.mcpServers`。
- Codex：`config.toml [mcp_servers]`。

### 界面

- 自绘 MCP 列表入口。
- 三端开关直接显示在卡片上。
- 自绘新增、编辑、导入和同步控件。
- 底部灰色显示 transport、命令和目标文件。

### 验收

- 可从三端 live config 导入。
- 单项启停只修改对应端。
- 未归 Xu 管理的配置保留。
- 修改、删除均有预览和备份。

## 阶段 3：Prompt 管理（基础已完成）

### 文件

- Claude：`~/.claude/CLAUDE.md`
- Codex：`~/.codex/AGENTS.md`
- OpenCode：`~/.config/opencode/AGENTS.md`

### 功能

- Prompt preset CRUD。
- 首次 live backfill。
- 切换前回填当前 live 内容。
- 三端独立启用，也支持从同一模板复制到三端。
- Markdown 预览、差异和恢复。

### 验收

- 不丢失用户手改内容。
- 切换有备份。
- Profile 可引用 Prompt ID。

## 阶段 4：Skills 和 Plugin 元数据（本地基础已完成）

### SSOT

`~/.codex/xu-skills/`

### 功能

- 本地目录、GitHub、ZIP 安装。
- SHA-256 更新检测。
- OpenCode/Claude/Codex 开关。
- `$HOME` 内优先 symlink；共享存储自动改用 copy。
- 卸载前备份。
- Codex++ Plugin 只管理对 Termux CLI 有意义的元数据和 Skill，不移植桌面注入插件。

### 验收

- 三端目录投影正确。
- 失效依赖给出诊断。
- 更新失败保留旧版本。

## 近期交付顺序（按 CC Switch 3.17 价值重新排序）

1. Project Profiles：整套 Provider/MCP/Skills/Prompt 状态的命名快照和自动保存切换。
2. Skills 远程仓库：GitHub/ZIP 安装、更新检测、卸载备份和恢复。
3. 全量 Xu 导入导出：versioned manifest、默认脱敏、恢复前安全快照。
4. Usage/Cost MVP：先从三端 session 增量导入，再接 Runtime 请求日志。
5. Universal Provider：一个 provider 显式绑定多个目标并事务同步。

自动 failover 和官方 ChatGPT OAuth 接管后置：前者依赖完整协议桥与错误分类，后者涉及账号和服务条款风险。

## 阶段 5：Universal Provider

一个模板生成三端配置，而不是假设三端协议相同。

### Adapter

- OpenCode adapter：仅 Chat-compatible。
- Claude adapter：Anthropic 直连，Chat 经 Runtime，Responses 拒绝。
- Codex adapter：Responses 直连，Chat/Anthropic 经 Runtime。

### 原子性

- 先验证全部目标。
- 生成全部 patch。
- dry-run 汇总。
- 一次确认后顺序应用。
- 任一失败回滚已写文件。

## 阶段 6：Project Profiles

当前状态：核心切换闭环 `DONE`。已实现 schema v1 store、create/list/show/rename/delete/capture、三端独立 scope、自动保存离开项目、悬空引用诊断、逐文件脱敏 diff、首页三端 current 摘要、TUI create/rename 堆叠文本表单和二次确认，以及 Provider/MCP/Prompt/Skills/route/current/Project store 的混合事务 apply。Skill 删除先隔离，提交后清理，失败时精确恢复原 copy/symlink。剩余 provider deactivation 和更多 profile 字段继续迭代。

Profile 快照包含：

- Provider 单/多供应商配置。
- MCP 开关。
- Skills 开关。
- Prompt。
- OpenCode permission。
- Runtime 路由策略。

### CC Switch 3.17 对齐语义

- Project 是全局命名实体，每个目标 scope 保存独立 snapshot 与 current project。
- 切换前自动捕获离开项目的当前状态。
- 快照引用已删除对象时预览页报告 dangling reference，不静默恢复错误对象。
- 只应用与当前状态的最小差异，避免无意义重写所有 live 配置。
- 首页顶部提供当前项目摘要和快速切换；窄屏进入独立 Projects 列表。

### 强化点

- 支持三端，包括 OpenCode。
- apply 前完整 dry-run。
- 自动备份。
- 失败回滚。
- 不采用 CC Switch “失败但仍标记 current”的行为。

## 阶段 7：Usage、Token 和费用

### 数据来源

- Xu Runtime 请求日志。
- Claude/Codex/OpenCode session 日志增量导入。

### 数据

- 请求数、成功率、输入/输出/cache token。
- latency、TTFB、duration。
- provider、model、session。
- 自定义模型价格。
- 费用为本地估算，明确不等于账单。

### 界面

- 触屏时间范围。
- Provider/模型筛选。
- 表格和紧凑字符趋势，不做桌面图表复刻。
- JSON/CSV/Markdown 导出。

## 阶段 8：多 Endpoint

### 第一层

- Endpoint CRUD。
- 并行测速。
- latency/status/error 缓存。
- 手动选择和显式选择最快地址。

### 诚实边界

完成请求级 endpoint fallback 前，只称“地址管理和测速”，不称 HA。

## 阶段 9：Codex++ 聚合供应商

### 策略

- `priority`：固定优先级。
- `session`：同一会话固定供应商，新会话轮转。
- `request`：每请求轮转。
- `weighted`：按权重选择。

### 路由状态

- 聚合组 ID。
- 成员 provider、顺序、权重、启用状态。
- 会话粘滞映射。
- 请求计数和最后选择。
- 不把 model suffix 路由误称为 failover。

### 兼容范围

- Codex 优先完整实现。
- Claude 仅支持可安全转为 Anthropic Messages 的 provider。
- OpenCode 暂不接管请求。

## 阶段 10：真实 Failover 和 Circuit Breaker

### Failover

- 可重试错误分类。
- 认证错误默认不可跨账号重试。
- first-byte、idle、non-stream timeout。
- 最大重试次数。
- provider queue。

### Circuit Breaker

- Closed / Open / HalfOpen。
- 连续失败阈值。
- 错误率阈值和最小请求数。
- 恢复等待。
- HalfOpen 单 probe。
- 状态和事件日志。

### Termux 运行

- daemon、PID、日志、graceful shutdown。
- Termux:Boot 与 wakelock 只作为可选集成。

## 阶段 11：Session 增强

- Markdown 导出。
- 单个/批量删除并校验路径。
- provider metadata 回填。
- Codex provider 切换后的历史会话修复。
- token usage 关联。
- 项目/cwd 分组。
- 保留当前有界预览策略。

## 阶段 12：WebDAV/S3 快照

### 命令

- `sync status`
- `sync diff`
- `sync push`
- `sync pull`

### 内容

- Provider、MCP、Prompt、Skills、Profiles、Pricing。
- 默认不上传 Session、Usage、Health。
- Secret 默认排除；可选独立加密包。

### 安全

- manifest、版本、size、SHA-256。
- ETag/revision 防盲覆盖。
- 下载后校验再替换。
- 本地备份和失败回滚。

## 不实施

- Windows/macOS Codex 桌面注入。
- Tauri/WebView、系统托盘、桌宠、原生菜单。
- AppImage/MSI/DMG 功能。
- 未核实的 OAuth 绕过或桌面 app patch。
- 只存在于 UI、没有真实运行逻辑的开关。

## 每阶段交付流程

1. 核对上游真实行为和三端配置 schema。
2. 先写 domain/store/adapter。
3. 写 CLI，便于自动测试。
4. 写触屏 TUI。
5. 补 dry-run、备份和回滚。
6. 单元测试、集成测试、clippy。
7. 真机只读验证；联网操作需显式触发。
8. 构建 release、安装、更新 Downloads 和校验文件。

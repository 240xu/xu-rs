# DSH Termux 兼容补丁链（技术参考）

> 适用：DSH `0.1.5-rc.2` × Termux (android-arm64, bionic)。
> 维护者入口：`spec doctor` 看漂移 → `spec agent install dsh` 一键补回。
> 本文描述每个补丁的**为什么、锚点在哪、怎么验证、怎么恢复**。所有补丁均幂等（重复执行不重复追加）。

## 设计原则

1. **有证据才打补丁**：每个补丁对应一个在本机复现过的故障（启动门 fatal / 会话打不开 / 模块加载失败），不做假设性防御。
2. **最小改动**：优先降级语义（如无锁续跑），不改行为契约；能懒加载不删功能。
3. **可检测**：每个补丁留一个稳定字符串标记，`spec doctor` 逐项核对。
4. **可恢复**：改前 `.bak` 时间戳备份；npm 重装丢补丁属预期，靠安装器重打。

## bionic/Termux 环境约束（补丁存在的原因）

| 约束 | 后果 |
|---|---|
| 无 `/usr/bin/env` | 一切 shebang 必须重写为绝对 node 路径 |
| 文件系统拒绝硬链接（link() → EACCES） | 会话持久化的 link/发布路径必须 rename 回退 |
| `node-addon-system` 无 android 绑定 | flock 不可用（ERR_FLOCK_UNSUPPORTED_PLATFORM） |
| 无 NDK | node-pty 无预编译；能 clang 直编则编，否则 import 期 stub |
| 无 libvips | sharp 拒绝 android-arm64，只能 import 期 stub |
| `@vscode/ripgrep` 不发 android 包 | fs-search 回退系统 `rg` |
| koffi 3.2.x 可加载（0.1.5 线） | sandbox-local 可留用；bash-sandbox 必须保留（提供 sandboxMode） |

## 补丁矩阵

| # | 名称 | 目标文件（prefix = `$PREFIX/lib/node_modules/@deepseek-ai/dsh`） | 稳定标记 | 作用 |
|---|---|---|---|---|
| 1 | dsh shebang | `lib/bin.js`（首行） | `#!/…/usr/bin/node --expose-internals` | 无 env 下的启动 |
| 2 | fs-search rg 回退 | `node_modules/@deepseek-ai/dsh-tool-fs-search/lib/index.js` | `compatibility patch` | 打包 rg 缺失时回退系统 rg |
| 3 | session flock 降级 | `node_modules/@deepseek-ai/dsh-session-persistence-jsonl/lib/index.js` | `posix-unlocked` | 写锁不可用时无锁续跑（单用户设备），`release()` 照常关 fd |
| 4 | session link→rename | 同上 | `error.code === "EACCES"` | 会话提交 link 失败时同目录 rename |
| 5 | session publish rename | 同上 | `rename(staged` | migration 发布（resume 必经）link EACCES 时 rename |
| 6 | assets 缓存头 | `node_modules/@deepseek-ai/dsh-host-frontend-static/lib/index.js` | `max-age=31536000, immutable` | `/assets/*`（内容哈希文件名）强缓存；index 不缓存 |
| 7 | apiproxy termux-open | `node_modules/@deepseek-ai/dsh-native-command/lib/index.js` | `termux-open` | android 平台打开文件 |
| 8 | archived decoder | `~/.dsh/profiles/web/node_modules/dsh-archived-sessions/lib/index.js` | `vendored storage-row decoder` | 0.1.5 移除 `decodeStorageRecord`，从 0.1.1（MIT）逐字移植 |
| 9 | websearch 0.1.5 兼容 | `~/.dsh/profiles/web/node_modules/@240xu/dsh-websearch/lib/index.js` | `settingsCtx.settings.installSection` | 旧 `installSettingsSection` API 已删，改走 settings service |
| 10 | sharp import stub | `node_modules/sharp/dist/index.mjs`（+ `.cjs`） | `Termux/bionic stub` | 保证 importable，调用时抛能力错误；原版在 `.bak-termux-stub-*` |
| 11 | node-pty | `node_modules/node-pty/` | `build/Release/pty.node` 存在 = 真编；否则 `lib/index.js` 含 stub | 先 clang 直编，失败落 stub（安装器 `rebuild_node_pty` 自动二选一） |
| 12 | node-gyp android 映射 | `$PREFIX/lib/node_modules/npm/…/gyp/pylib/gyp/input.py` | `variables["OS"] = "linux"` | 无 NDK 时按 linux 处理 gyp android 分支 |
| 13 | profile 沙箱适配 | `~/.dsh/profiles/{web,headless}/cordis.patch.yml` | `dsh-sandbox-local`（disabled 行） | 只禁 sandbox-local；**勿禁 bash-sandbox**（sandboxMode 唯一提供者，禁了 permission 守卫 fatal）；**勿插 bare bash-local**（抢 shell 位且无 sandboxMode，同样 fatal） |
| 14 | .bashrc 助手 | `~/.bashrc` | `dsh-open()` | `dsh-url`（取 token 链接）、`dsh-open`（一键跳浏览器）、`dsh()` 防重守卫 |

全局树与 profile 树的关键文件多为**同一 inode 硬链接**（改一边等于改两边）；npm 重装会同时洗掉两侧。

## 漂移检测与恢复

```sh
spec doctor            # 逐项列出 [ok]/[DRIFT]/[n/a]
spec agent install dsh # DRIFT 时一键补回（幂等，含回归测试）
```

- `DRIFT` = 文件在但标记丢失（npm 重装的典型后果）。
- `n/a` = 目标未安装（如 dsh 未装）。
- 升级 DSH 的标准流程：`npm i -g @deepseek-ai/dsh` → `spec doctor` → 有 DRIFT 则 `spec agent install dsh` → 重启 dsh web → 再跑一次 `spec doctor` 应全 ok。

## dsh web 运维（去壳后的原生形态）

```sh
# 启动（唯一方式；不要 runit/包装器）
setsid nohup $PREFIX/bin/dsh --profile web --no-open --port 3080 \
  >> ~/dsh-web-restart.log 2>&1 < /dev/null &

dsh-url    # 取最新 token 链接（每次启动轮换；旧标签页必失效）
dsh-open   # 直接跳浏览器（termux-open-url → termux-open → am → echo 回退）
dsh web    # 3080 已被占用时只提示不重启（防重守卫）
```

- 裸地址 `http://127.0.0.1:3080/` 返回 `401` 是认证门（token 强制，官方设计），有 HTTP 码 = 服务可达。
- 手抄 token 必 401（72 字符）；用 `dsh-url` / `dsh-open`。
- token 只验状态码，不打印内容。

## 已知故障模式速查

| 症状 | 根因 | 处置 |
|---|---|---|
| 启动即崩 `first frame is not exactly one header line` | 会话 v3 首帧必须是**恰好一行 header**（`assertZstdHeaderFrame`，Buffer 字节语义：首个 `\n` = 末字节）；手工重写会话时误把全部事件塞进单帧 | 用 `clean-sessions.js`（两帧格式：帧0=header 行，帧1=其余事件）重写 |
| resume 报 EACCES link | 硬链接被文件系统拒绝 | 补丁 4/5（rename 回退），勿手工改 |
| resume 报 flock unsupported | 无 android 绑定 | 补丁 3（无锁降级） |
| 一次会话撑到 35MB+ 打开即卡 | 模型退化输出（"Go. OK. Writing. Let me output." 无限循环）×3 份冗余（text/reasoning/replayState.stream） | `clean-sessions.js` 裁剪（备份在 `~/.dsh/sessions-backup-*`） |
| `plugin tree failed: … permission-presets` | bash-sandbox 被禁 / bare bash-local 抢 shell 位 | 补丁 13 的收敛逻辑，恢复正确 profile 形态 |
| `EADDRINUSE :3080` | 双实例互撞 | 守卫拦截 + `pgrep -f` 用括号锚定真实路径（防自匹配） |
| task-board 报瞬时 `corrupt Zstandard` | 有会话正被边写边读（活体 turn 重试） | 杀掉续写源（重启 dsh），确认 mtime 停止增长 |

## 会话归档（数据运维，非补丁）

```sh
# 归档清单生成（只读）后按清单移动，同盘 rename，MANIFEST 逐条记录
~/.dsh/sessions-archive-<date>/          # 归档根，保留 <项目>/<会话> 结构
~/.dsh/sessions-archive-<date>/MANIFEST.txt
```

- dsh 对消失的会话目录**优雅跳过**（源码确认 ENOENT 不致命）。
- projcache 悬空条目（对应会话已移走）一并归档，冷读自动重建。
- 回滚：从归档目录 `mv` 回原位。

## 相关文件

- 安装器实现：`src/agent_tools.rs`（`patch_*` 系列函数 + `dsh_patch_status()`）
- 会话清理脚本：`/usr/tmp/opencode/clean-sessions.js`（临时件，建议入库时移入 `scripts/`）
- 发版流程：见 `RELEASE_RUNBOOK.md`

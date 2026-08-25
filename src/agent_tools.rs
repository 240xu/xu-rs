use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::patch::atomic_write;

const OPENCODE_BUILD_PY: &[u8] = include_bytes!("../assets/opencode-build.py");
const OPENCODE_WRAPPER: &[u8] = include_bytes!("../assets/opencode-wrapper-aarch64");
const OPENCODE_SHIM: &[u8] = include_bytes!("../assets/opencode-bunfs-shim-aarch64.so");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentToolId {
    Codex,
    Claude,
    OpenCode,
    Dsh,
}

#[derive(Clone, Copy)]
pub struct AgentTool {
    pub id: AgentToolId,
    pub key: &'static str,
    pub label: &'static str,
    pub command: &'static str,
    pub npm_package: Option<&'static str>,
    pub kind: AgentToolKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentToolKind {
    Npm,
    OpenCode,
    Dsh,
}

#[derive(Clone)]
pub struct AgentToolStatus {
    pub tool: AgentTool,
    pub command_path: Option<String>,
    pub current_version: Option<String>,
    pub latest_version: Option<String>,
    pub ready: bool,
    pub note: String,
}

pub const AGENT_TOOLS: [AgentTool; 4] = [
    AgentTool {
        id: AgentToolId::Codex,
        key: "codex",
        label: "Codex CLI",
        command: "codex",
        npm_package: Some("@openai/codex"),
        kind: AgentToolKind::Npm,
    },
    AgentTool {
        id: AgentToolId::Claude,
        key: "claude",
        label: "Claude Code",
        command: "claude",
        npm_package: Some("@anthropic-ai/claude-code"),
        kind: AgentToolKind::Npm,
    },
    AgentTool {
        id: AgentToolId::OpenCode,
        key: "opencode",
        label: "OpenCode",
        command: "opencode",
        npm_package: None,
        kind: AgentToolKind::OpenCode,
    },
    AgentTool {
        id: AgentToolId::Dsh,
        key: "dsh",
        label: "DeepSeek Harness",
        command: "dsh",
        npm_package: Some("@deepseek-ai/dsh"),
        kind: AgentToolKind::Dsh,
    },
];

#[derive(Deserialize)]
struct NpmPackageJson {
    bin: Option<serde_json::Value>,
    #[serde(default, rename = "optionalDependencies")]
    optional_dependencies: std::collections::BTreeMap<String, String>,
}

pub fn tool_by_key(key: &str) -> Option<AgentTool> {
    AGENT_TOOLS.iter().copied().find(|tool| tool.key == key)
}

pub fn parse_tool_keys(args: &[String]) -> Result<Vec<AgentTool>, String> {
    let Some(first) = args.first() else {
        return Ok(AGENT_TOOLS.to_vec());
    };
    if first == "all" {
        return Ok(AGENT_TOOLS.to_vec());
    }
    let mut tools = Vec::new();
    for key in args.iter().filter(|arg| !arg.starts_with('-')) {
        tools.push(tool_by_key(key).ok_or_else(|| format!("unknown agent: {key}"))?);
    }
    if tools.is_empty() {
        Ok(AGENT_TOOLS.to_vec())
    } else {
        Ok(tools)
    }
}

pub fn status_rows(home: &Path, query_latest: bool) -> Vec<AgentToolStatus> {
    AGENT_TOOLS
        .iter()
        .copied()
        .map(|tool| tool_status(home, tool, query_latest))
        .collect()
}

pub fn tool_status(home: &Path, tool: AgentTool, query_latest: bool) -> AgentToolStatus {
    let command_path = command_path(tool.command);
    let current_version = current_version(home, tool);
    let (latest_version, latest_error) = if query_latest {
        match latest_version(tool) {
            Ok(version) => (Some(version), None),
            Err(error) => (None, Some(error)),
        }
    } else {
        (None, None)
    };
    let ready = current_version.is_some();
    let note = match (
        tool.id,
        current_version.as_deref(),
        latest_version.as_deref(),
        latest_error.as_deref(),
        command_path.as_deref(),
        ready,
        query_latest,
    ) {
        (_, None, Some(latest), _, _, false, _) => format!("未安装，可装 {latest}"),
        (_, None, None, Some(_), _, false, true) => "未安装，最新版查询失败".to_string(),
        (_, None, _, _, _, false, _) => "未安装/不可运行".to_string(),
        (_, Some(current), Some(latest), _, _, true, _)
            if compare_versions(current, latest) < 0 =>
        {
            format!("可更新 {current} → {latest}")
        }
        (_, Some(current), Some(latest), _, _, true, _) => {
            format!("已是最新 {current}（目标 {latest}）")
        }
        (_, Some(current), None, Some(_), _, true, true) => {
            format!("已安装 {current}，最新版查询失败")
        }
        (_, Some(current), None, _, Some(path), true, _) => {
            format!("已安装 {current} · {path}")
        }
        (_, Some(current), _, _, _, true, _) => format!("已安装 {current}"),
        _ => "状态未知".to_string(),
    };
    AgentToolStatus {
        tool,
        command_path,
        current_version,
        latest_version,
        ready,
        note,
    }
}

pub fn install_or_update(home: &Path, tool: AgentTool) -> Result<String, String> {
    let lock_path = home.join(".codex/xu-install.lock");
    let _guard = InstallLock::acquire(&lock_path)?;
    match tool.id {
        AgentToolId::OpenCode => install_opencode(home),
        AgentToolId::Claude | AgentToolId::Codex => install_npm_tool(home, tool),
        AgentToolId::Dsh => install_dsh(home),
    }
}

pub fn install_or_update_tools(home: &Path, tools: &[AgentTool]) -> Result<String, String> {
    let mut out = String::new();
    let mut failed = Vec::new();
    for tool in tools {
        let before = current_version(home, *tool);
        match install_or_update(home, *tool) {
            Ok(result) => {
                out.push_str(&result);
                if !result.ends_with('\n') {
                    out.push('\n');
                }
                let after = current_version(home, *tool);
                out.push_str(&format!(
                    "汇总 {}：{}\n",
                    tool.label,
                    version_transition_text(before.as_deref(), after.as_deref())
                ));
            }
            Err(error) => {
                failed.push(format!("{}：{error}", tool.label));
                out.push_str(&format!("{} 失败：{error}\n", tool.label));
            }
        }
    }
    if !failed.is_empty() {
        return Err(format!(
            "{out}\n失败项：{}\n已成功项保留；修复后可重试失败项。",
            failed.join("；")
        ));
    }
    if out.trim().is_empty() {
        return Err("没有可更新的客户端".to_string());
    }
    Ok(out)
}

pub fn preview_update_plan(home: &Path, tools: &[AgentTool], query_latest: bool) -> String {
    let mut out = install_plan_text(tools);
    out.push_str("\n版本对照：\n");
    for tool in tools {
        let status = tool_status(home, *tool, query_latest);
        out.push_str(&format!(
            "- {}：{}\n  状态：{}\n",
            tool.label,
            version_transition_text(
                status.current_version.as_deref(),
                status.latest_version.as_deref(),
            ),
            status.note
        ));
    }
    out
}

pub fn install_plan_text(tools: &[AgentTool]) -> String {
    let mut out = String::new();
    out.push_str("客户端安装/更新计划\n");
    out.push_str("确认前不会执行。确认后会联网，并可能运行 pkg/npm。\n");
    out.push_str("OpenCode / Claude / Codex 全部参与；已最新会自动跳过。\n");
    for tool in tools {
        out.push_str("- ");
        out.push_str(tool.label);
        out.push_str(": ");
        match tool.id {
            AgentToolId::Claude | AgentToolId::Codex => {
                out.push_str(
                    "依赖检查 → 查询版本 → 已最新则跳过 → 优先增量安装 → 失败/损坏则重装 → 检测",
                );
            }
            AgentToolId::OpenCode => {
                out.push_str(
                    "依赖检查 → 查询版本 → 已最新则跳过 → 安装官方 npm 平台包 → 修复二进制 → 检测",
                );
            }
            AgentToolId::Dsh => {
                out.push_str(
                    "依赖检查 → 查询版本 → 已最新则跳过 → --ignore-scripts 安装 → shebang/九件套 Termux 补丁 → 检测",
                );
            }
        }
        out.push('\n');
    }
    out
}

/// All three clients (OpenCode / Claude / Codex).
pub fn updatable_client_tools() -> Vec<AgentTool> {
    AGENT_TOOLS.to_vec()
}

pub fn version_transition_text(current: Option<&str>, latest: Option<&str>) -> String {
    match (current, latest) {
        (None, Some(latest)) => format!("未安装 → 将安装 {latest}"),
        (Some(current), Some(latest)) if compare_versions(current, latest) < 0 => {
            format!("{current} → {latest}")
        }
        (Some(current), Some(latest)) => format!("{current}（已是最新，目标仍为 {latest}）"),
        (Some(current), None) => format!("{current} → 将查询并安装最新版"),
        (None, None) => "未安装 → 将查询并安装最新版".to_string(),
    }
}

pub fn diagnostics(home: &Path) -> String {
    let mut out = String::from("Termux 客户端诊断\n");
    let is_termux = std::env::var_os("PREFIX").is_some() && command_path("pkg").is_some();
    out.push_str(&format!(
        "termux: {}\n",
        if is_termux { "yes" } else { "no" }
    ));
    out.push_str(&format!("architecture: {}\n", std::env::consts::ARCH));
    out.push_str(&format!("prefix: {}\n", prefix().display()));
    for package in [
        "nodejs",
        "glibc",
        "glibc-runner",
        "python",
        "tar",
        "gzip",
        "coreutils",
    ] {
        out.push_str(&format!(
            "dependency {package}: {}\n",
            if package_installed(package) {
                "就绪"
            } else {
                "缺失"
            }
        ));
    }
    out.push_str(&format!(
        "disk: {}\n",
        match check_disk_space(home, 1024 * 1024 * 1024) {
            Ok(()) => "ready (>= 1 GB free)".to_string(),
            Err(error) => error,
        }
    ));
    for row in status_rows(home, false) {
        out.push_str(&format!(
            "agent {}: {}{}\n",
            row.tool.key,
            row.current_version.as_deref().unwrap_or("不可运行"),
            row.command_path
                .as_deref()
                .map(|path| format!(" ({path})"))
                .unwrap_or_default()
        ));
    }
    out.push_str(
        "\n使用 `spec agent setup --yes` 安装缺失依赖，并更新 OpenCode / Claude / Codex。\n",
    );
    out
}

fn install_npm_tool(home: &Path, tool: AgentTool) -> Result<String, String> {
    if !matches!(tool.id, AgentToolId::Claude | AgentToolId::Codex) {
        return Err(format!("{} 不走 npm 更新路径", tool.label));
    }

    let force = std::env::var_os("XU_FORCE_REINSTALL").is_some()
        || std::env::args().any(|arg| arg == "--force");

    let mut log = String::new();
    log.push_str(&format!("== {} 更新开始 ==\n", tool.label));

    log.push_str("[1/7] 检查 Termux 依赖与 node/npm...\n");
    ensure_termux_packages(&["nodejs", "glibc-repo", "glibc", "glibc-runner"])?;
    if command_path("npm").is_none() || command_path("node").is_none() {
        return Err(format!("{log}[1/7] 未找到 node/npm，请先安装 nodejs"));
    }
    let required_space = if tool.id == AgentToolId::Claude {
        700 * 1024 * 1024
    } else {
        300 * 1024 * 1024
    };
    check_disk_space(home, required_space)?;
    log.push_str("[1/7] 依赖与磁盘空间检查通过\n");

    let old_version = current_version(home, tool);
    log.push_str(&format!(
        "[2/7] 当前版本：{}\n",
        old_version.as_deref().unwrap_or("未安装")
    ));

    log.push_str("[3/7] 查询目标版本...\n");
    let target_version = match latest_version(tool) {
        Ok(version) => {
            log.push_str(&format!("[3/7] 目标版本：{version}\n"));
            Some(version)
        }
        Err(error) => {
            log.push_str(&format!("[3/7] 目标版本查询失败，回退 @latest：{error}\n"));
            None
        }
    };

    // No-op when already latest (unless force).
    if !force {
        if let (Some(current), Some(target)) = (old_version.as_deref(), target_version.as_deref()) {
            if compare_versions(current, target) >= 0 {
                log.push_str(&format!(
                    "[4/7] 已是最新 {current}，跳过安装（需要强制重装可加 --force）\n"
                ));
                log.push_str(&format!(
                    "== {} 完成：{} ==\n",
                    tool.label,
                    version_transition_text(Some(current), Some(target))
                ));
                return Ok(log);
            }
        }
    } else {
        log.push_str("[4/7] 强制重装模式已开启\n");
    }

    let package = tool
        .npm_package
        .ok_or_else(|| format!("{} 没有 npm 包配置", tool.label))?;
    let install_spec = match &target_version {
        Some(version) => format!("{package}@{version}"),
        None => format!("{package}@latest"),
    };

    // Strategy: try incremental first; on failure or broken detect, reinstall.
    let mut used_reinstall = false;
    if force || old_version.is_none() {
        used_reinstall = true;
        log.push_str(&format!(
            "[5/7] 执行重装：卸载旧包并安装 {install_spec}...\n"
        ));
        if old_version.is_some() || npm_package_present(package) {
            let _ = run_inherited("npm", &["uninstall", "-g", package], 300);
        }
        if let Err(error) = run_inherited(
            "npm",
            &["install", "-g", &install_spec, "--os=linux", "--force"],
            600,
        ) {
            rollback_npm(tool, old_version.as_deref());
            return Err(format!("{log}[5/7] 重装失败：{error}"));
        }
    } else {
        log.push_str(&format!("[5/7] 优先增量安装 {install_spec}...\n"));
        match run_inherited("npm", &["install", "-g", &install_spec, "--os=linux"], 600) {
            Ok(()) => log.push_str("[5/7] 增量安装完成\n"),
            Err(error) => {
                log.push_str(&format!("[5/7] 增量失败，回退重装：{error}\n"));
                used_reinstall = true;
                if old_version.is_some() || npm_package_present(package) {
                    let _ = run_inherited("npm", &["uninstall", "-g", package], 300);
                }
                if let Err(error) = run_inherited(
                    "npm",
                    &["install", "-g", &install_spec, "--os=linux", "--force"],
                    600,
                ) {
                    rollback_npm(tool, old_version.as_deref());
                    return Err(format!("{log}[5/7] 重装仍失败：{error}"));
                }
            }
        }
    }

    log.push_str("[6/7] 平台修复与版本检测...\n");
    let repaired = ensure_npm_platform_binary(tool)?;
    if tool.id == AgentToolId::Claude && !repaired {
        // Incremental may leave broken platform package; force reinstall once.
        if !used_reinstall {
            log.push_str("[6/7] 平台修复失败，自动升级为重装...\n");
            used_reinstall = true;
            let _ = run_inherited("npm", &["uninstall", "-g", package], 300);
            if let Err(error) = run_inherited(
                "npm",
                &["install", "-g", &install_spec, "--os=linux", "--force"],
                600,
            ) {
                rollback_npm(tool, old_version.as_deref());
                return Err(format!("{log}[6/7] 重装失败：{error}"));
            }
            if !ensure_npm_platform_binary(tool)? {
                rollback_npm(tool, old_version.as_deref());
                return Err(format!("{log}[6/7] Claude 平台二进制修复失败，已尝试回滚"));
            }
        } else {
            rollback_npm(tool, old_version.as_deref());
            return Err(format!("{log}[6/7] Claude 平台二进制修复失败，已尝试回滚"));
        }
    }
    if tool.id == AgentToolId::Claude {
        install_claude_launcher(home)?;
        log.push_str("[6/7] Claude launcher 已写入\n");
    }

    let Some(final_version) = current_version(home, tool) else {
        if !used_reinstall {
            log.push_str("[6/7] 检测失败，自动升级为重装...\n");
            let _ = run_inherited("npm", &["uninstall", "-g", package], 300);
            if let Err(error) = run_inherited(
                "npm",
                &["install", "-g", &install_spec, "--os=linux", "--force"],
                600,
            ) {
                rollback_npm(tool, old_version.as_deref());
                return Err(format!("{log}重装失败：{error}"));
            }
            if tool.id == AgentToolId::Claude {
                let _ = ensure_npm_platform_binary(tool);
                let _ = install_claude_launcher(home);
            }
            if let Some(v) = current_version(home, tool) {
                log.push_str("[7/7] 重装后检测通过\n");
                log.push_str(&format!(
                    "== {} 完成：{} ==\n命令：{}\n路径：{}\n模式：重装\n",
                    tool.label,
                    version_transition_text(old_version.as_deref(), Some(&v)),
                    tool.command,
                    command_path(tool.command).unwrap_or_else(|| "未知".to_string()),
                ));
                return Ok(log);
            }
        }
        rollback_npm(tool, old_version.as_deref());
        return Err(format!(
            "{log}[6/7] 安装后仍无法运行 `{} --version`，已尝试回滚",
            tool.command
        ));
    };

    log.push_str("[7/7] 汇总结果\n");
    log.push_str(&format!(
        "== {} 完成：{} ==\n命令：{}\n路径：{}\n模式：{}\n",
        tool.label,
        version_transition_text(old_version.as_deref(), Some(&final_version)),
        tool.command,
        command_path(tool.command).unwrap_or_else(|| "未知".to_string()),
        if used_reinstall { "重装" } else { "增量" },
    ));
    Ok(log)
}

fn install_dsh(home: &Path) -> Result<String, String> {
    let force = std::env::var_os("XU_FORCE_REINSTALL").is_some()
        || std::env::args().any(|arg| arg == "--force");

    let mut log = String::new();
    log.push_str("== DeepSeek Harness 更新开始 ==\n");

    log.push_str("[1/8] 检查 Termux 依赖与 node/npm/clang/make...\n");
    ensure_termux_packages(&["nodejs", "clang", "make"])?;
    if command_path("npm").is_none() || command_path("node").is_none() {
        return Err(format!("{log}[1/8] 未找到 node/npm，请先安装 nodejs"));
    }
    check_disk_space(home, 300 * 1024 * 1024)?;
    log.push_str("[1/8] 依赖与磁盘空间检查通过\n");

    let old_version = current_version(home, dsh_tool());
    log.push_str(&format!(
        "[2/8] 当前版本：{}\n",
        old_version.as_deref().unwrap_or("未安装")
    ));

    log.push_str("[3/8] 查询目标版本...\n");
    let target_version = match latest_version(dsh_tool()) {
        Ok(version) => {
            log.push_str(&format!("[3/8] 目标版本：{version}\n"));
            Some(version)
        }
        Err(error) => {
            log.push_str(&format!("[3/8] 目标版本查询失败，回退 @latest：{error}\n"));
            None
        }
    };
    if !force {
        if let (Some(current), Some(target)) = (old_version.as_deref(), target_version.as_deref()) {
            if current == target {
                log.push_str(&format!(
                    "[4/8] 已是最新 {current}，跳过安装（需要强制重装可加 --force）\n"
                ));
                log.push_str("== DeepSeek Harness 完成：已是最新 ==\n");
                return Ok(log);
            }
        }
    } else {
        log.push_str("[4/8] 强制重装模式已开启\n");
    }

    let install_spec = match &target_version {
        Some(version) => format!("@deepseek-ai/dsh@{version}"),
        None => "@deepseek-ai/dsh@latest".to_string(),
    };

    log.push_str(&format!(
        "[5/8] npm install -g --ignore-scripts {install_spec}...\n"
    ));
    log.push_str("      （--ignore-scripts：koffi 无 bionic 预编译、node-pty 需 NDK，两者 install 脚本必失败）\n");
    if let Err(error) = run_inherited(
        "npm",
        &["install", "-g", "--ignore-scripts", &install_spec],
        600,
    ) {
        return Err(format!("{log}[5/8] npm 安装失败：{error}"));
    }

    log.push_str("[6/8] shebang 修复（Termux 无 /usr/bin/env）...\n");
    let bin_js = prefix().join("lib/node_modules/@deepseek-ai/dsh/lib/bin.js");
    if !bin_js.exists() {
        return Err(format!("{log}[6/8] 未找到 {}", bin_js.display()));
    }
    let content = fs::read_to_string(&bin_js).map_err(|e| e.to_string())?;
    if content.starts_with("#!/data/data/com.termux/files/usr/bin/node --expose-internals") {
        log.push_str("[6/8] shebang 已就绪\n");
    } else {
        let (_, rest) = content.split_once('\n').unwrap_or((&content, ""));
        let fixed =
            "#!/data/data/com.termux/files/usr/bin/node --expose-internals\n".to_string() + rest;
        atomic_write(&bin_js, fixed.as_bytes()).map_err(|e| e.to_string())?;
        // atomic_write 落盘为 600，bin.js 需要可执行位
        let mut perms = fs::metadata(&bin_js)
            .map_err(|e| e.to_string())?
            .permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        fs::set_permissions(&bin_js, perms).map_err(|e| e.to_string())?;
        log.push_str("[6/8] shebang 已写入 --expose-internals\n");
    }

    log.push_str("[7/8] 物化 profile bundles（首次需下载，koffi 报错属预期）...\n");
    let pp =
        home.join(".dsh/profiles/node_modules/@deepseek-ai/dsh-permission-presets/lib/index.js");
    let mut materialized = pp.exists();
    if !materialized {
        for round in 1..=3 {
            let _ = run_capture(
                "sh",
                &["-c", "dsh --profile headless \"bootstrap\" 2>&1"],
                420,
            );
            if pp.exists() {
                materialized = true;
                break;
            }
            log.push_str(&format!("[7/8] 第 {round} 轮启动未完成，重试...\n"));
        }
    }
    if !materialized {
        return Err(format!(
            "{log}[7/8] profile bundles 下载失败，请检查网络后重试"
        ));
    }

    log.push_str("[8/8] 应用 Termux 兼容补丁...\n");
    patch_permission_presets(home)?;
    patch_session_persistence(home)?;
    patch_node_gyp()?;
    rebuild_node_pty()?;
    patch_attachment_local(home)?;
    ensure_profile_patches(home)?;
    patch_subprocess_local()?;
    patch_fs_search()?;
    patch_playwright_ld_preload(home)?;
    patch_apiproxy_termux_open()?;
    patch_dsh_web_wrapper(home)?;

    log.push_str("[8/8] 验证 headless 启动...\n");
    let out =
        run_capture("sh", &["-c", "dsh --profile headless \"ping\" 2>&1"], 120).unwrap_or_default();
    if out.contains("Error:") && !out.contains("MISSING_CREDENTIAL") {
        return Err(format!(
            "{log}[8/8] 启动验证失败，请重跑 `spec agent install dsh --yes`\n{out}"
        ));
    }

    let final_version = current_version(home, dsh_tool());
    log.push_str("== DeepSeek Harness 完成：");
    log.push_str(&version_transition_text(
        old_version.as_deref(),
        final_version.as_deref(),
    ));
    log.push_str(&format!(
        " ==\n命令：{}\n路径：{}\nWeb UI: dsh web  →  终端问答: dsh --profile headless \"问题\"\n",
        "dsh",
        command_path("dsh").unwrap_or_else(|| "未知".to_string()),
    ));
    Ok(log)
}

fn dsh_tool() -> AgentTool {
    tool_by_key("dsh").expect("dsh in AGENT_TOOLS")
}

fn profiles_node_modules(home: &Path) -> PathBuf {
    home.join(".dsh/profiles/node_modules/@deepseek-ai")
}

fn patch_permission_presets(home: &Path) -> Result<(), String> {
    let path = profiles_node_modules(home).join("dsh-permission-presets/lib/index.js");
    let content = fs::read_to_string(&path).map_err(|e| format!("permission-presets 缺失：{e}"))?;
    if content.contains("sandboxMode === false") {
        return Ok(());
    }
    let fixed = content.replace("sandboxMode === void 0", "sandboxMode === false");
    if fixed == content {
        return Err("permission-presets 标记未找到（dsh 内部变更？）".to_string());
    }
    atomic_write(&path, fixed.as_bytes()).map_err(|e| e.to_string())
}

fn patch_session_persistence(home: &Path) -> Result<(), String> {
    let path = profiles_node_modules(home).join("dsh-session-persistence-jsonl/lib/index.js");
    let content =
        fs::read_to_string(&path).map_err(|e| format!("session-persistence 缺失：{e}"))?;
    if content.contains("error.code === \"EACCES\"") {
        return Ok(());
    }
    let imp_old = "import { link, mkdir, mkdtemp, open, readFile, readdir, realpath, rm, stat, truncate } from \"node:fs/promises\";";
    let imp_new = "import { link, mkdir, mkdtemp, open, readFile, readdir, realpath, rename, rm, stat, truncate } from \"node:fs/promises\";";
    let link_old = "await link(tmp, finalPath);\n\t\t\tlinked = true;";
    let link_new = "try {\n\t\t\t\tawait link(tmp, finalPath);\n\t\t\t\tlinked = true;\n\t\t\t} catch (error) {\n\t\t\t\tif (error.code === \"EACCES\" || error.code === \"EPERM\" || error.code === \"ENOSYS\") {\n\t\t\t\t\tawait rename(tmp, finalPath);\n\t\t\t\t\tlinked = true;\n\t\t\t\t} else throw error;\n\t\t\t}";
    if !content.contains(imp_old) || !content.contains(link_old) {
        return Err("session-persistence 标记未找到（dsh 内部变更？）".to_string());
    }
    let fixed = content
        .replace(imp_old, imp_new)
        .replace(link_old, link_new);
    atomic_write(&path, fixed.as_bytes()).map_err(|e| e.to_string())
}

fn patch_node_gyp() -> Result<(), String> {
    let path = prefix().join("lib/node_modules/npm/node_modules/node-gyp/gyp/pylib/gyp/input.py");
    let content = fs::read_to_string(&path).map_err(|e| format!("node-gyp input.py 缺失：{e}"))?;
    if content.contains("variables[\"OS\"] = \"linux\"") {
        return Ok(());
    }
    let marker = "        if eval(ast_code, env, variables):";
    if !content.contains(marker) {
        return Err("node-gyp eval 标记未找到（npm 版本变更？）".to_string());
    }
    let fixed = content.replace(
        marker,
        "        if sys.platform == \"android\":\n            # Termux/bionic: no NDK, gyp's android branch only yields\n            # \"Undefined variable android_ndk_path\". Treat as linux.\n            variables = dict(variables)\n            variables[\"OS\"] = \"linux\"\n        if eval(ast_code, env, variables):",
    );
    atomic_write(&path, fixed.as_bytes()).map_err(|e| e.to_string())
}

fn rebuild_node_pty() -> Result<(), String> {
    let dir = prefix().join("lib/node_modules/@deepseek-ai/dsh/node_modules/node-pty");
    if dir.join("build/Release/pty.node").exists() {
        return Ok(());
    }
    let gyp = prefix().join("lib/node_modules/npm/node_modules/node-gyp/bin/node-gyp.js");
    let status = Command::new("node")
        .arg(&gyp)
        .arg("rebuild")
        .current_dir(&dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("node-pty rebuild 启动失败：{e}"))?;
    if !status.success() {
        return Err(
            "node-pty rebuild 失败（需要 clang/make：pkg install -y clang make）".to_string(),
        );
    }
    Ok(())
}

fn patch_attachment_local(home: &Path) -> Result<(), String> {
    let path = profiles_node_modules(home).join("dsh-attachment-local/lib/index.js");
    let content = fs::read_to_string(&path).map_err(|e| format!("attachment-local 缺失：{e}"))?;
    if content.contains("loadSharp") {
        return Ok(());
    }
    let imp_old = "import sharp from \"sharp\";\n";
    if !content.contains(imp_old) {
        return Err("attachment-local sharp 标记未找到（dsh 内部变更？）".to_string());
    }
    let lazy = "// sharp has no android-arm64 prebuild (Termux/bionic). Load lazily so the\n// plugin and its attachments service stay usable; only image upload paths fail.\nlet sharpModule = null;\nasync function loadSharp() {\n\tif (sharpModule) return sharpModule;\n\ttry {\n\t\tsharpModule = (await import(\"sharp\")).default;\n\t} catch (error) {\n\t\tthrow new AttachmentError(\"sharp is unavailable on this platform.\", \"UNSUPPORTED_PLATFORM\", { cause: error });\n\t}\n\treturn sharpModule;\n}\n";
    let anchor = "//#region lib/types/image.js";
    let probe_old = "\t\treturn await imageMetadata(sharp(data, {\n\t\t\tfailOn: \"error\",\n\t\t\tlimitInputPixels: false\n\t\t}));";
    let probe_new = "\t\treturn await imageMetadata((await loadSharp())(data, {\n\t\t\tfailOn: \"error\",\n\t\t\tlimitInputPixels: false\n\t\t}));";
    let detect_old = "\t\tconst image = sharp(data, {\n\t\t\tfailOn: \"error\",\n\t\t\tlimitInputPixels: false\n\t\t});";
    let detect_new = "\t\tconst image = (await loadSharp())(data, {\n\t\t\tfailOn: \"error\",\n\t\t\tlimitInputPixels: false\n\t\t});";
    if !content.contains(anchor) || !content.contains(probe_old) || !content.contains(detect_old) {
        return Err("attachment-local 结构标记未找到（dsh 内部变更？）".to_string());
    }
    let fixed = content
        .replace(imp_old, "")
        .replace(anchor, &format!("{lazy}{anchor}"))
        .replace(probe_old, probe_new)
        .replace(detect_old, detect_new);
    atomic_write(&path, fixed.as_bytes()).map_err(|e| e.to_string())
}

/// Termux fix: dsh-host-apiproxy's native path opener only has darwin/win32/linux
/// branches, so on Android (`process.platform === "android"`) "Open configuration
/// file"/"Open path" requests throw "native path opener is unsupported on
/// android". Add an android branch that hands the path to `termux-open`, and let
/// canOpenNativePath report Android as openable so surfaces keep offering the button.
fn patch_apiproxy_termux_open() -> Result<(), String> {
    let path = prefix().join("lib/node_modules/@deepseek-ai/dsh/node_modules/@deepseek-ai/dsh-host-apiproxy/lib/index.js");
    if !path.exists() {
        return Err("dsh-host-apiproxy 缺失（dsh 未安装？）".to_string());
    }
    let content = fs::read_to_string(&path).map_err(|e| format!("apiproxy 读取失败：{e}"))?;
    if content.contains("termux-open") {
        return Ok(());
    }
    // 锚点使用 tab 缩进，与打包产物逐字节一致。
    let opener_old = "\tif (platform === \"linux\") {\n\t\tif (wsl) {\n\t\t\tawait openWslPath(path, signal, run);\n\t\t\treturn;\n\t\t}\n\t\tawait run(\"xdg-open\", [path], signal);\n\t\treturn;\n\t}\n\tthrow new Error(`native path opener is unsupported on ${platform}`);";
    let opener_new = "\tif (platform === \"linux\") {\n\t\tif (wsl) {\n\t\t\tawait openWslPath(path, signal, run);\n\t\t\treturn;\n\t\t}\n\t\tawait run(\"xdg-open\", [path], signal);\n\t\treturn;\n\t}\n\tif (platform === \"android\") {\n\t\tawait run(\"termux-open\", [path], signal);\n\t\treturn;\n\t}\n\tthrow new Error(`native path opener is unsupported on ${platform}`);";
    let can_old = "\tconst platform = internals.platform ?? process.platform;\n\tif (platform === \"darwin\" || platform === \"win32\") return true;\n\tif (platform !== \"linux\") return false;";
    let can_new = "\tconst platform = internals.platform ?? process.platform;\n\tif (platform === \"darwin\" || platform === \"win32\") return true;\n\tif (platform === \"android\") return true;\n\tif (platform !== \"linux\") return false;";
    if !content.contains(opener_old) || !content.contains(can_old) {
        return Err("apiproxy termux-open 标记未找到（dsh 内部变更？）".to_string());
    }
    let fixed = content
        .replace(opener_old, opener_new)
        .replace(can_old, can_new);
    atomic_write(&path, fixed.as_bytes()).map_err(|e| e.to_string())
}

fn patch_subprocess_local() -> Result<(), String> {
    let path = prefix().join("lib/node_modules/@deepseek-ai/dsh/node_modules/@deepseek-ai/dsh-subprocess-local/lib/index.js");
    if !path.exists() {
        return Err("subprocess-local 缺失（dsh 未安装？）".to_string());
    }
    let content =
        fs::read_to_string(&path).map_err(|e| format!("subprocess-local 读取失败：{e}"))?;
    if content.contains("android) return new LinuxProcessInspector") {
        return Ok(());
    }
    let old = "if (platform === \"linux\") return new LinuxProcessInspector(arch, internals);";
    if !content.contains(old) {
        return Err("subprocess-local 标记未找到（dsh 内部变更？）".to_string());
    }
    let fixed = content.replace(
        old,
        "if (platform === \"linux\" || platform === \"android\") return new LinuxProcessInspector(arch, internals);",
    );
    atomic_write(&path, fixed.as_bytes()).map_err(|e| e.to_string())
}

fn patch_fs_search() -> Result<(), String> {
    let path = prefix().join("lib/node_modules/@deepseek-ai/dsh/node_modules/@deepseek-ai/dsh-tool-fs-search/lib/index.js");
    if !path.exists() {
        return Err("dsh-tool-fs-search 缺失（dsh 未安装？）".to_string());
    }
    let content =
        fs::read_to_string(&path).map_err(|e| format!("dsh-tool-fs-search 读取失败：{e}"))?;
    if content.contains("android-arm64") {
        return Ok(());
    }
    let old = "function resolveRgPath() {\n\trgPathPromise ??= import(\"@vscode/ripgrep\").then((module) => module.rgPath);\n\treturn rgPathPromise;\n}";
    if !content.contains(old) {
        return Err("resolveRgPath 标记未找到（dsh 内部变更？）".to_string());
    }
    let neu = "function resolveRgPath() {\n\trgPathPromise ??= import(\"@vscode/ripgrep\").then((module) => module.rgPath).catch(async () => {\n\t\t// Termux/Android compatibility patch: the packaged @vscode/ripgrep-<platform>-<arch>\n\t\t// optional dependency is not shipped for android-arm64. Fall back to the system\n\t\t// ripgrep binary when the packaged one cannot be resolved.\n\t\tconst { execFileSync } = await import(\"node:child_process\");\n\t\texecFileSync(\"rg\", [\"--version\"], { stdio: \"ignore\" });\n\t\treturn \"rg\";\n\t});\n\treturn rgPathPromise;\n}";
    let fixed = content.replace(old, neu);
    atomic_write(&path, fixed.as_bytes()).map_err(|e| e.to_string())
}

fn patch_playwright_ld_preload(home: &Path) -> Result<(), String> {
    let pw_mcp = home.join(".local/bin/pw-mcp").display().to_string();
    for profile in ["web", "headless", "tui", "dsh-tui"] {
        let path = home.join(format!(".dsh/profiles/{profile}/cordis.patch.yml"));
        if !path.exists() {
            continue;
        }
        let content = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        if content.contains("LD_PRELOAD") || !content.contains("pw-mcp") {
            continue;
        }
        let old = format!("        command: {pw_mcp}\n        args:");
        if !content.contains(&old) {
            return Err(format!("{profile} pw-mcp 块结构未匹配（配置结构变更？）"));
        }
        let neu = format!("        command: {pw_mcp}\n        # Termux fix: the glibc chromium build breaks under termux-exec's bionic\n        # LD_PRELOAD (libtermux-exec-ld-preload.so). Strip it for this server.\n        env:\n          LD_PRELOAD: ''\n        args:");
        atomic_write(&path, content.replace(&old, &neu).as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Termux fix: a foreground `dsh web` dies with its terminal (SIGHUP when the
/// tty closes). Install a ~/.bashrc wrapper so `dsh web` always launches as a
/// detached (setsid) background instance; other invocation forms pass through.
fn patch_dsh_web_wrapper(home: &Path) -> Result<(), String> {
    let bashrc = home.join(".bashrc");
    let marker = "# dsh web: 自动脱离终端后台启动";
    if bashrc.exists()
        && fs::read_to_string(&bashrc)
            .map_err(|e| format!(".bashrc 读取失败：{e}"))?
            .contains(marker)
    {
        return Ok(());
    }
    let wrapper = format!(
        "\n{marker}\ndsh() {{\n  if [ \"$1\" = \"web\" ]; then\n    if pgrep -f '[d]sh web' > /dev/null 2>&1; then\n      echo \"[dsh] web 已在运行: $(pgrep -f '[d]sh web' | head -1)\"\n      curl -s -o /dev/null -w '[dsh] http=%{{http_code}}\\n' -m 2 http://127.0.0.1:3080/ 2>/dev/null || echo '[dsh] 3080 未响应'\n      return 0\n    fi\n    setsid nohup /data/data/com.termux/files/usr/bin/dsh web > ~/dsh-web-3080.log 2>&1 < /dev/null &\n    local pid=$!\n    echo \"[dsh] web 启动中 (PID $pid, 日志 ~/dsh-web-3080.log)\"\n    for i in $(seq 1 30); do\n      curl -s -o /dev/null -m 1 http://127.0.0.1:3080/ 2>/dev/null && {{ echo \"[dsh] 就绪: http://127.0.0.1:3080\"; return 0; }}\n      sleep 1\n    done\n    echo \"[dsh] 30s 未就绪，看日志: tail ~/dsh-web-3080.log\"\n    return 1\n  fi\n  command dsh \"$@\"\n}}\n"
    );
    let mut content = if bashrc.exists() {
        fs::read_to_string(&bashrc).map_err(|e| format!(".bashrc 读取失败：{e}"))?
    } else {
        String::new()
    };
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&wrapper);
    atomic_write(&bashrc, content.as_bytes()).map_err(|e| format!(".bashrc 写入失败：{e}"))
}

fn ensure_profile_patches(home: &Path) -> Result<(), String> {
    for profile in ["web", "headless"] {
        let dir = home.join(format!(".dsh/profiles/{profile}"));
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("cordis.patch.yml");
        let mut content = if path.exists() {
            fs::read_to_string(&path).map_err(|e| e.to_string())?
        } else {
            String::new()
        };
        // drop stray bare `[]` documents from the bootstrap template
        content = content
            .lines()
            .filter(|line| line.trim() != "[]")
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        if !content.contains("dsh-bash-local") {
            content.push_str(
                "\n# Termux (bionic) fix: dsh-sandbox-local requires the koffi native FFI\n# module, which has no bionic prebuild. Swap in the unsandboxed local bash\n# executor (dsh-bash-local) so the profile boots without native code.\n# Commands run without OS-level sandbox (no bwrap on Termux anyway).\n- id: sandbox\n  name: '@deepseek-ai/dsh-sandbox-local'\n  disabled: true\n\n- id: bash-sandbox\n  name: '@deepseek-ai/dsh-bash-sandbox'\n  disabled: true\n\n- insert:\n    - id: bash-local\n      name: '@deepseek-ai/dsh-bash-local'\n      config:\n        timeoutMs: 60000\n",
            );
        }
        if !content.contains("defaultPreset: workspace-write") {
            content.push_str(
                "\n- id: permission\n  name: '@deepseek-ai/dsh-permission-presets'\n  config:\n    defaultPreset: workspace-write\n    presets:\n      read-only:\n        sandbox: read-only\n        approval: ask\n      workspace-write:\n        sandbox: workspace-write\n        approval: ask\n      danger-full-access:\n        sandbox: danger-full-access\n        approval: never\n",
            );
        }
        atomic_write(&path, content.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn npm_package_present(package: &str) -> bool {
    prefix().join("lib/node_modules").join(package).exists()
}

fn install_opencode(home: &Path) -> Result<String, String> {
    ensure_termux_packages(&[
        "nodejs",
        "python",
        "tar",
        "gzip",
        "coreutils",
        "glibc-repo",
        "glibc",
        "glibc-runner",
    ])?;
    check_disk_space(home, 700 * 1024 * 1024)?;
    let version = latest_version(tool_by_key("opencode").unwrap())?;
    let data_dir = home.join(".local/share/xucodex");
    let loader_dir = data_dir.join("opencode-loader");
    install_opencode_loader_assets(&loader_dir)?;
    let build_py = loader_dir.join("build.py");
    let wrapper = loader_dir.join("wrapper");
    let shim = loader_dir.join("bunfs_shim.so");

    let tmp_root = temp_build_dir("xucodex-opencode")?;
    let result = (|| {
        run_in_dir(
            "npm",
            &["pack", &format!("opencode-linux-arm64@{version}")],
            &tmp_root,
            300,
        )?;
        let tgz = fs::read_dir(&tmp_root)
            .map_err(|e| format!("read {}: {e}", tmp_root.display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with("opencode-linux-arm64-") && name.ends_with(".tgz"))
                    .unwrap_or(false)
            })
            .ok_or_else(|| "npm pack 未生成 opencode-linux-arm64 包".to_string())?;
        run_in_dir("tar", &["-xzf", &tgz.display().to_string()], &tmp_root, 120)?;
        let raw_bin = tmp_root.join("package/bin/opencode");
        if !raw_bin.exists() {
            return Err("opencode-linux-arm64 包缺少 bin/opencode".to_string());
        }
        let wrapped = tmp_root.join("opencode-xu-termux");
        let python = if command_path("python3").is_some() {
            "python3"
        } else {
            "python"
        };
        run_in_dir(
            python,
            &[
                &build_py.display().to_string(),
                &raw_bin.display().to_string(),
                &wrapped.display().to_string(),
                "--wrapper",
                &wrapper.display().to_string(),
                "--shim",
                &shim.display().to_string(),
            ],
            &tmp_root,
            600,
        )?;
        if !wrapped.exists() {
            return Err("OpenCode Termux 包装构建未生成可执行文件".to_string());
        }
        let bin_dir = data_dir.join("opencode/bin");
        fs::create_dir_all(&bin_dir).map_err(|e| format!("create {}: {e}", bin_dir.display()))?;
        let target = bin_dir.join("opencode");
        let next = bin_dir.join("opencode.new");
        let rollback = bin_dir.join("opencode.before-xu-update");
        if target.exists() {
            fs::copy(&target, &rollback)
                .map_err(|e| format!("backup {}: {e}", target.display()))?;
            fs::set_permissions(&rollback, fs::Permissions::from_mode(0o755)).ok();
        }
        fs::copy(&wrapped, &next).map_err(|e| format!("copy {}: {e}", next.display()))?;
        fs::set_permissions(&next, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", next.display()))?;
        fs::rename(&next, &target)
            .or_else(|_| {
                fs::copy(&next, &target)?;
                fs::remove_file(&next).ok();
                Ok::<_, std::io::Error>(())
            })
            .map_err(|e| format!("install {}: {e}", target.display()))?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", target.display()))?;
        let verify = run_capture_path(&target, &["--version"], 30)
            .ok()
            .and_then(|output| parse_version(&output));
        let Some(final_version) = verify else {
            restore_opencode_binary(&target, &rollback);
            return Err("OpenCode installed but the managed binary failed --version".to_string());
        };
        let metadata = data_dir.join("opencode/metadata.json");
        if let Err(error) = install_opencode_launcher(home, &target) {
            restore_opencode_binary(&target, &rollback);
            return Err(error);
        }
        let metadata_text = format!(
            "{{\n  \"version\": \"{version}\",\n  \"package\": \"opencode-linux-arm64\",\n  \"source\": \"official-npm-platform-package\",\n  \"binary\": \"{}\"\n}}\n",
            target.display()
        );
        if let Err(error) = atomic_write(&metadata, metadata_text.as_bytes()) {
            restore_opencode_binary(&target, &rollback);
            return Err(format!("write {}: {error}", metadata.display()));
        }
        fs::remove_file(&rollback).ok();
        Ok(format!("OpenCode ready: {final_version}"))
    })();
    fs::remove_dir_all(&tmp_root).ok();
    result
}

fn restore_opencode_binary(target: &Path, rollback: &Path) {
    if rollback.exists() {
        let _ = fs::copy(rollback, target);
        let _ = fs::set_permissions(target, fs::Permissions::from_mode(0o755));
        let _ = fs::remove_file(rollback);
    } else {
        let _ = fs::remove_file(target);
    }
}

fn install_opencode_loader_assets(loader_dir: &Path) -> Result<(), String> {
    if std::env::consts::ARCH != "aarch64" {
        return Err(format!(
            "OpenCode Termux loader only supports aarch64; detected {}",
            std::env::consts::ARCH
        ));
    }
    fs::create_dir_all(loader_dir).map_err(|e| format!("create {}: {e}", loader_dir.display()))?;
    for (name, data) in [
        ("build.py", OPENCODE_BUILD_PY),
        ("wrapper", OPENCODE_WRAPPER),
        ("bunfs_shim.so", OPENCODE_SHIM),
    ] {
        let path = loader_dir.join(name);
        fs::write(&path, data).map_err(|e| format!("write {}: {e}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }
    Ok(())
}

fn install_opencode_launcher(home: &Path, target: &Path) -> Result<(), String> {
    let script = format!(
        "#!/bin/sh\n# Xu managed OpenCode launcher.\nexec \"{}\" \"$@\"\n",
        target.display()
    );
    install_launchers(home, "opencode", &script)
}

fn install_claude_launcher(home: &Path) -> Result<(), String> {
    let binary = prefix().join("lib/node_modules/@anthropic-ai/claude-code-linux-arm64/claude");
    if !binary.exists() || command_path("grun").is_none() {
        return Err("Claude platform binary or grun is missing after install".to_string());
    }
    let script = format!(
        "#!/bin/sh\n# Xu managed Claude Code launcher.\nexec grun \"{}\" \"$@\"\n",
        binary.display()
    );
    install_launchers(home, "claude", &script)
}

fn install_launchers(home: &Path, command: &str, script: &str) -> Result<(), String> {
    for dir in [home.join("bin"), prefix().join("bin")] {
        fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let path = dir.join(command);
        if is_legacy_xu_launcher(&path) {
            continue;
        }
        if path.exists() && !is_xu_launcher(&path) {
            let backup = dir.join(format!("{command}.before-xu"));
            if !backup.exists() {
                fs::rename(&path, &backup)
                    .map_err(|e| format!("backup {}: {e}", path.display()))?;
            } else {
                fs::remove_file(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
            }
        }
        fs::write(&path, script).map_err(|e| format!("write {}: {e}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }
    Ok(())
}

fn is_legacy_xu_launcher(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|text| text.contains("XuCodex managed"))
        .unwrap_or(false)
}

fn is_xu_launcher(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|text| text.contains("Xu managed") || text.contains("XuCodex managed"))
        .unwrap_or(false)
}

fn ensure_npm_platform_binary(tool: AgentTool) -> Result<bool, String> {
    let Some(package) = tool.npm_package else {
        return Ok(false);
    };
    let prefix = run_capture("npm", &["prefix", "-g"], 10)?;
    let node_modules = Path::new(prefix.trim()).join("lib/node_modules");
    let main_pkg = node_modules.join(package);
    let package_json = main_pkg.join("package.json");
    let text = fs::read_to_string(&package_json)
        .map_err(|e| format!("读取 {}: {e}", package_json.display()))?;
    let parsed: NpmPackageJson = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let npm_arch = npm_arch();
    let platform = parsed
        .optional_dependencies
        .iter()
        .find(|(name, _)| name.contains(&format!("linux-{npm_arch}")) && !name.contains("android"))
        .or_else(|| {
            parsed
                .optional_dependencies
                .iter()
                .find(|(name, _)| name.contains(&format!("linux-{npm_arch}")))
        });
    let Some((platform_name, platform_version)) = platform else {
        return Ok(false);
    };
    run_inherited(
        "npm",
        &[
            "install",
            "-g",
            &format!("{platform_name}@{platform_version}"),
            "--os=linux",
            "--force",
        ],
        300,
    )?;
    let platform_dirs = [
        node_modules.join(platform_name),
        main_pkg.join("node_modules").join(platform_name),
    ];
    let mut binary = None;
    for dir in platform_dirs.iter().filter(|dir| dir.exists()) {
        for candidate in [dir.join(tool.command), dir.join("bin").join(tool.command)] {
            if candidate.is_file() {
                binary = Some(candidate);
                break;
            }
        }
        if binary.is_some() {
            break;
        }
    }
    let Some(binary) = binary else {
        return Ok(false);
    };
    let target_rel = target_bin_rel(&parsed, tool.command);
    let target = main_pkg.join(target_rel);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    fs::copy(&binary, &target).map_err(|e| format!("copy {}: {e}", target.display()))?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("chmod {}: {e}", target.display()))?;
    Ok(true)
}

fn target_bin_rel(parsed: &NpmPackageJson, command: &str) -> PathBuf {
    match &parsed.bin {
        Some(serde_json::Value::String(value)) => PathBuf::from(value),
        Some(serde_json::Value::Object(map)) => map
            .get(command)
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("bin/{command}.exe"))),
        _ => PathBuf::from(format!("bin/{command}.exe")),
    }
}

fn current_version(home: &Path, tool: AgentTool) -> Option<String> {
    if tool.id == AgentToolId::OpenCode {
        let managed_bin = home.join(".local/share/xucodex/opencode/bin/opencode");
        if managed_bin.exists() {
            if let Ok(out) = run_capture_path(&managed_bin, &["--version"], 20) {
                if let Some(version) = parse_version(&out) {
                    return Some(version);
                }
            }
        }
    }
    if tool.id == AgentToolId::Claude {
        let glibc_claude =
            prefix().join("lib/node_modules/@anthropic-ai/claude-code-linux-arm64/claude");
        if command_path("grun").is_some() && glibc_claude.exists() {
            if let Ok(out) = run_capture(
                "grun",
                &[&glibc_claude.display().to_string(), "--version"],
                20,
            ) {
                if let Some(version) = parse_version(&out) {
                    return Some(version);
                }
            }
        }
    }
    if tool.id == AgentToolId::Codex {
        let codex_js = prefix().join("lib/node_modules/@openai/codex/bin/codex.js");
        if codex_js.exists() {
            if let Ok(out) =
                run_capture("node", &[&codex_js.display().to_string(), "--version"], 15)
            {
                if let Some(version) = parse_version(&out) {
                    return Some(version);
                }
            }
        }
    }
    run_capture(
        "sh",
        &["-c", &format!("{} --version 2>&1", tool.command)],
        15,
    )
    .ok()
    .and_then(|out| parse_version(&out))
}

fn latest_version(tool: AgentTool) -> Result<String, String> {
    let package = match tool.kind {
        AgentToolKind::Npm => tool.npm_package.unwrap_or(tool.command),
        AgentToolKind::OpenCode => "opencode-linux-arm64",
        AgentToolKind::Dsh => "@deepseek-ai/dsh",
    };
    let out = run_capture("npm", &["view", package, "version"], 30)?;
    parse_version(&out).ok_or_else(|| format!("无法解析最新版本：{package}"))
}

fn ensure_termux_packages(packages: &[&str]) -> Result<(), String> {
    let mut missing = Vec::new();
    for package in packages {
        if package_installed(package) {
            continue;
        }
        missing.push(*package);
    }
    if missing.is_empty() {
        return Ok(());
    }
    if std::env::var_os("PREFIX").is_none() || command_path("pkg").is_none() {
        return Err(format!("missing dependencies: {}", missing.join(", ")));
    }
    if (missing.contains(&"glibc-repo")
        || missing.contains(&"glibc")
        || missing.contains(&"glibc-runner"))
        && !package_installed("glibc-repo")
    {
        run_inherited("pkg", &["install", "-y", "glibc-repo"], 300)?;
    }
    missing.retain(|package| *package != "glibc-repo");
    if !missing.is_empty() {
        let mut args = vec!["install", "-y"];
        args.extend(missing);
        run_inherited("pkg", &args, 600)?;
    }
    Ok(())
}

fn package_installed(package: &str) -> bool {
    match package {
        "glibc" => prefix().join("glibc/lib/ld-linux-aarch64.so.1").exists(),
        "glibc-runner" => command_path("glibc-runner").is_some() || command_path("grun").is_some(),
        "python" => command_path("python3").is_some() || command_path("python").is_some(),
        "nodejs" => command_path("node").is_some(),
        "tar" => command_path("tar").is_some(),
        "gzip" => command_path("gzip").is_some(),
        "coreutils" => command_path("sha256sum").is_some(),
        other => {
            run_capture("dpkg", &["-s", other], 5)
                .map(|out| out.contains("Status: install ok installed"))
                .unwrap_or(false)
                || command_path(other).is_some()
        }
    }
}

fn check_disk_space(path: &Path, min_bytes: u64) -> Result<(), String> {
    let out = run_capture("df", &["-k", &path.display().to_string()], 5)?;
    let Some(line) = out.lines().last() else {
        return Ok(());
    };
    let parts: Vec<&str> = line.split_whitespace().collect();
    let Some(available_kb) = parts.get(3).and_then(|value| value.parse::<u64>().ok()) else {
        return Ok(());
    };
    let available = available_kb * 1024;
    if available < min_bytes {
        return Err(format!(
            "not enough disk space: available {} MB, need {} MB",
            available / 1024 / 1024,
            min_bytes / 1024 / 1024
        ));
    }
    Ok(())
}

fn rollback_npm(tool: AgentTool, old_version: Option<&str>) {
    let (Some(package), Some(version)) = (tool.npm_package, old_version) else {
        return;
    };
    let _ = run_inherited(
        "npm",
        &[
            "install",
            "-g",
            &format!("{package}@{version}"),
            "--os=linux",
        ],
        300,
    );
}

fn command_path(command: &str) -> Option<String> {
    run_capture(
        "sh",
        &["-c", &format!("command -v {command} 2>/dev/null")],
        5,
    )
    .ok()
    .map(|out| out.trim().to_string())
    .filter(|out| !out.is_empty())
}

fn run_capture(command: &str, args: &[&str], timeout_seconds: u64) -> Result<String, String> {
    let output = Command::new("timeout")
        .arg(format!("{timeout_seconds}s"))
        .arg(command)
        .args(args)
        .output()
        .map_err(|e| format!("run {command}: {e}"))?;
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(text)
    } else {
        Err(text.trim().to_string())
    }
}

fn run_capture_path(command: &Path, args: &[&str], timeout_seconds: u64) -> Result<String, String> {
    let output = Command::new("timeout")
        .arg(format!("{timeout_seconds}s"))
        .arg(command)
        .args(args)
        .output()
        .map_err(|e| format!("run {}: {e}", command.display()))?;
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(text)
    } else {
        Err(text.trim().to_string())
    }
}

fn run_inherited(command: &str, args: &[&str], timeout_seconds: u64) -> Result<(), String> {
    let status = Command::new("timeout")
        .arg(format!("{timeout_seconds}s"))
        .arg(command)
        .args(args)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("run {command}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{command} exited with {status}"))
    }
}

fn run_in_dir(
    command: &str,
    args: &[&str],
    dir: &Path,
    timeout_seconds: u64,
) -> Result<(), String> {
    let status = Command::new("timeout")
        .arg(format!("{timeout_seconds}s"))
        .arg(command)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("run {command}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{command} exited with {status}"))
    }
}

fn parse_version(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"\d+\.\d+\.\d+(?:-[A-Za-z0-9.]+)?").ok()?;
    re.find(text).map(|m| m.as_str().to_string())
}

pub fn compare_versions(a: &str, b: &str) -> i32 {
    let parse = |value: &str| -> [i32; 3] {
        let mut nums = [0, 0, 0];
        for (index, part) in value.split(['.', '-']).take(3).enumerate() {
            nums[index] = part.parse().unwrap_or(0);
        }
        nums
    };
    let left = parse(a);
    let right = parse(b);
    for index in 0..3 {
        if left[index] != right[index] {
            return left[index] - right[index];
        }
    }
    0
}

fn npm_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    }
}

fn prefix() -> PathBuf {
    std::env::var_os("PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/data/data/com.termux/files/usr"))
}

fn temp_build_dir(prefix: &str) -> Result<PathBuf, String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis();
    let dir = std::env::temp_dir().join(format!("{prefix}-{stamp}-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir)
}

struct InstallLock {
    path: PathBuf,
}

impl InstallLock {
    fn acquire(path: &Path) -> Result<Self, String> {
        if path.exists() {
            let active = fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
                .is_some_and(process_is_running);
            if active {
                return Err(format!(
                    "another spec install may be running: {}",
                    path.display()
                ));
            }
            fs::remove_file(path).map_err(|e| format!("remove stale {}: {e}", path.display()))?;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        fs::write(path, std::process::id().to_string())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).ok();
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

fn process_is_running(pid: u32) -> bool {
    // 截断守卫：pid_t 是 i32，>i32::MAX 的值会截成负数——
    //   -1 => kill(-1,0) 广播全部进程恒成功；0 => 探测自身进程组恒成功。
    // 两类损坏锁文件内容都会让 stale 清理永久跳过，越界一律按 stale 处理。
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // 用 libc::kill 而非外部 `kill -0`：procps 实现对超界 pid 的行为不一致。
    // 语义：0=存在；EPERM=存在但属他人；ESRCH/其它=不存在。
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::from_raw_os_error(rc).kind() == std::io::ErrorKind::PermissionDenied
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        fs::remove_file(&self.path).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_embedded_opencode_loader_assets() {
        if std::env::consts::ARCH != "aarch64" {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let loader = dir.path().join("loader");

        install_opencode_loader_assets(&loader).unwrap();

        assert_eq!(
            fs::read(loader.join("build.py")).unwrap(),
            OPENCODE_BUILD_PY
        );
        assert_eq!(fs::read(loader.join("wrapper")).unwrap(), OPENCODE_WRAPPER);
        assert_eq!(
            fs::read(loader.join("bunfs_shim.so")).unwrap(),
            OPENCODE_SHIM
        );
    }

    #[test]
    fn removes_stale_install_lock() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("xu-install.lock");
        // 非数字内容解析失败 => 视为 stale（不依赖外部 kill 的平台差异；
        // 曾用的 4294967295 会经 pid_t 截断成 -1 => kill(-1) 广播恒成功）。
        fs::write(&lock, "not-a-pid").unwrap();

        let guard = InstallLock::acquire(&lock).unwrap();

        assert!(lock.exists());
        drop(guard);
        assert!(!lock.exists());
    }

    #[test]
    fn truncated_pid_contents_are_treated_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        for content in ["4294967295", "0"] {
            let lock = dir.path().join("xu-install.lock");
            fs::write(&lock, content).unwrap();
            let guard = InstallLock::acquire(&lock)
                .unwrap_or_else(|e| panic!("content {content} must be stale: {e}"));
            drop(guard);
            assert!(!lock.exists());
        }
    }

    #[test]
    fn install_lock_rejects_when_owner_pid_is_alive() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("xu-install.lock");
        // 写入当前进程 pid（必然存活）=> 必须拒绝二次获取。
        fs::write(&lock, std::process::id().to_string()).unwrap();

        let Err(error) = InstallLock::acquire(&lock) else {
            panic!("alive-owner lock must be rejected");
        };
        assert!(
            error.contains("another spec install may be running"),
            "{error}"
        );
    }

    #[test]
    fn dsh_web_wrapper_appends_to_bashrc() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        fs::write(home.join(".bashrc"), "alias cm='cm-up'\n").unwrap();

        patch_dsh_web_wrapper(home).unwrap();

        let content = fs::read_to_string(home.join(".bashrc")).unwrap();
        assert!(content.contains("自动脱离终端后台启动"), "标记应写入");
        // 包装器用绝对路径 setsid 启动（路径随 PREFIX 变化），断言稳定子串。
        assert!(
            content.contains("setsid nohup") && content.contains("dsh web"),
            "包装函数应包含 setsid 脱离逻辑"
        );
        assert!(content.contains("command dsh \"$@\""), "其他参数应透传");
    }

    #[test]
    fn dsh_web_wrapper_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let marker = "# dsh web: 自动脱离终端后台启动\n已经打过补丁";
        fs::write(home.join(".bashrc"), marker).unwrap();

        patch_dsh_web_wrapper(home).unwrap();

        let content = fs::read_to_string(home.join(".bashrc")).unwrap();
        assert_eq!(content, marker, "标记存在时应原样保留");
        assert_eq!(
            content.matches("自动脱离终端后台启动").count(),
            1,
            "不应重复追加"
        );
    }

    /// 构造一个带 prefix() 布局的最小 apiproxy 文件，验证补丁可应用且幂等。
    /// patch_apiproxy_termux_open 直接读 prefix() 布局，测试通过临时替换
    /// TERMUX_PREFIX 不可行（编译期常量），因此这里直接测核心替换逻辑的
    /// 锚点字符串与真实文件逐字节一致。
    #[test]
    fn apiproxy_termux_open_anchors_match_real_file() {
        // 真实文件若已安装，锚点必须能命中（已打补丁则跳过）。
        let path = prefix().join("lib/node_modules/@deepseek-ai/dsh/node_modules/@deepseek-ai/dsh-host-apiproxy/lib/index.js");
        if !path.exists() {
            return;
        }
        let content = fs::read_to_string(&path).unwrap();
        if content.contains("termux-open") {
            return; // 已打补丁
        }
        let opener_old = "\tif (platform === \"linux\") {\n\t\tif (wsl) {\n\t\t\tawait openWslPath(path, signal, run);\n\t\t\treturn;\n\t\t}\n\t\tawait run(\"xdg-open\", [path], signal);\n\t\treturn;\n\t}\n\tthrow new Error(`native path opener is unsupported on ${platform}`);";
        let can_old = "\tconst platform = internals.platform ?? process.platform;\n\tif (platform === \"darwin\" || platform === \"win32\") return true;\n\tif (platform !== \"linux\") return false;";
        assert!(
            content.contains(opener_old) && content.contains(can_old),
            "未打补丁的真实文件必须命中锚点"
        );
    }

    #[test]
    fn apiproxy_termux_open_replacement_produces_android_branch() {
        let opener_old = "\tif (platform === \"linux\") {\n\t\tif (wsl) {\n\t\t\tawait openWslPath(path, signal, run);\n\t\t\treturn;\n\t\t}\n\t\tawait run(\"xdg-open\", [path], signal);\n\t\treturn;\n\t}\n\tthrow new Error(`native path opener is unsupported on ${platform}`);";
        let opener_new = "\tif (platform === \"linux\") {\n\t\tif (wsl) {\n\t\t\tawait openWslPath(path, signal, run);\n\t\t\treturn;\n\t\t}\n\t\tawait run(\"xdg-open\", [path], signal);\n\t\treturn;\n\t}\n\tif (platform === \"android\") {\n\t\tawait run(\"termux-open\", [path], signal);\n\t\treturn;\n\t}\n\tthrow new Error(`native path opener is unsupported on ${platform}`);";
        let sample = format!("pre\n{opener_old}\npost");
        let fixed = sample.replace(opener_old, opener_new);
        assert!(fixed.contains("if (platform === \"android\")"));
        assert!(fixed.contains("termux-open"));
        assert_eq!(fixed.matches("unsupported on").count(), 1);
        // 幂等：再跑一次不变
        let twice = fixed.replace(opener_old, opener_new);
        assert_eq!(fixed, twice);
    }
}

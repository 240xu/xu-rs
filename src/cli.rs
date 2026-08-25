use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde_json::Value;

use crate::agent_tools::{
    diagnostics as agent_diagnostics, install_or_update_tools, parse_tool_keys, preview_update_plan,
};
use crate::agents::{apply_agent, AgentTarget, ApplyPlan};
use crate::domain::{CacheMode, ProtocolKind};
use crate::mcp::{McpServer, McpStore, McpTargets, McpTransport};
use crate::patch::{
    apply_patch, atomic_write, read_before_checked, ConfigPatch, PatchOptions, PatchResult,
};
use crate::provider_check::{fetch_provider_models, test_provider_connection};
use crate::provider_presets::{
    all as provider_presets, find as find_provider_preset, protocol_key,
};
use crate::providers::{profiles_from_xu_chat_json, read_profiles};
use crate::state::{current_provider, read_state, state_path};

pub fn plan_from_home(
    home: &Path,
    target: AgentTarget,
    providers_override: Option<Vec<String>>,
) -> Result<ApplyPlan, String> {
    let provider_path = home.join(".codex/xu-chat-providers.json");
    let profiles = read_profiles(&provider_path)?;
    let provider_ids = match providers_override {
        Some(ids) if !ids.is_empty() => ids,
        _ => provider_ids_for_target(home, target).or_else(|err| {
            if target == AgentTarget::OpenCode {
                let ids: Vec<String> = profiles
                    .iter()
                    .filter(|p| p.protocol == ProtocolKind::OpenAiChat)
                    .map(|p| p.id.clone())
                    .collect();
                if ids.is_empty() {
                    Err("OpenCode has no OpenAI Chat-compatible providers".to_string())
                } else {
                    Ok(ids)
                }
            } else {
                Err(err)
            }
        })?,
    };
    apply_agent(home, target, &profiles, &provider_ids)
}

pub fn apply_plan(plan: &ApplyPlan, dry_run: bool) -> Result<Vec<PatchResult>, String> {
    let mut results = Vec::new();
    let mut applied = Vec::new();

    for patch in &plan.patches {
        let existed = patch.path.exists();
        match apply_patch(
            patch,
            PatchOptions {
                dry_run,
                backup: true,
            },
        ) {
            Ok(result) => {
                if !dry_run && result.changed {
                    applied.push((patch.path.clone(), existed, patch.before.clone()));
                }
                results.push(result);
            }
            Err(error) => {
                if !dry_run {
                    rollback_applied(applied)?;
                }
                return Err(error.to_string());
            }
        }
    }

    Ok(results)
}

fn rollback_applied(applied: Vec<(std::path::PathBuf, bool, String)>) -> Result<(), String> {
    let mut errors = Vec::new();
    for (path, existed, before) in applied.into_iter().rev() {
        if existed {
            if let Err(error) = atomic_write(&path, before.as_bytes()) {
                errors.push(format!("restore {}: {error}", path.display()));
            }
        } else if let Err(error) = fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                errors.push(format!("remove {}: {error}", path.display()));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("rollback failed: {}", errors.join("; ")))
    }
}

pub fn parse_target(s: &str) -> Option<AgentTarget> {
    match s {
        "opencode" | "open-code" => Some(AgentTarget::OpenCode),
        "claude" | "claude-code" => Some(AgentTarget::ClaudeCode),
        "codex" => Some(AgentTarget::Codex),
        _ => None,
    }
}

pub fn run_command(home: &Path, args: &[String]) -> Option<Result<String, String>> {
    let command = args.first()?.as_str();
    Some(match command {
        "list" | "ls" => list_providers(home),
        "current" => current_command(home, args.get(1).map(String::as_str)),
        "use" | "switch" => use_command(home, &args[1..]),
        "test" => test_command(home, &args[1..]),
        "doctor" => doctor_command(home),
        "agent" | "agents" => agent_command(home, &args[1..]),
        "provider" => provider_command(home, &args[1..]),
        "opencode" => opencode_command(home, &args[1..]),
        "mcp" => mcp_command(home, &args[1..]),
        "skill" | "skills" => skill_command(home, &args[1..]),
        "sync" => sync_command(home, &args[1..]),
        "web" => web_command(home, &args[1..]),
        "config" if args.get(1).map(String::as_str) == Some("path") => config_path_command(home),
        "help" | "--help" | "-h" => Ok(help_text()),
        _ => return None,
    })
}

fn sync_command(home: &Path, args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str) {
        Some("mcp") => sync_mcp_command(home, &args[1..]),
        Some("skills") => sync_skills_command(home, &args[1..]),
        Some("help" | "--help" | "-h") | None => {
            Ok("spec sync mcp <source> <target...> [--dry-run]\n\
             spec sync skills <source> <target...> [--dry-run]\n\
             \x20 source/target: opencode | claude | codex"
                .to_string())
        }
        other => Err(format!("unknown sync command: {}", other.unwrap_or(""))),
    }
}

fn sync_skills_command(home: &Path, args: &[String]) -> Result<String, String> {
    let usage = "usage: spec sync skills <source> <target...> [--dry-run]".to_string();
    let mut dry_run = false;
    let mut source: Option<AgentTarget> = None;
    let mut targets: Vec<AgentTarget> = Vec::new();
    for arg in args {
        if arg == "--dry-run" || arg == "-n" {
            dry_run = true;
            continue;
        }
        let target = parse_target(arg).ok_or_else(|| format!("unknown agent target: {arg}"))?;
        if source.is_none() {
            source = Some(target);
        } else {
            targets.push(target);
        }
    }
    let source = source.ok_or_else(|| usage.clone())?;
    if targets.is_empty() {
        return Err(usage);
    }
    let report = crate::sync::sync_skills(home, source, &targets, dry_run)?;
    let mut out = String::new();
    for line in &report.details {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!(
        "汇总: 新增 {} · 更新 {} · 删除 {} · 未变 {}",
        report.added, report.updated, report.removed, report.unchanged
    ));
    if dry_run {
        out.push_str("（dry-run 预览，未写入任何文件）\n");
    }
    Ok(out)
}

fn sync_mcp_command(home: &Path, args: &[String]) -> Result<String, String> {
    let dry_run = args.iter().any(|arg| arg == "--dry-run" || arg == "-n");
    let usage = "usage: spec sync mcp <source> <target...> [--dry-run]".to_string();
    let positional: Vec<&String> = args.iter().filter(|arg| !arg.starts_with('-')).collect();
    let source_name = positional.first().ok_or_else(|| usage.clone())?;
    if positional.len() < 2 {
        return Err(usage);
    }
    let source =
        parse_target(source_name).ok_or_else(|| format!("unknown source: {source_name}"))?;
    let mut targets = Vec::new();
    for name in &positional[1..] {
        targets.push(parse_target(name).ok_or_else(|| format!("unknown target: {name}"))?);
    }
    let report = crate::sync_mcp::sync_mcp(home, source, &targets, dry_run)?;
    let mut output = format!(
        "MCP sync {} · {} → {} servers\n",
        if dry_run { "dry-run" } else { "done" },
        source.label(),
        report.added + report.updated + report.unchanged,
    );
    output.push_str(&format!(
        "added: {} · updated: {} · removed: {} · unchanged: {}\n",
        report.added, report.updated, report.removed, report.unchanged
    ));
    for detail in &report.details {
        output.push_str(&format!("- {detail}\n"));
    }
    Ok(output)
}

fn skill_command(home: &Path, args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            let store = crate::skills::read_store(home)?;
            if store.skills.is_empty() {
                return Ok("No skills installed.".to_string());
            }
            let mut out = String::from("Skills\n");
            for skill in store.skills.values() {
                out.push_str(&format!(
                    "- {} · {} · {:?} · {}\n",
                    skill.id,
                    skill.name,
                    skill.sync_method,
                    skill_target_label(&skill.targets)
                ));
            }
            Ok(out)
        }
        "import" => {
            let id = args.get(1).ok_or_else(|| {
                "usage: spec skill import <id> --path <dir> [--method symlink|copy] [--yes]"
                    .to_string()
            })?;
            let path = flag_value(args, "--path")?
                .ok_or_else(|| "skill import requires --path".to_string())?;
            let method = match flag_value(args, "--method")?.unwrap_or("symlink") {
                "symlink" | "link" => crate::skills::SkillSyncMethod::Symlink,
                "copy" => crate::skills::SkillSyncMethod::Copy,
                value => return Err(format!("unknown skill sync method: {value}")),
            };
            crate::skills::import_local(
                home,
                id,
                flag_value(args, "--name")?.unwrap_or(id),
                Path::new(path),
                method,
                !yes_requested(args),
            )
        }
        "enable" | "disable" => {
            let enabled = args.first().map(String::as_str) == Some("enable");
            let id = args.get(1).ok_or_else(|| {
                "usage: spec skill enable|disable <id> --target <app> [--yes]".to_string()
            })?;
            let target_name = flag_value(args, "--target")?
                .ok_or_else(|| "skill enable/disable requires --target".to_string())?;
            let target = parse_target(target_name)
                .ok_or_else(|| format!("unknown target: {target_name}"))?;
            crate::skills::set_enabled(home, id, target, enabled, !yes_requested(args))
        }
        "verify" => {
            let store = crate::skills::read_store(home)?;
            let mut out = String::from("Skill verification\n");
            let mut failed = 0;
            for skill in store.skills.values() {
                match crate::skills::hash_directory(Path::new(&skill.path)) {
                    Ok(hash) if hash == skill.sha256 => {
                        out.push_str(&format!("- {}: OK\n", skill.id));
                    }
                    Ok(hash) => {
                        failed += 1;
                        out.push_str(&format!("- {}: CHANGED {}\n", skill.id, &hash[..12]));
                    }
                    Err(error) => {
                        failed += 1;
                        out.push_str(&format!("- {}: FAILED {error}\n", skill.id));
                    }
                }
            }
            out.push_str(&format!("Failures: {failed}\n"));
            Ok(out)
        }
        "install-zip" => {
            let id = args.get(1).ok_or_else(|| {
                "usage: spec skill install-zip <id> --path <zip> [--yes]".to_string()
            })?;
            let path = flag_value(args, "--path")?
                .ok_or_else(|| "skill install-zip requires --path".to_string())?;
            let method = match flag_value(args, "--method")?.unwrap_or("copy") {
                "symlink" | "link" => crate::skills::SkillSyncMethod::Symlink,
                "copy" => crate::skills::SkillSyncMethod::Copy,
                value => return Err(format!("unknown skill sync method: {value}")),
            };
            crate::skills::install_zip(
                home,
                id,
                flag_value(args, "--name")?.unwrap_or(id),
                Path::new(path),
                method,
                !yes_requested(args),
            )
        }
        "install-github" => {
            let id = args.get(1).ok_or_else(|| {
                "usage: spec skill install-github <id> --owner <owner> --repo <repo> [--branch main] [--subdir path] [--yes]"
                    .to_string()
            })?;
            let owner = flag_value(args, "--owner")?
                .ok_or_else(|| "skill install-github requires --owner".to_string())?;
            let repo = flag_value(args, "--repo")?
                .ok_or_else(|| "skill install-github requires --repo".to_string())?;
            let branch = flag_value(args, "--branch")?.unwrap_or("main");
            let subdir = flag_value(args, "--subdir")?;
            let method = match flag_value(args, "--method")?.unwrap_or("copy") {
                "symlink" | "link" => crate::skills::SkillSyncMethod::Symlink,
                "copy" => crate::skills::SkillSyncMethod::Copy,
                value => return Err(format!("unknown skill sync method: {value}")),
            };
            crate::skills::install_github(
                home,
                id,
                flag_value(args, "--name")?.unwrap_or(id),
                crate::skills::GitHubSkillSource {
                    owner,
                    repo,
                    branch,
                    subdir,
                },
                method,
                !yes_requested(args),
            )
        }
        "update" => {
            let id = args
                .get(1)
                .ok_or_else(|| "usage: spec skill update <id> [--yes]".to_string())?;
            crate::skills::update_skill(home, id, !yes_requested(args))
        }
        "uninstall" | "remove" | "rm" => {
            let id = args
                .get(1)
                .ok_or_else(|| "usage: spec skill uninstall <id> [--yes]".to_string())?;
            crate::skills::uninstall_skill(home, id, !yes_requested(args))
        }
        "backups" | "backup-list" => crate::skills::list_backups(home),
        "restore" | "backup-restore" => {
            let id = args
                .get(1)
                .ok_or_else(|| "usage: spec skill restore <backup-id> [--yes]".to_string())?;
            crate::skills::restore_backup(home, id, !yes_requested(args))
        }
        other => Err(format!("unknown skill command: {other}")),
    }
}

fn skill_target_label(targets: &crate::skills::SkillTargets) -> String {
    [
        (targets.opencode, "opencode"),
        (targets.claude, "claude"),
        (targets.codex, "codex"),
    ]
    .into_iter()
    .filter_map(|(enabled, label)| enabled.then_some(label))
    .collect::<Vec<_>>()
    .join(",")
}

fn mcp_command(home: &Path, args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            let store = crate::mcp::read_store(home)?;
            if store.servers.is_empty() {
                return Ok("No MCP servers configured.".to_string());
            }
            let mut out = String::from("MCP servers\n");
            for server in store.servers.values() {
                out.push_str(&format!(
                    "- {} · {} · {:?} · {}\n",
                    server.id,
                    server.name,
                    server.transport,
                    mcp_target_label(&server.targets)
                ));
            }
            Ok(out)
        }
        "show" => {
            let id = args
                .get(1)
                .ok_or_else(|| "usage: spec mcp show <id>".to_string())?;
            let store = crate::mcp::read_store(home)?;
            let server = store
                .servers
                .get(id)
                .ok_or_else(|| format!("unknown MCP server: {id}"))?;
            let mut value = serde_json::to_value(server).map_err(|error| error.to_string())?;
            if let Some(obj) = value.as_object_mut() {
                for key in ["env", "headers"] {
                    if let Some(serde_json::Value::Object(map)) = obj.get_mut(key) {
                        for item in map.values_mut() {
                            if !item.is_null() {
                                *item = serde_json::Value::String("<redacted>".to_string());
                            }
                        }
                    }
                }
            }
            serde_json::to_string_pretty(&value).map_err(|error| error.to_string())
        }
        "add" => mcp_add(home, &args[1..]),
        "update" | "edit" => mcp_update(home, &args[1..]),
        "preset" => mcp_preset(home, &args[1..]),
        "delete" | "remove" | "rm" => mcp_delete(home, &args[1..]),
        "enable" => mcp_set_enabled(home, &args[1..], true),
        "disable" => mcp_set_enabled(home, &args[1..], false),
        "apply" | "sync" => mcp_apply(home, &args[1..]),
        "import" => mcp_import(home, &args[1..]),
        other => Err(format!("unknown MCP command: {other}")),
    }
}

fn mcp_import(home: &Path, args: &[String]) -> Result<String, String> {
    let target_name = flag_value(args, "--target")?
        .or_else(|| {
            args.first()
                .filter(|value| !value.starts_with('-'))
                .map(String::as_str)
        })
        .ok_or_else(|| {
            "usage: spec mcp import --target opencode|claude|codex|all [--yes]".to_string()
        })?;
    let mut store = crate::mcp::read_store(home)?;
    let targets = if target_name == "all" {
        vec![
            AgentTarget::OpenCode,
            AgentTarget::ClaudeCode,
            AgentTarget::Codex,
        ]
    } else {
        vec![parse_target(target_name).ok_or_else(|| format!("unknown target: {target_name}"))?]
    };
    let mut imported = 0;
    let mut reports = Vec::new();
    for target in targets {
        let mut candidate = store.clone();
        match crate::mcp::import_live(home, target, &mut candidate) {
            Ok(count) => {
                store = candidate;
                imported += count;
                reports.push(format!("{}: imported {count}", target.label()));
            }
            Err(error) => reports.push(format!("{}: failed: {error}", target.label())),
        }
    }
    let patch = crate::mcp::store_patch(home, &store)?;
    let mut output = apply_mcp_patches(&[patch], yes_requested(args), "import")?;
    output.push_str(&format!("Imported definitions: {imported}\n"));
    for report in reports {
        output.push_str(&format!("- {report}\n"));
    }
    Ok(output)
}

fn mcp_preset(home: &Path, args: &[String]) -> Result<String, String> {
    if args.first().map(String::as_str).unwrap_or("list") == "list" {
        let mut out = String::from("MCP presets\n");
        for preset in crate::mcp::MCP_PRESETS {
            out.push_str(&format!(
                "- {} · {} · {}\n",
                preset.id, preset.name, preset.description
            ));
        }
        return Ok(out);
    }
    let id = args
        .first()
        .ok_or_else(|| "usage: spec mcp preset <id> [--target <app>] [--yes]".to_string())?;
    let mut store = crate::mcp::read_store(home)?;
    if store.servers.contains_key(id) {
        return Err(format!("MCP server already exists: {id}"));
    }
    let server = crate::mcp::server_from_preset(id, mcp_targets_from_flags(args)?)?;
    store.servers.insert(id.clone(), server);
    let patches = mcp_store_and_projection_patches(home, &store, &[])?;
    apply_mcp_patches(&patches, yes_requested(args), "preset")
}

fn mcp_add(home: &Path, args: &[String]) -> Result<String, String> {
    let id = args
        .first()
        .filter(|value| !value.starts_with('-'))
        .ok_or_else(|| {
            "usage: spec mcp add <id> --name <name> --transport stdio|http|sse [--command <cmd> --arg <arg> | --url <url>] [--target <app>] [--yes]".to_string()
        })?;
    let transport = match flag_value(args, "--transport")?.unwrap_or("stdio") {
        "stdio" => McpTransport::Stdio,
        "http" => McpTransport::Http,
        "sse" => McpTransport::Sse,
        value => return Err(format!("unknown MCP transport: {value}")),
    };
    let mut store = crate::mcp::read_store(home)?;
    if store.servers.contains_key(id) {
        return Err(format!("MCP server already exists: {id}"));
    }
    let server = McpServer {
        id: id.clone(),
        name: flag_value(args, "--name")?.unwrap_or(id).to_string(),
        transport,
        command: flag_value(args, "--command")?.map(str::to_string),
        args: flag_values_allow_dash(args, "--arg")?
            .into_iter()
            .map(str::to_string)
            .collect(),
        url: flag_value(args, "--url")?.map(str::to_string),
        env: parse_pairs(&flag_values(args, "--env")?, "--env")?,
        headers: parse_pairs(&flag_values(args, "--header")?, "--header")?,
        targets: mcp_targets_from_flags(args)?,
        description: flag_value(args, "--description")?.map(str::to_string),
        homepage: flag_value(args, "--homepage")?.map(str::to_string),
    };
    server.validate()?;
    store.servers.insert(id.clone(), server);
    let patches = mcp_store_and_projection_patches(home, &store, &[])?;
    apply_mcp_patches(&patches, yes_requested(args), "add")
}

fn mcp_update(home: &Path, args: &[String]) -> Result<String, String> {
    let id = args
        .first()
        .filter(|value| !value.starts_with('-'))
        .ok_or_else(|| "usage: spec mcp update <id> [fields] [--yes]".to_string())?;
    let mut store = crate::mcp::read_store(home)?;
    let server = store
        .servers
        .get_mut(id)
        .ok_or_else(|| format!("unknown MCP server: {id}"))?;
    if let Some(name) = flag_value(args, "--name")? {
        server.name = name.to_string();
    }
    if let Some(transport) = flag_value(args, "--transport")? {
        server.transport = match transport {
            "stdio" => McpTransport::Stdio,
            "http" => McpTransport::Http,
            "sse" => McpTransport::Sse,
            value => return Err(format!("unknown MCP transport: {value}")),
        };
    }
    if let Some(command) = flag_value(args, "--command")? {
        server.command = Some(command.to_string());
    }
    let replacement_args = flag_values_allow_dash(args, "--arg")?;
    if !replacement_args.is_empty() || args.iter().any(|arg| arg == "--clear-args") {
        server.args = replacement_args.into_iter().map(str::to_string).collect();
    }
    if let Some(url) = flag_value(args, "--url")? {
        server.url = Some(url.to_string());
    }
    let env = flag_values(args, "--env")?;
    if !env.is_empty() || args.iter().any(|arg| arg == "--clear-env") {
        server.env = parse_pairs(&env, "--env")?;
    }
    let headers = flag_values(args, "--header")?;
    if !headers.is_empty() || args.iter().any(|arg| arg == "--clear-headers") {
        server.headers = parse_pairs(&headers, "--header")?;
    }
    if let Some(description) = flag_value(args, "--description")? {
        server.description = Some(description.to_string());
    }
    if let Some(homepage) = flag_value(args, "--homepage")? {
        server.homepage = Some(homepage.to_string());
    }
    match server.transport {
        McpTransport::Stdio => {
            server.url = None;
            server.headers.clear();
        }
        McpTransport::Http | McpTransport::Sse => {
            server.command = None;
            server.args.clear();
            server.env.clear();
        }
    }
    server.validate()?;
    let patches = mcp_store_and_projection_patches(home, &store, &[])?;
    apply_mcp_patches(&patches, yes_requested(args), "update")
}

fn mcp_delete(home: &Path, args: &[String]) -> Result<String, String> {
    let id = args
        .first()
        .filter(|value| !value.starts_with('-'))
        .ok_or_else(|| "usage: spec mcp delete <id> [--yes]".to_string())?;
    let mut store = crate::mcp::read_store(home)?;
    if store.servers.remove(id).is_none() {
        return Err(format!("unknown MCP server: {id}"));
    }
    let remove_ids = vec![id.clone()];
    let patches = mcp_store_and_projection_patches(home, &store, &remove_ids)?;
    apply_mcp_patches(&patches, yes_requested(args), "delete")
}

fn mcp_set_enabled(home: &Path, args: &[String], enabled: bool) -> Result<String, String> {
    let id = args
        .first()
        .filter(|value| !value.starts_with('-'))
        .ok_or_else(|| "usage: spec mcp enable|disable <id> --target <app> [--yes]".to_string())?;
    let target_name = flag_value(args, "--target")?
        .or_else(|| {
            args.get(1)
                .filter(|value| !value.starts_with('-'))
                .map(String::as_str)
        })
        .ok_or_else(|| "MCP enable/disable requires --target opencode|claude|codex".to_string())?;
    let target =
        parse_target(target_name).ok_or_else(|| format!("unknown target: {target_name}"))?;
    let mut store = crate::mcp::read_store(home)?;
    let server = store
        .servers
        .get_mut(id)
        .ok_or_else(|| format!("unknown MCP server: {id}"))?;
    server.targets.set(target, enabled);
    let patches = vec![
        crate::mcp::store_patch(home, &store)?,
        crate::mcp::projection_patch(home, target, &store, &[])?,
    ];
    apply_mcp_patches(
        &patches,
        yes_requested(args),
        if enabled { "enable" } else { "disable" },
    )
}

fn mcp_apply(home: &Path, args: &[String]) -> Result<String, String> {
    let store = crate::mcp::read_store(home)?;
    let targets = match flag_value(args, "--target")? {
        Some("all") | None => vec![
            AgentTarget::OpenCode,
            AgentTarget::ClaudeCode,
            AgentTarget::Codex,
        ],
        Some(value) => vec![parse_target(value).ok_or_else(|| format!("unknown target: {value}"))?],
    };
    let patches = targets
        .into_iter()
        .map(|target| crate::mcp::projection_patch(home, target, &store, &[]))
        .collect::<Result<Vec<_>, _>>()?;
    apply_mcp_patches(&patches, yes_requested(args), "apply")
}

fn mcp_store_and_projection_patches(
    home: &Path,
    store: &McpStore,
    remove_ids: &[String],
) -> Result<Vec<ConfigPatch>, String> {
    Ok(vec![
        crate::mcp::store_patch(home, store)?,
        crate::mcp::projection_patch(home, AgentTarget::OpenCode, store, remove_ids)?,
        crate::mcp::projection_patch(home, AgentTarget::ClaudeCode, store, remove_ids)?,
        crate::mcp::projection_patch(home, AgentTarget::Codex, store, remove_ids)?,
    ])
}

fn apply_mcp_patches(patches: &[ConfigPatch], yes: bool, action: &str) -> Result<String, String> {
    let plan = ApplyPlan {
        target: AgentTarget::Other,
        routing_mode: crate::domain::RoutingMode::DirectFile,
        protocol_adapter: None,
        patches: patches.to_vec(),
        summary: Vec::new(),
        warnings: Vec::new(),
    };
    let results = apply_plan(&plan, !yes)?;
    let mut out = format!("{} MCP {action}\n", if yes { "Applied" } else { "Dry-run" });
    for result in results {
        out.push_str(&format!(
            "{} {}\n",
            if result.changed {
                "changed"
            } else {
                "unchanged"
            },
            result.path.display()
        ));
        if !yes && result.changed {
            out.push_str(&result.diff);
        } else if let Some(backup) = result.backup_path {
            out.push_str(&format!("backup {}\n", backup.display()));
        }
    }
    if !yes {
        out.push_str("Re-run with --yes to apply.\n");
    }
    Ok(out)
}

fn mcp_targets_from_flags(args: &[String]) -> Result<McpTargets, String> {
    let mut targets = McpTargets::default();
    for value in flag_values(args, "--target")? {
        let target = parse_target(value).ok_or_else(|| format!("unknown target: {value}"))?;
        targets.set(target, true);
    }
    Ok(targets)
}

fn mcp_target_label(targets: &McpTargets) -> String {
    [
        (targets.opencode, "opencode"),
        (targets.claude, "claude"),
        (targets.codex, "codex"),
    ]
    .into_iter()
    .filter_map(|(enabled, label)| enabled.then_some(label))
    .collect::<Vec<_>>()
    .join(",")
}

fn parse_pairs(
    values: &[&str],
    flag: &str,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut out = std::collections::BTreeMap::new();
    for value in values {
        let (key, value) = value
            .split_once('=')
            .or_else(|| value.split_once(':'))
            .ok_or_else(|| format!("{flag} requires KEY=VALUE"))?;
        if key.trim().is_empty() {
            return Err(format!("{flag} key cannot be empty"));
        }
        out.insert(key.trim().to_string(), value.trim().to_string());
    }
    Ok(out)
}

/// 被 `--arg`（allow-dash 值）消费的索引集合——布尔 flag 扫描必须跳过它们，
/// 否则 `--arg --yes` 会把 "--yes" 同时当值与开关，静默击穿 dry-run 底线。
fn consumed_allow_dash_indices(args: &[String]) -> std::collections::HashSet<usize> {
    let mut consumed = std::collections::HashSet::new();
    for (index, value) in args.iter().enumerate() {
        if value == "--arg" {
            if let Some(item) = args.get(index + 1) {
                consumed.insert(index + 1);
                let _ = item;
            }
        }
    }
    consumed
}

fn yes_requested(args: &[String]) -> bool {
    let skip = consumed_allow_dash_indices(args);
    args.iter()
        .enumerate()
        .any(|(index, arg)| arg == "--yes" && !skip.contains(&index))
}

fn yes_or_short_requested(args: &[String]) -> bool {
    yes_requested(args)
        || args
            .iter()
            .enumerate()
            .any(|(index, arg)| arg == "-y" && !consumed_allow_dash_indices(args).contains(&index))
}

fn flag_values_allow_dash<'a>(args: &'a [String], flag: &str) -> Result<Vec<&'a str>, String> {
    let mut out = Vec::new();
    for (index, value) in args.iter().enumerate() {
        if value == flag {
            let item = args
                .get(index + 1)
                .ok_or_else(|| format!("{flag} requires a value"))?;
            out.push(item.as_str());
        }
    }
    Ok(out)
}

fn opencode_command(home: &Path, args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str).unwrap_or("show") {
        "show" | "status" => Ok(format!(
            "OpenCode\npermission: {}\nconfig: {}",
            crate::opencode_settings::read_permission(home)?,
            home.join(".config/opencode/opencode.json").display()
        )),
        "permission" => {
            let mode = args.get(1).ok_or_else(|| {
                "usage: spec opencode permission allow|ask|deny [--dry-run]".to_string()
            })?;
            let dry_run = args.iter().any(|arg| arg == "--dry-run" || arg == "-n");
            let result = crate::opencode_settings::set_permission(home, mode, dry_run)?;
            let mut out = format!(
                "{} OpenCode permission: {}\n{}",
                if dry_run { "Dry-run" } else { "Updated" },
                mode,
                result.path.display()
            );
            if dry_run {
                out.push('\n');
                out.push_str(&result.diff);
            } else if let Some(backup) = result.backup_path {
                out.push_str(&format!("\nbackup {}", backup.display()));
            }
            Ok(out)
        }
        other => Err(format!("unknown opencode command: {other}")),
    }
}

fn list_providers(home: &Path) -> Result<String, String> {
    let profiles = read_profiles(&home.join(".codex/xu-chat-providers.json"))?;
    let health = read_state(home)
        .map(|state| state.provider_health)
        .unwrap_or_default();
    let mut out = String::new();
    if profiles.is_empty() {
        return Ok("No providers configured.".to_string());
    }
    out.push_str("Providers\n");
    for provider in profiles {
        out.push_str(&format!(
            "- {} · {} · {} · default {} · {}\n",
            provider.id,
            provider.name,
            provider.protocol.as_str(),
            provider.default_model,
            health
                .get(&provider.id)
                .map(provider_health_label)
                .unwrap_or("not tested".to_string())
        ));
    }
    Ok(out)
}

fn current_command(home: &Path, target_arg: Option<&str>) -> Result<String, String> {
    let target = match target_arg {
        Some(value) => parse_target(value).ok_or_else(|| format!("unknown target: {value}"))?,
        None => AgentTarget::OpenCode,
    };
    match current_provider(home, target)? {
        Some(id) => Ok(format!("{} current provider: {id}", target.label())),
        None => Ok(format!("{} current provider: not recorded", target.label())),
    }
}

fn use_command(home: &Path, args: &[String]) -> Result<String, String> {
    let provider_id = positional_arg(args).ok_or_else(|| {
        "usage: spec use <provider-id> [--target opencode|claude|codex] [--dry-run]".to_string()
    })?;
    let target = parse_target_flag(args)?.unwrap_or(AgentTarget::OpenCode);
    let dry_run = args.iter().any(|arg| arg == "--dry-run" || arg == "-n");
    let providers = read_profiles(&home.join(".codex/xu-chat-providers.json"))?;
    let mut plan = apply_agent(home, target, &providers, std::slice::from_ref(provider_id))?;
    plan.patches.push(crate::state::current_provider_patch(
        home,
        target,
        provider_id,
    )?);
    let results = apply_plan(&plan, dry_run)?;

    let mut out = String::new();
    out.push_str(if dry_run { "Dry-run\n" } else { "Applied\n" });
    out.push_str(&format!(
        "Target: {}\nProvider: {}\nMode: {}\n",
        target.label(),
        provider_id,
        plan.routing_mode.as_str()
    ));
    for line in &plan.summary {
        out.push_str(&format!("- {line}\n"));
    }
    for warning in &plan.warnings {
        out.push_str(&format!("! {warning}\n"));
    }
    for result in results {
        out.push_str(&format!(
            "{} {}\n",
            if result.changed {
                "changed"
            } else {
                "unchanged"
            },
            result.path.display()
        ));
        if dry_run {
            for line in result.diff.lines().take(24) {
                out.push_str(line);
                out.push('\n');
            }
        } else if let Some(path) = result.backup_path {
            out.push_str(&format!("backup {}\n", path.display()));
        }
    }
    if plan.routing_mode.needs_local_proxy() {
        out.push_str("Run `spec serve` while using this target.\n");
    }
    Ok(out)
}

fn test_command(home: &Path, args: &[String]) -> Result<String, String> {
    let provider_id = args
        .first()
        .ok_or_else(|| "usage: spec test <provider-id>".to_string())?;
    let profiles = read_profiles(&home.join(".codex/xu-chat-providers.json"))?;
    let provider = profiles
        .iter()
        .find(|profile| profile.id == *provider_id)
        .ok_or_else(|| format!("unknown provider: {provider_id}"))?;
    let result = test_provider_connection(provider);
    let cache_warning = crate::state::set_provider_health(
        home,
        &provider.id,
        &provider.api_key,
        result.ok,
        result.latency_ms,
        &result.message,
    )
    .err()
    .map(|error| format!("\nHealth cache warning: {error}"))
    .unwrap_or_default();
    Ok(format!(
        "Provider: {}\nProtocol: {}\nResult: {}\n{}{}",
        provider.id,
        provider.protocol.as_str(),
        if result.ok { "ok" } else { "failed/skipped" },
        result.message,
        cache_warning
    ))
}

fn doctor_command(home: &Path) -> Result<String, String> {
    let provider_path = home.join(".codex/xu-chat-providers.json");
    let provider_result = read_profiles(&provider_path);
    let mut out = String::new();
    out.push_str("spec doctor\n");
    out.push_str(&format!("home: {}\n", home.display()));
    out.push_str(&format!("providers: {}\n", provider_path.display()));
    match &provider_result {
        Ok(providers) => out.push_str(&format!("provider_count: {}\n", providers.len())),
        Err(error) => out.push_str(&format!("provider_error: {error}\n")),
    }
    out.push_str(&format!("state: {}\n", state_path(home).display()));
    if let Ok(state) = read_state(home) {
        out.push_str(&format!(
            "provider_health_count: {}\n",
            state.provider_health.len()
        ));
    }
    out.push_str(&format!(
        "runtime: {} ({})\n",
        if crate::runtime::is_running() {
            "running"
        } else {
            "not running"
        },
        crate::runtime::listen_addr()
    ));
    for path in [
        home.join(".config/opencode/opencode.json"),
        home.join(".claude/settings.json"),
        home.join(".codex/config.toml"),
    ] {
        out.push_str(&format!(
            "config {}: {}\n",
            path.display(),
            if path.exists() { "exists" } else { "missing" }
        ));
    }
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_BASE_URL",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "CODEX_HOME",
    ] {
        if std::env::var_os(key).is_some() {
            out.push_str(&format!(
                "env_warning: {key} is set and may override config\n"
            ));
        }
    }
    for target in [
        AgentTarget::OpenCode,
        AgentTarget::ClaudeCode,
        AgentTarget::Codex,
    ] {
        match current_provider(home, target) {
            Ok(Some(current)) => out.push_str(&format!("{}: {current}\n", target.id())),
            Ok(None) => out.push_str(&format!("{}: not recorded\n", target.id())),
            Err(error) => out.push_str(&format!("{}_state_error: {error}\n", target.id())),
        }
    }
    Ok(out)
}

fn agent_command(home: &Path, args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str).unwrap_or("status") {
        "status" | "list" | "ls" => agent_status_command(home, args),
        "install" | "update" => agent_install_command(home, &args[1..]),
        "setup" | "bootstrap" => agent_setup_command(home, &args[1..]),
        "doctor" | "check" => Ok(agent_diagnostics(home)),
        other => Err(format!("unknown agent command: {other}")),
    }
}

fn agent_setup_command(home: &Path, args: &[String]) -> Result<String, String> {
    let mut install_args = vec!["all".to_string()];
    install_args.extend(args.iter().cloned());
    agent_install_command(home, &install_args)
}

fn agent_status_command(home: &Path, args: &[String]) -> Result<String, String> {
    let query_latest = args.iter().any(|arg| arg == "--latest");
    let mut out = String::from("客户端状态\n");
    for row in crate::agent_tools::status_rows(home, query_latest) {
        let target = row.latest_version.as_deref().unwrap_or(if query_latest {
            "查询失败/未知"
        } else {
            "未查询"
        });
        out.push_str(&format!(
            "- {}\n  当前：{}\n  目标：{}\n  变更：{}\n  命令：{}\n  状态：{}\n",
            row.tool.label,
            row.current_version.as_deref().unwrap_or("未安装"),
            target,
            crate::agent_tools::version_transition_text(
                row.current_version.as_deref(),
                row.latest_version.as_deref(),
            ),
            row.command_path.as_deref().unwrap_or("未找到"),
            row.note
        ));
    }
    Ok(out)
}

fn agent_install_command(home: &Path, args: &[String]) -> Result<String, String> {
    let tools = parse_tool_keys(args)?;
    let yes = yes_requested(args);
    let force = args.iter().any(|arg| arg == "--force");
    if force {
        std::env::set_var("XU_FORCE_REINSTALL", "1");
    }
    if !yes {
        let mut out = preview_update_plan(home, &tools, true);
        if force {
            out.push_str("\n模式：强制重装\n");
        } else {
            out.push_str("\n模式：已最新跳过 · 优先增量 · 失败再重装\n");
        }
        out.push_str("加 --yes 才会真正执行。\n");
        return Ok(out);
    }
    install_or_update_tools(home, &tools)
}

fn provider_command(home: &Path, args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" | "ls" => list_providers(home),
        "show" => provider_show_command(home, args.get(1)),
        "add" => provider_add_command(home, &args[1..]),
        "update" | "edit" => provider_update_command(home, &args[1..]),
        "duplicate" | "copy" | "clone" => provider_duplicate_command(home, &args[1..]),
        "preset" | "presets" => provider_preset_command(home, &args[1..]),
        "models" => provider_models_command(home, &args[1..]),
        "fetch-models" => provider_fetch_models_command(home, &args[1..]),
        "sync-opencode" | "sync" => provider_sync_opencode_command(home, &args[1..]),
        "sync-providers" | "sync-from" => provider_sync_from_command(home, &args[1..]),
        "delete" | "rm" => provider_delete_command(home, &args[1..]),
        other => Err(format!("unknown provider command: {other}")),
    }
}

fn provider_sync_opencode_command(home: &Path, args: &[String]) -> Result<String, String> {
    let dry_run = args.iter().any(|arg| arg == "--预览" || arg == "--preview");
    let result = crate::providers::sync_opencode_providers_with(home, dry_run)?;
    let opencode_path = home.join(".config/opencode/opencode.json");
    if !opencode_path.exists() {
        return Ok("未找到 OpenCode 配置：~/.config/opencode/opencode.json".to_string());
    }
    let mode = if dry_run { "预览" } else { "已写入" };
    Ok(format!(
        "OpenCode provider sync（{mode}）\n来源：{}\n导入: {}\n更新: {}\n不变: {}\n跳过不兼容: {}\n{}",
        opencode_path.display(),
        result.imported,
        result.updated,
        result.unchanged,
        result.skipped,
        if dry_run && result.changed {
            "预览模式未写文件；正式执行请去掉 --预览。"
        } else if result.changed {
            "已备份 供应商配置。"
        } else {
            "无变化。"
        }
    ))
}

fn provider_show_command(home: &Path, id: Option<&String>) -> Result<String, String> {
    let id = id.ok_or_else(|| "usage: spec provider show <provider-id>".to_string())?;
    let profiles = read_profiles(&home.join(".codex/xu-chat-providers.json"))?;
    let provider = profiles
        .iter()
        .find(|provider| provider.id == *id)
        .ok_or_else(|| format!("unknown provider: {id}"))?;
    let metadata_models = provider
        .model_metadata
        .values()
        .filter(|value| value.as_object().is_some_and(|value| !value.is_empty()))
        .count();
    let health = read_state(home)
        .ok()
        .and_then(|state| state.provider_health.get(id).cloned())
        .map(|value| {
            format!(
                "{} at {}\nhealth_message: {}",
                provider_health_label(&value),
                value.checked_at,
                value.message
            )
        })
        .unwrap_or_else(|| "not tested".to_string());
    Ok(format!(
        "id: {}\nname: {}\nprotocol: {}\nbase_url: {}\nwebsite: {}\nnotes: {}\ndefault_model: {}\nmodels: {}\nmodels_with_metadata: {}\nhealth: {}\napi_key: <redacted>",
        provider.id,
        provider.name,
        provider.protocol.as_str(),
        provider.base_url,
        provider.website.as_deref().unwrap_or(""),
        provider.notes.as_deref().unwrap_or(""),
        provider.default_model,
        provider.models.join(", "),
        metadata_models,
        health
    ))
}

fn provider_add_command(home: &Path, args: &[String]) -> Result<String, String> {
    let id = positional_arg(args).ok_or_else(|| {
        "usage: spec provider add <id> [--preset preset-id] --kind chat|responses|anthropic --name <name> --base-url <url> [--api-key <key>] --model <model>".to_string()
    })?;
    let preset = match flag_value(args, "--preset")? {
        Some(id) => Some(find_provider_preset(id).ok_or_else(|| format!("unknown preset: {id}"))?),
        None => None,
    };
    let kind = flag_value(args, "--kind")?
        .or_else(|| preset.map(|preset| protocol_key(preset.protocol)))
        .unwrap_or("chat");
    let protocol = ProtocolKind::parse_legacy(kind)?;
    let name = flag_value(args, "--name")?
        .or_else(|| preset.map(|preset| preset.name))
        .unwrap_or(id);
    let base_url = flag_value(args, "--base-url")?
        .or(flag_value(args, "--baseURL")?)
        .or_else(|| preset.map(|preset| preset.base_url))
        .ok_or_else(|| "provider add requires --base-url or --preset".to_string())?;
    let api_key = flag_value(args, "--api-key")?
        .or(flag_value(args, "--apiKey")?)
        .unwrap_or_default();
    let notes = flag_value(args, "--notes")?;
    let website = flag_value(args, "--website")?;
    let model_flags = flag_values(args, "--model")?;
    let models: Vec<&str> = if model_flags.is_empty() {
        preset
            .map(|preset| preset.models.to_vec())
            .unwrap_or_default()
    } else {
        model_flags
    };
    if models.is_empty() {
        return Err("provider add requires at least one --model".to_string());
    }
    let dry_run = args.iter().any(|arg| arg == "--dry-run" || arg == "-n");

    let path = home.join(".codex/xu-chat-providers.json");
    let before = read_before_checked(&path)?;
    let mut root = provider_root(&before)?;
    let providers = root
        .get_mut("provider")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| "provider store missing provider object".to_string())?;
    if providers.contains_key(id) {
        return Err(format!("provider already exists: {id}"));
    }
    let models_map = models
        .iter()
        .map(|model| (model.to_string(), serde_json::json!({})))
        .collect::<serde_json::Map<_, _>>();
    let mut provider = serde_json::json!({
        "apiKind": api_kind_for_store(protocol),
        "name": name,
        "options": { "baseURL": base_url },
        "models": models_map,
        "defaultModel": models[0]
    });
    let provider_obj = provider
        .as_object_mut()
        .expect("provider literal is an object");
    if !api_key.is_empty() {
        provider_obj
            .get_mut("options")
            .and_then(serde_json::Value::as_object_mut)
            .expect("options literal is an object")
            .insert("apiKey".to_string(), serde_json::json!(api_key));
    }
    if let Some(notes) = notes {
        provider_obj.insert("notes".to_string(), serde_json::json!(notes));
    }
    if let Some(website) = website {
        provider_obj.insert("website".to_string(), serde_json::json!(website));
    }
    providers.insert(id.to_string(), provider);
    write_provider_store(path, before, root, dry_run)?;
    Ok(format!(
        "{} provider: {id}",
        if dry_run { "Dry-run add" } else { "Added" }
    ))
}

fn provider_preset_command(_home: &Path, args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            let mut out = String::from("Provider presets\n");
            for preset in provider_presets() {
                out.push_str(&format!(
                    "- {} · {} · {} · {} · models: {}\n",
                    preset.id,
                    preset.name,
                    protocol_key(preset.protocol),
                    preset.base_url,
                    preset.models.join(", ")
                ));
            }
            Ok(out)
        }
        "show" => {
            let id = args
                .get(1)
                .ok_or_else(|| "usage: spec provider preset show <preset-id>".to_string())?;
            let preset = find_provider_preset(id).ok_or_else(|| format!("unknown preset: {id}"))?;
            Ok(format!(
                "id: {}\nname: {}\nkind: {}\nbase_url: {}\nmodels: {}\n\nAdd example:\nxu provider add <your-id> --preset {} --api-key <key>",
                preset.id,
                preset.name,
                protocol_key(preset.protocol),
                preset.base_url,
                preset.models.join(", "),
                preset.id
            ))
        }
        other => Err(format!("unknown provider preset command: {other}")),
    }
}

fn provider_models_command(home: &Path, args: &[String]) -> Result<String, String> {
    let id = positional_arg(args)
        .ok_or_else(|| "usage: spec provider models <provider-id> [--apply --yes]".to_string())?;
    let apply = args.iter().any(|arg| arg == "--apply");
    let yes = yes_or_short_requested(args);
    let dry_run = args.iter().any(|arg| arg == "--dry-run" || arg == "-n") || (apply && !yes);
    let path = home.join(".codex/xu-chat-providers.json");
    let profiles = read_profiles(&path)?;
    let provider = profiles
        .iter()
        .find(|profile| profile.id == *id)
        .ok_or_else(|| format!("unknown provider: {id}"))?;
    let models = fetch_provider_models(provider)?;
    if !apply {
        return Ok(format!(
            "Provider: {}\nFetched models: {}\n{}",
            provider.id,
            models.len(),
            models.join("\n")
        ));
    }

    let before = read_before_checked(&path)?;
    let mut root = provider_root(&before)?;
    let providers = root
        .get_mut("provider")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| "provider store missing provider object".to_string())?;
    let provider_value = providers
        .get_mut(id)
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| format!("unknown provider: {id}"))?;
    let old_models = provider_value
        .get("models")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let model_map = models
        .iter()
        .map(|model| {
            (
                model.clone(),
                old_models
                    .get(model)
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({})),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let default_model = provider_value
        .get("defaultModel")
        .and_then(serde_json::Value::as_str)
        .filter(|model| models.iter().any(|candidate| candidate == model))
        .unwrap_or(&models[0])
        .to_string();
    provider_value.insert("models".to_string(), serde_json::json!(model_map));
    provider_value.insert("defaultModel".to_string(), serde_json::json!(default_model));
    write_provider_store(path, before, root, dry_run)?;
    Ok(format!(
        "{} provider models: {} ({} models){}",
        if dry_run { "Dry-run update" } else { "Updated" },
        id,
        models.len(),
        if dry_run {
            "\nRun with --apply --yes to write."
        } else {
            ""
        }
    ))
}

#[derive(Debug)]
struct FetchedModel {
    id: String,
    owned_by: Option<String>,
}

/// `spec provider fetch-models <provider-id> [--apply] [--timeout 15]`
///
/// Fetches the OpenAI-compatible `/v1/models` list from the provider
/// (falling back to `/models` on 404/405) and optionally writes it back into
/// the provider's `models` config, replacing the existing model list while
/// preserving per-model metadata.
fn provider_fetch_models_command(home: &Path, args: &[String]) -> Result<String, String> {
    let id = positional_arg(args).ok_or_else(|| {
        "usage: spec provider fetch-models <provider-id> [--apply] [--timeout 15]".to_string()
    })?;
    let apply = args.iter().any(|arg| arg == "--apply");
    let timeout = flag_value(args, "--timeout")?
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| "--timeout must be a number of seconds".to_string())
        })
        .transpose()?
        .unwrap_or(15);
    let path = home.join(".codex/xu-chat-providers.json");
    let profiles = read_profiles(&path)?;
    let provider = profiles
        .iter()
        .find(|profile| profile.id == *id)
        .ok_or_else(|| format!("unknown provider: {id}"))?;
    let models = fetch_models_from_provider(provider, timeout)?;

    let mut out = format!(
        "Provider: {}\nFetched models: {}\n",
        provider.id,
        models.len()
    );
    for model in &models {
        out.push_str(&model.id);
        if let Some(owner) = &model.owned_by {
            out.push_str(&format!(" (owned_by: {owner})"));
        }
        out.push('\n');
    }
    if !apply {
        return Ok(out);
    }

    let before = read_before_checked(&path)?;
    let mut root = provider_root(&before)?;
    let providers = root
        .get_mut("provider")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| "provider store missing provider object".to_string())?;
    let provider_value = providers
        .get_mut(id)
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| format!("unknown provider: {id}"))?;
    let old_models = provider_value
        .get("models")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let model_map = models
        .iter()
        .map(|model| {
            (
                model.id.clone(),
                old_models
                    .get(&model.id)
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({})),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let default_model = provider_value
        .get("defaultModel")
        .and_then(serde_json::Value::as_str)
        .filter(|model| models.iter().any(|candidate| &candidate.id == model))
        .unwrap_or(&models[0].id)
        .to_string();
    provider_value.insert("models".to_string(), serde_json::json!(model_map));
    provider_value.insert("defaultModel".to_string(), serde_json::json!(default_model));
    write_provider_store(path, before, root, false)?;
    out.push_str(&format!(
        "Updated provider models: {id} ({} models).",
        models.len()
    ));
    Ok(out)
}

/// Candidate model-list URLs, `/v1/models` first with `/models` as fallback
/// (CC Switch style). A base URL that already ends in `/v1` or `/models` is
/// honored rather than duplicating the segment.
fn models_url_candidates(base_url: &str) -> Vec<String> {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/models") {
        return vec![base.to_string()];
    }
    let root = base.strip_suffix("/v1").unwrap_or(base);
    vec![format!("{root}/v1/models"), format!("{root}/models")]
}

/// GET the provider's model list. Tries `/v1/models` first and falls back to
/// `/models` on 404/405. Any other status or network error is reported with
/// the status code and a body truncated to 512 characters.
fn fetch_models_from_provider(
    provider: &crate::domain::ProviderProfile,
    timeout_secs: u64,
) -> Result<Vec<FetchedModel>, String> {
    if provider.protocol == ProtocolKind::AnthropicMessages {
        return Err(
            "Anthropic-compatible provider 暂不自动拉取模型，避免误调用计费接口。".to_string(),
        );
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|error| format!("创建 HTTP client 失败：{error}"))?;
    let mut last_error = String::new();
    for url in models_url_candidates(&provider.base_url) {
        let mut request = client.get(&url).header("accept", "application/json");
        if !provider.api_key.trim().is_empty() {
            request = request.bearer_auth(provider.api_key.trim());
        }
        for (key, value) in &provider.extra_headers {
            request = request.header(key, value);
        }
        match request.send() {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    let value: Value = response
                        .json()
                        .map_err(|error| format!("解析响应失败：{error}"))?;
                    let models = parse_models_payload(&value);
                    if models.is_empty() {
                        return Err(format!("{url} 响应中没有找到模型 id"));
                    }
                    return Ok(models);
                }
                let body = response.text().unwrap_or_default();
                let truncated: String = body.chars().take(512).collect();
                last_error = format!("GET {url} -> {status} {truncated}");
                if status != reqwest::StatusCode::NOT_FOUND
                    && status != reqwest::StatusCode::METHOD_NOT_ALLOWED
                {
                    break;
                }
            }
            Err(error) => {
                last_error = format!("GET {url} 请求失败：{error}");
                break;
            }
        }
    }
    Err(if last_error.is_empty() {
        "无可用模型端点".to_string()
    } else {
        last_error
    })
}

fn parse_models_payload(value: &Value) -> Vec<FetchedModel> {
    let mut out = Vec::new();
    let mut push = |item: &Value| {
        if let Some(id) = item
            .as_str()
            .or_else(|| item.get("id").and_then(Value::as_str))
        {
            out.push(FetchedModel {
                id: id.to_string(),
                owned_by: item
                    .get("owned_by")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            });
        }
    };
    if let Some(data) = value.get("data").and_then(Value::as_array) {
        for item in data {
            push(item);
        }
    } else if let Some(items) = value.as_array() {
        for item in items {
            push(item);
        }
    } else if let Some(map) = value.get("models").and_then(Value::as_object) {
        out.extend(map.keys().map(|key| FetchedModel {
            id: key.clone(),
            owned_by: None,
        }));
    }
    out
}

fn provider_delete_command(home: &Path, args: &[String]) -> Result<String, String> {
    let id =
        positional_arg(args).ok_or_else(|| "usage: spec provider delete <id> --yes".to_string())?;
    if !yes_or_short_requested(args) {
        return Err("provider delete requires --yes".to_string());
    }
    let state = read_state(home)?;
    if state.current.values().any(|current| current == id) {
        return Err(format!("refusing to delete active provider: {id}"));
    }
    let dry_run = args.iter().any(|arg| arg == "--dry-run" || arg == "-n");
    let path = home.join(".codex/xu-chat-providers.json");
    let before = read_before_checked(&path)?;
    let mut root = provider_root(&before)?;
    let providers = root
        .get_mut("provider")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| "provider store missing provider object".to_string())?;
    if providers.remove(id).is_none() {
        return Err(format!("unknown provider: {id}"));
    }
    write_provider_store(path, before, root, dry_run)?;
    if !dry_run {
        crate::state::remove_provider_health(home, id)?;
    }
    Ok(format!(
        "{} provider: {id}",
        if dry_run { "Dry-run delete" } else { "Deleted" }
    ))
}

fn provider_duplicate_command(home: &Path, args: &[String]) -> Result<String, String> {
    let positional = positional_args(args);
    let [source_id, new_id] = positional.as_slice() else {
        return Err(
            "usage: spec provider duplicate <source-id> <new-id> [--name <name>] [--dry-run]"
                .to_string(),
        );
    };
    if source_id == new_id {
        return Err("duplicate provider requires a different new id".to_string());
    }
    let name = flag_value(args, "--name")?;
    let dry_run = args.iter().any(|arg| arg == "--dry-run" || arg == "-n");
    let path = home.join(".codex/xu-chat-providers.json");
    let before = read_before_checked(&path)?;
    let mut root = provider_root(&before)?;
    let providers = root
        .get_mut("provider")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| "provider store missing provider object".to_string())?;
    if providers.contains_key(*new_id) {
        return Err(format!("provider already exists: {new_id}"));
    }
    let mut duplicate = providers
        .get(*source_id)
        .cloned()
        .ok_or_else(|| format!("unknown provider: {source_id}"))?;
    if let Some(name) = name {
        duplicate
            .as_object_mut()
            .ok_or_else(|| format!("provider {source_id} must be an object"))?
            .insert("name".to_string(), serde_json::json!(name));
    }
    providers.insert((*new_id).to_string(), duplicate);
    write_provider_store(path, before, root, dry_run)?;
    Ok(format!(
        "{} provider: {} -> {}",
        if dry_run {
            "Dry-run duplicate"
        } else {
            "Duplicated"
        },
        source_id,
        new_id
    ))
}

fn provider_update_command(home: &Path, args: &[String]) -> Result<String, String> {
    let id = positional_arg(args).ok_or_else(|| {
        "usage: spec provider update <id> [--kind chat|responses|anthropic] [--name <name>] [--base-url <url>] [--api-key <key>] [--model <model>] [--cache-mode auto|compat|deepseek]".to_string()
    })?;
    let kind = flag_value(args, "--kind")?;
    let cache_mode = match flag_value(args, "--cache-mode")? {
        None => None,
        Some(value) => match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(CacheMode::Auto),
            "compat" => Some(CacheMode::Compat),
            "deepseek" | "ds" => Some(CacheMode::DeepSeek),
            _ => return Err("--cache-mode must be one of: auto | compat | deepseek".to_string()),
        },
    };
    let name = flag_value(args, "--name")?;
    let base_url = flag_value(args, "--base-url")?.or(flag_value(args, "--baseURL")?);
    let api_key = flag_value(args, "--api-key")?.or(flag_value(args, "--apiKey")?);
    let notes = flag_value(args, "--notes")?;
    let website = flag_value(args, "--website")?;
    let default_model = flag_value(args, "--default-model")?;
    let timeout = numeric_flag(args, "--timeout")?;
    let max_retries = numeric_flag(args, "--max-retries")?;
    let context_window = numeric_flag(args, "--context-window")?;
    let max_output_tokens = numeric_flag(args, "--max-output-tokens")?;
    let reasoning_effort = flag_value(args, "--reasoning-effort")?;
    let headers = flag_values(args, "--header")?;
    let clear_headers = args.iter().any(|arg| arg == "--clear-headers");
    let models = flag_values(args, "--model")?;
    let models_json = flag_value(args, "--models-json")?;
    let claude_slots: Vec<(String, String)> = flag_values(args, "--claude-slot")?
        .into_iter()
        .filter_map(|pair| {
            let (slot, model) = pair.split_once('=')?;
            Some((slot.to_string(), model.to_string()))
        })
        .collect();
    if kind.is_none()
        && cache_mode.is_none()
        && name.is_none()
        && base_url.is_none()
        && api_key.is_none()
        && notes.is_none()
        && website.is_none()
        && default_model.is_none()
        && timeout.is_none()
        && max_retries.is_none()
        && context_window.is_none()
        && max_output_tokens.is_none()
        && reasoning_effort.is_none()
        && headers.is_empty()
        && !clear_headers
        && models.is_empty()
        && models_json.is_none()
        && claude_slots.is_empty()
    {
        return Err("provider update requires at least one field flag".to_string());
    }
    let dry_run = args.iter().any(|arg| arg == "--dry-run" || arg == "-n");

    let path = home.join(".codex/xu-chat-providers.json");
    let before = read_before_checked(&path)?;
    let mut root = provider_root(&before)?;
    let providers = root
        .get_mut("provider")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| "provider store missing provider object".to_string())?;
    let provider = providers
        .get_mut(id)
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| format!("unknown provider: {id}"))?;

    if let Some(kind) = kind {
        let protocol = ProtocolKind::parse_legacy(kind)?;
        provider.insert(
            "apiKind".to_string(),
            serde_json::json!(api_kind_for_store(protocol)),
        );
    }
    if let Some(cache_mode) = cache_mode {
        provider.insert(
            "cacheMode".to_string(),
            serde_json::json!(cache_mode.as_str()),
        );
    }
    if let Some(name) = name {
        provider.insert("name".to_string(), serde_json::json!(name));
    }
    if let Some(notes) = notes {
        provider.insert("notes".to_string(), serde_json::json!(notes));
    }
    if !claude_slots.is_empty() {
        let slots = provider
            .entry("claudeSlots".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(slots) = slots.as_object_mut() {
            for (slot, model) in claude_slots {
                slots.insert(slot, serde_json::json!(model));
            }
        }
    }
    if let Some(website) = website {
        provider.insert("website".to_string(), serde_json::json!(website));
    }
    if base_url.is_some()
        || api_key.is_some()
        || timeout.is_some()
        || max_retries.is_some()
        || context_window.is_some()
        || max_output_tokens.is_some()
        || reasoning_effort.is_some()
        || !headers.is_empty()
        || clear_headers
    {
        let options = provider
            .entry("options".to_string())
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or_else(|| format!("provider {id} options is not an object"))?;
        if let Some(base_url) = base_url {
            options.insert("baseURL".to_string(), serde_json::json!(base_url));
        }
        if let Some(api_key) = api_key {
            if api_key.is_empty() {
                options.remove("apiKey");
            } else {
                options.insert("apiKey".to_string(), serde_json::json!(api_key));
            }
        }
        for (key, value) in [
            ("timeout", timeout),
            ("maxRetries", max_retries),
            ("contextWindow", context_window),
            ("maxOutputTokens", max_output_tokens),
        ] {
            if let Some(value) = value {
                options.insert(key.to_string(), serde_json::json!(value));
            }
        }
        if let Some(value) = reasoning_effort {
            options.insert("reasoningEffort".to_string(), serde_json::json!(value));
        }
        if clear_headers {
            options.remove("customHeaders");
            options.remove("headers");
        } else if !headers.is_empty() {
            let headers = parse_header_flags(&headers)?;
            options.insert("customHeaders".to_string(), serde_json::json!(headers));
        }
    }
    if let Some(json) = models_json {
        let value: Value = serde_json::from_str(json)
            .map_err(|error| format!("--models-json must be a JSON object or array: {error}"))?;
        let mut fallback_default = None;
        match &value {
            Value::Object(map) => {
                if map.is_empty() {
                    return Err("--models-json must not be an empty object".to_string());
                }
                if default_model.is_none() {
                    fallback_default = map.keys().next().cloned();
                }
            }
            Value::Array(items) => {
                if items.is_empty() {
                    return Err("--models-json must not be an empty array".to_string());
                }
                for item in items {
                    match item {
                        Value::String(_) => {}
                        Value::Object(map)
                            if map.contains_key("name") || map.contains_key("requestName") => {}
                        _ => {
                            return Err(
                                "--models-json array entries must be strings or objects with name/requestName"
                                    .to_string(),
                            )
                        }
                    }
                }
                if default_model.is_none() {
                    fallback_default = match &items[0] {
                        Value::String(name) => Some(name.clone()),
                        Value::Object(map) => map
                            .get("name")
                            .or_else(|| map.get("requestName"))
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                        _ => None,
                    };
                }
            }
            _ => return Err("--models-json must be a JSON object or array".to_string()),
        }
        provider.insert("models".to_string(), value);
        if let Some(first) = fallback_default {
            provider.insert("defaultModel".to_string(), serde_json::json!(first));
        }
    } else if !models.is_empty() {
        let existing = provider
            .get("models")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        let model_map = models
            .iter()
            .map(|model| {
                (
                    model.to_string(),
                    existing
                        .get(*model)
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        provider.insert("models".to_string(), serde_json::json!(model_map));
        if default_model.is_none() {
            provider.insert("defaultModel".to_string(), serde_json::json!(models[0]));
        }
    }
    if let Some(default_model) = default_model {
        provider.insert("defaultModel".to_string(), serde_json::json!(default_model));
    }

    write_provider_store(path, before, root, dry_run)?;
    Ok(format!(
        "{} provider: {id}",
        if dry_run { "Dry-run update" } else { "Updated" }
    ))
}

fn provider_root(before: &str) -> Result<serde_json::Value, String> {
    if before.trim().is_empty() {
        return Ok(serde_json::json!({ "provider": {} }));
    }
    let root: serde_json::Value = serde_json::from_str(before).map_err(|e| e.to_string())?;
    if root.as_array().is_some() {
        return Err("provider edit commands do not support legacy array stores".to_string());
    }
    if root
        .get("provider")
        .and_then(serde_json::Value::as_object)
        .is_none()
    {
        return Err("provider store missing provider object".to_string());
    }
    Ok(root)
}

fn write_provider_store(
    path: std::path::PathBuf,
    before: String,
    root: serde_json::Value,
    dry_run: bool,
) -> Result<(), String> {
    let after = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())? + "\n";
    for profile in profiles_from_xu_chat_json(&after)? {
        profile.validate_for_write()?;
    }
    apply_patch(
        &ConfigPatch::new(path, before, after),
        PatchOptions {
            dry_run,
            backup: true,
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn config_path_command(home: &Path) -> Result<String, String> {
    Ok(format!(
        "providers: {}\nstate: {}\nopencode: {}\nclaude: {}\ncodex: {}",
        home.join(".codex/xu-chat-providers.json").display(),
        state_path(home).display(),
        home.join(".config/opencode/opencode.json").display(),
        home.join(".claude/settings.json").display(),
        home.join(".codex/config.toml").display()
    ))
}

fn parse_target_flag(args: &[String]) -> Result<Option<AgentTarget>, String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--target" || arg == "--tool" || arg == "--app" {
            let value = iter
                .next()
                .ok_or_else(|| format!("{arg} requires opencode|claude|codex"))?;
            return parse_target(value)
                .map(Some)
                .ok_or_else(|| format!("unknown target: {value}"));
        }
    }
    Ok(None)
}

fn positional_arg(args: &[String]) -> Option<&String> {
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--target"
            | "--tool"
            | "--app"
            | "--kind"
            | "--name"
            | "--description"
            | "--path"
            | "--owner"
            | "--repo"
            | "--branch"
            | "--subdir"
            | "--method"
            | "--base-url"
            | "--baseURL"
            | "--api-key"
            | "--apiKey"
            | "--model"
            | "--models-json"
            | "--keep"
            | "--preset"
            | "--notes"
            | "--website"
            | "--default-model"
            | "--timeout"
            | "--max-retries"
            | "--context-window"
            | "--max-output-tokens"
            | "--reasoning-effort"
            | "--header"
            | "--transport"
            | "--command"
            | "--arg"
            | "--url"
            | "--env"
            | "--homepage" => index += 2,
            "--dry-run" | "-n" | "--yes" | "-y" | "--apply" | "--clear-headers"
            | "--clear-args" | "--clear-env" => index += 1,
            value if value.starts_with('-') => index += 1,
            _ => return args.get(index),
        }
    }
    None
}

fn positional_args(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--target"
            | "--tool"
            | "--app"
            | "--kind"
            | "--name"
            | "--description"
            | "--path"
            | "--owner"
            | "--repo"
            | "--branch"
            | "--subdir"
            | "--method"
            | "--base-url"
            | "--baseURL"
            | "--api-key"
            | "--apiKey"
            | "--model"
            | "--models-json"
            | "--keep"
            | "--preset"
            | "--notes"
            | "--website"
            | "--default-model"
            | "--timeout"
            | "--max-retries"
            | "--context-window"
            | "--max-output-tokens"
            | "--reasoning-effort"
            | "--header"
            | "--transport"
            | "--command"
            | "--arg"
            | "--url"
            | "--env"
            | "--homepage" => index += 2,
            "--dry-run" | "-n" | "--yes" | "-y" | "--apply" | "--clear-headers"
            | "--clear-args" | "--clear-env" => index += 1,
            value if value.starts_with('-') => index += 1,
            value => {
                out.push(value);
                index += 1;
            }
        }
    }
    out
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Result<Option<&'a str>, String> {
    let Some(index) = args.iter().position(|arg| arg == flag) else {
        return Ok(None);
    };
    let value = args
        .get(index + 1)
        .ok_or_else(|| format!("{flag} requires a value"))?;
    if value.starts_with('-') {
        return Err(format!("{flag} requires a value"));
    }
    Ok(Some(value))
}

fn flag_values<'a>(args: &'a [String], flag: &str) -> Result<Vec<&'a str>, String> {
    let mut out = Vec::new();
    for (index, value) in args.iter().enumerate() {
        if value == flag {
            let item = args
                .get(index + 1)
                .ok_or_else(|| format!("{flag} requires a value"))?;
            if item.starts_with('-') {
                return Err(format!("{flag} requires a value"));
            }
            out.push(item.as_str());
        }
    }
    Ok(out)
}

fn numeric_flag(args: &[String], flag: &str) -> Result<Option<u64>, String> {
    flag_value(args, flag)?
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| format!("{flag} must be a non-negative integer"))
        })
        .transpose()
}

fn parse_header_flags(
    values: &[&str],
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut headers = std::collections::BTreeMap::new();
    for value in values {
        let (key, value) = value
            .split_once(':')
            .ok_or_else(|| "--header must use 'Name: Value'".to_string())?;
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            return Err("--header name and value cannot be empty".to_string());
        }
        headers.insert(key.to_string(), value.to_string());
    }
    Ok(headers)
}

fn api_kind_for_store(protocol: ProtocolKind) -> &'static str {
    match protocol {
        ProtocolKind::OpenAiChat => "chat",
        ProtocolKind::OpenAiResponses => "responses",
        ProtocolKind::AnthropicMessages => "anthropic",
    }
}

fn provider_health_label(health: &crate::state::ProviderHealth) -> String {
    match (health.ok, health.latency_ms) {
        (true, Some(latency)) => format!("healthy ({latency} ms)"),
        (true, None) => "healthy".to_string(),
        (false, Some(latency)) => format!("failed ({latency} ms)"),
        (false, None) => "failed/skipped".to_string(),
    }
}

/// `spec web [--port N]`：独立运行 Web 控制台（阻塞，Ctrl+C 结束）。
fn web_command(home: &Path, args: &[String]) -> Result<String, String> {
    let mut port = crate::web::default_port();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--port" | "-p" => {
                let raw = iter
                    .next()
                    .ok_or_else(|| "usage: spec web [--port <port>]".to_string())?;
                port = raw
                    .parse::<u16>()
                    .map_err(|e| format!("invalid port {raw}: {e}"))?;
            }
            "--help" | "-h" => return Ok("usage: spec web [--port <port>]".to_string()),
            other => return Err(format!("unknown web argument: {other}")),
        }
    }
    let _ = home;
    let stop = Arc::new(AtomicBool::new(false));
    println!("Web 控制台：http://127.0.0.1:{port}（Ctrl+C 停止）");
    crate::web::serve(port, stop).map_err(|e| format!("web server error: {e}"))?;
    Ok("Web 控制台已停止".to_string())
}

fn help_text() -> String {
    "xcc-switch commands（命令名 xcc，兼容别名 spec）\n\n  xcc | spec                   open TUI\n  spec list                    list providers\n  spec agent doctor            诊断 Termux 客户端环境\n  spec agent setup             预览 OpenCode/Claude/Codex/Dsh 安装更新\n  spec agent setup --yes       执行 OpenCode/Claude/Codex/Dsh 安装更新\n  spec agent status [--latest] 显示客户端版本与状态\n  spec agent install <id|all>  预览安装/更新（显示 旧版→新版）\n  spec agent install <id|all> --yes 执行安装/更新与重装检测\n  spec provider preset list    list provider templates\n  spec provider add <id> ...   add provider\n  spec provider add <id> --preset openrouter --api-key <key>\n  spec provider models <id>    fetch models from /models\n  spec provider models <id> --apply --yes update stored models\n  spec provider update <id> ... update provider\n  spec provider duplicate <source> <new-id> copy provider\n  spec provider show <id>      show provider without secrets\n  spec provider delete <id> --yes delete provider\n  spec mcp list                list unified MCP servers\n  spec mcp add <id> ...        preview MCP add and projections\n  spec mcp add <id> ... --yes  add and project MCP server\n  spec mcp import --target <app> [--yes] import live MCP definitions\n  spec mcp enable <id> --target <app> [--yes]\n  spec mcp disable <id> --target <app> [--yes]\n  spec mcp apply [--target <app|all>] [--yes]\n  spec mcp delete <id> [--yes] remove from SSOT and live configs
  spec sync mcp <source> <target...> [--dry-run] sync MCP servers across agents\n  spec current [target]        show recorded current provider\n  spec use <id> [--target t]   apply provider to target\n  spec use <id> --dry-run      preview writes\n  spec test <id>               test provider connectivity\n  spec doctor                  show paths and current state\n  spec config path             show managed config paths\n  spec sync skills <src> <dst...> [--dry-run]  mirror skill dirs across agents\n  spec serve                   run local protocol adapter\n  spec web [--port N]            run local web console\n"
        .to_string()
}

fn provider_ids_for_target(home: &Path, target: AgentTarget) -> Result<Vec<String>, String> {
    if target == AgentTarget::OpenCode {
        return Err("OpenCode does not require xu-client-routes.json".to_string());
    }

    let routes_path = home.join(".codex/xu-client-routes.json");
    let text = fs::read_to_string(&routes_path)
        .map_err(|e| format!("read {}: {e}", routes_path.display()))?;
    let root: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let key = match target {
        AgentTarget::ClaudeCode => "claude",
        AgentTarget::Codex => "codex",
        AgentTarget::OpenCode => unreachable!(),
        AgentTarget::Other => "other",
    };
    let route = root
        .get("clients")
        .and_then(|v| v.get(key))
        .ok_or_else(|| format!("missing route for {key}"))?;
    let providers = route
        .get("providers")
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("route {key} missing providers"))?;
    let ids: Vec<String> = providers
        .iter()
        .filter_map(|v| v.as_str().map(ToString::to_string))
        .collect();
    if ids.is_empty() {
        return Err(format!("route {key} has no providers"));
    }
    Ok(ids)
}

/// 从任意 agent 端拉取供应商到 spec 配置（互相拉取）。
/// `spec provider sync-providers <opencode|claude|codex> [--预览]`
fn provider_sync_from_command(home: &Path, args: &[String]) -> Result<String, String> {
    let source = args.first().ok_or_else(|| {
        "usage: spec provider sync-providers <opencode|claude|codex> [--预览]".to_string()
    })?;
    let dry_run = args.iter().any(|arg| arg == "--预览" || arg == "--preview");
    match source.as_str() {
        "opencode" => provider_sync_opencode_command(home, &args[1..]),
        "claude" => sync_from_claude(home, dry_run),
        "codex" => sync_from_codex(home, dry_run),
        other => Err(format!(
            "unknown sync source: {other}（支持 opencode/claude/codex）"
        )),
    }
}

/// 从 ~/.claude/settings.json 的 env 提取（base_url/model/key）→ 生成 provider。
fn sync_from_claude(home: &Path, dry_run: bool) -> Result<String, String> {
    let path = home.join(".claude/settings.json");
    if !path.exists() {
        return Ok("未找到 Claude 配置：~/.claude/settings.json".to_string());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let doc: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let env = doc.get("env").and_then(serde_json::Value::as_object);
    let Some(env) = env else {
        return Ok("Claude settings 无 env 段，无可拉取内容。".to_string());
    };
    let base_url = env
        .get("ANTHROPIC_BASE_URL")
        .and_then(serde_json::Value::as_str);
    let Some(base_url) = base_url else {
        return Ok("Claude settings 未配置 ANTHROPIC_BASE_URL，无可拉取内容。".to_string());
    };
    let api_key = env
        .get("ANTHROPIC_AUTH_TOKEN")
        .or_else(|| env.get("ANTHROPIC_API_KEY"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let model = env
        .get("ANTHROPIC_MODEL")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("claude")
        .trim_end_matches("[1m]");
    let provider = serde_json::json!({
        "provider": {
            "claude": {
                "apiKind": "anthropic",
                "name": "Claude settings 拉取",
                "options": { "baseURL": base_url, "apiKey": api_key, "timeout": 90000, "maxRetries": 1 },
                "models": { model: {} },
                "defaultModel": model
            }
        }
    });
    apply_or_preview_provider_sync(home, &provider, "claude", dry_run)
}

/// 从 ~/.codex/config.toml 的 [model_providers.*] 提取（排除 spec 生成的 xu*/xucodex*）。
fn sync_from_codex(home: &Path, dry_run: bool) -> Result<String, String> {
    let path = home.join(".codex/config.toml");
    if !path.exists() {
        return Ok("未找到 Codex 配置：~/.codex/config.toml".to_string());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let doc: toml::Value =
        toml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let Some(providers) = doc.get("model_providers").and_then(toml::Value::as_table) else {
        return Ok("Codex config 无 model_providers 段，无可拉取内容。".to_string());
    };
    let mut store = serde_json::json!({ "provider": {} });
    let mut imported = 0;
    for (id, cfg) in providers {
        if id.starts_with("xu") || id.starts_with("xucodex") {
            continue;
        }
        let base_url = cfg
            .get("base_url")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        if base_url.is_empty() {
            continue;
        }
        let wire = cfg
            .get("wire_api")
            .and_then(toml::Value::as_str)
            .unwrap_or("chat");
        let api_kind = if wire == "responses" {
            "responses"
        } else {
            "chat"
        };
        let api_key = cfg
            .get("experimental_bearer_token")
            .or_else(|| cfg.get("env_key"))
            .and_then(toml::Value::as_str)
            .unwrap_or("");
        store["provider"][id] = serde_json::json!({
            "apiKind": api_kind,
            "name": format!("Codex 拉取：{id}"),
            "options": { "baseURL": base_url, "apiKey": api_key, "timeout": 90000, "maxRetries": 1 },
            "models": { "codex-model": {} },
            "defaultModel": "codex-model"
        });
        imported += 1;
    }
    if imported == 0 {
        return Ok("Codex 无第三方 model_providers（spec 生成项已跳过）。".to_string());
    }
    apply_or_preview_provider_sync(home, &store, "codex", dry_run)
}

fn apply_or_preview_provider_sync(
    home: &Path,
    provider_doc: &serde_json::Value,
    source: &str,
    dry_run: bool,
) -> Result<String, String> {
    let store_path = home.join(".codex/xu-chat-providers.json");
    let before = crate::patch::read_before_checked(&store_path)?;
    let mut store: serde_json::Value = if before.trim().is_empty() {
        serde_json::json!({ "provider": {} })
    } else {
        serde_json::from_str(&before).map_err(|e| format!("parse {}: {e}", store_path.display()))?
    };
    let target = store
        .get_mut("provider")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| "xu-chat-providers.json 缺 provider 对象".to_string())?;
    let incoming: serde_json::Map<String, serde_json::Value> = provider_doc
        .get("provider")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut imported = 0;
    let mut updated = 0;
    for (id, value) in &incoming {
        match target.get(id) {
            None => {
                target.insert(id.clone(), value.clone());
                imported += 1;
            }
            Some(existing) if existing == value => {}
            Some(_) => {
                target.insert(id.clone(), value.clone());
                updated += 1;
            }
        }
    }
    if (imported + updated) == 0 {
        return Ok(format!("从 {source} 拉取：无变化。"));
    }
    if !dry_run {
        let after = serde_json::to_string_pretty(&store).map_err(|e| e.to_string())? + "\n";
        crate::patch::apply_patch(
            &crate::patch::ConfigPatch::new(store_path, before, after),
            crate::patch::PatchOptions {
                dry_run: false,
                backup: true,
            },
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(format!(
        "从 {source} 拉取供应商（{}）\n导入: {}\n更新: {}\n{}",
        if dry_run { "预览" } else { "已写入" },
        imported,
        updated,
        if dry_run {
            "预览模式未写文件；去掉 --预览 正式执行。"
        } else {
            "已备份 供应商配置。"
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn test_provider(base_url: &str) -> crate::domain::ProviderProfile {
        use crate::domain::{CacheMode, ProviderProfile, ProviderVendor};
        ProviderProfile {
            id: "zen".to_string(),
            name: "Zen".to_string(),
            notes: None,
            website: None,
            vendor: ProviderVendor::CustomOpenAiCompatible,
            protocol: ProtocolKind::OpenAiChat,
            base_url: base_url.to_string(),
            api_key: "key".to_string(),
            models: vec!["old".to_string()],
            model_entries: Default::default(),
            model_metadata: Default::default(),
            claude_slots: Default::default(),
            default_model: "old".to_string(),
            extra_headers: Default::default(),
            request_url_mode: None,
            header_mode: None,
            timeout_ms: 60_000,
            max_retries: 10,
            context_window: 128_000,
            max_output_tokens: 32_768,
            reasoning_effort: None,
            cache_mode: CacheMode::Auto,
        }
    }

    struct MockServer {
        base_url: String,
        seen: std::sync::mpsc::Receiver<String>,
    }

    /// Tiny HTTP server serving one response per route tuple, in order.
    /// Records the request path of every connection on `seen`.
    fn mock_server(routes: &[(&str, u16, &str)]) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let routes: Vec<(String, u16, String)> = routes
            .iter()
            .map(|(path, status, body)| (path.to_string(), *status, body.to_string()))
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (path, status, body) in routes {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 8192];
                let read = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let request_path = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("")
                    .split('?')
                    .next()
                    .unwrap_or("")
                    .to_string();
                let _ = tx.send(request_path.clone());
                let (status, body) = if request_path == path {
                    (status, body)
                } else {
                    (404, r#"{"error":"not found"}"#.to_string())
                };
                let reason = if status == 200 { "OK" } else { "Not Found" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-length: {}\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        MockServer { base_url, seen: rx }
    }

    fn temp_home() -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("spec-fetch-models-{}-{stamp}", std::process::id()));
        fs::create_dir_all(dir.join(".codex")).expect("create temp home");
        dir
    }

    #[test]
    fn models_url_candidates_prefer_v1() {
        assert_eq!(
            models_url_candidates("https://api.example.com"),
            vec![
                "https://api.example.com/v1/models",
                "https://api.example.com/models"
            ]
        );
        assert_eq!(
            models_url_candidates("https://api.example.com/v1"),
            vec![
                "https://api.example.com/v1/models",
                "https://api.example.com/models"
            ]
        );
        assert_eq!(
            models_url_candidates("https://api.example.com/v1/models"),
            vec!["https://api.example.com/v1/models"]
        );
    }

    #[test]
    fn parse_models_payload_reads_data_array_with_owned_by() {
        let value = serde_json::json!({"data": [{"id": "a", "owned_by": "org1"}, {"id": "b"}]});

        let models = parse_models_payload(&value);

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "a");
        assert_eq!(models[0].owned_by.as_deref(), Some("org1"));
        assert_eq!(models[1].owned_by, None);
    }

    #[test]
    fn parse_models_payload_falls_back_to_array_and_map() {
        let models = parse_models_payload(&serde_json::json!(["x", {"id": "y"}]));
        assert_eq!(models.len(), 2);

        let models = parse_models_payload(&serde_json::json!({"models": {"m1": {}}}));
        assert_eq!(models[0].id, "m1");
        assert_eq!(models[0].owned_by, None);
    }

    #[test]
    fn fetch_models_hits_v1_models_first_and_reports_owned_by() {
        let server = mock_server(&[(
            "/v1/models",
            200,
            r#"{"data":[{"id":"a","owned_by":"org"},{"id":"b"}]}"#,
        )]);
        let provider = test_provider(&server.base_url);

        let models = fetch_models_from_provider(&provider, 15).unwrap();

        assert_eq!(server.seen.recv().unwrap(), "/v1/models");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].owned_by.as_deref(), Some("org"));
        assert_eq!(models[1].owned_by, None);
    }

    #[test]
    fn fetch_models_falls_back_to_models_on_404() {
        let server = mock_server(&[
            ("/v1/models", 404, r#"{"error":"gone"}"#),
            ("/models", 200, r#"{"data":[{"id":"fallback"}]}"#),
        ]);
        let provider = test_provider(&server.base_url);

        let models = fetch_models_from_provider(&provider, 15).unwrap();

        assert_eq!(server.seen.recv().unwrap(), "/v1/models");
        assert_eq!(server.seen.recv().unwrap(), "/models");
        assert_eq!(models[0].id, "fallback");
    }

    #[test]
    fn fetch_models_reports_status_and_truncated_body() {
        let long_body = format!(r#"{{"error":"{}"}}"#, "x".repeat(2000));
        let server = mock_server(&[("/v1/models", 500, &long_body)]);
        let provider = test_provider(&server.base_url);

        let err = fetch_models_from_provider(&provider, 15).unwrap_err();

        assert!(err.contains("500"), "got: {err}");
        assert!(
            err.len() < server.base_url.len() + 560,
            "error body not truncated: len={}",
            err.len()
        );
    }

    #[test]
    fn fetch_models_reports_network_failure() {
        let provider = test_provider("http://127.0.0.1:1");

        let err = fetch_models_from_provider(&provider, 5).unwrap_err();

        assert!(err.contains("请求失败"), "got: {err}");
    }

    #[test]
    fn fetch_models_command_without_apply_only_prints() {
        let server = mock_server(&[("/v1/models", 200, r#"{"data":[{"id":"a"},{"id":"b"}]}"#)]);
        let home = temp_home();
        write_config(
            &home,
            &format!(
                r#"{{"provider":{{"zen":{{"apiKind":"chat","options":{{"baseURL":"{}"}},"models":{{"old":{{}}}},"defaultModel":"old"}}}}}}"#,
                server.base_url
            ),
        );

        let out = provider_fetch_models_command(&home, &["zen".to_string()]).unwrap();

        assert!(out.contains("a\n") && out.contains("b\n"), "got: {out}");
        let text = fs::read_to_string(home.join(".codex/xu-chat-providers.json")).unwrap();
        assert!(
            text.contains("\"old\""),
            "config must be unchanged without --apply"
        );
    }

    #[test]
    fn fetch_models_command_apply_writes_back_with_backup_and_metadata() {
        let server = mock_server(&[("/v1/models", 200, r#"{"data":[{"id":"a"},{"id":"b"}]}"#)]);
        let home = temp_home();
        write_config(
            &home,
            &format!(
                r#"{{"provider":{{"zen":{{"apiKind":"chat","options":{{"baseURL":"{}","apiKey":"k"}},"models":{{"a":{{"limit":{{"context":1000}}}},"old":{{}}}},"defaultModel":"old"}}}}}}"#,
                server.base_url
            ),
        );

        let out = provider_fetch_models_command(&home, &["zen".to_string(), "--apply".to_string()])
            .unwrap();

        assert!(out.contains("Updated provider models: zen"), "got: {out}");
        let root: Value = serde_json::from_str(
            &fs::read_to_string(home.join(".codex/xu-chat-providers.json")).unwrap(),
        )
        .unwrap();
        let zen = &root["provider"]["zen"];
        let models = zen["models"].as_object().unwrap();
        assert_eq!(models.len(), 2);
        assert!(models.contains_key("a") && models.contains_key("b"));
        assert!(!models.contains_key("old"));
        assert_eq!(models["a"]["limit"]["context"], 1000, "metadata preserved");
        assert_eq!(
            zen["defaultModel"], "a",
            "default falls back to first fetched"
        );
        let backups: Vec<_> = fs::read_dir(home.join(".codex"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".bak"))
            .collect();
        assert!(!backups.is_empty(), "no backup written before apply");
    }

    #[test]
    fn provider_update_models_json_replaces_models_map_and_default() {
        let home = temp_home();
        write_config(
            &home,
            r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://zen.example/v1"},"models":{"old":{}},"defaultModel":"old"}}}"#,
        );

        let out = provider_update_command(
            &home,
            &[
                "zen".to_string(),
                "--models-json".to_string(),
                r#"{"DeepSeek V4":{"name":"DeepSeek V4","requestName":"deepseek-v4-flash-free"},"plain":{}}"#
                    .to_string(),
            ],
        )
        .unwrap();

        assert!(out.contains("Updated provider: zen"), "got: {out}");
        let root: Value = serde_json::from_str(
            &fs::read_to_string(home.join(".codex/xu-chat-providers.json")).unwrap(),
        )
        .unwrap();
        let zen = &root["provider"]["zen"];
        let models = zen["models"].as_object().unwrap();
        assert_eq!(models.len(), 2);
        assert!(!models.contains_key("old"));
        assert_eq!(
            models["DeepSeek V4"]["requestName"],
            "deepseek-v4-flash-free"
        );
        assert_eq!(
            zen["defaultModel"], "DeepSeek V4",
            "default falls to first key"
        );
    }

    #[test]
    fn provider_update_models_json_accepts_array_and_preserves_order() {
        let home = temp_home();
        write_config(
            &home,
            r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://zen.example/v1"},"models":{"old":{}},"defaultModel":"old"}}}"#,
        );

        let out = provider_update_command(
            &home,
            &[
                "zen".to_string(),
                "--models-json".to_string(),
                r#"[{"name":"Zebra","requestName":"zebra-upstream"},"Alpha","Mid"]"#.to_string(),
            ],
        )
        .unwrap();

        assert!(out.contains("Updated provider: zen"), "got: {out}");
        let root: Value = serde_json::from_str(
            &fs::read_to_string(home.join(".codex/xu-chat-providers.json")).unwrap(),
        )
        .unwrap();
        let zen = &root["provider"]["zen"];
        let models = zen["models"].as_array().unwrap();
        assert_eq!(models.len(), 3);
        assert_eq!(
            models[0]["requestName"], "zebra-upstream",
            "mapped entry keeps its request name"
        );
        assert_eq!(
            zen["defaultModel"], "Zebra",
            "default falls to first array entry"
        );
    }

    #[test]
    fn provider_update_models_json_rejects_invalid_or_empty_object() {
        let home = temp_home();
        write_config(
            &home,
            r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://zen.example/v1"},"models":{"old":{}}}}}"#,
        );
        assert!(provider_update_command(
            &home,
            &[
                "zen".to_string(),
                "--models-json".to_string(),
                "not-json".to_string()
            ]
        )
        .is_err());
        assert!(provider_update_command(
            &home,
            &[
                "zen".to_string(),
                "--models-json".to_string(),
                r#"[{"noName":1}]"#.to_string()
            ]
        )
        .is_err());
        assert!(provider_update_command(
            &home,
            &[
                "zen".to_string(),
                "--models-json".to_string(),
                "[]".to_string()
            ]
        )
        .is_err());
        assert!(provider_update_command(
            &home,
            &[
                "zen".to_string(),
                "--models-json".to_string(),
                r#"[{"noName":1}]"#.to_string()
            ]
        )
        .is_err());
        let text = fs::read_to_string(home.join(".codex/xu-chat-providers.json")).unwrap();
        assert!(text.contains("\"old\""), "config unchanged on error");
    }

    #[test]
    fn provider_update_claude_slot_round_trips_through_store_parser() {
        let home = temp_home();
        write_config(
            &home,
            r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://zen.example/v1"},"models":{"old":{}},"defaultModel":"old"}}}"#,
        );

        provider_update_command(
            &home,
            &[
                "zen".to_string(),
                "--claude-slot".to_string(),
                "opus=root".to_string(),
            ],
        )
        .unwrap();

        let text = fs::read_to_string(home.join(".codex/xu-chat-providers.json")).unwrap();
        let profiles = crate::providers::store::profiles_from_xu_chat_json(&text).unwrap();
        assert_eq!(profiles[0].claude_slot("opus"), Some("root"));
    }

    fn write_config(home: &Path, text: &str) {
        fs::write(home.join(".codex/xu-chat-providers.json"), text).expect("write config");
    }
}

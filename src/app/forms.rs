use super::loaders::{run_mcp_command, run_skill_command};
use crate::config;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum SkillFormKind {
    #[default]
    Local,
    Zip,
    Github,
}

#[derive(Default)]
pub struct SkillForm {
    pub kind: SkillFormKind,
    pub id: String,
    pub name: String,
    pub path: String,
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub subdir: String,
    pub field: usize,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum McpFormKind {
    #[default]
    Create,
    Edit,
}

#[derive(Default)]
pub struct McpForm {
    pub kind: McpFormKind,
    pub id: String,
    pub name: String,
    pub transport: String,
    pub command: String,
    pub args: String,
    pub url: String,
    pub env: String,
    pub headers: String,
    pub description: String,
    pub field: usize,
    pub error: Option<String>,
}

pub fn skill_form_title(form: &SkillForm) -> &'static str {
    match form.kind {
        SkillFormKind::Local => "导入本地 Skills",
        SkillFormKind::Zip => "安装压缩包 Skills",
        SkillFormKind::Github => "从仓库安装 Skills",
    }
}

pub fn skill_form_fields(form: &SkillForm) -> Vec<(String, String, bool)> {
    let mut fields = vec![
        ("标识".to_string(), form.id.clone(), false),
        ("名称".to_string(), form.name.clone(), false),
    ];
    match form.kind {
        SkillFormKind::Local | SkillFormKind::Zip => {
            fields.push((
                if form.kind == SkillFormKind::Zip {
                    "压缩包路径".to_string()
                } else {
                    "路径".to_string()
                },
                form.path.clone(),
                false,
            ));
        }
        SkillFormKind::Github => {
            fields.extend([
                ("所有者".to_string(), form.owner.clone(), false),
                ("仓库名".to_string(), form.repo.clone(), false),
                ("分支".to_string(), form.branch.clone(), false),
                ("子目录".to_string(), form.subdir.clone(), false),
            ]);
        }
    }
    fields
}

pub fn skill_form_push(form: &mut SkillForm, value: char) {
    form.error = None;
    match form.kind {
        SkillFormKind::Local | SkillFormKind::Zip => match form.field {
            0 => form.id.push(value),
            1 => form.name.push(value),
            2 => form.path.push(value),
            _ => {}
        },
        SkillFormKind::Github => match form.field {
            0 => form.id.push(value),
            1 => form.name.push(value),
            2 => form.owner.push(value),
            3 => form.repo.push(value),
            4 => form.branch.push(value),
            5 => form.subdir.push(value),
            _ => {}
        },
    }
}

pub fn skill_form_pop(form: &mut SkillForm) {
    form.error = None;
    match form.kind {
        SkillFormKind::Local | SkillFormKind::Zip => match form.field {
            0 => {
                form.id.pop();
            }
            1 => {
                form.name.pop();
            }
            2 => {
                form.path.pop();
            }
            _ => {}
        },
        SkillFormKind::Github => match form.field {
            0 => {
                form.id.pop();
            }
            1 => {
                form.name.pop();
            }
            2 => {
                form.owner.pop();
            }
            3 => {
                form.repo.pop();
            }
            4 => {
                form.branch.pop();
            }
            5 => {
                form.subdir.pop();
            }
            _ => {}
        },
    }
}

pub fn skill_form_preview(form: &SkillForm) -> Result<(Vec<String>, String), String> {
    let args = skill_form_args(form)?;
    let preview = run_skill_command(&config::home(), &args);
    if let Some(error) = preview.strip_prefix("Skills 操作失败：") {
        return Err(error.to_string());
    }
    Ok((args, preview))
}

pub fn skill_form_args(form: &SkillForm) -> Result<Vec<String>, String> {
    let id = form.id.trim();
    if id.is_empty()
        || !id
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
    {
        return Err("Skills 标识只能包含字母、数字、-、_".to_string());
    }
    let name = if form.name.trim().is_empty() {
        id
    } else {
        form.name.trim()
    };
    match form.kind {
        SkillFormKind::Local => {
            let path = form.path.trim();
            if path.is_empty() {
                return Err("本地 Skills 路径不能为空".to_string());
            }
            Ok(vec![
                "skill".to_string(),
                "import".to_string(),
                id.to_string(),
                "--path".to_string(),
                path.to_string(),
                "--name".to_string(),
                name.to_string(),
            ])
        }
        SkillFormKind::Zip => {
            let path = form.path.trim();
            if path.is_empty() {
                return Err("ZIP 路径不能为空".to_string());
            }
            Ok(vec![
                "skill".to_string(),
                "install-zip".to_string(),
                id.to_string(),
                "--path".to_string(),
                path.to_string(),
                "--name".to_string(),
                name.to_string(),
            ])
        }
        SkillFormKind::Github => {
            let owner = form.owner.trim();
            let repo = form.repo.trim();
            if owner.is_empty() || repo.is_empty() {
                return Err("GitHub owner/repo 不能为空".to_string());
            }
            let branch = if form.branch.trim().is_empty() {
                "main"
            } else {
                form.branch.trim()
            };
            let mut args = vec![
                "skill".to_string(),
                "install-github".to_string(),
                id.to_string(),
                "--owner".to_string(),
                owner.to_string(),
                "--repo".to_string(),
                repo.to_string(),
                "--branch".to_string(),
                branch.to_string(),
                "--name".to_string(),
                name.to_string(),
            ];
            if !form.subdir.trim().is_empty() {
                args.extend(["--subdir".to_string(), form.subdir.trim().to_string()]);
            }
            Ok(args)
        }
    }
}

pub fn mcp_form_title(form: &McpForm) -> &'static str {
    match form.kind {
        McpFormKind::Create => "新增 MCP 服务",
        McpFormKind::Edit => "编辑 MCP 服务",
    }
}

pub fn mcp_edit_form(server: &spec::mcp::McpServer) -> McpForm {
    McpForm {
        kind: McpFormKind::Edit,
        id: server.id.clone(),
        name: server.name.clone(),
        transport: match server.transport {
            spec::mcp::McpTransport::Stdio => "stdio".to_string(),
            spec::mcp::McpTransport::Http => "http".to_string(),
            spec::mcp::McpTransport::Sse => "sse".to_string(),
        },
        command: server.command.clone().unwrap_or_default(),
        args: server.args.join(" "),
        url: server.url.clone().unwrap_or_default(),
        env: server
            .env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(";"),
        headers: server
            .headers
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(";"),
        description: server.description.clone().unwrap_or_default(),
        field: 1,
        error: None,
    }
}

pub fn mcp_form_fields(form: &McpForm) -> Vec<(String, String, bool)> {
    let remote = matches!(form.transport.trim(), "http" | "sse");
    vec![
        (
            "标识".to_string(),
            form.id.clone(),
            form.kind == McpFormKind::Edit,
        ),
        ("名称".to_string(), form.name.clone(), false),
        ("传输方式".to_string(), form.transport.clone(), false),
        ("命令".to_string(), form.command.clone(), remote),
        ("参数".to_string(), form.args.clone(), remote),
        ("地址".to_string(), form.url.clone(), !remote),
        ("环境变量".to_string(), form.env.clone(), remote),
        ("请求头".to_string(), form.headers.clone(), !remote),
        ("说明".to_string(), form.description.clone(), false),
    ]
}

pub fn mcp_form_push(form: &mut McpForm, value: char) {
    form.error = None;
    if form.kind == McpFormKind::Edit && form.field == 0 {
        return;
    }
    match form.field {
        0 => form.id.push(value),
        1 => form.name.push(value),
        2 => form.transport.push(value),
        3 => form.command.push(value),
        4 => form.args.push(value),
        5 => form.url.push(value),
        6 => form.env.push(value),
        7 => form.headers.push(value),
        8 => form.description.push(value),
        _ => {}
    }
}

pub fn mcp_form_pop(form: &mut McpForm) {
    form.error = None;
    if form.kind == McpFormKind::Edit && form.field == 0 {
        return;
    }
    match form.field {
        0 => {
            form.id.pop();
        }
        1 => {
            form.name.pop();
        }
        2 => {
            form.transport.pop();
        }
        3 => {
            form.command.pop();
        }
        4 => {
            form.args.pop();
        }
        5 => {
            form.url.pop();
        }
        6 => {
            form.env.pop();
        }
        7 => {
            form.headers.pop();
        }
        8 => {
            form.description.pop();
        }
        _ => {}
    }
}

pub fn mcp_form_preview(form: &McpForm) -> Result<(Vec<String>, String), String> {
    let args = mcp_form_args(form)?;
    let preview = run_mcp_command(&config::home(), &args);
    if let Some(error) = preview.strip_prefix("MCP 操作失败：") {
        return Err(error.to_string());
    }
    Ok((args, preview))
}

pub fn mcp_form_args(form: &McpForm) -> Result<Vec<String>, String> {
    let id = form.id.trim();
    if id.is_empty()
        || !id
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
    {
        return Err("MCP ID 只能包含 ASCII 字母、数字、-、_".to_string());
    }
    let name = if form.name.trim().is_empty() {
        id
    } else {
        form.name.trim()
    };
    let transport = form.transport.trim().to_ascii_lowercase();
    if !matches!(transport.as_str(), "stdio" | "http" | "sse") {
        return Err("Transport 只能是 stdio、http 或 sse".to_string());
    }
    let mut args = vec![
        "mcp".to_string(),
        match form.kind {
            McpFormKind::Create => "add".to_string(),
            McpFormKind::Edit => "update".to_string(),
        },
        id.to_string(),
        "--name".to_string(),
        name.to_string(),
        "--transport".to_string(),
        transport.clone(),
    ];
    match transport.as_str() {
        "stdio" => {
            let command = form.command.trim();
            if command.is_empty() {
                return Err("stdio MCP 需要 command".to_string());
            }
            args.extend(["--command".to_string(), command.to_string()]);
            let tokens = shell_like_tokens(&form.args);
            if tokens.is_empty() {
                if form.kind == McpFormKind::Edit {
                    args.push("--clear-args".to_string());
                }
            } else {
                for token in tokens {
                    args.extend(["--arg".to_string(), token]);
                }
            }
            let env_pairs = semicolon_pairs(&form.env)?;
            if env_pairs.is_empty() {
                if form.kind == McpFormKind::Edit {
                    args.push("--clear-env".to_string());
                }
            } else {
                for pair in env_pairs {
                    args.extend(["--env".to_string(), pair]);
                }
            }
        }
        _ => {
            let url = form.url.trim();
            if url.is_empty() {
                return Err("远程 MCP 需要 URL".to_string());
            }
            args.extend(["--url".to_string(), url.to_string()]);
            let headers = semicolon_pairs(&form.headers)?;
            if headers.is_empty() {
                if form.kind == McpFormKind::Edit {
                    args.push("--clear-headers".to_string());
                }
            } else {
                for pair in headers {
                    args.extend(["--header".to_string(), pair]);
                }
            }
        }
    }
    if !form.description.trim().is_empty() {
        args.extend([
            "--description".to_string(),
            form.description.trim().to_string(),
        ]);
    }
    Ok(args)
}

pub fn shell_like_tokens(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn semicolon_pairs(value: &str) -> Result<Vec<String>, String> {
    let mut pairs = Vec::new();
    for part in value.split([';', ',']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if !part.contains('=') {
            return Err(format!("键值对必须使用 key=value：{part}"));
        }
        pairs.push(part.to_string());
    }
    Ok(pairs)
}

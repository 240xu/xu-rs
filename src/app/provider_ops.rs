use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::config;
use crate::ProviderState;
use spec::agents::AgentTarget;
use spec::domain::{ProtocolKind, ProviderProfile};
use spec::patch::PatchOptions;

/// One row of the CC Switch style model mapping table: the client-facing
/// display name (col 1) and the upstream request name (col 2).
/// `client_name` is the stable local client key (base of the
/// `<client_name>_<provider_id>` slug) used for row identity and mutation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderModelRow {
    pub client_name: String,
    pub display_name: String,
    pub request_name: String,
    /// 逗号分隔的 opencode 思考强度档位（low,medium,high,xhigh,max）。
    /// 序列化为 `"variants": {"low": {}, ...}` 对象，空串表示无档位。
    pub variants: String,
}

#[derive(Default)]
pub struct ProviderAddForm {
    pub id: String,
    pub name: String,
    pub kind_index: usize,
    pub base_url: String,
    pub api_key: String,
    pub models: String,
    pub model_rows: Vec<ProviderModelRow>,
    /// Focused table cell as (row, column); column 0 = display name, 1 = request name.
    pub model_cell: Option<(usize, usize)>,
    pub model_fetch_msg: Option<String>,
    pub bypass_permissions: bool,
    pub default_model: String,
    pub timeout_ms: u64,
    pub max_retries: u32,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub reasoning_index: usize,
    pub headers: String,
    pub website: String,
    pub notes: String,
    pub page: usize,
    pub field: usize,
    pub error: Option<String>,
}

pub fn build_plan_preview_for_ids(
    state: &ProviderState,
    target: AgentTarget,
    provider_ids: &[String],
) -> String {
    if provider_ids.is_empty() {
        return "没有选中的 provider。".to_string();
    }
    let home = config::home();
    let plan = match spec::agents::apply_agent(&home, target, &state.providers, provider_ids) {
        Ok(plan) => plan,
        Err(error) => return format!("无法生成计划：{error}"),
    };

    let mut out = String::new();
    out.push_str(plan.target.label());
    out.push('\n');
    out.push_str("模式：");
    out.push_str(plan.routing_mode.as_str());
    out.push('\n');
    out.push_str("供应商顺序：");
    out.push_str(&provider_ids.join(" -> "));
    out.push('\n');
    for line in &plan.summary {
        out.push_str("- ");
        out.push_str(line);
        out.push('\n');
    }
    for warning in &plan.warnings {
        out.push_str("! ");
        out.push_str(warning);
        out.push('\n');
    }

    out.push_str("\n将修改：\n");
    for patch in &plan.patches {
        out.push_str("- ");
        out.push_str(&patch.path.display().to_string());
        out.push('\n');
    }

    out.push_str("\nDiff 预览：\n");
    for patch in &plan.patches {
        match spec::patch::apply_patch(
            patch,
            PatchOptions {
                dry_run: true,
                backup: true,
            },
        ) {
            Ok(result) => {
                out.push_str(&result.path.display().to_string());
                out.push('\n');
                for line in result.diff.lines().take(16) {
                    out.push_str(line);
                    out.push('\n');
                }
            }
            Err(error) => {
                out.push_str("diff failed: ");
                out.push_str(&error.to_string());
                out.push('\n');
            }
        }
    }
    out.push_str("\n当前只是 预览，不会写文件。按 a 进入应用确认，Esc 返回。");
    out
}

pub fn selected_provider_ids(state: &ProviderState) -> Vec<String> {
    state
        .providers
        .get(state.selected)
        .map(|provider| vec![provider.id.clone()])
        .unwrap_or_default()
}

pub fn toggle_multi_provider(state: &ProviderState, index: usize, selected: &mut Vec<String>) {
    let Some(provider) = state.providers.get(index) else {
        return;
    };
    if let Some(position) = selected.iter().position(|id| id == &provider.id) {
        selected.remove(position);
    } else {
        selected.push(provider.id.clone());
    }
}

pub fn move_multi_provider(
    state: &ProviderState,
    selected: &mut [String],
    provider_cursor: usize,
    down: bool,
) {
    let Some(provider_id) = state
        .providers
        .get(provider_cursor)
        .map(|provider| provider.id.clone())
    else {
        return;
    };
    let Some(position) = selected.iter().position(|id| id == &provider_id) else {
        return;
    };
    let target = if down {
        (position + 1).min(selected.len().saturating_sub(1))
    } else {
        position.saturating_sub(1)
    };
    selected.swap(position, target);
}

pub fn confirm_message(
    state: &ProviderState,
    target: Option<AgentTarget>,
    provider_ids: &[String],
) -> String {
    if provider_ids.is_empty() {
        return "没有选中的 provider。按 Esc 返回。".to_string();
    }
    let Some(target) = target else {
        return "没有选中的目标 agent。按 Esc 返回。".to_string();
    };

    format!(
        "即将把供应商 [{}] 按此顺序应用到 {}。\n\n{}\n这会写入真实配置文件，并在原文件存在时自动创建备份。\n密钥 不会在 diff 中明文显示，但会写入目标 agent 所需配置。\n\n按 y 确认应用，按 n 或 Esc 取消。",
        provider_ids
            .iter()
            .map(|id| {
                state
                    .providers
                    .iter()
                    .find(|provider| &provider.id == id)
                    .map(|provider| provider.name.as_str())
                    .unwrap_or(id)
            })
            .collect::<Vec<_>>()
            .join(" -> "),
        target.label(),
        if provider_ids.len() > 1 { "多供应商模式需要 spec Runtime；这是模型路由，不是自动故障转移。\n" } else { "单供应商模式会自动选择直连或 spec Runtime。\n" },
    )
}

pub fn provider_delete_confirm_message(state: &ProviderState) -> String {
    let Some(provider) = state.providers.get(state.selected) else {
        return "没有选中的 provider。按 Esc 返回。".to_string();
    };
    format!(
        "将删除 provider：{} ({})\n\n删除会修改 ~/.codex/xu-chat-providers.json，并自动备份原文件。\n如果该 provider 当前正在被任何目标使用，删除会被拒绝。\n\n按 y 确认删除，按 n 或 Esc 取消。",
        provider.name, provider.id
    )
}

pub fn provider_add_form_from_preset(
    preset: &spec::provider_presets::ProviderPreset,
) -> ProviderAddForm {
    ProviderAddForm {
        id: String::new(),
        name: preset.name.to_string(),
        kind_index: match preset.protocol {
            ProtocolKind::OpenAiChat => 0,
            ProtocolKind::OpenAiResponses => 1,
            ProtocolKind::AnthropicMessages => 2,
        },
        base_url: preset.base_url.to_string(),
        api_key: String::new(),
        models: preset.models.join(", "),
        model_rows: preset
            .models
            .iter()
            .map(|model| ProviderModelRow {
                client_name: model.to_string(),
                display_name: model.to_string(),
                request_name: model.to_string(),
                variants: String::new(),
            })
            .collect(),
        model_cell: None,
        model_fetch_msg: None,
        bypass_permissions: read_bypass_permissions(&config::home()),
        default_model: preset.models[0].to_string(),
        timeout_ms: 60000,
        max_retries: 10,
        context_window: 128000,
        max_output_tokens: 32768,
        reasoning_index: 0,
        headers: String::new(),
        website: String::new(),
        notes: String::new(),
        page: 0,
        field: 0,
        error: None,
    }
}

pub fn provider_add_kind(form: &ProviderAddForm) -> &'static str {
    ["chat", "responses", "anthropic"]
        .get(form.kind_index)
        .copied()
        .unwrap_or("chat")
}

pub fn provider_add_fields(form: &ProviderAddForm) -> Vec<(usize, String, String, bool)> {
    vec![
        (0, "id（留空自动生成）".to_string(), form.id.clone(), false),
        (1, "name".to_string(), form.name.clone(), false),
        (
            2,
            "API 协议".to_string(),
            provider_add_kind(form).to_string(),
            true,
        ),
        (3, "Base URL".to_string(), form.base_url.clone(), false),
        (
            4,
            "API Key（可留空，本地/免费端点不填）".to_string(),
            if form.api_key.is_empty() {
                String::new()
            } else {
                format!("<已输入 {} 字符>", form.api_key.chars().count())
            },
            false,
        ),
        (
            5,
            "模型列表（逗号分隔）".to_string(),
            form.models.clone(),
            false,
        ),
    ]
}

pub fn select_add_field(form: &mut ProviderAddForm, index: usize) {
    if index < 6 {
        form.field = index;
    }
}

/// 生成稳定的 ASCII 供应商 id：显示名称小写、非字母数字折叠为连字符。
/// 纯中文等无法生成 slug 时，回退到基于名称的稳定短哈希，保证预览与提交一致。
pub fn provider_id_from_name(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut pending_dash = false;
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(c);
            pending_dash = false;
        } else if !slug.is_empty() {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        let mut hash = 0x9e37_79b9_7f4a_7c15u64;
        for b in name.trim().bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("provider-{:04x}", (hash >> 32) & 0xffff)
    } else {
        slug
    }
}

pub fn provider_reasoning(form: &ProviderAddForm) -> &'static str {
    ["medium", "low", "high", "xhigh", "max"]
        .get(form.reasoning_index)
        .copied()
        .unwrap_or("medium")
}

pub fn reasoning_index(value: Option<&str>) -> usize {
    match value.unwrap_or("medium") {
        "low" => 1,
        "high" => 2,
        "xhigh" => 3,
        "max" => 4,
        _ => 0,
    }
}

pub fn edit_page_fields(page: usize) -> &'static [usize] {
    match page {
        // A3：协议/BaseURL/Key 详情页已就地编辑，「连接」页收缩为仅名称。
        0 => &[1],
        // Page 1 (模型) is the mapping table + search + toolbar; only the
        // default-model field stays in the regular field list.
        1 => &[6],
        2 => &[7, 8, 9, 10],
        3 => &[11, 12],
        _ => &[13, 14],
    }
}

pub fn provider_edit_fields(form: &ProviderAddForm) -> Vec<(usize, String, String, bool)> {
    edit_page_fields(form.page)
        .iter()
        .map(|field| match field {
            1 => (1, "显示名称".to_string(), form.name.clone(), false),
            2 => (
                2,
                "API 协议".to_string(),
                provider_add_kind(form).to_string(),
                true,
            ),
            3 => (3, "Base URL".to_string(), form.base_url.clone(), false),
            4 => (
                4,
                "API Key（留空保留）".to_string(),
                if form.api_key.is_empty() {
                    "<保留现有密钥>".to_string()
                } else {
                    format!("<已输入 {} 字符>", form.api_key.chars().count())
                },
                false,
            ),
            5 => (
                5,
                "模型列表（逗号分隔）".to_string(),
                form.models.clone(),
                false,
            ),
            6 => (6, "默认模型".to_string(), form.default_model.clone(), false),
            7 => (
                7,
                "请求超时".to_string(),
                format!("{} ms", form.timeout_ms),
                true,
            ),
            8 => (
                8,
                "最大重试".to_string(),
                form.max_retries.to_string(),
                true,
            ),
            9 => (
                9,
                "上下文窗口".to_string(),
                form.context_window.to_string(),
                true,
            ),
            10 => (
                10,
                "最大输出 tokens".to_string(),
                form.max_output_tokens.to_string(),
                true,
            ),
            11 => (
                11,
                "Reasoning effort".to_string(),
                provider_reasoning(form).to_string(),
                true,
            ),
            12 => (
                12,
                "自定义 Headers（分号分隔）".to_string(),
                redact_headers_for_display(&form.headers),
                false,
            ),
            13 => (13, "供应商网站".to_string(), form.website.clone(), false),
            _ => (14, "备注".to_string(), form.notes.clone(), false),
        })
        .collect()
}

pub fn select_adjacent_edit_field(form: &mut ProviderAddForm, forward: bool) {
    let fields = edit_page_fields(form.page);
    let position = fields
        .iter()
        .position(|field| *field == form.field)
        .unwrap_or(0);
    form.field = if forward {
        fields[(position + 1).min(fields.len() - 1)]
    } else {
        fields[position.saturating_sub(1)]
    };
}

pub fn adjust_provider_edit_field(form: &mut ProviderAddForm, increase: bool) {
    form.error = None;
    match form.field {
        2 => {
            form.kind_index = if increase {
                (form.kind_index + 1) % 3
            } else {
                (form.kind_index + 2) % 3
            };
        }
        7 => form.timeout_ms = adjust_u64(form.timeout_ms, 15_000, 5_000, 600_000, increase),
        8 => form.max_retries = adjust_u64(form.max_retries as u64, 1, 0, 20, increase) as u32,
        9 => {
            form.context_window = adjust_u64(
                form.context_window as u64,
                16_000,
                16_000,
                2_000_000,
                increase,
            ) as u32
        }
        10 => {
            form.max_output_tokens = adjust_u64(
                form.max_output_tokens as u64,
                4_096,
                1_024,
                256_000,
                increase,
            ) as u32
        }
        11 => {
            form.reasoning_index = if increase {
                (form.reasoning_index + 1) % 5
            } else {
                (form.reasoning_index + 4) % 5
            };
        }
        _ => {}
    }
}

pub fn adjust_u64(value: u64, step: u64, minimum: u64, maximum: u64, increase: bool) -> u64 {
    if increase {
        value.saturating_add(step).min(maximum)
    } else {
        value.saturating_sub(step).max(minimum)
    }
}

pub fn split_provider_headers(value: &str) -> Result<Vec<String>, String> {
    let mut headers = Vec::new();
    for header in value
        .split(';')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let (name, content) = header
            .split_once(':')
            .ok_or_else(|| "Header 必须使用 Name: Value，多个用分号分隔".to_string())?;
        if name.trim().is_empty() || content.trim().is_empty() {
            return Err("Header 名称和值不能为空".to_string());
        }
        headers.push(format!("{}: {}", name.trim(), content.trim()));
    }
    Ok(headers)
}

pub fn redact_headers_for_display(value: &str) -> String {
    value
        .split(';')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|header| {
            let Some((name, content)) = header.split_once(':') else {
                return header.to_string();
            };
            let lower = name.trim().to_ascii_lowercase();
            if ["authorization", "token", "secret", "api-key", "api_key"]
                .iter()
                .any(|sensitive| lower.contains(sensitive))
            {
                format!("{}: <hidden>", name.trim())
            } else {
                format!("{}: {}", name.trim(), content.trim())
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// 新增表单「模型列表」文本与表格行保持同步：文本每次变化后按逗号重建
/// 表格行（显示名=请求名），并确保默认模型仍指向表格中的行（缺失时回退
/// 到首行，与 preset 初始化语义一致）。
fn sync_add_models_from_text(form: &mut ProviderAddForm) {
    let models = split_provider_models(&form.models).unwrap_or_default();
    form.model_rows = models
        .iter()
        .map(|model| ProviderModelRow {
            client_name: model.clone(),
            display_name: model.clone(),
            request_name: model.clone(),
            variants: String::new(),
        })
        .collect();
    if !form
        .model_rows
        .iter()
        .any(|row| row.display_name == form.default_model)
    {
        form.default_model = form
            .model_rows
            .first()
            .map(|row| row.display_name.clone())
            .unwrap_or_default();
    }
}

pub fn provider_add_form_push(form: &mut ProviderAddForm, c: char) {
    form.error = None;
    if form.field == 2 {
        match c {
            '1' => form.kind_index = 0,
            '2' => form.kind_index = 1,
            '3' => form.kind_index = 2,
            _ => {}
        }
        return;
    }
    match form.field {
        0 => form.id.push(c),
        1 => form.name.push(c),
        3 => form.base_url.push(c),
        4 => form.api_key.push(c),
        5 => form.models.push(c),
        _ => {}
    }
    if form.field == 5 {
        sync_add_models_from_text(form);
    }
}

pub fn provider_add_form_pop(form: &mut ProviderAddForm) {
    form.error = None;
    match form.field {
        0 => {
            form.id.pop();
        }
        1 => {
            form.name.pop();
        }
        3 => {
            form.base_url.pop();
        }
        4 => {
            form.api_key.pop();
        }
        5 => {
            form.models.pop();
        }
        _ => {}
    }
    if form.field == 5 {
        sync_add_models_from_text(form);
    }
}

pub fn provider_edit_form_push(form: &mut ProviderAddForm, c: char) {
    form.error = None;
    if form.field == 0 {
        return;
    }
    match form.field {
        1 => form.name.push(c),
        3 => form.base_url.push(c),
        4 => form.api_key.push(c),
        5 => form.models.push(c),
        6 => form.default_model.push(c),
        12 => form.headers.push(c),
        13 => form.website.push(c),
        14 => form.notes.push(c),
        _ => {}
    }
}

pub fn provider_edit_form_pop(form: &mut ProviderAddForm) {
    form.error = None;
    if form.field == 0 {
        return;
    }
    match form.field {
        1 => {
            form.name.pop();
        }
        3 => {
            form.base_url.pop();
        }
        4 => {
            form.api_key.pop();
        }
        5 => {
            form.models.pop();
        }
        6 => {
            form.default_model.pop();
        }
        12 => {
            form.headers.pop();
        }
        13 => {
            form.website.pop();
        }
        14 => {
            form.notes.pop();
        }
        _ => {}
    }
}

/// Case-insensitive substring match against display name or request name.
/// An empty/whitespace search matches everything.
/// 校准模型表格的单元格焦点：行号越出当前行数（表格被删除/替换后）
/// 时把焦点移到第一个单元格；表格全空时清空单元格焦点，回到默认模型
/// 字段，保证后续击键不会落在不存在的行上被静默丢弃。
pub fn reconcile_model_cell_focus(form: &mut ProviderAddForm) {
    let Some((row, col)) = form.model_cell else {
        return;
    };
    if row < form.model_rows.len() {
        return;
    }
    if form.model_rows.is_empty() {
        form.model_cell = None;
        form.field = 6;
    } else {
        form.model_cell = Some((0, col));
    }
}

/// Move focus on the model page. Order: cells (row-major) ->
/// default-model field -> cells.
pub fn advance_model_focus(form: &mut ProviderAddForm, forward: bool) {
    // 行数变化可能已让焦点行失效：先校准，保证导航只在现有行内移动。
    reconcile_model_cell_focus(form);
    if let Some((row, col)) = form.model_cell {
        if forward {
            if col < 2 {
                form.model_cell = Some((row, col + 1));
            } else if row + 1 < form.model_rows.len() {
                form.model_cell = Some((row + 1, 0));
            } else {
                form.model_cell = None;
                form.field = 6;
            }
        } else if col > 0 {
            form.model_cell = Some((row, col - 1));
        } else if row > 0 {
            form.model_cell = Some((row - 1, 2));
        } else {
            form.model_cell = None;
            form.field = 6;
        }
    } else if !form.model_rows.is_empty() {
        // 默认模型字段（或刚切页后的任意字段）：前/后都回到表格单元格。
        if forward {
            form.model_cell = Some((0, 0));
        } else {
            form.model_cell = Some((form.model_rows.len() - 1, 2));
        }
    }
}

/// Switch the focused table cell between the display (0), request (1) and
/// variants (2) columns (cyclic).
pub fn model_switch_column(form: &mut ProviderAddForm, right: bool) {
    if let Some((row, col)) = form.model_cell {
        let next = if right { (col + 1) % 3 } else { (col + 2) % 3 };
        form.model_cell = Some((row, next));
    }
}

/// Focus a regular form field, clearing the table cell focus so subsequent
/// keystrokes reach the field instead of the last cell.
pub fn focus_edit_field(form: &mut ProviderAddForm, field: usize) {
    form.field = field;
    form.model_cell = None;
}

/// Focus a table cell, yielding the regular field focus.
pub fn focus_model_cell(form: &mut ProviderAddForm, row: usize, column: usize) {
    form.model_cell = Some((row, column));
    form.field = 6;
}

/// Typing on the model page: edits the focused cell text.
pub fn model_cell_push(form: &mut ProviderAddForm, c: char) {
    form.error = None;
    if form.model_cell.is_some() {
        // 焦点行可能已越界（表格被替换/删除）：先校准，让击键落到现有行。
        reconcile_model_cell_focus(form);
        if let Some((row, col)) = form.model_cell {
            if let Some(entry) = form.model_rows.get_mut(row) {
                match col {
                    0 => entry.display_name.push(c),
                    1 => entry.request_name.push(c),
                    _ => entry.variants.push(c),
                }
            }
        }
        return;
    }
    provider_edit_form_push(form, c);
}

/// Backspace on the model page: pops the focused cell.
pub fn model_cell_pop(form: &mut ProviderAddForm) {
    form.error = None;
    if form.model_cell.is_some() {
        // 焦点行可能已越界：先校准，再弹字。
        reconcile_model_cell_focus(form);
        if let Some((row, col)) = form.model_cell {
            if let Some(entry) = form.model_rows.get_mut(row) {
                match col {
                    0 => entry.display_name.pop(),
                    1 => entry.request_name.pop(),
                    _ => entry.variants.pop(),
                };
            }
        }
        return;
    }
    provider_edit_form_pop(form);
}

/// Append an empty row (clearing the filter so it is visible) and focus its
/// display cell.
pub fn model_add_row(form: &mut ProviderAddForm) {
    form.error = None;
    form.model_rows.push(ProviderModelRow {
        client_name: String::new(),
        display_name: String::new(),
        request_name: String::new(),
        variants: String::new(),
    });
    form.model_cell = Some((form.model_rows.len().saturating_sub(1), 0));
    form.field = 6;
}

/// Remove the focused row; focus moves to the row that takes its place (or to
/// the search box when the table becomes empty).
pub fn model_delete_focused(form: &mut ProviderAddForm) {
    form.error = None;
    let Some((row, col)) = form.model_cell else {
        return;
    };
    if row < form.model_rows.len() {
        form.model_rows.remove(row);
    }
    if form.model_rows.is_empty() {
        form.model_cell = None;
        form.field = 6;
    } else if row >= form.model_rows.len() {
        form.model_cell = Some((form.model_rows.len() - 1, col));
    }
}

pub fn build_provider_add_preview(form: &ProviderAddForm) -> Result<String, String> {
    let args = provider_add_args(form, true)?;
    match spec::cli::run_command(&config::home(), &args) {
        Some(Ok(output)) => Ok(format!(
            "{output}\n\n即将添加 provider：{}\n协议：{}\nBase URL：{}\n模型：{}\n密钥：<hidden>\n\n这一步还没有写文件。按 y 确认添加，按 n 或 Esc 返回编辑。",
            form.id.trim(),
            provider_add_kind(form),
            form.base_url.trim(),
            model_rows_preview(form),
        )),
        Some(Err(error)) => Err(error),
        None => Err("provider add 命令不可用".to_string()),
    }
}

pub fn add_provider_from_form(form: &ProviderAddForm) -> String {
    let args = match provider_add_args(form, false) {
        Ok(args) => args,
        Err(error) => return format!("添加失败：{error}\n\nEsc 返回供应商列表。"),
    };
    match spec::cli::run_command(&config::home(), &args) {
        Some(Ok(output)) => format!("{output}\n\n已备份 供应商配置。Esc 返回供应商列表。"),
        Some(Err(error)) => format!("添加失败：{error}\n\nEsc 返回供应商列表。"),
        None => "添加失败：provider add 命令不可用。".to_string(),
    }
}

pub fn edit_form_from_selected_provider(state: &ProviderState) -> Option<ProviderAddForm> {
    let provider = state.providers.get(state.selected)?;
    Some(ProviderAddForm {
        id: provider.id.clone(),
        name: provider.name.clone(),
        kind_index: match provider.protocol {
            ProtocolKind::OpenAiChat => 0,
            ProtocolKind::OpenAiResponses => 1,
            ProtocolKind::AnthropicMessages => 2,
        },
        base_url: provider.base_url.clone(),
        api_key: String::new(),
        models: provider.models.join(", "),
        model_rows: provider
            .models
            .iter()
            .map(|model| {
                let entry = provider.model_entries.get(model);
                ProviderModelRow {
                    client_name: model.clone(),
                    display_name: entry
                        .map(|entry| entry.display_name.clone())
                        .unwrap_or_else(|| model.clone()),
                    request_name: entry
                        .map(|entry| entry.request_name.clone())
                        .unwrap_or_else(|| model.clone()),
                    variants: variants_from_metadata(provider, model),
                }
            })
            .collect(),
        model_cell: None,
        model_fetch_msg: None,
        bypass_permissions: read_bypass_permissions(&config::home()),
        default_model: provider.default_model.clone(),
        timeout_ms: provider.timeout_ms,
        max_retries: provider.max_retries,
        context_window: provider.context_window,
        max_output_tokens: provider.max_output_tokens,
        reasoning_index: reasoning_index(provider.reasoning_effort.as_deref()),
        headers: provider
            .extra_headers
            .iter()
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join("; "),
        website: provider.website.clone().unwrap_or_default(),
        notes: provider.notes.clone().unwrap_or_default(),
        page: 0,
        field: 1,
        error: None,
    })
}

pub fn build_provider_update_preview(form: &ProviderAddForm) -> Result<String, String> {
    let args = provider_update_args(form, true)?;
    let resolved_id = if form.id.trim().is_empty() {
        provider_id_from_name(form.name.as_str())
    } else {
        form.id.trim().to_string()
    };
    match spec::cli::run_command(&config::home(), &args) {
        Some(Ok(output)) => Ok(format!(
            "{output}\n\n即将更新 provider：{}\n协议：{}\nBase URL：{}\n默认模型：{}\n模型：{}\n超时/重试：{} ms / {}\n上下文/输出：{} / {}\nReasoning：{}\n密钥：{}\n\n这一步还没有写文件。按 y 确认更新，按 n 或 Esc 返回编辑。",
            resolved_id,
            provider_add_kind(form),
            form.base_url.trim(),
            form.default_model.trim(),
            model_rows_preview(form),
            form.timeout_ms,
            form.max_retries,
            form.context_window,
            form.max_output_tokens,
            provider_reasoning(form),
            if form.api_key.trim().is_empty() { "<preserve existing>" } else { "<hidden>" },
        )),
        Some(Err(error)) => Err(error),
        None => Err("provider update 命令不可用".to_string()),
    }
}

pub fn update_provider_from_form(form: &ProviderAddForm) -> String {
    let args = match provider_update_args(form, false) {
        Ok(args) => args,
        Err(error) => return format!("更新失败：{error}\n\nEsc 返回供应商列表。"),
    };
    match spec::cli::run_command(&config::home(), &args) {
        Some(Ok(output)) => format!("{output}\n\n已备份 供应商配置。Esc 返回供应商列表。"),
        Some(Err(error)) => format!("更新失败：{error}\n\nEsc 返回供应商列表。"),
        None => "更新失败：provider update 命令不可用。".to_string(),
    }
}

pub fn provider_update_args(form: &ProviderAddForm, dry_run: bool) -> Result<Vec<String>, String> {
    let id = if form.id.trim().is_empty() {
        provider_id_from_name(form.name.as_str())
    } else {
        form.id.trim().to_string()
    };
    let base_url = form.base_url.trim();
    if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
        return Err("baseURL 必须以 http:// 或 https:// 开头".to_string());
    }
    let models_json = model_rows_json(form)?;
    // 默认模型以稳定 client key 为准（store 按 models 的 client key 校验）；
    // 同时接受显示名输入，落盘前解析回 client key。两遍查找：第一遍按
    // 非空 client key 匹配全部行，第二遍才按显示名回退——避免某行显示名
    // 恰好等于另一行 client key 时默认模型被解析到错误的行。
    let default_model = form.default_model.trim();
    let default_client = form
        .model_rows
        .iter()
        .find(|row| {
            let client = row.client_name.trim();
            !client.is_empty() && client == default_model
        })
        .or_else(|| {
            form.model_rows.iter().find(|row| {
                let display = row.display_name.trim();
                !display.is_empty() && display == default_model
            })
        })
        .map(|row| {
            let client = row.client_name.trim();
            if client.is_empty() {
                // 新增行的 client key 为空：model_rows_json 折叠后 store 解析器
                // 用显示名派生 client key，此处回退发出显示名（= 用户输入），
                // 避免发出 "" 被 validate_for_write 放行而静默丢弃默认模型。
                row.display_name.trim().to_string()
            } else {
                client.to_string()
            }
        });
    if default_client.is_none() {
        return Err("默认模型必须包含在模型列表中".to_string());
    }
    let mut args = vec![
        "provider".to_string(),
        "update".to_string(),
        id.clone(),
        "--kind".to_string(),
        provider_add_kind(form).to_string(),
        "--base-url".to_string(),
        base_url.to_string(),
        "--default-model".to_string(),
        default_client.unwrap_or_default(),
        "--models-json".to_string(),
        models_json,
        "--timeout".to_string(),
        form.timeout_ms.to_string(),
        "--max-retries".to_string(),
        form.max_retries.to_string(),
        "--context-window".to_string(),
        form.context_window.to_string(),
        "--max-output-tokens".to_string(),
        form.max_output_tokens.to_string(),
        "--reasoning-effort".to_string(),
        provider_reasoning(form).to_string(),
    ];
    let name = form.name.trim();
    if !name.is_empty() {
        args.push("--name".to_string());
        args.push(name.to_string());
    }
    let api_key = form.api_key.trim();
    if !api_key.is_empty() {
        args.push("--api-key".to_string());
        args.push(api_key.to_string());
    }
    if !form.website.trim().is_empty() {
        args.extend(["--website".to_string(), form.website.trim().to_string()]);
    } else {
        args.extend(["--website".to_string(), String::new()]);
    }
    if !form.notes.trim().is_empty() {
        args.extend(["--notes".to_string(), form.notes.trim().to_string()]);
    } else {
        args.extend(["--notes".to_string(), String::new()]);
    }
    let headers = split_provider_headers(&form.headers)?;
    if headers.is_empty() {
        args.push("--clear-headers".to_string());
    } else {
        for header in headers {
            args.extend(["--header".to_string(), header]);
        }
    }
    if dry_run {
        args.push("--预览".to_string());
    }
    Ok(args)
}

/// Preview text for the mapping table: `显示名 → 请求名` when they differ,
/// plain `显示名` otherwise.
pub fn model_rows_preview(form: &ProviderAddForm) -> String {
    form.model_rows
        .iter()
        .map(|row| {
            let display = row.display_name.trim();
            let request = row.request_name.trim();
            if request.is_empty() || request == display {
                display.to_string()
            } else {
                format!("{display} → {request}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// 解析逗号分隔的 variants 输入（去空白、去空项，保序）。
fn parse_variants(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// 把档位名列表序列化为 opencode 的 variants 对象（每档空对象）。
fn variants_json(names: &[String]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for name in names {
        map.insert(
            name.clone(),
            serde_json::Value::Object(serde_json::Map::new()),
        );
    }
    serde_json::Value::Object(map)
}

/// 从 provider 元数据提取某模型的 variants 档位（逗号分隔）。
/// 模型对象整体保留在 `model_metadata`，variants 是其中任意未知字段之一。
fn variants_from_metadata(provider: &ProviderProfile, model: &str) -> String {
    provider
        .model_metadata
        .get(model)
        .and_then(|value| value.get("variants"))
        .and_then(serde_json::Value::as_object)
        .map(|variants| variants.keys().cloned().collect::<Vec<_>>().join(","))
        .unwrap_or_default()
}

/// Serialize the mapping table into the store models value. Rows where the
/// display name equals the request name collapse to a plain string (identity
/// entry, store-readable); mapped rows become `{"name", "requestName"}`
/// objects, plus `clientName` when the stable client key differs from the
/// display name. Rows with variants always serialize as objects and carry
/// `"variants": {"low": {}, ...}` (opencode thinking-strength presets). An
/// array is used so the table row order survives the JSON round trip — a JSON
/// object would re-sort keys alphabetically (BTreeMap).
pub fn model_rows_json(form: &ProviderAddForm) -> Result<String, String> {
    if form.model_rows.is_empty() {
        return Err("至少需要一个 model".to_string());
    }
    let mut items = Vec::new();
    let mut display_names = Vec::new();
    for (index, row) in form.model_rows.iter().enumerate() {
        let display = row.display_name.trim();
        if display.is_empty() {
            return Err(format!("第 {} 行模型缺少显示名", index + 1));
        }
        let request = row.request_name.trim();
        let request = if request.is_empty() {
            display.to_string()
        } else {
            request.to_string()
        };
        if display_names.iter().any(|name: &String| name == display) {
            return Err(format!("模型显示名重复：{display}"));
        }
        display_names.push(display.to_string());
        let client = row.client_name.trim();
        let variants = parse_variants(&row.variants);
        let mut value = if !client.is_empty() && client != display {
            // 本地 client key 与显示名不同 → 显式携带 clientName（与
            // detail_multi_models_args 写入规则一致，重解析后稳定键不丢）。
            serde_json::json!({
                "clientName": client,
                "name": display,
                "requestName": request,
            })
        } else if request != display || !variants.is_empty() {
            // 有请求名映射或带思考强度档位 → 必须对象化（纯字符串会丢字段）。
            serde_json::json!({ "name": display, "requestName": request })
        } else {
            serde_json::Value::String(display.to_string())
        };
        if !variants.is_empty() {
            value
                .as_object_mut()
                .expect("variants rows always serialize as objects")
                .insert("variants".to_string(), variants_json(&variants));
        }
        items.push(value);
    }
    serde_json::to_string(&serde_json::Value::Array(items)).map_err(|error| error.to_string())
}

/// Parse `spec provider fetch-models` output into table rows. The output is
/// `Provider: <id>\nFetched models: <n>\n<model-id> (owned_by: ...)\n...`.
pub fn parse_fetch_models_output(output: &str) -> Result<Vec<ProviderModelRow>, String> {
    let mut lines = output.lines();
    let header = lines.next().unwrap_or("");
    lines.next(); // "Fetched models: <n>"
    if !header.starts_with("Provider:") {
        return Err("无法解析模型列表输出".to_string());
    }
    let mut rows = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let id = line.split(" (owned_by: ").next().unwrap_or(line).trim();
        if !id.is_empty() {
            rows.push(ProviderModelRow {
                client_name: id.to_string(),
                display_name: id.to_string(),
                request_name: id.to_string(),
                variants: String::new(),
            });
        }
    }
    if rows.is_empty() {
        return Err("没有解析到模型".to_string());
    }
    Ok(rows)
}

/// Apply the result of a background model fetch to the edit form (preview
/// only; nothing is saved until the update is confirmed). Pure: never touches
/// the network or `spec::cli` — the worker thread owns the fetch.
pub fn apply_models_fetch_result(
    form: &mut ProviderAddForm,
    result: Result<Vec<ProviderModelRow>, String>,
) {
    match result {
        Ok(rows) => {
            let count = rows.len();
            form.model_rows = rows;
            form.model_cell = None;
            form.model_fetch_msg = Some(format!("拉取成功：{count} 个模型（预览，按 确认 保存）"));
        }
        Err(error) => form.model_fetch_msg = Some(format!("拉取失败：{error}")),
    }
}

/// Claude Code settings file that carries `permissions.defaultMode`.
pub fn claude_settings_path(home: &Path) -> PathBuf {
    home.join(".claude/settings.json")
}

/// 详情页 Claude 四槽位（fable/opus/sonnet/haiku）。
pub const DETAIL_SLOT_NAMES: [(&str, &str); 4] = [
    ("fable", "Fable"),
    ("opus", "Opus"),
    ("sonnet", "Sonnet"),
    ("haiku", "Haiku"),
];

/// 槽位当前生效值：claudeSlots 配置优先，未配置时回退默认模型（再回退第一个模型）。
pub fn claude_slot_value(provider: &ProviderProfile, slot: &str) -> String {
    provider
        .claude_slot(slot)
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            if !provider.default_model.trim().is_empty() {
                provider.default_model.clone()
            } else {
                provider.models.first().cloned().unwrap_or_default()
            }
        })
}

/// 详情页字段当前值（点击输入框时作为编辑初值）。
/// 0 端点 / 1 密钥（不回显，从空开始）/ 2 默认模型 / 3 备注 / 4..7 Claude 槽位 /
/// 8 超时毫秒 / 9 最大重试。
pub fn detail_field_value(provider: &ProviderProfile, field: usize) -> String {
    match field {
        0 => provider.base_url.clone(),
        1 => String::new(),
        2 => provider.default_model.clone(),
        3 => provider.notes.clone().unwrap_or_default(),
        4..=7 => claude_slot_value(provider, DETAIL_SLOT_NAMES[field - 4].0),
        8 => provider.timeout_ms.to_string(),
        9 => provider.max_retries.to_string(),
        _ => String::new(),
    }
}

/// 构造详情页字段保存参数：`provider update <id>` 加对应字段 flag
/// （--base-url / --api-key / --default-model / --notes / --claude-slot <slot>=<value> /
/// --timeout / --max-retries）。
pub fn detail_update_args(
    provider: &ProviderProfile,
    field: usize,
    value: &str,
) -> Result<Vec<String>, String> {
    let value = value.trim();
    let mut args = vec![
        "provider".to_string(),
        "update".to_string(),
        provider.id.clone(),
    ];
    match field {
        0 => {
            if !(value.is_empty() || value.starts_with("https://") || value.starts_with("http://"))
            {
                return Err("端点必须以 http:// 或 https:// 开头".to_string());
            }
            args.extend(["--base-url".to_string(), value.to_string()]);
        }
        1 => args.extend(["--api-key".to_string(), value.to_string()]),
        2 => {
            // 与编辑表单 provider_update_args 一致：接受 client key 或显示名，
            // 落盘前解析回 client key（store 按 client key 校验）；未命中时
            // 用同一错误信息 fail-closed，不再把显示名原样发给 CLI。
            let rows = provider_model_rows(provider);
            let default_client = rows
                .iter()
                .find(|row| {
                    let client = row.client_name.trim();
                    !client.is_empty() && client == value
                })
                .or_else(|| {
                    rows.iter().find(|row| {
                        let display = row.display_name.trim();
                        !display.is_empty() && display == value
                    })
                })
                .map(|row| {
                    let client = row.client_name.trim();
                    if client.is_empty() {
                        row.display_name.trim().to_string()
                    } else {
                        client.to_string()
                    }
                })
                .ok_or_else(|| "默认模型必须包含在模型列表中".to_string())?;
            args.extend(["--default-model".to_string(), default_client]);
        }
        3 => args.extend(["--notes".to_string(), value.to_string()]),
        4..=7 => args.extend([
            "--claude-slot".to_string(),
            format!("{}={}", DETAIL_SLOT_NAMES[field - 4].0, value),
        ]),
        8 => {
            value
                .parse::<u64>()
                .map_err(|_| "timeout 必须是数字（毫秒）".to_string())?;
            args.extend(["--timeout".to_string(), value.to_string()]);
        }
        9 => {
            value
                .parse::<u32>()
                .map_err(|_| "max_retries 必须是整数".to_string())?;
            args.extend(["--max-retries".to_string(), value.to_string()]);
        }
        _ => return Err("未知字段".to_string()),
    }
    Ok(args)
}

/// 详情页模型行：按 `models` 归一化顺序生成，行携带稳定 client key。
/// 优先 model_entries 元数据（client_name|显示名|请求名），无条目时
/// 三字段同为裸模型名。
pub fn provider_model_rows(provider: &ProviderProfile) -> Vec<ProviderModelRow> {
    provider
        .models
        .iter()
        .map(
            |client_name| match provider.model_entries.get(client_name) {
                Some(entry) => ProviderModelRow {
                    client_name: client_name.clone(),
                    display_name: entry.display_name.clone(),
                    request_name: entry.request_name.clone(),
                    variants: variants_from_metadata(provider, client_name),
                },
                None => ProviderModelRow {
                    client_name: client_name.clone(),
                    display_name: client_name.clone(),
                    request_name: client_name.clone(),
                    variants: variants_from_metadata(provider, client_name),
                },
            },
        )
        .collect()
}

/// 获取模型多选保存：把勾选下标（按拉取列表顺序）写为模型数组
/// → `provider update <id> --models-json [...]`。
pub fn detail_multi_models_args(
    provider: &ProviderProfile,
    list: &[String],
    selected: &BTreeSet<usize>,
) -> Result<Vec<String>, String> {
    let models: Vec<String> = selected
        .iter()
        .filter_map(|index| list.get(*index).cloned())
        .collect();
    if models.is_empty() {
        return Err("至少勾选一个模型".to_string());
    }
    let entries: Vec<serde_json::Value> = models
        .into_iter()
        .map(|name| {
            // 先按客户端键直查；再按请求名回查（拉取列表可能给的是请求名）。
            let entry = provider.model_entries.get(&name).or_else(|| {
                provider.models.iter().find_map(|client_name| {
                    provider
                        .model_entries
                        .get(client_name)
                        .filter(|entry| entry.request_name == name)
                })
            });
            match entry {
                // 本地 client key 与显示名不同 → 显式携带 clientName。
                Some(entry) if entry.client_name != entry.display_name => {
                    let mut value = serde_json::json!({
                        "clientName": entry.client_name,
                        "name": entry.display_name,
                        "requestName": entry.request_name,
                    });
                    attach_metadata_variants(provider, &name, &mut value);
                    value
                }
                // client key == 显示名：保留旧 JSON 形状（无 clientName）。
                Some(entry) if entry.request_name != entry.display_name => {
                    let mut value = serde_json::json!({
                        "name": entry.display_name,
                        "requestName": entry.request_name,
                    });
                    attach_metadata_variants(provider, &name, &mut value);
                    value
                }
                _ => {
                    // 恒等/plain 行：仅当带 variants 时升级为对象，否则维持字符串。
                    let mut value = serde_json::Value::String(name.clone());
                    attach_metadata_variants(provider, &name, &mut value);
                    value
                }
            }
        })
        .collect();
    let json = serde_json::to_string(&serde_json::Value::Array(entries))
        .map_err(|error| error.to_string())?;
    Ok(vec![
        "provider".to_string(),
        "update".to_string(),
        provider.id.clone(),
        "--models-json".to_string(),
        json,
    ])
}

/// 序列化时把 provider 元数据中的 variants 挂到模型值上（写入 opencode 时
/// 思考强度档位不丢）。`name` 可能是 client key 或请求名：元数据按 client
/// key 索引，先直查再按请求名反查。值为字符串（恒等/plain 行）且带档位时
/// 升级为 `{"name": ..., "variants": ...}` 对象。
fn attach_metadata_variants(provider: &ProviderProfile, name: &str, value: &mut serde_json::Value) {
    let client = if provider.model_metadata.contains_key(name) {
        name.to_string()
    } else {
        provider
            .models
            .iter()
            .find(|client| {
                provider
                    .model_entries
                    .get(*client)
                    .map(|entry| entry.request_name == name)
                    .unwrap_or(false)
            })
            .cloned()
            .unwrap_or_default()
    };
    let variants = variants_from_metadata(provider, &client);
    if !variants.is_empty() {
        let variants_value = variants_json(&parse_variants(&variants));
        if let Some(object) = value.as_object_mut() {
            object.insert("variants".to_string(), variants_value);
        } else {
            let mut object = serde_json::Map::new();
            object.insert("name".to_string(), value.clone());
            object.insert("variants".to_string(), variants_value);
            *value = serde_json::Value::Object(object);
        }
    }
}

/// OpenCode「生效」状态：opencode.json 的 provider 段是否包含该供应商。
pub fn read_opencode_provider_active(home: &Path, provider_id: &str) -> bool {
    let path = home.join(".config/opencode/opencode.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    root.get("provider")
        .and_then(|provider| provider.get(provider_id))
        .is_some()
}

/// OpenCode「生效」开关：开 = 应用写入 opencode.json（含备份）；关 = 移除该供应商。
pub fn toggle_opencode_provider(
    state: &mut ProviderState,
    provider_id: &str,
    enable: bool,
) -> Result<String, String> {
    let home = config::home();
    if enable {
        // 开：走完整应用流程（diff 补丁 + 备份 + 状态写入）。
        return Ok(apply_provider_plan(
            state,
            Some(AgentTarget::OpenCode),
            &[provider_id.to_string()],
        ));
    }
    // 关：从 opencode.json 的 provider 段移除该 id，保留其他 provider 与顶层字段。
    let path = home.join(".config/opencode/opencode.json");
    let Ok(text) = fs::read_to_string(&path) else {
        // 文件不存在 = 本来就未生效，静默视为关闭成功（并清 state）。
        clear_opencode_state(&home, state);
        return Ok(format!("已生效关闭：{} 未在 opencode.json 中", provider_id));
    };
    let mut root: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("解析 {} 失败：{error}", path.display()))?;
    let Some(provider) = root
        .get_mut("provider")
        .and_then(serde_json::Value::as_object_mut)
    else {
        clear_opencode_state(&home, state);
        return Ok(format!("已生效关闭：{} 未在 opencode.json 中", provider_id));
    };
    if provider.remove(provider_id).is_none() {
        clear_opencode_state(&home, state);
        return Ok(format!("已生效关闭：{} 未在 opencode.json 中", provider_id));
    }
    if provider.is_empty() {
        root.as_object_mut()
            .and_then(|object| object.remove("provider"));
    }
    let serialized =
        serde_json::to_string_pretty(&root).map_err(|error| format!("序列化失败：{error}"))?;
    if backup_path(&path).is_none() {
        eprintln!("opencode.json 备份失败（继续写入）");
    }
    atomic_write(&path, &serialized)?;
    clear_opencode_state(&home, state);
    Ok(format!("已生效关闭：从 opencode.json 移除 {}", provider_id))
}

/// 持久化 opencode 端「未生效」状态（与 enable 路径的 current_provider_patch
/// 对称），失败仅告警（文件已改，无法回滚）。
fn clear_opencode_state(home: &Path, state: &mut ProviderState) {
    let Ok(patch) = spec::state::current_provider_patch(home, AgentTarget::OpenCode, "") else {
        eprintln!("opencode 生效关闭后 state 写入失败");
        return;
    };
    if let Err(error) = spec::patch::apply_patch(
        &patch,
        spec::patch::PatchOptions {
            dry_run: false,
            backup: true,
        },
    ) {
        eprintln!("opencode 生效关闭后 state 写入失败：{error}");
    }
    state.current.insert("opencode".to_string(), String::new());
}

/// 生成备份路径并返回（写文件前调用；命名与 patch.rs 一致：
/// `<file>.spec.<ts>.bak`，同毫秒碰撞时递增 `.1/.2` 后缀；失败返回 None）。
pub fn backup_path(path: &Path) -> Option<PathBuf> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let name = path.file_name().unwrap_or_else(|| "config".as_ref());
    let name = name.to_string_lossy();
    let mut backup = path.with_file_name(format!("{name}.spec.{stamp}.bak"));
    let mut counter = 0u32;
    while backup.exists() {
        counter += 1;
        backup = path.with_file_name(format!("{name}.spec.{stamp}.{counter}.bak"));
    }
    if fs::copy(path, &backup).is_ok() {
        Some(backup)
    } else {
        None
    }
}

/// 槽位展开选择：`provider update <id> --claude-slot <slot>=<model>`。
pub fn detail_slot_args(
    provider: &ProviderProfile,
    slot: usize,
    model: &str,
) -> Result<Vec<String>, String> {
    let model = model.trim();
    if model.is_empty() {
        return Err("模型名不能为空".to_string());
    }
    let (slot_id, _) = DETAIL_SLOT_NAMES
        .get(slot)
        .ok_or_else(|| "未知槽位".to_string())?;
    Ok(vec![
        "provider".to_string(),
        "update".to_string(),
        provider.id.clone(),
        "--claude-slot".to_string(),
        format!("{slot_id}={model}"),
    ])
}

/// 协议切换：`provider update <id> --kind <chat|responses|anthropic>`。
pub fn detail_set_kind_args(provider: &ProviderProfile, kind: ProtocolKind) -> Vec<String> {
    vec![
        "provider".to_string(),
        "update".to_string(),
        provider.id.clone(),
        "--kind".to_string(),
        kind.as_str().to_string(),
    ]
}

/// 同步拉取上游模型列表（详情页「获取模型」→ 点击选择）。
pub fn detail_fetch_models(provider: &ProviderProfile) -> Result<Vec<String>, String> {
    let args = vec![
        "provider".to_string(),
        "fetch-models".to_string(),
        provider.id.clone(),
    ];
    match spec::cli::run_command(&config::home(), &args) {
        Some(Ok(output)) => parse_fetch_models_output(&output)
            .map(|rows| rows.into_iter().map(|row| row.display_name).collect()),
        Some(Err(error)) => Err(format!("拉取失败：{error}")),
        None => Err("拉取失败：provider fetch-models 命令不可用".to_string()),
    }
}

/// 模型列表本地缓存路径：`~/.config/spec/models-cache/<id>.json`。
/// id 经白名单过滤（`..`/路径分隔符一律拒绝），防止注入逃逸目录。
fn models_cache_path(provider_id: &str) -> std::path::PathBuf {
    let safe_id: String = provider_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .collect();
    config::home()
        .join(".config")
        .join("spec")
        .join("models-cache")
        .join(format!("{safe_id}.json"))
}

/// 模型缓存有效期（过期后展开选择器不再秒开，需重新拉取）。
const MODELS_CACHE_TTL_SECS: i64 = 24 * 60 * 60;

/// 读取模型列表本地缓存（None = 无缓存/损坏/过期/归属不符）。
pub fn read_models_cache(provider_id: &str) -> Option<Vec<String>> {
    let path = models_cache_path(provider_id);
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    // 归属校验：缓存 JSON 记录来源 provider_id，防止 `a.b` 与 `ab`
    // 同文件名碰撞导致跨供应商串读。
    if value.get("provider_id").and_then(serde_json::Value::as_str) != Some(provider_id) {
        return None;
    }
    // TTL：过期视为无缓存（上游模型可能已变化）。
    let fetched_at = value.get("fetched_at")?.as_str()?;
    let fetched = chrono::DateTime::parse_from_rfc3339(fetched_at).ok()?;
    if chrono::Utc::now()
        .signed_duration_since(fetched)
        .num_seconds()
        > MODELS_CACHE_TTL_SECS
    {
        return None;
    }
    let models = value.get("models")?.as_array()?;
    Some(
        models
            .iter()
            .filter_map(|m| m.as_str().map(str::to_string))
            .collect(),
    )
}

/// 写入模型列表本地缓存（含拉取时间与来源 provider_id，供 UI/归属校验）。
pub fn write_models_cache(provider_id: &str, models: &[String]) -> Result<(), String> {
    let value = serde_json::json!({
        "fetched_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "provider_id": provider_id,
        "models": models,
    });
    let serialized =
        serde_json::to_string(&value).map_err(|error| format!("缓存序列化失败：{error}"))?;
    atomic_write(&models_cache_path(provider_id), &serialized)
}

/// 获取模型列表：`force=false` 时优先读本地缓存（秒开）；
/// `force=true` 强制联网拉取并刷新缓存。
pub fn fetch_models_cached(provider: &ProviderProfile, force: bool) -> Result<Vec<String>, String> {
    if !force {
        if let Some(cached) = read_models_cache(&provider.id) {
            return Ok(cached);
        }
    }
    let models = detail_fetch_models(provider)?;
    let _ = write_models_cache(&provider.id, &models);
    Ok(models)
}

/// Whether `~/.claude/settings.json` currently has
/// `permissions.defaultMode == "bypassPermissions"`.
pub fn read_bypass_permissions(home: &Path) -> bool {
    let Ok(text) = fs::read_to_string(claude_settings_path(home)) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    root.get("permissions")
        .and_then(|permissions| permissions.get("defaultMode"))
        .and_then(serde_json::Value::as_str)
        .map(|mode| mode == "bypassPermissions")
        .unwrap_or(false)
}

/// Toggle `permissions.defaultMode` in `~/.claude/settings.json`.
/// Enabling only adds `defaultMode`; disabling removes exactly that key, and
/// drops the whole `permissions` object only when it becomes empty. All other
/// settings fields are preserved verbatim.
pub fn set_bypass_permissions(home: &Path, enabled: bool) -> Result<String, String> {
    let path = claude_settings_path(home);
    // 关闭且文件不存在：无事可做，不新建空 settings.json。
    if !enabled && !path.exists() {
        return Ok(
            "已关闭 Claude Code 免确认（~/.claude/settings.json 不存在，无需修改）".to_string(),
        );
    }
    let text = fs::read_to_string(&path).unwrap_or_default();
    let mut root: serde_json::Value = if text.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&text)
            .map_err(|error| format!("解析 {} 失败：{error}", path.display()))?
    };
    if enabled {
        let permissions = match root.get_mut("permissions") {
            Some(serde_json::Value::Object(map)) => map,
            Some(_) => {
                return Err(format!("{} 的 permissions 不是对象", path.display()));
            }
            None => {
                let object = root
                    .as_object_mut()
                    .ok_or_else(|| format!("{} 顶层不是对象", path.display()))?;
                object.insert("permissions".to_string(), serde_json::json!({}));
                root.get_mut("permissions")
                    .and_then(serde_json::Value::as_object_mut)
                    .expect("permissions just inserted")
            }
        };
        permissions.insert(
            "defaultMode".to_string(),
            serde_json::json!("bypassPermissions"),
        );
    } else if let Some(serde_json::Value::Object(permissions)) = root.get_mut("permissions") {
        permissions.remove("defaultMode");
        if permissions.is_empty() {
            if let Some(object) = root.as_object_mut() {
                object.remove("permissions");
            }
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建 {} 失败：{error}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(&root)
        .map_err(|error| format!("序列化 {} 失败：{error}", path.display()))?;
    atomic_write(&path, &serialized)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("设置 {} 权限失败：{error}", path.display()))?;
    Ok(if enabled {
        "已开启 Claude Code 免确认（~/.claude/settings.json）".to_string()
    } else {
        "已关闭 Claude Code 免确认（~/.claude/settings.json）".to_string()
    })
}

/// `~/.claude/settings.json` env 的 `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` 是否为 "1"。
pub fn read_terminal_title_disabled(home: &Path) -> bool {
    let Ok(text) = fs::read_to_string(claude_settings_path(home)) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    root.get("env")
        .and_then(|env| env.get("CLAUDE_CODE_DISABLE_TERMINAL_TITLE"))
        .and_then(serde_json::Value::as_str)
        .map(|value| value == "1")
        .unwrap_or(false)
}

/// `~/.config/opencode/opencode.json` 顶层 `permission == "allow"`。
pub fn read_opencode_permission_allow(home: &Path) -> bool {
    let path = home.join(".config/opencode/opencode.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    root.get("permission")
        .and_then(serde_json::Value::as_str)
        .map(|mode| mode == "allow")
        .unwrap_or(false)
}

/// 三端路由开关状态：`(clients.<target>.enabled, providers 含该供应商)`。
pub fn read_route_status(home: &Path, target: AgentTarget, provider_id: &str) -> (bool, bool) {
    let Ok(text) = fs::read_to_string(home.join(".codex/xu-client-routes.json")) else {
        return (false, false);
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (false, false);
    };
    let client = root
        .get("clients")
        .and_then(|clients| clients.get(target.id()));
    let enabled = client
        .and_then(|client| client.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let in_providers = client
        .and_then(|client| client.get("providers"))
        .and_then(serde_json::Value::as_array)
        .map(|list| list.iter().any(|value| value.as_str() == Some(provider_id)))
        .unwrap_or(false);
    (enabled, in_providers)
}

/// Toggle `env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE` in `~/.claude/settings.json`。
/// 开启只增该键；关闭移除该键（env 空则整体移除）。其余字段原样保留。
pub fn toggle_terminal_title(home: &Path, enabled: bool) -> Result<String, String> {
    let path = claude_settings_path(home);
    if !enabled && !path.exists() {
        return Ok("已关闭会话标题抑制（~/.claude/settings.json 不存在，无需修改）".to_string());
    }
    let text = fs::read_to_string(&path).unwrap_or_default();
    let mut root: serde_json::Value = if text.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&text)
            .map_err(|error| format!("解析 {} 失败：{error}", path.display()))?
    };
    if enabled {
        let env = match root.get_mut("env") {
            Some(serde_json::Value::Object(map)) => map,
            Some(_) => {
                return Err(format!("{} 的 env 不是对象", path.display()));
            }
            None => {
                let object = root
                    .as_object_mut()
                    .ok_or_else(|| format!("{} 顶层不是对象", path.display()))?;
                object.insert("env".to_string(), serde_json::json!({}));
                root.get_mut("env")
                    .and_then(serde_json::Value::as_object_mut)
                    .expect("env just inserted")
            }
        };
        env.insert(
            "CLAUDE_CODE_DISABLE_TERMINAL_TITLE".to_string(),
            serde_json::json!("1"),
        );
    } else if let Some(serde_json::Value::Object(env)) = root.get_mut("env") {
        env.remove("CLAUDE_CODE_DISABLE_TERMINAL_TITLE");
        if env.is_empty() {
            if let Some(object) = root.as_object_mut() {
                object.remove("env");
            }
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建 {} 失败：{error}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(&root)
        .map_err(|error| format!("序列化 {} 失败：{error}", path.display()))?;
    atomic_write(&path, &serialized)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("设置 {} 权限失败：{error}", path.display()))?;
    Ok(if enabled {
        "已开启 Claude Code 会话标题抑制（~/.claude/settings.json）".to_string()
    } else {
        "已关闭 Claude Code 会话标题抑制（~/.claude/settings.json）".to_string()
    })
}

/// Toggle `permission` in `~/.config/opencode/opencode.json`：
/// 开启写 "allow"；关闭移除整个键。其余字段原样保留。
pub fn toggle_opencode_permission(home: &Path, enabled: bool) -> Result<String, String> {
    let path = home.join(".config/opencode/opencode.json");
    if !enabled && !path.exists() {
        return Ok("已关闭 OpenCode 完全权限（opencode.json 不存在，无需修改）".to_string());
    }
    let text = fs::read_to_string(&path).unwrap_or_default();
    let mut root: serde_json::Value = if text.trim().is_empty() {
        serde_json::json!({ "$schema": "https://opencode.ai/config.json" })
    } else {
        serde_json::from_str(&text)
            .map_err(|error| format!("解析 {} 失败：{error}", path.display()))?
    };
    let object = root
        .as_object_mut()
        .ok_or_else(|| format!("{} 顶层不是对象", path.display()))?;
    if enabled {
        object.insert("permission".to_string(), serde_json::json!("allow"));
    } else {
        // 仅当现值恰为 allow（本开关管理的值）才删除；自定义对象/其他值保留。
        if object.get("permission").and_then(serde_json::Value::as_str) == Some("allow") {
            object.remove("permission");
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建 {} 失败：{error}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(&root)
        .map_err(|error| format!("序列化 {} 失败：{error}", path.display()))?;
    atomic_write(&path, &serialized)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("设置 {} 权限失败：{error}", path.display()))?;
    Ok(if enabled {
        "已开启 OpenCode 完全权限（~/.config/opencode/opencode.json）".to_string()
    } else {
        "已关闭 OpenCode 完全权限（~/.config/opencode/opencode.json）".to_string()
    })
}

/// 三端路由开关：`~/.codex/xu-client-routes.json` clients.<target> 的
/// `enabled` + `providers` 增删该供应商（其余字段保留；缺失条目按 single 创建）。
pub fn toggle_route(home: &Path, target: AgentTarget, provider_id: &str) -> Result<String, String> {
    let path = home.join(".codex/xu-client-routes.json");
    let text = fs::read_to_string(&path).unwrap_or_default();
    let mut root: serde_json::Value = if text.trim().is_empty() {
        serde_json::json!({ "clients": {} })
    } else {
        serde_json::from_str(&text)
            .map_err(|error| format!("解析 {} 失败：{error}", path.display()))?
    };
    let clients = root
        .get_mut("clients")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| format!("{} 的 clients 不是对象", path.display()))?;
    let entry = clients.entry(target.id().to_string()).or_insert_with(
        || serde_json::json!({ "enabled": false, "mode": "single", "providers": [] }),
    );
    let entry = entry
        .as_object_mut()
        .ok_or_else(|| format!("{} 的 clients.{} 不是对象", path.display(), target.id()))?;
    let enabled = entry
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let (turning_on, list_empty) = {
        let providers = entry
            .entry("providers".to_string())
            .or_insert_with(|| serde_json::json!([]));
        let list = providers
            .as_array_mut()
            .ok_or_else(|| format!("{} 的 providers 不是数组", path.display()))?;
        let present = list.iter().any(|value| value.as_str() == Some(provider_id));
        let turning_on = !(enabled && present);
        if turning_on {
            if !present {
                list.push(serde_json::json!(provider_id));
            }
        } else {
            list.retain(|value| value.as_str() != Some(provider_id));
        }
        (turning_on, list.is_empty())
    };
    if turning_on {
        entry.insert("enabled".to_string(), serde_json::json!(true));
        entry
            .entry("mode".to_string())
            .or_insert_with(|| serde_json::json!("single"));
    } else {
        // 列表里还有其他供应商时保持路由启用；清空才关闭。
        entry.insert("enabled".to_string(), serde_json::json!(!list_empty));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建 {} 失败：{error}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(&root)
        .map_err(|error| format!("序列化 {} 失败：{error}", path.display()))?;
    atomic_write(&path, &serialized)?;
    Ok(if turning_on {
        format!("已开启 {} 路由（{provider_id}）", target.label())
    } else {
        format!("已关闭 {} 路由（{provider_id}）", target.label())
    })
}

pub fn provider_add_args(form: &ProviderAddForm, dry_run: bool) -> Result<Vec<String>, String> {
    let raw_name = form.name.trim();
    let id = if form.id.trim().is_empty() {
        provider_id_from_name(raw_name)
    } else {
        form.id.trim().to_string()
    };
    if id.starts_with('-')
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("provider id 只能包含 ASCII 字母、数字、-、_、.".to_string());
    }
    let name = if raw_name.is_empty() {
        id.clone()
    } else {
        raw_name.to_string()
    };
    let base_url = form.base_url.trim();
    if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
        return Err("baseURL 必须以 http:// 或 https:// 开头".to_string());
    }
    let api_key = form.api_key.trim();
    // 以表格行（model_rows）为唯一来源生成 --model，保证新增表单里表格的
    // 增删改/拉取结果在预览与保存中都生效；表格为空时回退到「模型列表」文本。
    // 注意：cli provider add 仅支持 --model（无 --models-json），映射行的
    // requestName 无法透传，落库为显示名同名的 identity 条目。
    let models = if form.model_rows.is_empty() {
        split_provider_models(&form.models)?
    } else {
        let rows = form
            .model_rows
            .iter()
            .map(|row| row.display_name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return Err("至少需要一个 model".to_string());
        }
        rows
    };
    let mut args = vec![
        "provider".to_string(),
        "add".to_string(),
        id.clone(),
        "--kind".to_string(),
        provider_add_kind(form).to_string(),
        "--name".to_string(),
        name.clone(),
        "--base-url".to_string(),
        base_url.to_string(),
    ];
    if !api_key.is_empty() {
        args.push("--api-key".to_string());
        args.push(api_key.to_string());
    }
    for model in models {
        args.push("--model".to_string());
        args.push(model);
    }
    if dry_run {
        args.push("--预览".to_string());
    }
    Ok(args)
}

pub fn split_provider_models(value: &str) -> Result<Vec<String>, String> {
    let models = value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if models.is_empty() {
        Err("至少需要一个 model".to_string())
    } else {
        Ok(models)
    }
}

pub fn delete_selected_provider(state: &mut ProviderState) -> String {
    let Some(provider) = state.providers.get(state.selected) else {
        return "删除失败：没有选中的 provider。".to_string();
    };
    let args = vec![
        "provider".to_string(),
        "delete".to_string(),
        provider.id.clone(),
        "--yes".to_string(),
    ];
    match spec::cli::run_command(&config::home(), &args) {
        Some(Ok(output)) => format!("{output}\n\nEsc 返回供应商列表。"),
        Some(Err(error)) => format!("删除失败：{error}\n\nEsc 返回供应商列表。"),
        None => "删除失败：provider delete 命令不可用。".to_string(),
    }
}

pub fn apply_provider_plan(
    state: &mut ProviderState,
    target: Option<AgentTarget>,
    provider_ids: &[String],
) -> String {
    if provider_ids.is_empty() {
        return "应用失败：没有选中的 provider。".to_string();
    }
    let Some(target) = target else {
        return "应用失败：没有选中的目标 agent。".to_string();
    };

    let home = config::home();
    let mut plan = match spec::agents::apply_agent(&home, target, &state.providers, provider_ids) {
        Ok(plan) => plan,
        Err(error) => return format!("应用失败：无法生成计划：{error}"),
    };
    let state_value = provider_ids.join(",");
    let state_patch = match spec::state::current_provider_patch(&home, target, &state_value) {
        Ok(patch) => patch,
        Err(error) => return format!("应用失败：无法生成状态计划：{error}"),
    };
    plan.patches.push(state_patch);

    let results = match spec::cli::apply_plan(&plan, false) {
        Ok(results) => results,
        Err(error) => return format!("应用失败：{error}"),
    };
    state.current.insert(target.id().to_string(), state_value);

    let mut out = String::new();
    out.push_str("应用成功\n");
    out.push_str(plan.target.label());
    out.push_str(" · ");
    out.push_str(plan.routing_mode.as_str());
    out.push_str("\n\n");
    for result in results {
        out.push_str(if result.changed {
            "已写入："
        } else {
            "无变化："
        });
        out.push_str(&result.path.display().to_string());
        out.push('\n');
        if let Some(backup) = result.backup_path {
            out.push_str("备份：");
            out.push_str(&backup.display().to_string());
            out.push('\n');
        }
    }
    if plan.routing_mode.needs_local_proxy() {
        out.push_str("\n需要运行：spec serve\n");
    }
    out.push_str(
        "生效提示：通常需要重启目标 agent/终端会话；local routing 需要保持 spec serve 运行。\n",
    );
    out.push_str("\nEsc 返回 provider 详情。");
    out
}

/// 原子写文件：先写临时文件再 rename，避免并发读写的 lost-update。
/// 临时文件按 0600 创建，避免含密钥的配置文件（如 opencode.json）被
/// rename 后权限降级为 umask 默认（0644）导致其他用户可读。
fn atomic_write(path: &std::path::Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建 {} 失败：{error}", parent.display()))?;
    }
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, content)
        .map_err(|error| format!("写入 {} 失败：{error}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|error| format!("替换 {} 失败：{error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec::providers::store::profiles_from_xu_chat_json;

    fn detail_profile() -> ProviderProfile {
        ProviderProfile {
            id: "zen".to_string(),
            name: "Zen".to_string(),
            notes: Some("备注".to_string()),
            website: None,
            vendor: spec::domain::ProviderVendor::CustomOpenAiCompatible,
            protocol: ProtocolKind::OpenAiChat,
            base_url: "https://zen.example/v1".to_string(),
            api_key: "sk-secret".to_string(),
            models: vec!["A".to_string(), "B".to_string()],
            model_entries: std::collections::BTreeMap::new(),
            model_metadata: std::collections::BTreeMap::new(),
            claude_slots: std::collections::BTreeMap::new(),
            default_model: "A".to_string(),
            extra_headers: std::collections::BTreeMap::new(),
            request_url_mode: None,
            header_mode: None,
            timeout_ms: 0,
            max_retries: 0,
            context_window: 0,
            max_output_tokens: 0,
            reasoning_effort: None,
            cache_mode: spec::domain::CacheMode::Auto,
        }
    }

    #[test]
    fn claude_slot_value_reads_slots_with_defaults() {
        let mut p = detail_profile();
        // 未配置 claudeSlots：回退默认模型
        assert_eq!(claude_slot_value(&p, "opus"), "A");
        p.default_model = String::new();
        // 无默认模型：回退第一个模型
        assert_eq!(claude_slot_value(&p, "fable"), "A");
        p.claude_slots = [("opus", "claude-opus-4"), ("fable", "")]
            .into_iter()
            .map(|(slot, model)| (slot.to_string(), model.to_string()))
            .collect();
        assert_eq!(claude_slot_value(&p, "opus"), "claude-opus-4");
        // 槽位配置为空字符串 → 回退
        assert_eq!(claude_slot_value(&p, "fable"), "A");
        assert_eq!(claude_slot_value(&p, "sonnet"), "A");
    }

    #[test]
    fn models_cache_roundtrip_persists_list() {
        let id = format!("cache-test-{}", std::process::id());
        let models = vec![
            "claude-opus-5".to_string(),
            "claude-sonnet-5".to_string(),
            "deepseek-v4-flash".to_string(),
        ];
        write_models_cache(&id, &models).expect("写缓存应成功");
        let cached = read_models_cache(&id).expect("读缓存应成功");
        assert_eq!(cached, models);
        // 归属校验：同文件名碰撞（白名单过滤后相同）的其他 id 读不到。
        let colliding = format!("cache-test-{}.x", std::process::id());
        if models_cache_path(&colliding) == models_cache_path(&id) {
            assert!(read_models_cache(&colliding).is_none());
        }
        let _ = std::fs::remove_file(models_cache_path(&id));
    }

    #[test]
    fn models_cache_missing_or_broken_returns_none() {
        let id = format!("cache-missing-{}", std::process::id());
        assert!(read_models_cache(&id).is_none());
        let path = models_cache_path(&id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not-json").unwrap();
        assert!(read_models_cache(&id).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn detail_field_value_seeds_edit_buffers() {
        let p = detail_profile();
        assert_eq!(detail_field_value(&p, 0), "https://zen.example/v1");
        assert_eq!(detail_field_value(&p, 1), ""); // 密钥不回显
        assert_eq!(detail_field_value(&p, 2), "A");
        assert_eq!(detail_field_value(&p, 3), "备注");
        assert_eq!(detail_field_value(&p, 4), "A");
        assert_eq!(detail_field_value(&p, 7), "A");
    }

    #[test]
    fn detail_update_args_build_correct_flags() {
        let p = detail_profile();
        let args = detail_update_args(&p, 0, "https://new.example/v1").unwrap();
        assert_eq!(&args[..3], ["provider", "update", "zen"]);
        assert_eq!(&args[3..], ["--base-url", "https://new.example/v1"]);
        assert_eq!(
            &detail_update_args(&p, 1, "sk-2").unwrap()[3..],
            ["--api-key", "sk-2"]
        );
        assert_eq!(
            &detail_update_args(&p, 2, "B").unwrap()[3..],
            ["--default-model", "B"]
        );
        assert_eq!(
            &detail_update_args(&p, 3, "n").unwrap()[3..],
            ["--notes", "n"]
        );
        assert_eq!(
            &detail_update_args(&p, 4, "claude-fable").unwrap()[3..],
            ["--claude-slot", "fable=claude-fable"]
        );
        assert_eq!(
            &detail_update_args(&p, 7, "claude-haiku").unwrap()[3..],
            ["--claude-slot", "haiku=claude-haiku"]
        );
        // 端点必须合法；空值允许（清空）
        assert!(detail_update_args(&p, 0, "not-a-url").is_err());
        assert!(detail_update_args(&p, 0, "").is_ok());
        assert!(detail_update_args(&p, 9, "x").is_err());
    }

    #[test]
    fn detail_slot_args_build_claude_slot_flag() {
        let p = detail_profile();
        let args = detail_slot_args(&p, 0, "claude-opus-4").unwrap();
        assert_eq!(&args[..3], ["provider", "update", "zen"]);
        assert_eq!(&args[3..], ["--claude-slot", "fable=claude-opus-4"]);
        assert_eq!(
            &detail_slot_args(&p, 3, "claude-haiku-4").unwrap()[3..],
            ["--claude-slot", "haiku=claude-haiku-4"]
        );
        assert!(detail_slot_args(&p, 0, "  ").is_err());
        assert!(detail_slot_args(&p, 9, "x").is_err());
    }

    #[test]
    fn detail_set_kind_args_build_kind_flag() {
        let p = detail_profile();
        let args = detail_set_kind_args(&p, ProtocolKind::OpenAiResponses);
        assert_eq!(&args[..3], ["provider", "update", "zen"]);
        assert_eq!(&args[3..], ["--kind", "openai-responses"]);
        let args = detail_set_kind_args(&p, ProtocolKind::AnthropicMessages);
        assert_eq!(&args[3..], ["--kind", "anthropic-messages"]);
    }

    #[test]
    fn detail_multi_models_args_keeps_list_order_and_rejects_empty() {
        let p = detail_profile();
        let list = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let selected: BTreeSet<usize> = [2usize, 0].into_iter().collect();
        let args = detail_multi_models_args(&p, &list, &selected).unwrap();
        assert_eq!(&args[..3], ["provider", "update", "zen"]);
        assert_eq!(&args[3..], ["--models-json", "[\"a\",\"c\"]"]);
        assert!(detail_multi_models_args(&p, &list, &BTreeSet::new()).is_err());
    }

    #[test]
    fn provider_model_rows_preserve_normalized_model_order() {
        let mut provider = detail_profile();
        provider.models = vec!["Zebra".into(), "Alpha".into(), "Mid".into()];
        provider.model_entries.clear();
        provider.model_entries.insert(
            "Zebra".into(),
            spec::domain::ModelEntry {
                client_name: "Zebra".into(),
                display_name: "Zebra".into(),
                request_name: "zebra-upstream".into(),
            },
        );
        provider.model_entries.insert(
            "Alpha".into(),
            spec::domain::ModelEntry {
                client_name: "Alpha".into(),
                display_name: "Alpha".into(),
                request_name: "alpha-upstream".into(),
            },
        );
        provider.model_entries.insert(
            "Mid".into(),
            spec::domain::ModelEntry {
                client_name: "Mid".into(),
                display_name: "Mid".into(),
                request_name: "Mid".into(),
            },
        );
        let rows = provider_model_rows(&provider);
        assert_eq!(rows[0].client_name, "Zebra");
    }

    #[test]
    fn detail_multi_models_keeps_aliased_entries_client_key() {
        let mut provider = detail_profile();
        provider.models = vec!["client-key".into()];
        provider.model_entries.insert(
            "client-key".into(),
            spec::domain::ModelEntry {
                client_name: "client-key".into(),
                display_name: "Client key".into(),
                request_name: "upstream-name".into(),
            },
        );
        let args =
            detail_multi_models_args(&provider, &["upstream-name".into()], &BTreeSet::from([0]))
                .unwrap();
        assert_eq!(
            args.last().unwrap(),
            r#"[{"clientName":"client-key","name":"Client key","requestName":"upstream-name"}]"#
        );
    }

    fn form_with_rows(rows: Vec<(&str, &str)>) -> ProviderAddForm {
        ProviderAddForm {
            id: "zen".to_string(),
            name: "Zen".to_string(),
            base_url: "https://zen.example/v1".to_string(),
            default_model: "DeepSeek V4".to_string(),
            model_rows: rows
                .into_iter()
                .map(|(display, request)| ProviderModelRow {
                    client_name: display.to_string(),
                    display_name: display.to_string(),
                    request_name: request.to_string(),
                    variants: String::new(),
                })
                .collect(),
            ..ProviderAddForm::default()
        }
    }

    #[test]
    fn model_rows_json_identity_rows_collapse_to_strings() {
        let form = form_with_rows(vec![("deepseek-v4", "deepseek-v4")]);
        let json = model_rows_json(&form).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value, serde_json::json!(["deepseek-v4"]));
    }

    #[test]
    fn model_rows_json_mapped_rows_keep_names() {
        let form = form_with_rows(vec![
            ("DeepSeek V4", "deepseek-v4-flash-free"),
            ("plain", "plain"),
        ]);
        let json = model_rows_json(&form).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value,
            serde_json::json!([
                { "name": "DeepSeek V4", "requestName": "deepseek-v4-flash-free" },
                "plain"
            ])
        );
    }

    #[test]
    fn model_rows_json_preserves_row_order_through_store_parser() {
        let form = form_with_rows(vec![
            ("Zebra", "zebra-upstream"),
            ("Alpha", "alpha-upstream"),
            ("Mid", "Mid"),
        ]);
        let json = model_rows_json(&form).unwrap();
        let store = format!(
            r#"{{"provider":{{"zen":{{"apiKind":"chat","options":{{"baseURL":"https://zen.example/v1"}},"models":{json},"defaultModel":"Zebra"}}}}}}"#
        );
        let profiles = profiles_from_xu_chat_json(&store).unwrap();
        assert_eq!(
            profiles[0].models,
            vec!["Zebra", "Alpha", "Mid"],
            "table row order must survive the JSON round trip"
        );
        assert_eq!(
            profiles[0].request_name_for("Alpha"),
            "alpha-upstream",
            "mapped rows keep their request names"
        );
        assert_eq!(profiles[0].request_name_for("Mid"), "Mid");
    }

    #[test]
    fn model_rows_json_rejects_empty_display_duplicates_and_no_rows() {
        let empty = ProviderAddForm::default();
        assert!(model_rows_json(&empty)
            .unwrap_err()
            .contains("至少需要一个"));
        let missing = form_with_rows(vec![("", "request")]);
        let err = model_rows_json(&missing).unwrap_err();
        assert!(err.contains("缺少显示名"), "got: {err}");
        let duplicate = form_with_rows(vec![("dup", "a"), ("dup", "b")]);
        let err = model_rows_json(&duplicate).unwrap_err();
        assert!(err.contains("重复"), "got: {err}");
    }

    #[test]
    fn model_rows_json_roundtrips_through_store_parser() {
        let form = form_with_rows(vec![
            ("DeepSeek V4", "deepseek-v4-flash-free"),
            ("plain", "plain"),
        ]);
        let json = model_rows_json(&form).unwrap();
        let store = format!(
            r#"{{"provider":{{"zen":{{"apiKind":"chat","options":{{"baseURL":"https://zen.example/v1"}},"models":{json},"defaultModel":"DeepSeek V4"}}}}}}"#
        );
        let profiles = profiles_from_xu_chat_json(&store).unwrap();
        assert_eq!(profiles[0].models.len(), 2);
        assert_eq!(
            profiles[0].request_name_for("DeepSeek V4"),
            "deepseek-v4-flash-free"
        );
        assert_eq!(profiles[0].display_name_for("DeepSeek V4"), "DeepSeek V4");
        assert_eq!(profiles[0].request_name_for("plain"), "plain");
    }

    #[test]
    fn aliased_form_save_roundtrip_preserves_client_key_and_default() {
        // F1 回归：Task 2 写入的别名条目（clientName ≠ 显示名）走编辑表单
        // 保存后再重载，client key 与请求名必须保持；default_model 播种与
        // 落盘都必须是 client key（store 按 client key 校验）。
        let store = r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://zen.example/v1"},"models":[{"clientName":"client-key","name":"Client key","requestName":"upstream-name"}],"defaultModel":"client-key"}}}"#;
        let profiles = profiles_from_xu_chat_json(store).unwrap();
        assert_eq!(profiles[0].models, vec!["client-key"]);
        assert_eq!(profiles[0].default_model, "client-key");

        let state = ProviderState {
            providers: profiles,
            error: None,
            sync_notice: None,
            selected: 0,
            current: std::collections::BTreeMap::new(),
            health: std::collections::BTreeMap::new(),
        };
        let form = edit_form_from_selected_provider(&state).unwrap();
        assert_eq!(
            form.default_model, "client-key",
            "表单默认模型必须播种为 client key"
        );
        assert_eq!(form.model_rows[0].client_name, "client-key");
        assert_eq!(form.model_rows[0].display_name, "Client key");

        // 保存：默认模型按 client key 通过校验，--default-model 发出 client key，
        // --models-json 携带 clientName。
        let args = provider_update_args(&form, true).unwrap();
        let default_position = args
            .iter()
            .position(|arg| arg == "--default-model")
            .unwrap();
        assert_eq!(args[default_position + 1], "client-key");
        let json_position = args.iter().position(|arg| arg == "--models-json").unwrap();
        assert_eq!(
            args[json_position + 1],
            r#"[{"clientName":"client-key","name":"Client key","requestName":"upstream-name"}]"#
        );

        // 重载：client key、显示名、请求名与默认模型全部保持。
        let json = model_rows_json(&form).unwrap();
        let reloaded = format!(
            r#"{{"provider":{{"zen":{{"apiKind":"chat","options":{{"baseURL":"https://zen.example/v1"}},"models":{json},"defaultModel":"client-key"}}}}}}"#
        );
        let profiles = profiles_from_xu_chat_json(&reloaded).unwrap();
        assert_eq!(profiles[0].models, vec!["client-key"]);
        assert_eq!(profiles[0].default_model, "client-key");
        assert_eq!(profiles[0].request_name_for("client-key"), "upstream-name");
        assert_eq!(profiles[0].display_name_for("client-key"), "Client key");
    }

    #[test]
    fn model_rows_json_emits_variants_object() {
        // 行带 variants（逗号分隔档位）→ 序列化为 opencode 的
        // "variants": {"low": {}, "medium": {}, "high": {}} 对象。
        let form = ProviderAddForm {
            id: "zen".to_string(),
            name: "Zen".to_string(),
            base_url: "https://zen.example/v1".to_string(),
            default_model: "gpt".to_string(),
            model_rows: vec![ProviderModelRow {
                client_name: "gpt".to_string(),
                display_name: "GPT".to_string(),
                request_name: "gpt-upstream".to_string(),
                variants: "low, medium, high".to_string(),
            }],
            ..ProviderAddForm::default()
        };
        let json = model_rows_json(&form).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value,
            serde_json::json!([{
                "clientName": "gpt",
                "name": "GPT",
                "requestName": "gpt-upstream",
                "variants": { "low": {}, "medium": {}, "high": {} }
            }])
        );
    }

    #[test]
    fn model_rows_json_upgrades_plain_row_to_object_when_variants_present() {
        // client==display==request 的恒等行带 variants → 必须升级为对象，
        // 不能折叠成纯字符串（否则 variants 丢失）。
        let form = ProviderAddForm {
            id: "zen".to_string(),
            name: "Zen".to_string(),
            base_url: "https://zen.example/v1".to_string(),
            default_model: "plain".to_string(),
            model_rows: vec![ProviderModelRow {
                client_name: "plain".to_string(),
                display_name: "plain".to_string(),
                request_name: "plain".to_string(),
                variants: "xhigh,max".to_string(),
            }],
            ..ProviderAddForm::default()
        };
        let json = model_rows_json(&form).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value,
            serde_json::json!([{
                "name": "plain",
                "requestName": "plain",
                "variants": { "xhigh": {}, "max": {} }
            }])
        );
    }

    #[test]
    fn edit_form_from_selected_provider_loads_variants() {
        // 对象形式模型带 variants → 编辑表单回填为逗号分隔档位串。
        let store = r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://zen.example/v1"},"models":{"gpt-5.6":{"name":"GPT-5.6","variants":{"low":{},"medium":{},"high":{},"xhigh":{},"max":{}}}},"defaultModel":"gpt-5.6"}}}"#;
        let profiles = profiles_from_xu_chat_json(store).unwrap();
        let state = ProviderState {
            providers: profiles,
            error: None,
            sync_notice: None,
            selected: 0,
            current: std::collections::BTreeMap::new(),
            health: std::collections::BTreeMap::new(),
        };
        let form = edit_form_from_selected_provider(&state).unwrap();
        // 元数据按 BTreeMap 排序（serde_json 默认 Map），断言按集合比较。
        let mut loaded: Vec<&str> = form.model_rows[0].variants.split(',').collect();
        loaded.sort_unstable();
        assert_eq!(loaded, ["high", "low", "max", "medium", "xhigh"]);
    }

    #[test]
    fn detail_multi_models_args_preserves_variants() {
        // 详情页多选写入：模型带 variants 时输出必须携带 variants 对象。
        let store = r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://zen.example/v1"},"models":{"gpt-5.6":{"name":"GPT-5.6","variants":{"low":{},"high":{}}}},"defaultModel":"gpt-5.6"}}}"#;
        let profiles = profiles_from_xu_chat_json(store).unwrap();
        let args = detail_multi_models_args(
            &profiles[0],
            &["gpt-5.6".to_string()],
            &[0].into_iter().collect(),
        )
        .unwrap();
        let json_position = args.iter().position(|arg| arg == "--models-json").unwrap();
        let value: serde_json::Value = serde_json::from_str(&args[json_position + 1]).unwrap();
        assert_eq!(
            value,
            serde_json::json!([{
                "clientName": "gpt-5.6",
                "name": "GPT-5.6",
                "requestName": "gpt-5.6",
                "variants": { "low": {}, "high": {} }
            }])
        );
    }

    #[test]
    fn model_cell_push_edits_variants_column() {
        let mut form = ProviderAddForm::default();
        form.model_rows.push(ProviderModelRow {
            client_name: "m".to_string(),
            display_name: "M".to_string(),
            request_name: "m-up".to_string(),
            variants: String::new(),
        });
        form.model_cell = Some((0, 2));
        model_cell_push(&mut form, 'l');
        model_cell_push(&mut form, 'o');
        model_cell_push(&mut form, 'w');
        assert_eq!(form.model_rows[0].variants, "low");
        assert_eq!(form.model_rows[0].display_name, "M");
        assert_eq!(form.model_rows[0].request_name, "m-up");
    }

    #[test]
    fn model_switch_column_cycles_three_cells() {
        let mut form = ProviderAddForm::default();
        form.model_rows.push(ProviderModelRow::default());
        form.model_cell = Some((0, 0));
        model_switch_column(&mut form, true);
        assert_eq!(form.model_cell, Some((0, 1)));
        model_switch_column(&mut form, true);
        assert_eq!(form.model_cell, Some((0, 2)));
        model_switch_column(&mut form, true);
        assert_eq!(form.model_cell, Some((0, 0)));
        model_switch_column(&mut form, false);
        assert_eq!(form.model_cell, Some((0, 2)));
    }

    #[test]
    fn aliased_form_default_typed_as_display_name_resolves_to_client_key() {
        // 用户在表单手打显示名：落盘前解析为 client key（store 只认 client key）。
        let store = r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://zen.example/v1"},"models":[{"clientName":"client-key","name":"Client key","requestName":"upstream-name"}],"defaultModel":"client-key"}}}"#;
        let profiles = profiles_from_xu_chat_json(store).unwrap();
        let state = ProviderState {
            providers: profiles,
            error: None,
            sync_notice: None,
            selected: 0,
            current: std::collections::BTreeMap::new(),
            health: std::collections::BTreeMap::new(),
        };
        let mut form = edit_form_from_selected_provider(&state).unwrap();
        form.default_model = "Client key".to_string();
        let args = provider_update_args(&form, true).unwrap();
        let default_position = args
            .iter()
            .position(|arg| arg == "--default-model")
            .unwrap();
        assert_eq!(args[default_position + 1], "client-key");
    }

    #[test]
    fn empty_client_row_default_typed_as_display_name_emits_display_name() {
        // 回归（round 2）：model_add_row 新增行的 client key 为空，用户填显示名
        // "Foo"、默认模型填 "Foo" → --default-model 必须发出 "Foo"（落盘后
        // client key 由显示名派生，store 校验可过），不得发出 ""（空值被
        // validate_for_write 放行 → 默认模型静默丢弃）。
        let mut form = ProviderAddForm {
            id: "zen".to_string(),
            name: "Zen".to_string(),
            base_url: "https://zen.example/v1".to_string(),
            ..ProviderAddForm::default()
        };
        model_add_row(&mut form);
        assert_eq!(form.model_rows[0].client_name, "", "新增行 client key 为空");
        form.model_rows[0].display_name = "Foo".to_string();
        form.model_rows[0].request_name = "req".to_string();
        form.default_model = "Foo".to_string();

        let args = provider_update_args(&form, true).unwrap();
        let default_position = args
            .iter()
            .position(|arg| arg == "--default-model")
            .unwrap();
        assert_eq!(args[default_position + 1], "Foo");
    }

    #[test]
    fn cross_row_display_name_collision_resolves_client_key_first() {
        // 行 A 的显示名恰好等于行 B 的 client key：默认模型按行 B 的 client key
        // 播种时，client-key 匹配必须优先于显示名匹配，否则默认模型被行 A 劫持
        // （--default-model 错误发出 key-a，保存时默认模型静默翻转）。
        let mut form = ProviderAddForm {
            id: "zen".to_string(),
            name: "Zen".to_string(),
            base_url: "https://zen.example/v1".to_string(),
            ..ProviderAddForm::default()
        };
        form.model_rows.push(ProviderModelRow {
            client_name: "key-a".to_string(),
            display_name: "key-b".to_string(),
            request_name: "req-a".to_string(),
            variants: String::new(),
        });
        form.model_rows.push(ProviderModelRow {
            client_name: "key-b".to_string(),
            display_name: "Display B".to_string(),
            request_name: "req-b".to_string(),
            variants: String::new(),
        });
        form.default_model = "key-b".to_string();

        let args = provider_update_args(&form, true).unwrap();
        let default_position = args
            .iter()
            .position(|arg| arg == "--default-model")
            .unwrap();
        assert_eq!(
            args[default_position + 1],
            "key-b",
            "client key 匹配必须优先：显示名撞车不得把默认模型解析到别的行"
        );
    }

    #[test]
    fn detail_default_typed_as_display_name_resolves_to_client_key() {
        // 详情页与编辑表单一致：默认模型字段接受显示名输入，落盘前解析回
        // client key，不得把显示名原样发给 CLI（会得到 fail-closed 的
        // "default_model is not listed in models"）。
        let mut provider = detail_profile();
        provider.models = vec!["client-key".into()];
        provider.model_entries.insert(
            "client-key".into(),
            spec::domain::ModelEntry {
                client_name: "client-key".into(),
                display_name: "Client key".into(),
                request_name: "upstream-name".into(),
            },
        );
        let args = detail_update_args(&provider, 2, "Client key").unwrap();
        assert_eq!(&args[3..], ["--default-model", "client-key"]);
    }

    #[test]
    fn advance_model_focus_cycles_cells_and_default_field() {
        let mut form = form_with_rows(vec![("a", "a1"), ("b", "b1")]);
        advance_model_focus(&mut form, true);
        assert_eq!(form.model_cell, Some((0, 0)));
        advance_model_focus(&mut form, true);
        assert_eq!(form.model_cell, Some((0, 1)));
        advance_model_focus(&mut form, true);
        assert_eq!(form.model_cell, Some((0, 2)));
        advance_model_focus(&mut form, true);
        assert_eq!(form.model_cell, Some((1, 0)));
        advance_model_focus(&mut form, true);
        assert_eq!(form.model_cell, Some((1, 1)));
        advance_model_focus(&mut form, true);
        assert_eq!(form.model_cell, Some((1, 2)));
        advance_model_focus(&mut form, true);
        assert!(form.model_cell.is_none() && form.field == 6);
        advance_model_focus(&mut form, true);
        assert_eq!(form.model_cell, Some((0, 0)));
        // 从首行回退 → 回到默认模型字段
        advance_model_focus(&mut form, false);
        assert!(form.model_cell.is_none() && form.field == 6);
    }

    #[test]
    fn model_cell_edits_target_the_focused_column() {
        let mut form = form_with_rows(vec![("a", "a1")]);
        form.model_cell = Some((0, 0));
        model_cell_push(&mut form, 'X');
        assert_eq!(form.model_rows[0].display_name, "aX");
        model_cell_push(&mut form, ' ');
        assert_eq!(form.model_rows[0].display_name, "aX ");
        form.model_cell = Some((0, 1));
        model_cell_push(&mut form, '9');
        assert_eq!(form.model_rows[0].request_name, "a19");
        model_cell_pop(&mut form);
        assert_eq!(form.model_rows[0].request_name, "a1");
    }

    #[test]
    fn model_add_row_focuses_new_row() {
        let mut form = form_with_rows(vec![("a", "a1")]);
        model_add_row(&mut form);
        assert_eq!(form.model_rows.len(), 2);
        assert_eq!(form.model_cell, Some((1, 0)));
    }

    #[test]
    fn model_delete_focused_removes_row_and_clamps() {
        let mut form = form_with_rows(vec![("a", "a1"), ("b", "b1"), ("c", "c1")]);
        form.model_cell = Some((1, 1));
        model_delete_focused(&mut form);
        assert_eq!(form.model_rows.len(), 2);
        assert_eq!(form.model_rows[1].display_name, "c");
        assert_eq!(form.model_cell, Some((1, 1)));
        form.model_cell = Some((0, 0));
        model_delete_focused(&mut form);
        form.model_cell = Some((0, 0));
        model_delete_focused(&mut form);
        assert!(form.model_rows.is_empty());
        assert!(form.model_cell.is_none());
        assert_eq!(form.field, 6);
    }

    #[test]
    fn model_stale_cell_focus_keystroke_lands_on_existing_row() {
        let mut form = form_with_rows(vec![("kimi-a", "kimi-a"), ("kimi-b", "kimi-b")]);
        form.model_cell = Some((5, 1)); // 越界的陈旧焦点（行已被删除/替换）
        model_cell_push(&mut form, 'Z');
        assert_eq!(form.model_cell, Some((0, 1)));
        assert_eq!(form.model_rows[0].request_name, "kimi-aZ");
    }

    #[test]
    fn model_navigation_wraps_within_rows_and_default_field() {
        let mut form = form_with_rows(vec![("kimi-a", "a"), ("kimi-b", "b"), ("other", "o")]);
        advance_model_focus(&mut form, true);
        assert_eq!(form.model_cell, Some((0, 0)));
        advance_model_focus(&mut form, true);
        advance_model_focus(&mut form, true);
        advance_model_focus(&mut form, true);
        assert_eq!(form.model_cell, Some((1, 0)));
        advance_model_focus(&mut form, true);
        advance_model_focus(&mut form, true);
        advance_model_focus(&mut form, true);
        assert_eq!(form.model_cell, Some((2, 0)));
        advance_model_focus(&mut form, true);
        advance_model_focus(&mut form, true);
        assert_eq!(form.model_cell, Some((2, 2)));
        advance_model_focus(&mut form, true);
        assert!(form.model_cell.is_none() && form.field == 6);
        advance_model_focus(&mut form, true);
        assert_eq!(form.model_cell, Some((0, 0)));
        // 陈旧焦点也能从现有行继续导航
        form.model_cell = Some((2, 0));
        advance_model_focus(&mut form, true);
        assert_eq!(form.model_cell, Some((2, 1)));
        form.model_cell = Some((2, 1));
        advance_model_focus(&mut form, false);
        assert_eq!(form.model_cell, Some((2, 0)));
        advance_model_focus(&mut form, false);
        assert_eq!(form.model_cell, Some((1, 2)));
    }

    #[test]
    fn parse_fetch_models_output_extracts_ids_and_strips_owned_by() {
        let output = "Provider: zen\nFetched models: 2\ndeepseek-v4-flash-free (owned_by: deepseek)\nplain-model\n";
        let rows = parse_fetch_models_output(output).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].display_name, "deepseek-v4-flash-free");
        assert_eq!(rows[0].request_name, "deepseek-v4-flash-free");
        assert_eq!(rows[1].display_name, "plain-model");
    }

    #[test]
    fn parse_fetch_models_output_rejects_unexpected_shape() {
        assert!(parse_fetch_models_output("garbage output").is_err());
        assert!(parse_fetch_models_output("Provider: zen\nFetched models: 0\n").is_err());
    }

    #[test]
    fn apply_models_fetch_result_updates_rows_and_feedback() {
        let mut form = form_with_rows(vec![("old", "old")]);
        form.model_cell = Some((0, 1));
        apply_models_fetch_result(
            &mut form,
            Ok(vec![ProviderModelRow {
                client_name: "new".into(),
                display_name: "new".into(),
                request_name: "new-upstream".into(),
                variants: String::new(),
            }]),
        );
        assert_eq!(form.model_rows[0].request_name, "new-upstream");
        assert_eq!(form.model_cell, None);
        assert!(form
            .model_fetch_msg
            .as_deref()
            .unwrap()
            .contains("拉取成功"));
    }

    #[test]
    fn apply_models_fetch_error_preserves_rows_and_cell() {
        let mut form = form_with_rows(vec![("old", "old")]);
        form.model_cell = Some((0, 1));
        apply_models_fetch_result(&mut form, Err("network down".into()));
        assert_eq!(form.model_rows[0].request_name, "old");
        assert_eq!(form.model_cell, Some((0, 1)));
        assert_eq!(
            form.model_fetch_msg.as_deref(),
            Some("拉取失败：network down")
        );
    }

    #[test]
    fn update_args_emit_models_json_and_validate_default() {
        let form = form_with_rows(vec![("DeepSeek V4", "deepseek-v4-flash-free")]);
        let args = provider_update_args(&form, true).unwrap();
        let position = args.iter().position(|arg| arg == "--models-json").unwrap();
        let json = &args[position + 1];
        assert_eq!(
            json,
            r#"[{"name":"DeepSeek V4","requestName":"deepseek-v4-flash-free"}]"#
        );
        assert!(!args.iter().any(|arg| arg == "--model"));

        let bad = ProviderAddForm {
            default_model: "missing".to_string(),
            ..form
        };
        let err = provider_update_args(&bad, true).unwrap_err();
        assert!(err.contains("默认模型"), "got: {err}");
    }

    #[test]
    fn bypass_read_false_when_settings_missing_or_empty() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        assert!(!read_bypass_permissions(home));
        let path = claude_settings_path(home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{}").unwrap();
        assert!(!read_bypass_permissions(home));
    }

    #[test]
    fn bypass_enable_adds_only_default_mode_and_preserves_other_fields() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = claude_settings_path(home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://x"},"permissions":{"additionalDirectories":["/sdcard"]},"model":"opus"}"#,
        )
        .unwrap();

        set_bypass_permissions(home, true).unwrap();

        let root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["permissions"]["defaultMode"], "bypassPermissions");
        assert_eq!(root["permissions"]["additionalDirectories"][0], "/sdcard");
        assert_eq!(root["env"]["ANTHROPIC_BASE_URL"], "https://x");
        assert_eq!(root["model"], "opus");
        assert!(read_bypass_permissions(home));
    }

    #[test]
    fn bypass_disable_removes_only_default_mode_keeping_other_permission_keys() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = claude_settings_path(home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://x"},"permissions":{"defaultMode":"bypassPermissions","additionalDirectories":["/sdcard"],"allow":["Bash(npm run:*):*"]},"model":"opus"}"#,
        )
        .unwrap();

        set_bypass_permissions(home, false).unwrap();

        let root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(root["permissions"].get("defaultMode").is_none());
        assert_eq!(root["permissions"]["additionalDirectories"][0], "/sdcard");
        assert!(root["permissions"]["allow"][0]
            .as_str()
            .unwrap()
            .starts_with("Bash(npm"));
        assert_eq!(root["env"]["ANTHROPIC_BASE_URL"], "https://x");
        assert_eq!(root["model"], "opus");
        assert!(!read_bypass_permissions(home));
    }

    #[test]
    fn bypass_disable_drops_whole_permissions_object_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = claude_settings_path(home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"env":{"X":"1"},"permissions":{"defaultMode":"bypassPermissions"}}"#,
        )
        .unwrap();

        set_bypass_permissions(home, false).unwrap();

        let root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(root.get("permissions").is_none());
        assert_eq!(root["env"]["X"], "1");
    }

    #[test]
    fn terminal_title_toggle_adds_and_removes_env_key() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = claude_settings_path(home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://x","CLAUDE_CODE_DISABLE_TERMINAL_TITLE":"1"},"model":"opus"}"#,
        )
        .unwrap();
        assert!(read_terminal_title_disabled(home));

        toggle_terminal_title(home, false).unwrap();
        let root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(root["env"]
            .get("CLAUDE_CODE_DISABLE_TERMINAL_TITLE")
            .is_none());
        assert_eq!(root["env"]["ANTHROPIC_BASE_URL"], "https://x");
        assert_eq!(root["model"], "opus");
        assert!(!read_terminal_title_disabled(home));

        toggle_terminal_title(home, true).unwrap();
        let root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["env"]["CLAUDE_CODE_DISABLE_TERMINAL_TITLE"], "1");
        assert!(read_terminal_title_disabled(home));
    }

    #[test]
    fn terminal_title_disable_drops_env_when_empty_and_skips_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = claude_settings_path(home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"env":{"CLAUDE_CODE_DISABLE_TERMINAL_TITLE":"1"}}"#,
        )
        .unwrap();
        toggle_terminal_title(home, false).unwrap();
        let root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(root.get("env").is_none());
        assert!(!read_terminal_title_disabled(home));

        let empty = tempfile::tempdir().unwrap();
        assert!(toggle_terminal_title(empty.path(), false).is_ok());
    }

    #[test]
    fn opencode_permission_toggle_inserts_allow_and_removes_key() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = home.join(".config/opencode/opencode.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"$schema":"https://opencode.ai/config.json","permission":"allow"}"#,
        )
        .unwrap();
        assert!(read_opencode_permission_allow(home));

        toggle_opencode_permission(home, false).unwrap();
        let root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(root.get("permission").is_none());
        assert!(!read_opencode_permission_allow(home));

        toggle_opencode_permission(home, true).unwrap();
        let root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["permission"], "allow");
        assert!(read_opencode_permission_allow(home));
        // 关闭时文件不存在 → 无需修改
        let empty = tempfile::tempdir().unwrap();
        assert!(toggle_opencode_permission(empty.path(), false).is_ok());
    }

    #[test]
    fn route_toggle_enables_and_disables_with_provider_list() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let routes = home.join(".codex/xu-client-routes.json");
        fs::create_dir_all(routes.parent().unwrap()).unwrap();
        fs::write(
            &routes,
            r#"{"clients":{"codex":{"enabled":true,"mode":"single","providers":["zen"]}}}"#,
        )
        .unwrap();
        assert_eq!(
            read_route_status(home, AgentTarget::Codex, "zen"),
            (true, true)
        );
        assert_eq!(
            read_route_status(home, AgentTarget::ClaudeCode, "zen"),
            (false, false)
        );

        // 关闭：移除 providers + enabled=false
        toggle_route(home, AgentTarget::Codex, "zen").unwrap();
        assert_eq!(
            read_route_status(home, AgentTarget::Codex, "zen"),
            (false, false)
        );
        // 再开：加回 providers
        toggle_route(home, AgentTarget::Codex, "zen").unwrap();
        assert_eq!(
            read_route_status(home, AgentTarget::Codex, "zen"),
            (true, true)
        );
        // 缺失条目：按 single 创建
        toggle_route(home, AgentTarget::ClaudeCode, "zen").unwrap();
        assert_eq!(
            read_route_status(home, AgentTarget::ClaudeCode, "zen"),
            (true, true)
        );
        let root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&routes).unwrap()).unwrap();
        assert_eq!(root["clients"]["claude"]["mode"], "single");
        assert_eq!(root["clients"]["codex"]["mode"], "single");
    }

    #[test]
    fn route_toggle_preserves_other_clients_and_fields() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let routes = home.join(".codex/xu-client-routes.json");
        fs::create_dir_all(routes.parent().unwrap()).unwrap();
        fs::write(
            &routes,
            r#"{"clients":{"opencode":{"enabled":true,"mode":"multi","providers":["a","b"]},"codex":{"enabled":true,"mode":"single","providers":["zen","other"]}}}"#,
        )
        .unwrap();
        toggle_route(home, AgentTarget::Codex, "zen").unwrap();
        let root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&routes).unwrap()).unwrap();
        assert_eq!(root["clients"]["opencode"]["mode"], "multi");
        assert_eq!(root["clients"]["opencode"]["providers"][1], "b");
        assert_eq!(
            root["clients"]["codex"]["providers"],
            serde_json::json!(["other"])
        );
        // 移除一个供应商但列表仍有其他项时，路由保持启用（新契约）。
        assert_eq!(root["clients"]["codex"]["enabled"], true);
    }

    #[test]
    fn bypass_disable_without_permissions_leaves_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = claude_settings_path(home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = r#"{"env":{"X":"1"},"model":"sonnet"}"#;
        fs::write(&path, original).unwrap();

        set_bypass_permissions(home, false).unwrap();

        let root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root, serde_json::json!({"env":{"X":"1"},"model":"sonnet"}));
    }

    #[test]
    fn bypass_enable_creates_settings_file_from_scratch() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        set_bypass_permissions(home, true).unwrap();
        let root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(claude_settings_path(home)).unwrap()).unwrap();
        assert_eq!(root["permissions"]["defaultMode"], "bypassPermissions");
        let mode = fs::metadata(claude_settings_path(home))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn model_rows_preview_marks_mapped_rows() {
        let form = form_with_rows(vec![
            ("DeepSeek V4", "deepseek-v4-flash-free"),
            ("plain", "plain"),
        ]);
        assert_eq!(
            model_rows_preview(&form),
            "DeepSeek V4 → deepseek-v4-flash-free, plain"
        );
    }

    #[test]
    fn provider_add_args_use_table_rows_in_order() {
        let form = form_with_rows(vec![
            ("DeepSeek V4", "deepseek-v4-flash-free"),
            ("plain", "plain"),
        ]);
        let args = provider_add_args(&form, true).unwrap();
        let model_args = args
            .windows(2)
            .filter_map(|pair| (pair[0] == "--model").then_some(pair[1].clone()))
            .collect::<Vec<_>>();
        assert_eq!(model_args, ["DeepSeek V4", "plain"]);
        assert_eq!(args[1], "add");
    }

    #[test]
    fn provider_add_args_fall_back_to_text_models_when_table_empty() {
        let form = ProviderAddForm {
            id: "demo".to_string(),
            base_url: "https://example.com/v1".to_string(),
            models: "gpt-4.1, o3".to_string(),
            ..ProviderAddForm::default()
        };
        let args = provider_add_args(&form, true).unwrap();
        let model_args = args
            .windows(2)
            .filter_map(|pair| (pair[0] == "--model").then_some(pair[1].clone()))
            .collect::<Vec<_>>();
        assert_eq!(model_args, ["gpt-4.1", "o3"]);
    }

    #[test]
    fn add_form_text_edit_rebuilds_table_rows_and_rebases_default() {
        let mut form = ProviderAddForm {
            models: "a, b".to_string(),
            default_model: "zz".to_string(),
            ..ProviderAddForm::default()
        };
        sync_add_models_from_text(&mut form);
        assert_eq!(form.model_rows.len(), 2);
        assert_eq!(form.model_rows[1].display_name, "b");
        assert_eq!(form.default_model, "a");
        form.field = 5;
        provider_add_form_push(&mut form, 'c');
        assert_eq!(form.model_rows.len(), 2);
        assert_eq!(form.model_rows[1].display_name, "bc");
        provider_add_form_pop(&mut form);
        assert_eq!(form.model_rows.len(), 2);
        assert_eq!(form.model_rows[1].display_name, "b");
        provider_add_form_pop(&mut form);
        assert_eq!(form.model_rows.len(), 1);
    }

    #[test]
    fn focus_switch_between_cell_and_field_routes_typing() {
        let mut form = form_with_rows(vec![("a", "a1")]);
        form.default_model.clear();
        focus_model_cell(&mut form, 0, 0);
        model_cell_push(&mut form, 'x');
        assert_eq!(form.model_rows[0].display_name, "ax");
        focus_edit_field(&mut form, 6);
        model_cell_push(&mut form, 'y');
        assert_eq!(form.default_model, "y");
        assert_eq!(form.model_rows[0].display_name, "ax", "击键不得再进单元格");
        model_cell_pop(&mut form);
        assert_eq!(form.default_model, "");
        focus_model_cell(&mut form, 0, 1);
        model_cell_push(&mut form, '9');
        assert_eq!(form.model_rows[0].request_name, "a19");
    }

    #[test]
    fn bypass_disable_without_settings_file_does_not_create_it() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = claude_settings_path(home);
        assert!(!path.exists());
        let message = set_bypass_permissions(home, false).unwrap();
        assert!(message.contains("已关闭"));
        assert!(!path.exists(), "关闭免确认不应创建 settings.json");
    }
}

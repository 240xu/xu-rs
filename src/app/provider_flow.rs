use std::sync::mpsc::Receiver;

use crate::config;
use crate::ProviderState;
use spec::domain::ProviderProfile;

pub fn start_provider_test(state: &ProviderState) -> Option<Receiver<String>> {
    let provider = state.providers.get(state.selected)?;
    let provider = provider.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = test_provider(provider);
        let _ = tx.send(result);
    });
    Some(rx)
}

pub fn start_provider_models_fetch(state: &ProviderState) -> Option<Receiver<String>> {
    let provider = state.providers.get(state.selected)?;
    let provider = provider.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = fetch_provider_models_message(provider);
        let _ = tx.send(result);
    });
    Some(rx)
}

/// 模型列表异步拉取（带本地缓存）：线程内联网拉取并刷新缓存。
/// `force=true` 忽略缓存；返回的通道在完成后给出 列表/错误。
pub fn start_models_fetch_cached(
    state: &ProviderState,
    force: bool,
) -> Option<Receiver<Result<Vec<String>, String>>> {
    let provider = state.providers.get(state.selected)?.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = crate::app::provider_ops::fetch_models_cached(&provider, force);
        let _ = tx.send(result);
    });
    Some(rx)
}

/// 编辑表单的模型拉取任务：携带 generation 与 provider id，事件循环据此
/// 丢弃陈旧结果（表单可编辑的 `form.id` 可以与 `ProviderState.selected`
/// 不同，不能复用详情页的按选中 provider 派发的通道）。
pub struct FormModelsFetchJob {
    pub generation: u64,
    pub provider_id: String,
    pub receiver: Receiver<Result<Vec<crate::app::provider_ops::ProviderModelRow>, String>>,
}

/// 为编辑表单启动一次后台模型拉取：id 去空格后为空 → `None`；
/// 否则在线程内执行 `provider fetch-models <id>` 并把结果发回通道。
/// 只有 worker 闭包调用 `spec::cli::run_command()`，事件循环不阻塞。
pub fn start_form_models_fetch(provider_id: String, generation: u64) -> Option<FormModelsFetchJob> {
    let id = provider_id.trim();
    if id.is_empty() {
        return None;
    }
    let id = id.to_string();
    let id_for_worker = id.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let args = vec![
            "provider".to_string(),
            "fetch-models".to_string(),
            id_for_worker,
        ];
        let result = match spec::cli::run_command(&config::home(), &args) {
            Some(Ok(output)) => crate::app::provider_ops::parse_fetch_models_output(&output),
            Some(Err(error)) => Err(error),
            None => Err("provider fetch-models 命令不可用".to_string()),
        };
        let _ = tx.send(result);
    });
    Some(FormModelsFetchJob {
        generation,
        provider_id: id,
        receiver: rx,
    })
}

pub fn provider_test_start_message() -> String {
    "测试中…\n\n正在请求供应商接口（密钥不会显示）。\n完成后自动更新结果。".to_string()
}

pub fn provider_models_start_message() -> String {
    "模型拉取中…\n\n正在请求供应商模型列表。仅预览，不自动写回配置。".to_string()
}

pub fn test_provider(provider: ProviderProfile) -> String {
    let result = spec::provider_check::test_provider_connection(&provider);
    let cache_note = spec::state::set_provider_health(
        &config::home(),
        &provider.id,
        &provider.api_key,
        result.ok,
        result.latency_ms,
        &result.message,
    )
    .err()
    .map(|error| format!("\n健康状态缓存失败：{error}"))
    .unwrap_or_default();
    format!(
        "供应商：{}\n协议：{}\n结果：{}\n\n{}{}\n\n密钥未显示。Esc 返回详情。",
        provider.name,
        provider.protocol.as_str(),
        if result.ok { "成功" } else { "失败/跳过" },
        result.message,
        cache_note,
    )
}

pub fn fetch_provider_models_message(provider: ProviderProfile) -> String {
    match spec::provider_check::fetch_provider_models(&provider) {
        Ok(models) => {
            let preview = models
                .iter()
                .take(80)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let more = if models.len() > 80 {
                format!("\n... 还有 {} 个模型未显示", models.len() - 80)
            } else {
                String::new()
            };
            format!(
                "供应商：{}\n结果：找到 {} 个模型\n\n{}{}\n\n未写文件。应用可用：\nxu provider models {} --apply --yes\n\nEsc 返回详情。",
                provider.name,
                models.len(),
                preview,
                more,
                provider.id,
            )
        }
        Err(error) => format!(
            "供应商：{}\n模型拉取失败/跳过：{}\n\n密钥未显示。Esc 返回详情。",
            provider.name, error
        ),
    }
}

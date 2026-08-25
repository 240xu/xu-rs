use std::net::IpAddr;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde_json::{json, Value};

use super::ir::{BridgeError, MediaIr};
use super::request::{object, reject_unknown_fields, required_string};

pub(crate) const OPENAI_REASONING_ITEM_PREFIX: &str = "ccswitch-openai-reasoning-v1:";
const MAX_MEDIA_DATA_BYTES: usize = 2 * 1024 * 1024;
const MAX_REASONING_ENVELOPE_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn reasoning_summary_text(item: &Value) -> String {
    item.get("summary")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| {
            matches!(
                part.get("type").and_then(Value::as_str),
                Some("summary_text" | "reasoning_text")
            )
            .then(|| part.get("text").and_then(Value::as_str))
            .flatten()
        })
        .collect::<Vec<_>>()
        .join("")
}

pub(crate) fn encode_openai_reasoning_item(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return None;
    }
    let bytes = serde_json::to_vec(item).ok()?;
    Some(format!(
        "{OPENAI_REASONING_ITEM_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(bytes)
    ))
}

pub(crate) fn decode_openai_reasoning_item(encoded: &str) -> Option<Value> {
    if encoded.len() > MAX_REASONING_ENVELOPE_BYTES {
        return None;
    }
    let payload = encoded.strip_prefix(OPENAI_REASONING_ITEM_PREFIX)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let item: Value = serde_json::from_slice(&bytes).ok()?;
    (item.get("type").and_then(Value::as_str) == Some("reasoning")).then_some(item)
}

pub(crate) fn anthropic_block_from_openai_reasoning_item(item: &Value) -> Option<Value> {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return None;
    }

    let text = reasoning_summary_text(item);
    let has_encrypted_content = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty());

    if has_encrypted_content {
        let envelope = encode_openai_reasoning_item(item)?;
        if text.is_empty() {
            return Some(json!({
                "type": "redacted_thinking",
                "data": envelope
            }));
        }
        return Some(json!({
            "type": "thinking",
            "thinking": text,
            "signature": envelope
        }));
    }

    (!text.is_empty()).then(|| {
        json!({
            "type": "thinking",
            "thinking": text
        })
    })
}

pub(crate) fn openai_reasoning_item_from_anthropic_block(block: &Value) -> Option<Value> {
    match block.get("type").and_then(Value::as_str) {
        Some("thinking") => block
            .get("signature")
            .and_then(Value::as_str)
            .and_then(decode_openai_reasoning_item),
        Some("redacted_thinking") => block
            .get("data")
            .and_then(Value::as_str)
            .and_then(decode_openai_reasoning_item),
        _ => None,
    }
}

pub(super) fn media_from_url(url: &str) -> Result<MediaIr, BridgeError> {
    if url.trim().is_empty() {
        return Err(BridgeError::Unsupported {
            field: "media.url".to_string(),
        });
    }
    if url.starts_with("data:") {
        return parse_data_url(url);
    }
    validate_remote_url(url)?;
    Ok(MediaIr {
        media_type: None,
        url: Some(url.to_string()),
        data: None,
    })
}

pub(super) fn media_from_source(source: &Value) -> Result<MediaIr, BridgeError> {
    let object = object(source)?;
    reject_unknown_fields(object, &["type", "media_type", "data", "url"])?;
    match object
        .get("type")
        .and_then(Value::as_str)
        .ok_or(BridgeError::InvalidRequest)?
    {
        "base64" => media_from_base64(
            object.get("media_type").and_then(Value::as_str),
            &required_string(object, "data")?,
        ),
        "url" => {
            let mut media = media_from_url(&required_string(object, "url")?)?;
            if media.media_type.is_none() {
                media.media_type = object
                    .get("media_type")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Ok(media)
        }
        _ => Err(BridgeError::Unsupported {
            field: "media.source.type".to_string(),
        }),
    }
}

pub(super) fn media_from_base64(
    media_type: Option<&str>,
    data: &str,
) -> Result<MediaIr, BridgeError> {
    validate_media_type(media_type)?;
    validate_base64(data)?;
    Ok(MediaIr {
        media_type: media_type.map(str::to_owned),
        url: None,
        data: Some(data.to_string()),
    })
}

pub(super) fn media_to_anthropic_source(media: &MediaIr) -> Result<Value, BridgeError> {
    if let Some(url) = &media.url {
        if url.starts_with("data:") {
            let parsed = parse_data_url(url)?;
            return media_to_anthropic_source(&parsed);
        }
        validate_remote_url(url)?;
        return Ok(json!({ "type": "url", "url": url }));
    }

    let media_type = media.media_type.as_deref();
    let data = media.data.as_deref().ok_or(BridgeError::InvalidRequest)?;
    validate_media_type(media_type)?;
    validate_base64(data)?;
    Ok(json!({
        "type": "base64",
        "media_type": media_type.ok_or(BridgeError::InvalidRequest)?,
        "data": data
    }))
}

pub(super) fn media_to_url(media: &MediaIr) -> Result<String, BridgeError> {
    if let Some(url) = &media.url {
        return if url.starts_with("data:") {
            parse_data_url(url).and_then(|parsed| media_to_url(&parsed))
        } else {
            validate_remote_url(url)?;
            Ok(url.clone())
        };
    }

    let media_type = media.media_type.as_deref();
    let data = media.data.as_deref().ok_or(BridgeError::InvalidRequest)?;
    validate_media_type(media_type)?;
    validate_base64(data)?;
    Ok(format!(
        "data:{};base64,{}",
        media_type.ok_or(BridgeError::InvalidRequest)?,
        data
    ))
}

pub(super) fn media_to_responses_file(media: &MediaIr) -> Result<Value, BridgeError> {
    if media.data.is_some() {
        return Ok(json!({ "file_data": media_to_url(media)? }));
    }
    Ok(json!({ "file_url": media_to_url(media)? }))
}

fn parse_data_url(url: &str) -> Result<MediaIr, BridgeError> {
    let payload = url.strip_prefix("data:").ok_or(BridgeError::Unsupported {
        field: "media.url".to_string(),
    })?;
    let (metadata, data) = payload.split_once(',').ok_or(BridgeError::Unsupported {
        field: "media.url".to_string(),
    })?;
    let (media_type, encoding) = metadata.split_once(';').ok_or(BridgeError::Unsupported {
        field: "media.url".to_string(),
    })?;
    if encoding != "base64" {
        return Err(BridgeError::Unsupported {
            field: "media.url".to_string(),
        });
    }
    validate_media_type(Some(media_type))?;
    validate_base64(data)?;
    Ok(MediaIr {
        media_type: Some(media_type.to_string()),
        url: None,
        data: Some(data.to_string()),
    })
}

fn validate_media_type(media_type: Option<&str>) -> Result<(), BridgeError> {
    let Some(media_type) = media_type else {
        return Err(BridgeError::Unsupported {
            field: "media.media_type".to_string(),
        });
    };
    let valid = media_type.contains('/')
        && media_type
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/+-.".contains(&byte));
    if !valid {
        return Err(BridgeError::Unsupported {
            field: "media.media_type".to_string(),
        });
    }
    Ok(())
}

fn validate_base64(data: &str) -> Result<(), BridgeError> {
    if data.is_empty() {
        return Err(BridgeError::Unsupported {
            field: "media.data".to_string(),
        });
    }
    if data.len() > MAX_MEDIA_DATA_BYTES {
        return Err(BridgeError::ResourceLimit);
    }
    if data.len() % 4 == 1 {
        return Err(BridgeError::Unsupported {
            field: "media.data".to_string(),
        });
    }
    let mut padding = 0;
    for (index, byte) in data.bytes().enumerate() {
        if byte == b'=' {
            padding += 1;
            if index < data.len().saturating_sub(2) || padding > 2 {
                return Err(BridgeError::Unsupported {
                    field: "media.data".to_string(),
                });
            }
        } else if padding > 0 || !(byte.is_ascii_alphanumeric() || b"+/-_".contains(&byte)) {
            return Err(BridgeError::Unsupported {
                field: "media.data".to_string(),
            });
        }
    }
    if padding > 0 && !data.len().is_multiple_of(4) {
        return Err(BridgeError::Unsupported {
            field: "media.data".to_string(),
        });
    }
    Ok(())
}

fn validate_remote_url(url: &str) -> Result<(), BridgeError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| BridgeError::Unsupported {
        field: "media.url".to_string(),
    })?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(BridgeError::Unsupported {
            field: "media.url".to_string(),
        });
    }
    let host = parsed
        .host_str()
        .expect("checked above")
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or_else(|| parsed.host_str().expect("checked above"))
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() || is_local_hostname(&host) {
        return Err(BridgeError::Unsupported {
            field: "media.url".to_string(),
        });
    }
    // 字面 IP 直接校验；域名尽力而为：解析成功则对全部结果校验
    // （防 DNS→内网绕过），解析器故障不硬拒——后续真实拉取会自然失败。
    let addresses: Vec<IpAddr> = if let Ok(address) = host.parse::<IpAddr>() {
        vec![address]
    } else {
        use std::net::ToSocketAddrs;
        match (host.as_str(), 0u16).to_socket_addrs() {
            Ok(sockets) => sockets.map(|socket| socket.ip()).collect(),
            Err(_) => return Ok(()),
        }
    };
    if addresses.is_empty() || addresses.iter().any(|a| resolved_ip_is_local(*a)) {
        return Err(BridgeError::Unsupported {
            field: "media.url".to_string(),
        });
    }
    Ok(())
}

/// 解析结果的判定比字面 IP 宽：198.18.0.0/15 同时是 IANA 基准测试保留段与
/// Clash/mihomo fake-ip 默认池。在客户端侧校验 URL 时，域名经本机 DNS 落到
/// 该段属于正常代理上网而非 SSRF；直写该段的字面 IP 仍被 is_local_address 拒绝。
fn resolved_ip_is_local(address: IpAddr) -> bool {
    if let IpAddr::V4(v4) = address {
        if v4.octets()[0] == 198 && (v4.octets()[1] & 0xFE) == 18 {
            return false;
        }
    }
    if let IpAddr::V6(v6) = address {
        if let Some(mapped) = v6.to_ipv4_mapped() {
            if mapped.octets()[0] == 198 && (mapped.octets()[1] & 0xFE) == 18 {
                return false;
            }
        }
    }
    is_local_address(address)
}

fn is_local_hostname(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "localhost.localdomain"
        || host.ends_with(".localdomain")
        || host == "local"
        || host.ends_with(".local")
        || host == "broadcasthost"
        || host == "ip6-localhost"
        || host == "ip6-loopback"
        || host == "host.docker.internal"
        || host == "gateway.docker.internal"
        || host == "host.containers.internal"
        || host == "gateway.containers.internal"
        || host.ends_with(".internal")
}

fn is_local_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_loopback()
                || address.is_private()
                || address.is_unspecified()
                || address.is_link_local()
                || address.is_multicast()
                || address.is_broadcast()
                // CGNAT 100.64.0.0/10 与基准测试段 198.18.0.0/15 同属不可公网路由。
                || address.octets()[0] == 100 && (address.octets()[1] & 0b1100_0000) == 64
                || (address.octets()[0] == 198 && (address.octets()[1] & 0xFE) == 18)
        }
        IpAddr::V6(address) => {
            address
                .to_ipv4_mapped()
                .is_some_and(|mapped| is_local_address(IpAddr::V4(mapped)))
                || address.is_loopback()
                || address.is_unspecified()
                || address.is_unique_local()
                || address.is_unicast_link_local()
                || address.is_multicast()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_envelope_preserves_summary_and_rejects_wrong_signatures() {
        let item = json!({
            "id": "rs_1",
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": "Need a tool."}],
            "encrypted_content": "opaque"
        });
        let block = anthropic_block_from_openai_reasoning_item(&item).expect("block");
        assert_eq!(block["type"], json!("thinking"));
        assert_eq!(block["thinking"], json!("Need a tool."));
        assert_eq!(
            openai_reasoning_item_from_anthropic_block(&block),
            Some(item)
        );
        assert!(openai_reasoning_item_from_anthropic_block(&json!({
            "type": "thinking",
            "thinking": "visible",
            "signature": "arbitrary"
        }))
        .is_none());
        assert!(openai_reasoning_item_from_anthropic_block(&json!({
            "type": "thinking",
            "thinking": "visible",
            "signature": "ccswitch-openai-reasoning-v1:eyJ0eXBlIjoiY2hhdCJ9"
        }))
        .is_none());
    }

    #[test]
    fn reasoning_envelope_handles_encrypted_only_and_visible_only_items() {
        let encrypted = json!({
            "type": "reasoning",
            "summary": [],
            "encrypted_content": "opaque"
        });
        let encrypted_block =
            anthropic_block_from_openai_reasoning_item(&encrypted).expect("block");
        assert_eq!(encrypted_block["type"], json!("redacted_thinking"));
        assert_eq!(
            openai_reasoning_item_from_anthropic_block(&encrypted_block),
            Some(encrypted)
        );

        let visible = json!({
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": "visible"}]
        });
        let visible_block = anthropic_block_from_openai_reasoning_item(&visible).expect("block");
        assert_eq!(visible_block["type"], json!("thinking"));
        assert!(visible_block.get("signature").is_none());
        assert!(openai_reasoning_item_from_anthropic_block(&visible_block).is_none());
    }

    #[test]
    fn reasoning_envelope_uses_url_safe_no_pad_encoding() {
        let item = json!({"type": "reasoning", "summary": [], "encrypted_content": "opaque"});
        let encoded = encode_openai_reasoning_item(&item).expect("encoded");
        assert!(encoded.starts_with(OPENAI_REASONING_ITEM_PREFIX));
        assert!(!encoded.contains('='));
        assert_eq!(decode_openai_reasoning_item(&encoded), Some(item));
        assert!(decode_openai_reasoning_item("wrong-prefix").is_none());
    }

    #[test]
    fn media_accepts_safe_sources_without_decoding_into_unbounded_buffers() {
        let data = media_from_url("data:image/png;base64,aGVsbG8").expect("data URL");
        assert_eq!(
            media_to_url(&data).expect("data URL output"),
            "data:image/png;base64,aGVsbG8"
        );
        assert!(media_from_url("/tmp/image.png").is_err());
        assert!(media_from_url("data:image/png;base64,not valid").is_err());
        assert!(media_from_url("http://127.0.0.1/image.png").is_err());
        assert!(media_from_url("https://cdn.example/image.png").is_ok());
    }
}

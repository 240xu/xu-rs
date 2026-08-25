use serde_json::Value;

use super::ir::BridgeError;
use super::WireProtocol;

/// Extension point for error-driven request rectification (CC Switch style):
/// when an upstream rejects a converted request, a rectifier may rewrite the
/// body and allow a retry. `DefaultRectifier` (production) strips
/// thinking/redacted_thinking fields when the upstream rejects them;
/// `NoopRectifier` is the historical no-op behavior.
pub trait RequestRectifier {
    /// Returns a rewritten request body to retry, or `None` to surface the
    /// original error. Implementations must never leak credentials into the
    /// rewritten body.
    fn rectify(&self, protocol: WireProtocol, body: &Value, error: &BridgeError) -> Option<Value>;
}

/// Conservative signal that an upstream error body is about the `thinking`
/// field: the caller reads the bounded error body and only attempts
/// rectification when this matches, so `DefaultRectifier` never rewrites a
/// request for an unrelated rejection.
pub fn thinking_rejection(error_body: &str) -> bool {
    error_body.contains("thinking")
}

pub struct NoopRectifier;

impl RequestRectifier for NoopRectifier {
    fn rectify(
        &self,
        _protocol: WireProtocol,
        _body: &Value,
        _error: &BridgeError,
    ) -> Option<Value> {
        None
    }
}

/// Strips thinking-related fields from a converted request body after the
/// upstream rejects them: the top-level `thinking` object (Anthropic
/// protocol) and `thinking`/`redacted_thinking` content blocks inside
/// assistant messages. Only the fields are removed; everything else is
/// preserved, and `None` is returned when there is nothing to strip.
pub struct DefaultRectifier;

impl RequestRectifier for DefaultRectifier {
    fn rectify(&self, protocol: WireProtocol, body: &Value, error: &BridgeError) -> Option<Value> {
        if !matches!(error, BridgeError::InvalidUpstream) {
            return None;
        }
        let mut body = body.clone();
        let mut changed = false;
        if protocol == WireProtocol::AnthropicMessages {
            if let Some(object) = body.as_object_mut() {
                changed |= object.remove("thinking").is_some();
            }
        }
        if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
            for message in messages {
                if message.get("role").and_then(Value::as_str) != Some("assistant") {
                    continue;
                }
                let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
                    continue;
                };
                let before = blocks.len();
                blocks.retain(|block| {
                    !matches!(
                        block.get("type").and_then(Value::as_str),
                        Some("thinking") | Some("redacted_thinking")
                    )
                });
                changed |= blocks.len() != before;
            }
        }
        changed.then_some(body)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::super::ir::BridgeError;
    use super::super::WireProtocol;
    use super::{thinking_rejection, DefaultRectifier, NoopRectifier, RequestRectifier};

    #[test]
    fn noop_rectifier_never_rewrites() {
        let body = json!({"model": "m", "thinking": {"type": "enabled"}});
        let error = BridgeError::InvalidUpstream;
        assert_eq!(
            NoopRectifier.rectify(WireProtocol::AnthropicMessages, &body, &error),
            None
        );
    }

    #[test]
    fn rectifier_trait_is_implementable() {
        struct StripThinking;
        impl RequestRectifier for StripThinking {
            fn rectify(
                &self,
                _protocol: WireProtocol,
                body: &Value,
                error: &BridgeError,
            ) -> Option<Value> {
                if !matches!(error, BridgeError::InvalidUpstream) {
                    return None;
                }
                let mut body = body.clone();
                if let Some(object) = body.as_object_mut() {
                    object.remove("thinking");
                }
                Some(body)
            }
        }
        let body = json!({"model": "m", "thinking": {"type": "enabled"}});
        let rewritten = StripThinking
            .rectify(
                WireProtocol::AnthropicMessages,
                &body,
                &BridgeError::InvalidUpstream,
            )
            .expect("rewritten");
        assert!(rewritten.get("thinking").is_none());
        assert_eq!(rewritten["model"], "m");
    }

    #[test]
    fn default_rectifier_strips_top_level_thinking_only_for_anthropic() {
        let body = json!({
            "model": "m",
            "thinking": {"type": "enabled", "budget_tokens": 64},
            "messages": [{"role": "user", "content": "hi"}]
        });
        let rewritten = DefaultRectifier
            .rectify(
                WireProtocol::AnthropicMessages,
                &body,
                &BridgeError::InvalidUpstream,
            )
            .expect("anthropic body must be rewritten");
        assert!(rewritten.get("thinking").is_none());
        assert_eq!(rewritten["model"], "m");
        assert_eq!(rewritten["messages"], body["messages"]);

        let unchanged = DefaultRectifier.rectify(
            WireProtocol::OpenAiChat,
            &body,
            &BridgeError::InvalidUpstream,
        );
        assert!(
            unchanged.is_none(),
            "chat bodies carry no anthropic thinking"
        );
    }

    #[test]
    fn default_rectifier_strips_assistant_thinking_blocks() {
        let body = json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "secret", "signature": "sig"},
                    {"type": "text", "text": "kept"},
                    {"type": "redacted_thinking", "data": "encrypted"}
                ]},
                {"role": "assistant", "content": [{"type": "text", "text": "plain"}]}
            ]
        });
        let rewritten = DefaultRectifier
            .rectify(
                WireProtocol::OpenAiChat,
                &body,
                &BridgeError::InvalidUpstream,
            )
            .expect("assistant thinking blocks must be stripped");
        assert_eq!(
            rewritten["messages"][1]["content"],
            json!([{"type": "text", "text": "kept"}])
        );
        assert_eq!(rewritten["messages"][2], body["messages"][2]);
    }

    #[test]
    fn default_rectifier_returns_none_when_nothing_to_strip() {
        let body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert_eq!(
            DefaultRectifier.rectify(
                WireProtocol::AnthropicMessages,
                &body,
                &BridgeError::InvalidUpstream
            ),
            None
        );
        assert_eq!(
            DefaultRectifier.rectify(
                WireProtocol::AnthropicMessages,
                &body,
                &BridgeError::Timeout
            ),
            None,
            "non-upstream errors never trigger rectification"
        );
    }

    #[test]
    fn thinking_rejection_matches_the_thinking_keyword() {
        assert!(thinking_rejection(
            r#"{"error":{"message":"unsupported field: thinking"}}"#
        ));
        assert!(thinking_rejection(
            r#"{"error":{"message":"thinking not supported"}}"#
        ));
        assert!(!thinking_rejection(
            r#"{"error":{"message":"rate limited"}}"#
        ));
    }
}

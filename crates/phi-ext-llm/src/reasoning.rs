//! Explicit provider wire controls. Model capabilities and user policy belong to callers.
use crate::ApiStyle;
pub use phi_kernel::ReasoningEffort;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningDialect {
    #[default]
    Generic,
    OpenAi,
    DeepSeek,
    Gemini,
    Qwen,
    SiliconFlow,
    OpenRouter,
}

impl ReasoningDialect {
    pub(crate) fn scope_name(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::OpenAi => "openai",
            Self::DeepSeek => "deepseek",
            Self::Gemini => "gemini",
            Self::Qwen => "qwen",
            Self::SiliconFlow => "siliconflow",
            Self::OpenRouter => "openrouter",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningMode {
    /// Omit controls; this does not mean reasoning is disabled.
    #[default]
    ProviderDefault,
    Disabled,
    /// Only valid for a dialect with a real explicit on/off control.
    Enabled,
    Effort(ReasoningEffort),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReasoningConfig {
    pub dialect: ReasoningDialect,
    pub mode: ReasoningMode,
}

impl ReasoningConfig {
    /// Check wire representability only; callers still validate the model's capabilities.
    pub fn validate(self, style: ApiStyle) -> Result<(), String> {
        use ReasoningDialect as D;
        use ReasoningEffort as E;
        use ReasoningMode as M;
        if style == ApiStyle::Responses
            && matches!(self.dialect, D::Gemini | D::Qwen | D::SiliconFlow)
        {
            return Err("this reasoning dialect requires Chat Completions".into());
        }
        let supported = match (self.dialect, self.mode) {
            (_, M::ProviderDefault) => true,
            (D::Generic, _) => false,
            (D::OpenAi | D::Gemini, M::Enabled) => false,
            (D::DeepSeek, M::Enabled) => style == ApiStyle::Completions,
            (D::Qwen, M::Effort(_)) => false,
            (D::DeepSeek, M::Effort(e)) => matches!(e, E::Low | E::High | E::Max),
            (D::Gemini, M::Effort(e)) => matches!(e, E::Minimal | E::Low | E::Medium | E::High),
            (D::SiliconFlow, M::Effort(e)) => matches!(e, E::High | E::Max),
            // OpenRouter's enabled=true picks a gateway default effort. Require
            // an explicit effort rather than silently mapping an on/off choice.
            (D::OpenRouter, M::Enabled) => false,
            _ => true,
        };
        if supported {
            Ok(())
        } else {
            Err("reasoning choice cannot be represented by this provider dialect".into())
        }
    }

    pub(crate) fn apply(self, style: ApiStyle, body: &mut Value) -> Result<(), String> {
        self.validate(style)?;
        use ReasoningDialect as D;
        use ReasoningMode as M;
        if self.mode == M::ProviderDefault {
            return Ok(());
        }
        let effort = match self.mode {
            M::Effort(e) => Some(e.as_str()),
            M::Disabled => Some("none"),
            _ => None,
        };
        match self.dialect {
            D::Generic => unreachable!("validated above"),
            D::OpenAi | D::Gemini => {
                if style == ApiStyle::Responses {
                    body["reasoning"] = json!({"effort":effort});
                } else {
                    body["reasoning_effort"] = json!(effort);
                }
            }
            D::DeepSeek if style == ApiStyle::Responses => {
                body["reasoning"] = json!({"effort":effort});
            }
            D::DeepSeek => {
                body["thinking"] =
                    json!({"type": if self.mode == M::Disabled { "disabled" } else { "enabled" }});
                if let M::Effort(e) = self.mode {
                    body["reasoning_effort"] = json!(e.as_str());
                }
            }
            D::Qwen | D::SiliconFlow => {
                body["enable_thinking"] = json!(self.mode != M::Disabled);
                if let M::Effort(e) = self.mode {
                    body["reasoning_effort"] = json!(e.as_str());
                }
            }
            D::OpenRouter => {
                body["reasoning"] = json!({"effort":effort});
                body["provider"] = json!({"require_parameters":true});
            }
        }
        Ok(())
    }

    pub(crate) fn replay_reasoning(self) -> bool {
        self.mode != ReasoningMode::Disabled
            && matches!(
                self.dialect,
                ReasoningDialect::DeepSeek | ReasoningDialect::SiliconFlow
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // External HTTP contracts, verified 2026-09-22 against each provider's
    // reasoning documentation. These fixtures test custom request encoding.
    #[test]
    fn explicit_choices_use_the_endpoint_dialect_without_effort_conversion() {
        use ReasoningDialect as D;
        use ReasoningEffort as E;
        use ReasoningMode as M;
        for (dialect, style, mode, expected) in [
            (
                D::OpenAi,
                ApiStyle::Completions,
                M::Disabled,
                json!({"reasoning_effort":"none"}),
            ),
            (
                D::OpenAi,
                ApiStyle::Responses,
                M::Effort(E::Xhigh),
                json!({"reasoning":{"effort":"xhigh"}}),
            ),
            (
                D::Gemini,
                ApiStyle::Completions,
                M::Effort(E::Low),
                json!({"reasoning_effort":"low"}),
            ),
            (
                D::DeepSeek,
                ApiStyle::Completions,
                M::Enabled,
                json!({"thinking":{"type":"enabled"}}),
            ),
            (
                D::DeepSeek,
                ApiStyle::Completions,
                M::Disabled,
                json!({"thinking":{"type":"disabled"}}),
            ),
            (
                D::DeepSeek,
                ApiStyle::Responses,
                M::Disabled,
                json!({"reasoning":{"effort":"none"}}),
            ),
            (
                D::DeepSeek,
                ApiStyle::Responses,
                M::Effort(E::Max),
                json!({"reasoning":{"effort":"max"}}),
            ),
            (
                D::Qwen,
                ApiStyle::Completions,
                M::Enabled,
                json!({"enable_thinking":true}),
            ),
            (
                D::Qwen,
                ApiStyle::Completions,
                M::Disabled,
                json!({"enable_thinking":false}),
            ),
            (
                D::SiliconFlow,
                ApiStyle::Completions,
                M::Effort(E::Max),
                json!({"enable_thinking":true,"reasoning_effort":"max"}),
            ),
            (
                D::OpenRouter,
                ApiStyle::Completions,
                M::Disabled,
                json!({"reasoning":{"effort":"none"},"provider":{"require_parameters":true}}),
            ),
            (
                D::OpenRouter,
                ApiStyle::Responses,
                M::Effort(E::High),
                json!({"reasoning":{"effort":"high"},"provider":{"require_parameters":true}}),
            ),
        ] {
            let mut request = json!({});
            ReasoningConfig { dialect, mode }
                .apply(style, &mut request)
                .unwrap();
            assert_eq!(request, expected, "{dialect:?} {style:?} {mode:?}");
            let mut default_request = json!({});
            ReasoningConfig {
                dialect,
                mode: M::ProviderDefault,
            }
            .apply(style, &mut default_request)
            .unwrap();
            assert_eq!(
                default_request,
                json!({}),
                "default must not inject controls"
            );
        }
        for (dialect, style, mode) in [
            (D::Generic, ApiStyle::Completions, M::Disabled),
            (D::OpenAi, ApiStyle::Responses, M::Enabled),
            (D::Gemini, ApiStyle::Responses, M::ProviderDefault),
            (D::DeepSeek, ApiStyle::Responses, M::Enabled),
            (D::DeepSeek, ApiStyle::Completions, M::Effort(E::Medium)),
            (D::SiliconFlow, ApiStyle::Completions, M::Effort(E::Low)),
            (D::Qwen, ApiStyle::Completions, M::Effort(E::High)),
        ] {
            assert!(ReasoningConfig { dialect, mode }.validate(style).is_err());
        }
    }
}

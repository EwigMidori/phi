//! Typed connection and OpenAI adapter configuration.
use std::fmt;
use strum::{Display, EnumString};

// ── Config ─────────────────────────────────────────────────────────────────

/// Wire protocol for the HTTP adapter.
///
/// String form (strum): `responses` | `completions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Display, EnumString, strum::AsRefStr)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive)]
pub enum ApiStyle {
    /// `POST {base}/responses`
    #[default]
    Responses,
    /// `POST {base}/chat/completions`
    Completions,
}

/// Provider model id (e.g. `gpt-4o-mini`). Non-empty after trim.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct ModelId(String);

impl ModelId {
    /// Reject empty / whitespace-only.
    pub fn try_new(value: impl AsRef<str>) -> Result<Self, String> {
        let s = value.as_ref().trim();
        if s.is_empty() {
            return Err("model id empty".into());
        }
        Ok(Self(s.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ModelId").field(&self.0).finish()
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for ModelId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// API root URL without a trailing slash (e.g. `https://api.openai.com/v1`).
///
/// Non-empty after trim; trailing `/` stripped. Scheme is not enforced (hosts may
/// use proxies or placeholders).
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct ApiBase(String);

impl ApiBase {
    /// Trim, reject empty, strip trailing `/`.
    pub fn try_new(value: impl AsRef<str>) -> Result<Self, String> {
        let s = value.as_ref().trim().trim_end_matches('/');
        if s.is_empty() {
            return Err("api base empty".into());
        }
        Ok(Self(s.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiBase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ApiBase").field(&self.0).finish()
    }
}

impl fmt::Display for ApiBase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for ApiBase {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Bearer credential. Non-empty after trim. [`Debug`] redacts the secret.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct ApiKey(String);

impl ApiKey {
    /// Reject empty / whitespace-only.
    pub fn try_new(value: impl AsRef<str>) -> Result<Self, String> {
        let s = value.as_ref().trim();
        if s.is_empty() {
            return Err("api key empty".into());
        }
        Ok(Self(s.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

impl AsRef<str> for ApiKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Endpoint settings for [`crate::OpenAiProtocol`].
///
/// Construct with typed fields; product hosts map env / config files → these newtypes.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api_base: ApiBase,
    pub api_key: ApiKey,
    pub model: ModelId,
    pub api_style: ApiStyle,
}

impl LlmConfig {
    #[must_use]
    pub fn new(api_base: ApiBase, api_key: ApiKey, model: ModelId, api_style: ApiStyle) -> Self {
        Self {
            api_base,
            api_key,
            model,
            api_style,
        }
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;
    #[test]
    fn configuration_rejects_empty_values_and_redacts_keys() {
        assert!(ModelId::try_new(" ").is_err());
        assert!(ApiBase::try_new("///").is_err());
        assert!(ApiKey::try_new("").is_err());
        assert_eq!(
            ApiBase::try_new("https://example.test/v1/")
                .unwrap()
                .as_str(),
            "https://example.test/v1"
        );
        let key = ApiKey::try_new("private-key").unwrap();
        assert!(!format!("{key:?}").contains("private-key"));
    }
}

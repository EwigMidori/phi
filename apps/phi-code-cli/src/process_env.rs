//! Process-boundary product config (`PHI_*`).
//!
//! Not UI. Not phi-code-core. Side effects (env reads) live on [`ProcessEnv`] methods.

use phi_code_core::{
    ApiBase, ApiKey, ApiStyle, ContextWindowSize, LlmConfig, ModelId, TurnDriver,
};

/// Process-boundary product config (`PHI_*`).
pub struct ProcessEnv;

impl ProcessEnv {
    /// Read `PHI_*` and open a product [`TurnDriver`].
    #[must_use]
    pub fn open_turn_driver() -> TurnDriver {
        let context_window = Self::load_context_window();
        match Self::load_llm_config() {
            Ok(cfg) => TurnDriver::from_config(cfg, context_window),
            Err(e) => TurnDriver::unconfigured(e, context_window),
        }
    }

    /// Product-owned env mapping (not in `phi-ext-llm` / `phi-code-core`).
    fn load_llm_config() -> Result<LlmConfig, String> {
        let api_key = ApiKey::try_new(Self::env_trim("PHI_API_KEY"))
            .map_err(|_| "set PHI_API_KEY to call a real model".to_owned())?;

        let base_raw = Self::env_trim("PHI_API_BASE");
        let api_base = if base_raw.is_empty() {
            ApiBase::try_new("https://api.openai.com/v1")
                .expect("default api base is non-empty")
        } else {
            ApiBase::try_new(base_raw)?
        };

        let model_raw = Self::env_trim("PHI_MODEL");
        let model = if model_raw.is_empty() {
            ModelId::try_new("gpt-4o-mini").expect("default model is non-empty")
        } else {
            ModelId::try_new(model_raw)?
        };

        let style_raw = Self::env_trim("PHI_API_STYLE");
        let api_style = if style_raw.is_empty() {
            ApiStyle::default()
        } else {
            style_raw.parse::<ApiStyle>().map_err(|_| {
                format!(
                    "invalid PHI_API_STYLE `{style_raw}` (use `responses` or `completions`)"
                )
            })?
        };

        Ok(LlmConfig::new(api_base, api_key, model, api_style))
    }

    /// Context window denominator (`PHI_CONTEXT_WINDOW`, default [`ContextWindowSize::DEFAULT`]).
    fn load_context_window() -> ContextWindowSize {
        let raw = Self::env_trim("PHI_CONTEXT_WINDOW");
        if raw.is_empty() {
            return ContextWindowSize::DEFAULT;
        }
        match raw.parse::<u64>() {
            Ok(n) => ContextWindowSize::try_new(n).unwrap_or(ContextWindowSize::DEFAULT),
            Err(_) => ContextWindowSize::DEFAULT,
        }
    }

    fn env_trim(key: &str) -> String {
        std::env::var(key)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .unwrap_or_default()
    }
}

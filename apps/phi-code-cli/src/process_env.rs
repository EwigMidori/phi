//! Process-boundary product config (`PHI_*`).
//!
//! Not UI. Not phi-code-core. Side effects (env reads) live on [`ProcessEnv`] methods.

use phi_code_core::{ApiStyle, LlmConfig, TurnDriver};

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
        let api_key = Self::env_trim("PHI_API_KEY");
        if api_key.is_empty() {
            return Err("set PHI_API_KEY to call a real model".into());
        }
        let api_base = Self::env_trim("PHI_API_BASE");
        let api_base = if api_base.is_empty() {
            "https://api.openai.com/v1".into()
        } else {
            api_base.trim_end_matches('/').to_owned()
        };
        let model = Self::env_trim("PHI_MODEL");
        let model = if model.is_empty() {
            "gpt-4o-mini".into()
        } else {
            model
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
        Ok(LlmConfig {
            api_base,
            api_key,
            model,
            api_style,
        })
    }

    /// Context window denominator (`PHI_CONTEXT_WINDOW`, default 128000).
    fn load_context_window() -> u64 {
        let raw = Self::env_trim("PHI_CONTEXT_WINDOW");
        if raw.is_empty() {
            return 128_000;
        }
        raw.parse::<u64>().unwrap_or(128_000).max(1)
    }

    fn env_trim(key: &str) -> String {
        std::env::var(key)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .unwrap_or_default()
    }
}

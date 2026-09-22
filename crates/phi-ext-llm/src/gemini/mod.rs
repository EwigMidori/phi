//! Gemini GenerateContent v1beta. Both transports share native response state.
mod content;
mod response;
#[cfg(test)]
mod tests;

use async_trait::async_trait;
use phi_kernel::{AgentPrefix, ReasoningEffort, SessionId, TurnCancel, TurnItem};
use reqwest::{Client, Url};
use serde_json::{Value, json};

use crate::{
    HttpConnection, ModelId, ResponseMode,
    images::PreparedImages,
    protocol::{ProviderError, ProviderProtocol, ProviderResponse},
};
use content::GeminiHistory;
use response::GeminiResponse;

/// Exact native controls. Model capability policy belongs to the host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GeminiThinking {
    #[default]
    ProviderDefault,
    /// -1 is dynamic, 0 disables thinking; positive values are explicit budgets.
    Budget(i32),
    Level(ReasoningEffort),
}

impl GeminiThinking {
    fn validate(self) -> Result<(), String> {
        match self {
            Self::Budget(budget) if budget < -1 => {
                Err("Gemini thinking budget must be -1 or nonnegative".into())
            }
            Self::Level(level)
                if !matches!(
                    level,
                    ReasoningEffort::Minimal
                        | ReasoningEffort::Low
                        | ReasoningEffort::Medium
                        | ReasoningEffort::High
                ) =>
            {
                Err("reasoning effort cannot be represented as a Gemini thinking level".into())
            }
            _ => Ok(()),
        }
    }

    fn apply(self, config: &mut Value) {
        match self {
            Self::ProviderDefault => {}
            Self::Budget(budget) => {
                config["thinkingConfig"] =
                    json!({"thinkingBudget": budget, "includeThoughts": true})
            }
            Self::Level(level) => {
                config["thinkingConfig"] =
                    json!({"thinkingLevel": level.as_str(), "includeThoughts": true})
            }
        }
    }
}

/// A protocol object, not a second agent runtime or tool execution loop.
#[derive(Clone)]
pub struct GeminiProtocol {
    connection: HttpConnection,
    model: ModelId,
    mode: ResponseMode,
    thinking: GeminiThinking,
    required_signatures: bool,
    root: Url,
    client: Client,
}

impl GeminiProtocol {
    pub fn new(
        connection: HttpConnection,
        model: ModelId,
        mode: ResponseMode,
    ) -> Result<Self, String> {
        let root =
            Url::parse(connection.api_base.as_str()).map_err(|_| "invalid Gemini API root")?;
        if !matches!(root.scheme(), "http" | "https")
            || root.host_str().is_none()
            || !root.username().is_empty()
            || root.password().is_some()
            || root.query().is_some()
            || root.fragment().is_some()
        {
            return Err(
                "Gemini API root must be an HTTP(S) URL without credentials, query or fragment"
                    .into(),
            );
        }
        let name = model
            .as_str()
            .strip_prefix("models/")
            .unwrap_or(model.as_str());
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err("invalid Gemini model resource name".into());
        }
        let model = ModelId::try_new(name)?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "could not create Gemini HTTP client")?;
        Ok(Self {
            connection,
            model,
            mode,
            thinking: GeminiThinking::ProviderDefault,
            required_signatures: false,
            root,
            client,
        })
    }

    pub fn with_thinking(mut self, thinking: GeminiThinking) -> Result<Self, String> {
        thinking.validate()?;
        self.thinking = thinking;
        Ok(self)
    }

    /// Hosts select the model's signature contract; no model-name table lives here.
    #[must_use]
    pub fn with_required_thought_signatures(mut self, required: bool) -> Self {
        self.required_signatures = required;
        self
    }

    fn scope(&self) -> String {
        format!(
            "gemini-native/v1|{}|{}",
            self.root.as_str().trim_end_matches('/'),
            self.model.as_str()
        )
    }

    fn endpoint(&self) -> Url {
        let mut url = self.root.clone();
        let action = match self.mode {
            ResponseMode::Buffered => "generateContent",
            ResponseMode::Streaming => "streamGenerateContent",
        };
        url.set_path(&format!(
            "{}/models/{}:{action}",
            self.root.path().trim_end_matches('/'),
            self.model.as_str()
        ));
        if self.mode == ResponseMode::Streaming {
            url.set_query(Some("alt=sse"));
        }
        url
    }

    fn request_body(
        &self,
        prefix: &AgentPrefix,
        history: &[TurnItem],
        images: &PreparedImages,
    ) -> Result<Value, String> {
        let contents =
            GeminiHistory::new(self.scope(), self.required_signatures, images).encode(history)?;
        let mut config = json!({"candidateCount": 1});
        self.thinking.apply(&mut config);
        let mut body = json!({"contents": contents, "generationConfig": config});
        let preamble = prefix.render_preamble();
        if !preamble.is_empty() {
            body["systemInstruction"] = json!({"parts": [{"text": preamble}]});
        }
        if !prefix.tools.is_empty() {
            let mut functions = Vec::with_capacity(prefix.tools.len());
            for spec in &prefix.tools {
                let mut function =
                    json!({"name": spec.name.as_str(), "description": spec.description});
                if let Some(parameters) = &spec.parameters {
                    if !parameters.is_object() {
                        return Err("Gemini tool parameters must be a JSON schema object".into());
                    }
                    function["parametersJsonSchema"] = parameters.clone();
                }
                functions.push(function);
            }
            body["tools"] = json!([{"functionDeclarations": functions}]);
            body["toolConfig"] = json!({"functionCallingConfig": {"mode": "AUTO"}});
        }
        images.validate_request_bytes(
            serde_json::to_vec(&body)
                .map_err(|_| "could not encode Gemini request")?
                .len(),
        )?;
        Ok(body)
    }
}

#[async_trait]
impl ProviderProtocol for GeminiProtocol {
    fn connection(&self) -> &HttpConnection {
        &self.connection
    }

    fn validate_request(
        &self,
        _: &SessionId,
        prefix: &AgentPrefix,
        history: &[TurnItem],
        images: &PreparedImages,
    ) -> Result<(), String> {
        self.request_body(prefix, history, images).map(|_| ())
    }

    async fn open_response(
        &self,
        _: &SessionId,
        prefix: &AgentPrefix,
        history: &[TurnItem],
        images: &PreparedImages,
        cancel: &TurnCancel,
    ) -> Result<Box<dyn ProviderResponse>, ProviderError> {
        let body = self.request_body(prefix, history, images)?;
        let request = self
            .connection
            .authorize(self.client.post(self.endpoint()))
            .json(&body);
        let response = tokio::select! {
            () = cancel.cancelled() => return Err(ProviderError::cancelled()),
            result = request.send() => result.map_err(|error| {
                if error.is_builder() {
                    ProviderError::configuration("Gemini HTTP request configuration is invalid")
                } else {
                    ProviderError::transport("Gemini HTTP request failed")
                }
            })?,
        };
        if !response.status().is_success() {
            return Err(ProviderError::http(response.status().as_u16()));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let expected = match self.mode {
            ResponseMode::Streaming => "text/event-stream",
            ResponseMode::Buffered => "application/json",
        };
        if content_type != expected {
            return Err(ProviderError::invalid(
                "Gemini response has an unexpected content type",
            ));
        }
        Ok(Box::new(GeminiResponse::new(
            response,
            self.mode,
            cancel.clone(),
            self.scope(),
            self.required_signatures,
        )))
    }
}

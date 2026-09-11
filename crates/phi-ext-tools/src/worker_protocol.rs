//! Private worker wire contract, separate from model tool input.
use crate::CapturedOutput;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct JavaScriptRequest {
    pub code: String,
    pub timeout_ms: u64,
    pub output_limit: usize,
    pub result_limit: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct JavaScriptResponse {
    pub stdout: CapturedOutput,
    pub stderr: CapturedOutput,
    pub value: Option<Value>,
    pub error: Option<ExecutionError>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionError {
    pub kind: String,
    pub message: String,
}

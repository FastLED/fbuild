//! MCP protocol value types: tools, resources, prompts, content blocks.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(super) struct ToolDefinition {
    pub(super) name: String,
    pub(super) description: String,
    #[serde(rename = "inputSchema")]
    pub(super) input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) annotations: Option<ToolAnnotations>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ToolAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) read_only_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) destructive_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) idempotent_hint: Option<bool>,
}

#[derive(Debug, Serialize)]
pub(super) struct ResourceDefinition {
    pub(super) uri: String,
    pub(super) name: String,
    pub(super) description: String,
    #[serde(rename = "mimeType")]
    pub(super) mime_type: String,
}

#[derive(Debug, Serialize)]
pub(super) struct PromptDefinition {
    pub(super) name: String,
    pub(super) description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) arguments: Option<Vec<PromptArgument>>,
}

#[derive(Debug, Serialize)]
pub(super) struct PromptArgument {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) required: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct TextContent {
    #[serde(rename = "type")]
    pub(super) content_type: String,
    pub(super) text: String,
}

#[derive(Debug, Serialize)]
// ResourceContent is used by the MCP resources/read response format.
#[allow(dead_code)]
pub(super) struct ResourceContent {
    pub(super) uri: String,
    #[serde(rename = "mimeType")]
    pub(super) mime_type: String,
    pub(super) text: String,
}

#[derive(Debug, Serialize)]
pub(super) struct PromptMessage {
    pub(super) role: String,
    pub(super) content: TextContent,
}

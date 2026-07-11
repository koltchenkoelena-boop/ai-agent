use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Roles & Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    pub fn assistant(content: Option<String>, tool_calls: Option<Vec<ToolCall>>) -> Self {
        Self {
            role: Role::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Streaming chunks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ChatChunk {
    pub delta_content: Option<String>,
    pub delta_tool_calls: Option<Vec<ToolCallChunk>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallChunk {
    pub index: Option<usize>,
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub r#type: Option<String>,
    pub function: Option<FunctionCallChunk>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FunctionCallChunk {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

// ---------------------------------------------------------------------------
// Snapshot — a point-in-time capture of a branch for reference / merge
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub branch_id: String,
    pub branch_name: String,
    pub message_count: usize,
    pub messages: Vec<Message>,
}

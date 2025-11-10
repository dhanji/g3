use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub agent: AgentType,
    pub content: String,
    pub message_type: MessageType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    Coach,
    Player,
    Single,
    User,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MessageType {
    Text,
    ToolCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub tool_name: String,
    pub parameters: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub execution_time_ms: Option<u64>,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub message: String,
    pub fields: serde_json::Value,
}

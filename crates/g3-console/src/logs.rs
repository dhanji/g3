use crate::models::{InstanceStats, TurnInfo};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: Option<DateTime<Utc>>,
    pub role: Option<String>,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<Value>>,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub timestamp: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub parameters: Value,
    pub result: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
}

pub struct LogParser;

impl LogParser {
    /// Parse logs from a workspace directory
    pub fn parse_logs(workspace: &Path) -> Result<Vec<LogEntry>> {
        let logs_dir = workspace.join("logs");
        
        if !logs_dir.exists() {
            return Ok(Vec::new());
        }

        let mut entries = Vec::new();

        // Read all JSON log files
        for entry in fs::read_dir(&logs_dir).context("Failed to read logs directory")? {
            let entry = entry?;
            let path = entry.path();
            
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(json) = serde_json::from_str::<Value>(&content) {
                        // Try to parse as a log session
                        if let Some(messages) = json.get("messages").and_then(|m| m.as_array()) {
                            for msg in messages {
                                entries.push(LogEntry {
                                    timestamp: msg.get("timestamp")
                                        .and_then(|t| t.as_str())
                                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                                        .map(|dt| dt.with_timezone(&Utc)),
                                    role: msg.get("role")
                                        .and_then(|r| r.as_str())
                                        .map(String::from),
                                    content: msg.get("content")
                                        .and_then(|c| c.as_str())
                                        .map(String::from),
                                    tool_calls: msg.get("tool_calls")
                                        .and_then(|tc| tc.as_array())
                                        .map(|arr| arr.clone()),
                                    raw: msg.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Sort by timestamp
        entries.sort_by(|a, b| {
            match (&a.timestamp, &b.timestamp) {
                (Some(t1), Some(t2)) => t1.cmp(t2),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });

        Ok(entries)
    }

    /// Extract chat messages from log entries
    pub fn extract_chat_messages(entries: &[LogEntry]) -> Vec<ChatMessage> {
        entries
            .iter()
            .filter_map(|entry| {
                let role = entry.role.clone()?;
                let content = entry.content.clone()?;
                
                Some(ChatMessage {
                    role,
                    content,
                    timestamp: entry.timestamp,
                })
            })
            .collect()
    }

    /// Extract tool calls from log entries
    pub fn extract_tool_calls(entries: &[LogEntry]) -> Vec<ToolCall> {
        let mut tool_calls = Vec::new();

        for entry in entries {
            if let Some(calls) = &entry.tool_calls {
                for call in calls {
                    if let Some(name) = call.get("name").and_then(|n| n.as_str()) {
                        tool_calls.push(ToolCall {
                            name: name.to_string(),
                            parameters: call.get("parameters")
                                .cloned()
                                .unwrap_or(Value::Object(serde_json::Map::new())),
                            result: call.get("result")
                                .and_then(|r| r.as_str())
                                .map(String::from),
                            timestamp: entry.timestamp,
                        });
                    }
                }
            }
        }

        tool_calls
    }
}

pub struct StatsAggregator;

impl StatsAggregator {
    /// Aggregate statistics from log entries
    pub fn aggregate_stats(
        entries: &[LogEntry],
        start_time: DateTime<Utc>,
        is_ensemble: bool,
    ) -> InstanceStats {
        let total_tokens = Self::count_tokens(entries);
        let tool_calls = Self::count_tool_calls(entries);
        let errors = Self::count_errors(entries);
        
        let duration_secs = if let Some(last_entry) = entries.last() {
            if let Some(last_time) = last_entry.timestamp {
                (last_time - start_time).num_seconds().max(0) as u64
            } else {
                (Utc::now() - start_time).num_seconds().max(0) as u64
            }
        } else {
            (Utc::now() - start_time).num_seconds().max(0) as u64
        };

        let turns = if is_ensemble {
            Some(Self::extract_turns(entries))
        } else {
            None
        };

        InstanceStats {
            total_tokens,
            tool_calls,
            errors,
            duration_secs,
            turns,
        }
    }

    /// Get the latest message content from log entries
    pub fn get_latest_message(entries: &[LogEntry]) -> Option<String> {
        entries
            .iter()
            .rev()
            .find(|entry| entry.role.as_deref() == Some("assistant"))
            .and_then(|entry| entry.content.clone())
            .or_else(|| {
                entries
                    .iter()
                    .rev()
                    .find(|entry| entry.content.is_some())
                    .and_then(|entry| entry.content.clone())
            })
    }

    fn count_tokens(entries: &[LogEntry]) -> u64 {
        // Try to extract token counts from metadata
        entries
            .iter()
            .filter_map(|entry| {
                entry.raw.get("usage")
                    .and_then(|u| u.get("total_tokens"))
                    .and_then(|t| t.as_u64())
            })
            .sum()
    }

    fn count_tool_calls(entries: &[LogEntry]) -> u64 {
        entries
            .iter()
            .filter_map(|entry| entry.tool_calls.as_ref())
            .map(|calls| calls.len() as u64)
            .sum()
    }

    fn count_errors(entries: &[LogEntry]) -> u64 {
        entries
            .iter()
            .filter(|entry| {
                entry.raw.get("error").is_some()
                    || entry.content.as_ref().map(|c| c.to_lowercase().contains("error")).unwrap_or(false)
            })
            .count() as u64
    }

    fn extract_turns(entries: &[LogEntry]) -> Vec<TurnInfo> {
        // Simple implementation: group consecutive assistant messages as turns
        let mut turns = Vec::new();
        let mut current_turn_start: Option<DateTime<Utc>> = None;
        let mut turn_count = 0;

        for entry in entries {
            if entry.role.as_deref() == Some("assistant") {
                if current_turn_start.is_none() {
                    current_turn_start = entry.timestamp;
                    turn_count += 1;
                }
            } else if entry.role.as_deref() == Some("user") {
                if let Some(start) = current_turn_start {
                    if let Some(end) = entry.timestamp {
                        let duration = (end - start).num_seconds().max(0) as u64;
                        turns.push(TurnInfo {
                            agent: format!("agent-{}", turn_count),
                            duration_secs: duration,
                            status: "completed".to_string(),
                            color: Self::get_turn_color(turn_count),
                        });
                    }
                    current_turn_start = None;
                }
            }
        }

        turns
    }

    fn get_turn_color(turn_number: usize) -> String {
        let colors = vec!["blue", "green", "purple", "orange", "pink", "teal"];
        colors[turn_number % colors.len()].to_string()
    }
}

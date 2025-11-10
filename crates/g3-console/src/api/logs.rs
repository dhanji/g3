use crate::logs::LogParser;
use crate::process::ProcessDetector;
use axum::{extract::State, http::StatusCode, Json};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::error;

pub type LogState = Arc<Mutex<ProcessDetector>>;

pub async fn get_instance_logs(
    State(detector): State<LogState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut detector = detector.lock().await;
    
    match detector.detect_instances() {
        Ok(instances) => {
            if let Some(instance) = instances.into_iter().find(|i| i.id == id) {
                match LogParser::parse_logs(&instance.workspace) {
                    Ok(entries) => {
                        let messages = LogParser::extract_chat_messages(&entries);
                        let tool_calls = LogParser::extract_tool_calls(&entries);
                        
                        Ok(Json(serde_json::json!({
                            "messages": messages,
                            "tool_calls": tool_calls,
                        })))
                    }
                    Err(e) => {
                        error!("Failed to parse logs: {}", e);
                        Err(StatusCode::INTERNAL_SERVER_ERROR)
                    }
                }
            } else {
                Err(StatusCode::NOT_FOUND)
            }
        }
        Err(e) => {
            error!("Failed to detect instances: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

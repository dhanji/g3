use crate::launch::ConsoleState;
use axum::{http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::os::unix::fs::PermissionsExt;
use tracing::{error, info};

pub async fn get_state() -> Result<Json<ConsoleState>, StatusCode> {
    let state = ConsoleState::load();
    Ok(Json(state))
}

pub async fn save_state(
    Json(state): Json<ConsoleState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match state.save() {
        Ok(_) => {
            info!("Console state saved successfully");
            Ok(Json(serde_json::json!({
                "status": "saved"
            })))
        }
        Err(e) => {
            error!("Failed to save console state: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BrowseRequest {
    pub path: Option<String>,
    pub browse_type: String, // "directory" or "file"
}

#[derive(Debug, Serialize)]
pub struct BrowseResponse {
    pub current_path: String,
    pub parent_path: Option<String>,
    pub entries: Vec<FileEntry>,
}

#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_executable: bool,
}

pub async fn browse_filesystem(
    Json(request): Json<BrowseRequest>,
) -> Result<Json<BrowseResponse>, StatusCode> {
    use std::fs;
    
    let path = if let Some(p) = request.path {
        PathBuf::from(p)
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    
    let current_path = path.canonicalize()
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .to_string_lossy()
        .to_string();
    
    let parent_path = path.parent()
        .and_then(|p| p.to_str())
        .map(|s| s.to_string());
    
    let mut entries = Vec::new();
    
    if let Ok(read_dir) = fs::read_dir(&path) {
        for entry in read_dir.flatten() {
            if let Ok(metadata) = entry.metadata() {
                entries.push(FileEntry {
                    name: entry.file_name().to_string_lossy().to_string(),
                    path: entry.path().to_string_lossy().to_string(),
                    is_dir: metadata.is_dir(),
                    is_executable: metadata.permissions().mode() & 0o111 != 0,
                });
            }
        }
    }
    
    entries.sort_by(|a, b| {
        match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        }
    });
    
    Ok(Json(BrowseResponse {
        current_path,
        parent_path,
        entries,
    }))
}

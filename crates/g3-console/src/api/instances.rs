use crate::logs::{LogParser, StatsAggregator};
use crate::models::*;
use crate::process::ProcessDetector;
use axum::{extract::{Query, State}, http::StatusCode, Json};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, warn};

pub type AppState = Arc<Mutex<ProcessDetector>>;

pub async fn list_instances(
    State(detector): State<AppState>,
) -> Result<Json<Vec<InstanceDetail>>, StatusCode> {
    let mut detector = detector.lock().await;
    
    match detector.detect_instances() {
        Ok(instances) => {
            let mut details = Vec::new();
            
            for instance in instances {
                match get_instance_detail(&instance) {
                    Ok(detail) => details.push(detail),
                    Err(e) => {
                        error!("Failed to get instance detail: {}", e);
                        // Continue with other instances
                    }
                }
            }
            
            Ok(Json(details))
        }
        Err(e) => {
            error!("Failed to detect instances: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub async fn get_instance(
    State(detector): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<InstanceDetail>, StatusCode> {
    let mut detector = detector.lock().await;
    
    match detector.detect_instances() {
        Ok(instances) => {
            if let Some(instance) = instances.into_iter().find(|i| i.id == id) {
                match get_instance_detail(&instance) {
                    Ok(detail) => Ok(Json(detail)),
                    Err(e) => {
                        error!("Failed to get instance detail: {}", e);
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

fn get_instance_detail(instance: &Instance) -> anyhow::Result<InstanceDetail> {
    // Parse logs - don't fail if logs don't exist yet
    let log_entries = match LogParser::parse_logs(&instance.workspace) {
        Ok(entries) => entries,
        Err(e) => {
            warn!("Failed to parse logs for instance {}: {}. Instance may be newly started.", instance.id, e);
            Vec::new()
        }
    };
    
    // Aggregate stats
    let is_ensemble = instance.instance_type == crate::models::InstanceType::Ensemble;
    let stats = StatsAggregator::aggregate_stats(&log_entries, instance.start_time, is_ensemble);
    
    // Get latest message
    let latest_message = StatsAggregator::get_latest_message(&log_entries);
    
    // Get git status - don't fail if not a git repo
    let git_status = match get_git_status(&instance.workspace) {
        Some(status) => Some(status),
        None => {
            debug!("No git status available for workspace: {:?}", instance.workspace);
            None
        }
    };
    
    // Get project files
    let project_files = get_project_files(&instance.workspace);
    
    Ok(InstanceDetail {
        instance: instance.clone(),
        stats,
        latest_message,
        git_status,
        project_files,
    })
}

fn get_git_status(workspace: &std::path::Path) -> Option<GitStatus> {
    use std::process::Command;
    
    // Get current branch
    let branch = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .arg("branch")
        .arg("--show-current")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| s.trim().to_string())?;
    
    // Get status
    let status_output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .arg("status")
        .arg("--porcelain")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())?;
    
    let mut modified_files = Vec::new();
    let mut added_files = Vec::new();
    let mut deleted_files = Vec::new();
    
    for line in status_output.lines() {
        if line.len() < 4 {
            continue;
        }
        
        let status = &line[0..2];
        let file = line[3..].trim();
        
        match status.trim() {
            "M" | "MM" => modified_files.push(file.to_string()),
            "A" | "AM" => added_files.push(file.to_string()),
            "D" => deleted_files.push(file.to_string()),
            _ => modified_files.push(file.to_string()),
        }
    }
    
    let uncommitted_changes = modified_files.len() + added_files.len() + deleted_files.len();
    
    Some(GitStatus {
        branch,
        uncommitted_changes,
        modified_files,
        added_files,
        deleted_files,
    })
}

fn get_project_files(workspace: &std::path::Path) -> ProjectFiles {
    let requirements = read_file_snippet(workspace, "requirements.md");
    let readme = read_file_snippet(workspace, "README.md");
    let agents = read_file_snippet(workspace, "AGENTS.md");
    
    ProjectFiles {
        requirements,
        readme,
        agents,
    }
}

fn read_file_snippet(workspace: &std::path::Path, filename: &str) -> Option<String> {
    use std::fs;
    
    let path = workspace.join(filename);
    if !path.exists() {
        return None;
    }
    
    fs::read_to_string(&path)
        .ok()
        .map(|content| {
            // Return first 10 lines
            content
                .lines()
                .take(10)
                .collect::<Vec<_>>()
                .join("\n")
        })
}

#[derive(Deserialize)]
pub struct FileQuery {
    name: String,
}

pub async fn get_file_content(
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(query): Query<FileQuery>,
    State(detector): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut detector = detector.lock().await;
    
    // Find the instance
    let instances = detector.detect_instances().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let instance = instances.iter().find(|i| i.id == id).ok_or(StatusCode::NOT_FOUND)?;
    
    // Read the full file
    let file_path = instance.workspace.join(&query.name);
    if !file_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }
    
    let content = std::fs::read_to_string(&file_path)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    Ok(Json(serde_json::json!({
        "name": query.name,
        "content": content,
    })))
}

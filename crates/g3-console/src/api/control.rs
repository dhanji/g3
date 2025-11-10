use crate::models::*;
use crate::process::ProcessController;
use axum::{extract::State, http::StatusCode, Json};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info};

pub type ControllerState = Arc<Mutex<ProcessController>>;

pub async fn kill_instance(
    State(controller): State<ControllerState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Extract PID from ID (format: "pid_timestamp")
    let pid = id
        .split('_')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;

    let mut controller = controller.lock().await;
    
    match controller.kill_process(pid) {
        Ok(_) => {
            info!("Successfully killed process {}", pid);
            Ok(Json(serde_json::json!({
                "status": "terminating"
            })))
        }
        Err(e) => {
            error!("Failed to kill process {}: {}", pid, e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub async fn restart_instance(
    State(controller): State<ControllerState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<LaunchResponse>, StatusCode> {
    info!("Restarting instance: {}", id);
    
    // Extract PID from instance ID (format: pid_timestamp)
    let pid: u32 = id
        .split('_')
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    
    let mut controller = controller.lock().await;
    
    // Get stored launch params
    let params = controller.get_launch_params(pid)
        .ok_or(StatusCode::NOT_FOUND)?;
    
    // Launch new instance with same parameters
    let new_pid = controller.launch_g3(
        params.workspace.to_str().unwrap(),
        &params.provider,
        &params.model,
        &params.prompt,
        params.autonomous,
        params.g3_binary_path.as_deref(),
    ).map_err(|e| {
        error!("Failed to restart instance: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    
    let new_id = format!("{}_{}", new_pid, chrono::Utc::now().timestamp());
    
    Ok(Json(LaunchResponse {
        id: new_id,
        status: "starting".to_string(),
    }))
}

pub async fn launch_instance(
    State(controller): State<ControllerState>,
    Json(request): Json<LaunchRequest>,
) -> Result<Json<LaunchResponse>, (StatusCode, Json<serde_json::Value>)> {
    info!("Launching new g3 instance: {:?}", request);
    
    // Validate binary path if provided
    if let Some(ref binary_path) = request.g3_binary_path {
        // Expand relative paths and resolve to absolute
        let path = if binary_path.starts_with("./") || binary_path.starts_with("../") {
            std::env::current_dir()
                .map(|cwd| cwd.join(binary_path))
                .unwrap_or_else(|_| std::path::PathBuf::from(binary_path))
        } else {
            std::path::PathBuf::from(binary_path)
        };
        
        // Check if file exists
        if !path.exists() {
            error!("G3 binary not found: {}", binary_path);
            return Err((StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "G3 binary not found",
                "message": format!("The specified g3 binary does not exist: {}", binary_path)
            }))));
        }
        
        // Check if file is executable (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(path) {
                if metadata.permissions().mode() & 0o111 == 0 {
                    error!("G3 binary is not executable: {}", binary_path);
                    return Err((StatusCode::BAD_REQUEST, Json(serde_json::json!({
                        "error": "G3 binary is not executable",
                        "message": format!("The specified g3 binary is not executable: {}", binary_path)
                    }))));
                }
            }
        }
    }
    
    let workspace = request.workspace.to_str().ok_or_else(|| {
        (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": "Invalid workspace path",
            "message": "The workspace path contains invalid characters"
        })))
    })?;
    let autonomous = request.mode == LaunchMode::Ensemble;
    let g3_binary_path = request.g3_binary_path.as_deref();
    
    let mut controller = controller.lock().await;
    
    match controller.launch_g3(
        workspace,
        &request.provider,
        &request.model,
        &request.prompt,
        autonomous,
        g3_binary_path,
    ) {
        Ok(pid) => {
            let id = format!("{}_{}", pid, chrono::Utc::now().timestamp());
            info!("Successfully launched g3 instance with PID {}", pid);
            Ok(Json(LaunchResponse {
                id,
                status: "starting".to_string(),
            }))
        }
        Err(e) => {
            error!("Failed to launch g3 instance: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "error": "Failed to launch instance",
                "message": format!("Error: {}", e)
            }))))
        }
    }
}

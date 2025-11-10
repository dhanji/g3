use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub pid: u32,
    pub workspace: PathBuf,
    pub start_time: DateTime<Utc>,
    pub status: InstanceStatus,
    pub instance_type: InstanceType,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub execution_method: ExecutionMethod,
    pub command_line: String,
    // Store original launch parameters for restart
    pub launch_params: Option<LaunchParams>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchParams {
    pub workspace: PathBuf,
    pub provider: String,
    pub model: String,
    pub prompt: String,
    pub autonomous: bool,
    pub g3_binary_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum InstanceStatus {
    Running,
    Completed,
    Failed,
    Idle,
    Terminated,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum InstanceType {
    Single,
    Ensemble,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMethod {
    Binary,
    CargoRun,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceStats {
    pub total_tokens: u64,
    pub tool_calls: u64,
    pub errors: u64,
    pub duration_secs: u64,
    pub turns: Option<Vec<TurnInfo>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceDetail {
    #[serde(flatten)]
    pub instance: Instance,
    pub stats: InstanceStats,
    pub latest_message: Option<String>,
    pub git_status: Option<GitStatus>,
    pub project_files: ProjectFiles,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStatus {
    pub branch: String,
    pub uncommitted_changes: usize,
    pub modified_files: Vec<String>,
    pub added_files: Vec<String>,
    pub deleted_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectFiles {
    pub requirements: Option<String>,
    pub readme: Option<String>,
    pub agents: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRequest {
    pub prompt: String,
    pub workspace: PathBuf,
    pub provider: String,
    pub model: String,
    pub mode: LaunchMode,
    pub g3_binary_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LaunchMode {
    Single,
    Ensemble,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchResponse {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnInfo {
    pub agent: String,
    pub duration_secs: u64,
    pub status: String,
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressInfo {
    pub mode: InstanceType,
    pub duration_secs: u64,
    pub estimated_finish_secs: Option<u64>,
    pub turns: Vec<TurnInfo>,
}

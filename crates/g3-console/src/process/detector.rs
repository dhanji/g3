use crate::models::{ExecutionMethod, Instance, InstanceStatus, InstanceType};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::path::PathBuf;
use sysinfo::{System, Pid, Process};
use tracing::{debug, info, warn};

pub struct ProcessDetector {
    system: System,
}

impl ProcessDetector {
    pub fn new() -> Self {
        Self {
            system: System::new_all(),
        }
    }

    pub fn detect_instances(&mut self) -> Result<Vec<Instance>> {
        info!("Scanning for g3 processes...");
        // Refresh all processes to ensure we catch newly started ones
        // Using refresh_all() instead of just refresh_processes() to ensure
        // we get complete information about new processes
        self.system.refresh_all();
        let mut instances = Vec::new();

        // Find all g3 processes
        for (pid, process) in self.system.processes() {
            let cmd = process.cmd();
            if cmd.is_empty() {
                continue;
            }

            // Check if this is a g3 process (binary or cargo run)
            if let Some(instance) = self.parse_g3_process(*pid, process, cmd) {
                instances.push(instance);
            }
        }

        info!("Detected {} g3 instances", instances.len());
        Ok(instances)
    }

    fn parse_g3_process(
        &self,
        pid: Pid,
        process: &Process,
        cmd: &[String],
    ) -> Option<Instance> {
        let cmd_str = cmd.join(" ");
        
        // Exclude g3-console itself
        if cmd_str.contains("g3-console") {
            return None;
        }
        
        // Check if this is a g3 binary (more comprehensive check)
        let is_g3_binary = cmd.get(0).map(|s| {
            (s.ends_with("g3") || s.ends_with("/g3") || s.contains("/target/release/g3") || s.contains("/target/debug/g3"))
            && !s.contains("g3-") // Exclude other g3-* binaries
        }).unwrap_or(false);
        
        // Check if this is cargo run with g3 (not g3-console or other variants)
        let is_cargo_run = cmd.get(0).map(|s| s.contains("cargo")).unwrap_or(false) 
            && cmd.iter().any(|s| s == "run")
            && !cmd_str.contains("g3-console");
        
        // Also check if command line has g3-specific flags
        let has_g3_flags = cmd_str.contains("--workspace") || cmd_str.contains("--autonomous");
        
        // Accept if it's a g3 binary or cargo run with g3, and has typical g3 patterns
        let is_g3_process = is_g3_binary || (is_cargo_run && has_g3_flags);
        
        if !is_g3_process {
            return None;
        }

        // Extract workspace directory
        let workspace = self.extract_workspace(pid, process, cmd)?;

        // Determine execution method
        let execution_method = if is_cargo_run {
            ExecutionMethod::CargoRun
        } else {
            ExecutionMethod::Binary
        };

        // Determine instance type (ensemble if --autonomous flag present)
        let instance_type = if cmd.iter().any(|s| s == "--autonomous") {
            InstanceType::Ensemble
        } else {
            InstanceType::Single
        };

        // Extract provider and model
        let provider = self.extract_flag_value(cmd, "--provider");
        let model = self.extract_flag_value(cmd, "--model");

        // Get start time
        let start_time = DateTime::from_timestamp(process.start_time() as i64, 0)
            .unwrap_or_else(Utc::now);

        // Generate instance ID from PID and start time
        let id = format!("{}_{}", pid, start_time.timestamp());

        Some(Instance {
            id,
            pid: pid.as_u32(),
            workspace,
            start_time,
            status: InstanceStatus::Running,
            instance_type,
            provider,
            model,
            execution_method,
            command_line: cmd_str,
            launch_params: None, // Not available for detected processes
        })
    }

    fn extract_workspace(&self, pid: Pid, _process: &Process, cmd: &[String]) -> Option<PathBuf> {
        // Look for --workspace flag
        for i in 0..cmd.len() {
            if cmd[i] == "--workspace" && i + 1 < cmd.len() {
                return Some(PathBuf::from(&cmd[i + 1]));
            }
            if cmd[i] == "-w" && i + 1 < cmd.len() {
                return Some(PathBuf::from(&cmd[i + 1]));
            }
        }

        // Fallback: Try to get the working directory of the process
        #[cfg(target_os = "linux")]
        {
            // On Linux, read /proc/<pid>/cwd symlink
            let cwd_path = format!("/proc/{}/cwd", pid.as_u32());
            if let Ok(cwd) = std::fs::read_link(&cwd_path) {
                debug!("Found workspace via /proc for PID {}: {:?}", pid, cwd);
                return Some(cwd);
            }
        }
        
        #[cfg(target_os = "macos")]
        {
            // On macOS, use lsof to get the current working directory
            if let Ok(output) = std::process::Command::new("lsof")
                .args(["-p", &pid.as_u32().to_string(), "-a", "-d", "cwd", "-Fn"])
                .output()
            {
                if let Ok(stdout) = String::from_utf8(output.stdout) {
                    if let Some(line) = stdout.lines().find(|l| l.starts_with('n')) {
                        let cwd = PathBuf::from(&line[1..]);
                        debug!("Found workspace via lsof for PID {}: {:?}", pid, cwd);
                        return Some(cwd);
                    }
                }
            }
        }
        
        // Final fallback: use current directory of console
        warn!("Could not determine workspace for PID {}, using current directory", pid);
        std::env::current_dir().ok()
    }

    fn extract_flag_value(&self, cmd: &[String], flag: &str) -> Option<String> {
        for i in 0..cmd.len() {
            if cmd[i] == flag && i + 1 < cmd.len() {
                return Some(cmd[i + 1].clone());
            }
        }
        None
    }

    pub fn get_process_status(&mut self, pid: u32) -> Option<InstanceStatus> {
        self.system.refresh_all();
        
        let sysinfo_pid = Pid::from_u32(pid);
        if self.system.process(sysinfo_pid).is_some() {
            Some(InstanceStatus::Running)
        } else {
            Some(InstanceStatus::Terminated)
        }
    }
}

impl Default for ProcessDetector {
    fn default() -> Self {
        Self::new()
    }
}

use anyhow::{anyhow, Context, Result};
use std::process::{Command, Stdio};
use std::os::unix::process::CommandExt;
use std::collections::HashMap;
use std::sync::Mutex;
use std::path::PathBuf;
use sysinfo::{Pid, Signal, System, Process};
use tracing::{debug, info};
use crate::models::LaunchParams;

pub struct ProcessController {
    system: System,
    launch_params: Mutex<HashMap<u32, LaunchParams>>,
}

impl ProcessController {
    pub fn new() -> Self {
        Self {
            system: System::new_all(),
            launch_params: Mutex::new(HashMap::new()),
        }
    }

    pub fn kill_process(&mut self, pid: u32) -> Result<()> {
        let sysinfo_pid = Pid::from_u32(pid);
        self.system.refresh_processes();

        if let Some(process) = self.system.process(sysinfo_pid) {
            info!("Killing process {} ({})", pid, process.name());
            
            // Try SIGTERM first
            if process.kill_with(Signal::Term).is_some() {
                debug!("Sent SIGTERM to process {}", pid);
                
                // Wait a bit and check if it's still running
                std::thread::sleep(std::time::Duration::from_secs(2));
                self.system.refresh_processes();
                
                if self.system.process(sysinfo_pid).is_some() {
                    // Still running, send SIGKILL
                    if let Some(proc) = self.system.process(sysinfo_pid) {
                        proc.kill_with(Signal::Kill);
                        debug!("Sent SIGKILL to process {}", pid);
                    }
                }
                
                Ok(())
            } else {
                Err(anyhow!("Failed to send signal to process {}", pid))
            }
        } else {
            Err(anyhow!("Process {} not found", pid))
        }
    }

    #[cfg(unix)]
    pub fn launch_g3(
        &mut self,
        workspace: &str,
        provider: &str,
        model: &str,
        prompt: &str,
        autonomous: bool,
        g3_binary_path: Option<&str>,
    ) -> Result<u32> {
        let binary = g3_binary_path.unwrap_or("g3");
        
        let mut cmd = Command::new(binary);
        cmd.arg("--workspace")
            .arg(workspace)
            .arg("--provider")
            .arg(provider)
            .arg("--model")
            .arg(model);

        if autonomous {
            cmd.arg("--autonomous");
        }

        cmd.arg(prompt);

        // Run in background with proper detachment
        cmd.stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());

        // Double-fork technique to prevent zombie processes:
        // 1. Fork once to create intermediate process
        // 2. Intermediate process forks again and exits immediately
        // 3. Grandchild is adopted by init (PID 1) which will reap it
        unsafe {
            cmd.pre_exec(|| {
                // Fork again inside the child
                match libc::fork() {
                    -1 => return Err(std::io::Error::last_os_error()),
                    0 => {
                        // Grandchild: create new session and continue
                        libc::setsid();
                        // Continue execution (this becomes the actual g3 process)
                    }
                    _ => {
                        // Child: exit immediately so parent can reap it
                        libc::_exit(0);
                    }
                }
                Ok(())
            });
        }

        info!("Launching g3: {:?}", cmd);
        
        // Spawn and wait for the intermediate process to exit
        let mut child = cmd.spawn().context("Failed to spawn g3 process")?;
        let intermediate_pid = child.id();
        
        // Wait for intermediate process (it will exit immediately after forking)
        child.wait().context("Failed to wait for intermediate process")?;
        
        // The actual g3 process is now running as orphan
        // We need to scan for it by matching workspace and recent start time
        info!("Scanning for newly launched g3 process in workspace: {}", workspace);
        
        // Wait even longer for the process to fully start and appear in process list
        std::thread::sleep(std::time::Duration::from_millis(2500));
        
        // Refresh and scan for the process
        self.system.refresh_processes();
        let workspace_path = PathBuf::from(workspace);
        let mut found_pid = None;
        
        for (pid, process) in self.system.processes() {
            let cmd = process.cmd();
            let cmd_str = cmd.join(" ");
            
            // Check if this is a g3 process
            let is_g3 = process.name().contains("g3") || cmd_str.contains("g3");
            if !is_g3 {
                continue;
            }
            
            // Check if it has our workspace
            let has_workspace = cmd.iter().any(|arg| {
                if let Ok(path) = PathBuf::from(arg).canonicalize() {
                    if let Ok(ws) = workspace_path.canonicalize() {
                        return path == ws;
                    }
                }
                false
            });
            
            if has_workspace {
                // Check if it's recent (started within last 10 seconds)
                let now = std::time::SystemTime::now();
                let start_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(process.start_time());
                if let Ok(duration) = now.duration_since(start_time) {
                    if duration.as_secs() < 10 {
                        found_pid = Some(pid.as_u32());
                        break;
                    }
                }
            }
        }
        
        let pid = if let Some(found) = found_pid {
            found
        } else {
            // If we couldn't find it, try one more refresh after a longer delay
            info!("Process not found on first scan, trying again...");
            std::thread::sleep(std::time::Duration::from_millis(2000));
            self.system.refresh_processes();
            
            // Try the scan again with full logic
            let mut retry_found = None;
            for (pid, process) in self.system.processes() {
                let cmd = process.cmd();
                let cmd_str = cmd.join(" ");
                
                let is_g3 = process.name().contains("g3") || cmd_str.contains("g3");
                if !is_g3 {
                    continue;
                }
                
                let has_workspace = cmd.iter().any(|arg| {
                    if let Ok(path) = PathBuf::from(arg).canonicalize() {
                        if let Ok(ws) = workspace_path.canonicalize() {
                            return path == ws;
                        }
                    }
                    false
                });
                
                if has_workspace {
                    retry_found = Some(pid.as_u32());
                    break;
                }
            }
            
            retry_found.unwrap_or(intermediate_pid)
        };

        info!("Launched g3 process with PID {}", pid);
        
        // Store launch params for restart
        let params = LaunchParams {
            workspace: workspace.into(),
            provider: provider.to_string(),
            model: model.to_string(),
            prompt: prompt.to_string(),
            autonomous,
            g3_binary_path: g3_binary_path.map(|s| s.to_string()),
        };
        
        if let Ok(mut map) = self.launch_params.lock() {
            map.insert(pid, params);
        }
        
        Ok(pid)
    }
    
    pub fn get_launch_params(&mut self, pid: u32) -> Option<LaunchParams> {
        // First check if we have stored params (for console-launched instances)
        if let Ok(map) = self.launch_params.lock() {
            if let Some(params) = map.get(&pid) {
                return Some(params.clone());
            }
        }
        
        // If not found, try to parse from process command line (for detected instances)
        self.system.refresh_processes();
        let sysinfo_pid = Pid::from_u32(pid);
        
        if let Some(process) = self.system.process(sysinfo_pid) {
            let cmd = process.cmd();
            return self.parse_launch_params_from_cmd(cmd);
        }
        
        None
    }
    
    fn parse_launch_params_from_cmd(&self, cmd: &[String]) -> Option<LaunchParams> {
        let mut workspace = None;
        let mut provider = None;
        let mut model = None;
        let mut prompt = None;
        let mut autonomous = false;
        let mut g3_binary_path = None;
        
        let mut i = 0;
        while i < cmd.len() {
            match cmd[i].as_str() {
                "--workspace" | "-w" if i + 1 < cmd.len() => {
                    workspace = Some(PathBuf::from(&cmd[i + 1]));
                    i += 2;
                }
                "--provider" if i + 1 < cmd.len() => {
                    provider = Some(cmd[i + 1].clone());
                    i += 2;
                }
                "--model" if i + 1 < cmd.len() => {
                    model = Some(cmd[i + 1].clone());
                    i += 2;
                }
                "--autonomous" => {
                    autonomous = true;
                    i += 1;
                }
                _ => {
                    // Last non-flag argument is likely the prompt
                    if !cmd[i].starts_with('-') && i == cmd.len() - 1 {
                        prompt = Some(cmd[i].clone());
                    }
                    i += 1;
                }
            }
        }
        
        // Try to determine binary path from cmd[0]
        if !cmd.is_empty() {
            let first = &cmd[0];
            if first.contains("g3") && !first.contains("cargo") {
                g3_binary_path = Some(first.clone());
            }
        }
        
        // Only return params if we have the minimum required fields
        if let (Some(ws), Some(prov), Some(mdl), Some(prmt)) = (workspace, provider, model, prompt) {
            Some(LaunchParams {
                workspace: ws,
                provider: prov,
                model: mdl,
                prompt: prmt,
                autonomous,
                g3_binary_path,
            })
        } else {
            None
        }
    }
}

impl Default for ProcessController {
    fn default() -> Self {
        Self::new()
    }
}

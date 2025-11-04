use sysinfo::{System, Pid};

fn main() {
    let mut sys = System::new_all();
    sys.refresh_processes();
    
    println!("Looking for g3 processes...");
    
    for (pid, process) in sys.processes() {
        let cmd = process.cmd();
        if cmd.is_empty() {
            continue;
        }
        
        let cmd_str = cmd.join(" ");
        
        // Check if this contains 'g3'
        if cmd_str.contains("g3") {
            println!("\nFound potential g3 process:");
            println!("  PID: {}", pid);
            println!("  Name: {}", process.name());
            println!("  Cmd[0]: {:?}", cmd.get(0));
            println!("  Full cmd: {:?}", cmd);
            
            // Check detection logic
            let is_g3_binary = cmd.get(0).map(|s| s.ends_with("g3")).unwrap_or(false);
            let is_cargo_run = cmd.get(0).map(|s| s.contains("cargo")).unwrap_or(false)
                && cmd.iter().any(|s| s == "run" || s.contains("g3"));
            
            println!("  is_g3_binary: {}", is_g3_binary);
            println!("  is_cargo_run: {}", is_cargo_run);
            
            // Check workspace
            let has_workspace = cmd.iter().any(|s| s == "--workspace" || s == "-w");
            println!("  has_workspace: {}", has_workspace);
        }
    }
}

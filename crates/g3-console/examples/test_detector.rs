use sysinfo::{System, Pid};

fn main() {
    let mut sys = System::new_all();
    sys.refresh_processes();
    
    // Test with known PIDs
    let pids = vec![68123, 72749];
    
    for pid_num in pids {
        let pid = Pid::from_u32(pid_num);
        if let Some(process) = sys.process(pid) {
            println!("\nPID: {}", pid_num);
            println!("Name: {}", process.name());
            println!("Cmd: {:?}", process.cmd());
            println!("Exe: {:?}", process.exe());
        }
    }
}

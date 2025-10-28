/// Simple output helper for printing messages
pub struct SimpleOutput {
    machine_mode: bool,
}

impl SimpleOutput {
    pub fn new() -> Self {
        SimpleOutput { machine_mode: false }
    }

    pub fn new_with_mode(machine_mode: bool) -> Self {
        SimpleOutput { machine_mode }
    }

    pub fn print(&self, message: &str) {
        if !self.machine_mode {
            println!("{}", message);
        }
    }

    pub fn print_smart(&self, message: &str) {
        if !self.machine_mode {
            println!("{}", message);
        }
    }
}

impl Default for SimpleOutput {
    fn default() -> Self {
        Self::new()
    }
}

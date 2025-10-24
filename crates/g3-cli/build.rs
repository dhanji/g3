use std::env;

fn main() {
    // Only add rpaths on macOS
    if env::var("CARGO_CFG_TARGET_OS").unwrap() == "macos" {
        // Add rpath so libVisionBridge.dylib can be found at runtime
        // @executable_path means "relative to the executable"
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
    }
}

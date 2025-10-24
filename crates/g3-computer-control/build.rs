use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Only build Vision bridge on macOS
    if env::var("CARGO_CFG_TARGET_OS").unwrap() != "macos" {
        return;
    }

    println!("cargo:rerun-if-changed=vision-bridge/Sources/VisionBridge/VisionOCR.swift");
    println!("cargo:rerun-if-changed=vision-bridge/Sources/VisionBridge/VisionBridge.h");
    println!("cargo:rerun-if-changed=vision-bridge/Package.swift");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let vision_bridge_dir = manifest_dir.join("vision-bridge");

    // Build Swift package
    println!("cargo:warning=Building VisionBridge Swift package...");
    let build_status = Command::new("swift")
        .args(&["build", "-c", "release"])
        .current_dir(&vision_bridge_dir)
        .status()
        .expect("Failed to build Swift package");

    if !build_status.success() {
        panic!("Swift build failed");
    }

    // Find the built library
    let lib_path = vision_bridge_dir
        .join(".build/release")
        .canonicalize()
        .expect("Failed to find .build/release directory");

    // Copy the dylib to the output directory so it can be found at runtime
    let target_dir = manifest_dir.parent().unwrap().parent().unwrap().join("target");
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let output_dir = target_dir.join(&profile);
    
    let dylib_src = lib_path.join("libVisionBridge.dylib");
    let dylib_dst = output_dir.join("libVisionBridge.dylib");
    
    std::fs::copy(&dylib_src, &dylib_dst)
        .expect(&format!("Failed to copy dylib from {} to {}", dylib_src.display(), dylib_dst.display()));
    
    println!("cargo:warning=Copied libVisionBridge.dylib to {}", dylib_dst.display());
    
    // Add rpath so the dylib can be found at runtime
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
    println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
    println!("cargo:rustc-link-search=native={}", lib_path.display());
    println!("cargo:rustc-link-lib=dylib=VisionBridge");

    // Link required frameworks
    println!("cargo:rustc-link-lib=framework=Vision");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=CoreGraphics");
    println!("cargo:rustc-link-lib=framework=CoreImage");

    println!("cargo:warning=VisionBridge built successfully at {}", lib_path.display());
}

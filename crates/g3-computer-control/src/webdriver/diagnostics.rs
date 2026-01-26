//! Chrome WebDriver diagnostics module
//!
//! Checks for common setup issues and provides detailed fix suggestions.

use std::path::PathBuf;
use std::process::Command;

/// Result of a diagnostic check
#[derive(Debug, Clone)]
pub struct DiagnosticResult {
    pub name: String,
    pub status: DiagnosticStatus,
    pub message: String,
    pub fix_suggestion: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiagnosticStatus {
    Ok,
    Warning,
    Error,
}

/// Full diagnostic report for Chrome headless setup
#[derive(Debug)]
pub struct ChromeDiagnosticReport {
    pub results: Vec<DiagnosticResult>,
    pub chrome_version: Option<String>,
    pub chromedriver_version: Option<String>,
    pub chrome_path: Option<PathBuf>,
    pub chromedriver_path: Option<PathBuf>,
    pub config_chrome_binary: Option<String>,
}

impl ChromeDiagnosticReport {
    /// Check if all diagnostics passed
    pub fn all_ok(&self) -> bool {
        self.results.iter().all(|r| r.status == DiagnosticStatus::Ok)
    }

    /// Check if there are any errors (not just warnings)
    pub fn has_errors(&self) -> bool {
        self.results.iter().any(|r| r.status == DiagnosticStatus::Error)
    }

    /// Format the report as a human-readable string
    pub fn format_report(&self) -> String {
        let mut output = String::new();
        output.push_str("\n╔══════════════════════════════════════════════════════════════╗\n");
        output.push_str("║           Chrome Headless Diagnostic Report                  ║\n");
        output.push_str("╚══════════════════════════════════════════════════════════════╝\n\n");

        // Summary section
        output.push_str("📋 **Summary**\n");
        if let Some(ref path) = self.chrome_path {
            output.push_str(&format!("   Chrome: {}\n", path.display()));
        }
        if let Some(ref ver) = self.chrome_version {
            output.push_str(&format!("   Chrome Version: {}\n", ver));
        }
        if let Some(ref path) = self.chromedriver_path {
            output.push_str(&format!("   ChromeDriver: {}\n", path.display()));
        }
        if let Some(ref ver) = self.chromedriver_version {
            output.push_str(&format!("   ChromeDriver Version: {}\n", ver));
        }
        if let Some(ref binary) = self.config_chrome_binary {
            output.push_str(&format!("   Config chrome_binary: {}\n", binary));
        }
        output.push_str("\n");

        // Results section
        output.push_str("🔍 **Diagnostic Results**\n\n");
        
        for result in &self.results {
            let icon = match result.status {
                DiagnosticStatus::Ok => "✅",
                DiagnosticStatus::Warning => "⚠️",
                DiagnosticStatus::Error => "❌",
            };
            output.push_str(&format!("{} **{}**\n", icon, result.name));
            output.push_str(&format!("   {}\n", result.message));
            
            if let Some(ref fix) = result.fix_suggestion {
                output.push_str(&format!("   💡 Fix: {}\n", fix));
            }
            output.push_str("\n");
        }

        // Overall status
        if self.all_ok() {
            output.push_str("🎉 **All checks passed!** Chrome headless is ready to use.\n");
        } else if self.has_errors() {
            output.push_str("\n🛠️ **Action Required**\n");
            output.push_str("   Some issues need to be fixed before Chrome headless will work.\n");
            output.push_str("   You can ask me to help fix these issues.\n");
        } else {
            output.push_str("\n⚠️ **Warnings Present**\n");
            output.push_str("   Chrome headless may work, but there are potential issues.\n");
        }

        output
    }
}

/// Run all Chrome headless diagnostics
pub fn run_diagnostics(config_chrome_binary: Option<&str>) -> ChromeDiagnosticReport {
    let mut results = Vec::new();
    let mut chrome_version = None;
    let mut chromedriver_version = None;
    let mut chrome_path = None;
    let mut chromedriver_path = None;

    // 1. Check for ChromeDriver in PATH
    let chromedriver_check = check_chromedriver_installed();
    if chromedriver_check.status == DiagnosticStatus::Ok {
        chromedriver_path = find_chromedriver_path();
        chromedriver_version = get_chromedriver_version();
    }
    results.push(chromedriver_check);

    // 2. Check for Chrome installation
    let chrome_check = check_chrome_installed(config_chrome_binary);
    if chrome_check.status == DiagnosticStatus::Ok {
        chrome_path = find_chrome_path(config_chrome_binary);
        chrome_version = get_chrome_version(config_chrome_binary);
    }
    results.push(chrome_check);

    // 3. Check version compatibility
    if chrome_version.is_some() && chromedriver_version.is_some() {
        results.push(check_version_compatibility(
            chrome_version.as_deref(),
            chromedriver_version.as_deref(),
        ));
    }

    // 4. Check config.toml chrome_binary setting
    results.push(check_config_chrome_binary(config_chrome_binary, chrome_path.as_ref()));

    // 5. Check for Chrome for Testing installation
    results.push(check_chrome_for_testing());

    // 6. Check ChromeDriver is executable (macOS quarantine)
    if chromedriver_path.is_some() {
        results.push(check_chromedriver_executable());
    }

    ChromeDiagnosticReport {
        results,
        chrome_version,
        chromedriver_version,
        chrome_path,
        chromedriver_path,
        config_chrome_binary: config_chrome_binary.map(String::from),
    }
}

/// Check if ChromeDriver is installed and in PATH
fn check_chromedriver_installed() -> DiagnosticResult {
    match Command::new("which").arg("chromedriver").output() {
        Ok(output) if output.status.success() => {
            DiagnosticResult {
                name: "ChromeDriver Installation".to_string(),
                status: DiagnosticStatus::Ok,
                message: "ChromeDriver found in PATH".to_string(),
                fix_suggestion: None,
            }
        }
        _ => {
            // Check common locations
            let common_paths = [
                // Chrome for Testing - macOS
                dirs::home_dir().map(|h| h.join(".chrome-for-testing/chromedriver-mac-arm64/chromedriver")),
                dirs::home_dir().map(|h| h.join(".chrome-for-testing/chromedriver-mac-x64/chromedriver")),
                // Chrome for Testing - Linux
                dirs::home_dir().map(|h| h.join(".chrome-for-testing/chromedriver-linux64/chromedriver")),
                dirs::home_dir().map(|h| h.join(".chrome-for-testing/chromedriver-linux-arm64/chromedriver")),
                // System locations
                Some(PathBuf::from("/usr/local/bin/chromedriver")),
                Some(PathBuf::from("/opt/homebrew/bin/chromedriver")),
            ];

            for path in common_paths.iter().flatten() {
                if path.exists() {
                    return DiagnosticResult {
                        name: "ChromeDriver Installation".to_string(),
                        status: DiagnosticStatus::Warning,
                        message: format!("ChromeDriver found at {} but not in PATH", path.display()),
                        fix_suggestion: Some(format!(
                            "Add to your shell config (~/.zshrc or ~/.bashrc):\nexport PATH=\"{}:$PATH\"",
                            path.parent().unwrap().display()
                        )),
                    };
                }
            }

            DiagnosticResult {
                name: "ChromeDriver Installation".to_string(),
                status: DiagnosticStatus::Error,
                message: "ChromeDriver not found".to_string(),
                fix_suggestion: Some(
                    "Install ChromeDriver using one of these methods:\n\
                     1. Run: ./scripts/setup-chrome-for-testing.sh (recommended)\n\
                     2. Or: brew install chromedriver".to_string()
                ),
            }
        }
    }
}

/// Check if Chrome is installed
fn check_chrome_installed(config_binary: Option<&str>) -> DiagnosticResult {
    // First check configured binary
    if let Some(binary) = config_binary {
        if PathBuf::from(binary).exists() {
            return DiagnosticResult {
                name: "Chrome Installation".to_string(),
                status: DiagnosticStatus::Ok,
                message: format!("Chrome found at configured path: {}", binary),
                fix_suggestion: None,
            };
        } else {
            return DiagnosticResult {
                name: "Chrome Installation".to_string(),
                status: DiagnosticStatus::Error,
                message: format!("Configured chrome_binary not found: {}", binary),
                fix_suggestion: Some(
                    "Update chrome_binary in ~/.config/g3/config.toml to a valid Chrome path,\n\
                     or remove it to use system Chrome".to_string()
                ),
            };
        }
    }

    // Check common Chrome locations
    let chrome_paths = get_chrome_search_paths();
    
    for path in &chrome_paths {
        if path.exists() {
            return DiagnosticResult {
                name: "Chrome Installation".to_string(),
                status: DiagnosticStatus::Ok,
                message: format!("Chrome found at: {}", path.display()),
                fix_suggestion: None,
            };
        }
    }

    DiagnosticResult {
        name: "Chrome Installation".to_string(),
        status: DiagnosticStatus::Error,
        message: "Chrome/Chromium not found".to_string(),
        fix_suggestion: Some(
            "Install Chrome using one of these methods:\n\
             1. Run: ./scripts/setup-chrome-for-testing.sh (recommended)\n\
             2. Download from: https://www.google.com/chrome/\n\
             3. Or: brew install --cask google-chrome".to_string()
        ),
    }
}

/// Check Chrome and ChromeDriver version compatibility
fn check_version_compatibility(
    chrome_ver: Option<&str>,
    chromedriver_ver: Option<&str>,
) -> DiagnosticResult {
    let chrome_major = chrome_ver.and_then(extract_major_version);
    let driver_major = chromedriver_ver.and_then(extract_major_version);

    match (chrome_major, driver_major) {
        (Some(cv), Some(dv)) if cv == dv => {
            DiagnosticResult {
                name: "Version Compatibility".to_string(),
                status: DiagnosticStatus::Ok,
                message: format!("Chrome ({}) and ChromeDriver ({}) versions match", cv, dv),
                fix_suggestion: None,
            }
        }
        (Some(cv), Some(dv)) => {
            DiagnosticResult {
                name: "Version Compatibility".to_string(),
                status: DiagnosticStatus::Error,
                message: format!(
                    "Version mismatch! Chrome is v{} but ChromeDriver is v{}",
                    cv, dv
                ),
                fix_suggestion: Some(
                    "Fix version mismatch:\n\
                     1. Run: ./scripts/setup-chrome-for-testing.sh (installs matching versions)\n\
                     2. Or update ChromeDriver: brew upgrade chromedriver".to_string()
                ),
            }
        }
        _ => {
            DiagnosticResult {
                name: "Version Compatibility".to_string(),
                status: DiagnosticStatus::Warning,
                message: "Could not determine version compatibility".to_string(),
                fix_suggestion: None,
            }
        }
    }
}

/// Check config.toml chrome_binary setting
fn check_config_chrome_binary(
    config_binary: Option<&str>,
    detected_chrome: Option<&PathBuf>,
) -> DiagnosticResult {
    match (config_binary, detected_chrome) {
        (Some(binary), _) if PathBuf::from(binary).exists() => {
            DiagnosticResult {
                name: "Config chrome_binary".to_string(),
                status: DiagnosticStatus::Ok,
                message: "chrome_binary is configured and valid".to_string(),
                fix_suggestion: None,
            }
        }
        (Some(binary), _) => {
            DiagnosticResult {
                name: "Config chrome_binary".to_string(),
                status: DiagnosticStatus::Error,
                message: format!("chrome_binary path does not exist: {}", binary),
                fix_suggestion: Some(
                    "Update ~/.config/g3/config.toml with a valid chrome_binary path".to_string()
                ),
            }
        }
        (None, Some(chrome)) => {
            // Check if it's Chrome for Testing - recommend configuring it
            let chrome_str = chrome.to_string_lossy();
            if chrome_str.contains("chrome-for-testing") || chrome_str.contains("Chrome for Testing") {
                DiagnosticResult {
                    name: "Config chrome_binary".to_string(),
                    status: DiagnosticStatus::Warning,
                    message: "Chrome for Testing detected but not configured in config.toml".to_string(),
                    fix_suggestion: Some(format!(
                        "Add to ~/.config/g3/config.toml:\n\
                         [webdriver]\n\
                         chrome_binary = \"{}\"",
                        chrome.display()
                    )),
                }
            } else {
                DiagnosticResult {
                    name: "Config chrome_binary".to_string(),
                    status: DiagnosticStatus::Ok,
                    message: "Using system Chrome (no chrome_binary configured)".to_string(),
                    fix_suggestion: None,
                }
            }
        }
        (None, None) => {
            DiagnosticResult {
                name: "Config chrome_binary".to_string(),
                status: DiagnosticStatus::Warning,
                message: "No chrome_binary configured and no Chrome detected".to_string(),
                fix_suggestion: Some(
                    "Install Chrome and optionally configure chrome_binary in config.toml".to_string()
                ),
            }
        }
    }
}

/// Check for Chrome for Testing installation
fn check_chrome_for_testing() -> DiagnosticResult {
    let cft_dir = dirs::home_dir().map(|h| h.join(".chrome-for-testing"));

    match cft_dir {
        Some(dir) if dir.exists() => {
            // Check for both Chrome and ChromeDriver (support macOS and Linux)
            let has_chrome = dir.join("chrome-mac-arm64").exists()
                || dir.join("chrome-mac-x64").exists()
                || dir.join("chrome-linux64").exists()
                || dir.join("chrome-linux-arm64").exists();
            let has_driver = dir.join("chromedriver-mac-arm64").exists()
                || dir.join("chromedriver-mac-x64").exists()
                || dir.join("chromedriver-linux64").exists()
                || dir.join("chromedriver-linux-arm64").exists();

            if has_chrome && has_driver {
                DiagnosticResult {
                    name: "Chrome for Testing".to_string(),
                    status: DiagnosticStatus::Ok,
                    message: "Chrome for Testing is installed with matching ChromeDriver".to_string(),
                    fix_suggestion: None,
                }
            } else if has_chrome {
                DiagnosticResult {
                    name: "Chrome for Testing".to_string(),
                    status: DiagnosticStatus::Warning,
                    message: "Chrome for Testing found but ChromeDriver is missing".to_string(),
                    fix_suggestion: Some(
                        "Run: ./scripts/setup-chrome-for-testing.sh to install matching ChromeDriver".to_string()
                    ),
                }
            } else {
                DiagnosticResult {
                    name: "Chrome for Testing".to_string(),
                    status: DiagnosticStatus::Warning,
                    message: "Chrome for Testing directory exists but is incomplete".to_string(),
                    fix_suggestion: Some(
                        "Run: ./scripts/setup-chrome-for-testing.sh to reinstall".to_string()
                    ),
                }
            }
        }
        _ => {
            DiagnosticResult {
                name: "Chrome for Testing".to_string(),
                status: DiagnosticStatus::Ok,
                message: "Chrome for Testing not installed (using system Chrome)".to_string(),
                fix_suggestion: None,
            }
        }
    }
}

/// Check if ChromeDriver is executable (macOS quarantine issue)
fn check_chromedriver_executable() -> DiagnosticResult {
    match Command::new("chromedriver").arg("--version").output() {
        Ok(output) if output.status.success() => {
            DiagnosticResult {
                name: "ChromeDriver Executable".to_string(),
                status: DiagnosticStatus::Ok,
                message: "ChromeDriver is executable".to_string(),
                fix_suggestion: None,
            }
        }
        Ok(_) => {
            DiagnosticResult {
                name: "ChromeDriver Executable".to_string(),
                status: DiagnosticStatus::Error,
                message: "ChromeDriver found but failed to execute".to_string(),
                fix_suggestion: Some(
                    "Remove macOS quarantine attribute:\n\
                     xattr -d com.apple.quarantine $(which chromedriver)".to_string()
                ),
            }
        }
        Err(_) => {
            DiagnosticResult {
                name: "ChromeDriver Executable".to_string(),
                status: DiagnosticStatus::Error,
                message: "ChromeDriver not executable or not in PATH".to_string(),
                fix_suggestion: Some(
                    "Ensure ChromeDriver is in PATH and executable:\n\
                     chmod +x $(which chromedriver)".to_string()
                ),
            }
        }
    }
}

// Helper functions

fn find_chromedriver_path() -> Option<PathBuf> {
    Command::new("which")
        .arg("chromedriver")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
}

fn find_chrome_path(config_binary: Option<&str>) -> Option<PathBuf> {
    if let Some(binary) = config_binary {
        let path = PathBuf::from(binary);
        if path.exists() {
            return Some(path);
        }
    }

    for path in get_chrome_search_paths() {
        if path.exists() {
            return Some(path);
        }
    }
    None
}

fn get_chrome_search_paths() -> Vec<PathBuf> {
    // Check current platform to prioritize appropriate paths
    #[cfg(target_os = "linux")]
    {
        let mut paths = Vec::new();

        // Linux Chrome for Testing paths (prioritized for Linux)
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(".chrome-for-testing/chrome-linux64/chrome"));
            paths.push(home.join(".chrome-for-testing/chrome-linux-arm64/chrome"));
        }

        // Linux system paths
        paths.extend([
            PathBuf::from("/usr/bin/google-chrome"),
            PathBuf::from("/usr/bin/google-chrome-stable"),
            PathBuf::from("/usr/bin/chromium"),
            PathBuf::from("/usr/bin/chromium-browser"),
        ]);

        // macOS paths (fallback)
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(".chrome-for-testing/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"));
            paths.push(home.join(".chrome-for-testing/chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"));
        }
        paths.push(PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
        paths.push(PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"));

        paths
    }

    #[cfg(target_os = "macos")]
    {
        let mut paths = vec![
            // macOS paths (prioritized for macOS)
            PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
        ];

        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(".chrome-for-testing/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"));
            paths.push(home.join(".chrome-for-testing/chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"));
            // Linux paths (fallback)
            paths.push(home.join(".chrome-for-testing/chrome-linux64/chrome"));
            paths.push(home.join(".chrome-for-testing/chrome-linux-arm64/chrome"));
        }

        paths
    }

    #[cfg(target_os = "windows")]
    {
        let mut paths = Vec::new();

        // Windows paths
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(r"AppData\Local\Google\Chrome\Application\chrome.exe"));
        }
        paths.push(PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe"));
        paths.push(PathBuf::from(r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"));

        paths
    }
}

fn get_chromedriver_version() -> Option<String> {
    Command::new("chromedriver")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn get_chrome_version(config_binary: Option<&str>) -> Option<String> {
    let chrome_path = find_chrome_path(config_binary)?;
    
    Command::new(&chrome_path)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn extract_major_version(version_str: &str) -> Option<u32> {
    // Extract version number from strings like:
    // "Google Chrome 120.0.6099.109"
    // "ChromeDriver 120.0.6099.109"
    version_str
        .split_whitespace()
        .find(|s| s.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false))
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_major_version() {
        assert_eq!(extract_major_version("Google Chrome 120.0.6099.109"), Some(120));
        assert_eq!(extract_major_version("ChromeDriver 120.0.6099.109"), Some(120));
        assert_eq!(extract_major_version("120.0.6099.109"), Some(120));
        assert_eq!(extract_major_version("invalid"), None);
    }
}

//! Browser automation via WebDriver (Safari, headless Chrome).
//!
//! This crate used to also hold desktop control (mouse/keyboard, OCR, window
//! screenshots via CoreGraphics/X11/Win32). That layer was removed 2026-11:
//! its only surviving tool (`take_screenshot`) had no dispatch entry, so it
//! could never be called, yet the controller was still initialised on every
//! startup. WebDriver is what butler actually uses.

pub mod webdriver;

// Re-export webdriver types for convenience
pub use webdriver::{
    chrome::ChromeDriver, safari::SafariDriver, WebDriverController, WebElement,
    diagnostics::{run_diagnostics as run_chrome_diagnostics, ChromeDiagnosticReport, DiagnosticStatus},
};

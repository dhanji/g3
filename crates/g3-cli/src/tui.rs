use crossterm::style::Color;
use crossterm::style::{SetForegroundColor, ResetColor};
use std::io::{self, Write};
use termimad::MadSkin;

/// Simple output handler with markdown support
pub struct SimpleOutput {
    mad_skin: MadSkin,
}

impl SimpleOutput {
    pub fn new() -> Self {
        let mut mad_skin = MadSkin::default();
        // Dracula color scheme
        // Background: #282a36, Foreground: #f8f8f2
        // Colors: Cyan #8be9fd, Green #50fa7b, Orange #ffb86c, Pink #ff79c6, Purple #bd93f9, Red #ff5555, Yellow #f1fa8c
        
        mad_skin.set_headers_fg(Color::Rgb { r: 189, g: 147, b: 249 }); // Purple for headers
        mad_skin.bold.set_fg(Color::Rgb { r: 255, g: 121, b: 198 });    // Pink for bold
        mad_skin.italic.set_fg(Color::Rgb { r: 139, g: 233, b: 253 });  // Cyan for italic
        mad_skin.code_block.set_bg(Color::Rgb { r: 68, g: 71, b: 90 }); // Dracula background variant
        mad_skin.code_block.set_fg(Color::Rgb { r: 80, g: 250, b: 123 }); // Green for code text
        mad_skin.inline_code.set_bg(Color::Rgb { r: 68, g: 71, b: 90 }); // Same background for inline code
        mad_skin.inline_code.set_fg(Color::Rgb { r: 241, g: 250, b: 140 }); // Yellow for inline code
        mad_skin.quote_mark.set_fg(Color::Rgb { r: 98, g: 114, b: 164 }); // Comment purple for quote marks
        mad_skin.strikeout.set_fg(Color::Rgb { r: 255, g: 85, b: 85 });  // Red for strikethrough
        
        Self { mad_skin }
    }

    /// Detect if text contains markdown formatting
    fn has_markdown(&self, text: &str) -> bool {
        // Check for common markdown patterns
        text.contains("**") ||
        text.contains("```") ||
        text.contains("`") ||
        text.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with('#') ||
            trimmed.starts_with("- ") ||
            trimmed.starts_with("* ") ||
            trimmed.starts_with("+ ") ||
            (trimmed.len() > 2 && 
             trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) &&
             trimmed.chars().nth(1) == Some('.') &&
             trimmed.chars().nth(2) == Some(' ')) ||
            (trimmed.contains('[') && trimmed.contains("]("))
        }) ||
        (text.matches('*').count() >= 2 && !text.contains("/*") && !text.contains("*/"))
    }

    pub fn print(&self, text: &str) {
        println!("{}", text);
    }

    /// Smart print that automatically detects and renders markdown
    pub fn print_smart(&self, text: &str) {
        if self.has_markdown(text) {
            self.print_markdown(text);
        } else {
            self.print(text);
        }
    }

    pub fn print_markdown(&self, markdown: &str) {
        self.mad_skin.print_text(markdown);
    }

    pub fn _print_status(&self, status: &str) {
        println!("📊 {}", status);
    }

    pub fn print_context(&self, used: u32, total: u32, percentage: f32) {
        let total_dots = 10;
        let filled_dots = ((percentage / 100.0) * total_dots as f32) as usize;
        let empty_dots = total_dots.saturating_sub(filled_dots);

        let filled_str = "●".repeat(filled_dots);
        let empty_str = "○".repeat(empty_dots);
        
        // Determine color based on percentage
        let color = if percentage < 40.0 {
            crossterm::style::Color::Green
        } else if percentage < 60.0 {
            crossterm::style::Color::Yellow
        } else if percentage < 80.0 {
            crossterm::style::Color::Rgb { r: 255, g: 165, b: 0 } // Orange
        } else {
            crossterm::style::Color::Red
        };

        // Print with colored progress bar
        print!("Context: ");
        print!("{}", SetForegroundColor(color));
        print!("{}{}", filled_str, empty_str);
        print!("{}", ResetColor);
        println!(" {:.0}% ({}/{} tokens)", percentage, used, total);
    }

    pub fn print_context_thinning(&self, message: &str) {
        // Animated highlight for context thinning
        // Use bright cyan/green with a quick flash animation
        
        // Flash animation: print with bright background, then normal
        let frames = vec![
            "\x1b[1;97;46m",  // Frame 1: Bold white on cyan background
            "\x1b[1;97;42m",  // Frame 2: Bold white on green background
            "\x1b[1;96;40m",  // Frame 3: Bold cyan on black background
        ];
        
        println!();
        
        // Quick flash animation
        for frame in &frames {
            print!("\r{} ✨ {} ✨\x1b[0m", frame, message);
            let _ = io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
        
        // Final display with bright cyan and sparkle emojis
        print!("\r\x1b[1;96m✨ {} ✨\x1b[0m", message);
        println!();
        
        // Add a subtle "success" indicator line
        println!("\x1b[2;36m   └─ Context optimized successfully\x1b[0m");
        println!();
        
        let _ = io::stdout().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_markdown_detection() {
        let output = SimpleOutput::new();
        
        // Should detect markdown
        assert!(output.has_markdown("**bold text**"));
        assert!(output.has_markdown("`code`"));
        assert!(output.has_markdown("```\ncode block\n```"));
        assert!(output.has_markdown("# Header"));
        assert!(output.has_markdown("- list item"));
        assert!(output.has_markdown("* list item"));
        assert!(output.has_markdown("+ list item"));
        assert!(output.has_markdown("1. numbered item"));
        assert!(output.has_markdown("[link](url)"));
        assert!(output.has_markdown("*italic* text"));
        
        // Should NOT detect markdown
        assert!(!output.has_markdown("plain text"));
        assert!(!output.has_markdown("file.txt"));
        assert!(!output.has_markdown("/* comment */"));
        assert!(!output.has_markdown("just one * asterisk"));
        assert!(!output.has_markdown("📁 Workspace: /path/to/dir"));
        assert!(!output.has_markdown("✅ Success message"));
    }
}
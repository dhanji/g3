use crossterm::style::Color;
use crossterm::style::{SetForegroundColor, ResetColor};
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

    pub fn print(&self, text: &str) {
        println!("{}", text);
    }

    pub fn print_markdown(&self, markdown: &str) {
        self.mad_skin.print_text(markdown);
    }

    pub fn _print_status(&self, status: &str) {
        println!("📊 {}", status);
    }

    pub fn print_context(&self, used: u32, total: u32, percentage: f32) {
        let bar_width = 10;
        let filled_width = ((percentage / 100.0) * bar_width as f32) as usize;
        let empty_width = bar_width - filled_width;

        let filled_chars = "●".repeat(filled_width);
        let empty_chars = "○".repeat(empty_width);
        
        // Determine color based on percentage
        let color = if percentage < 60.0 {
            crossterm::style::Color::Green
        } else if percentage < 80.0 {
            crossterm::style::Color::Yellow
        } else {
            crossterm::style::Color::Red
        };

        // Print with colored progress bar
        print!("Context: ");
        print!("{}", SetForegroundColor(color));
        print!("{}{}", filled_chars, empty_chars);
        print!("{}", ResetColor);
        println!(" {:.1}% | {}/{} tokens", percentage, used, total);
    }
}

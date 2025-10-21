use g3_computer_control::SafariDriver;
use g3_computer_control::webdriver::WebDriverController;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    println!("Safari WebDriver Demo");
    println!("=====================\n");
    
    println!("Make sure to:");
    println!("1. Enable 'Allow Remote Automation' in Safari's Develop menu");
    println!("2. Run: /usr/bin/safaridriver --enable");
    println!("3. Start safaridriver in another terminal: safaridriver --port 4444\n");
    
    println!("Connecting to SafariDriver...");
    let mut driver = SafariDriver::new().await?;
    println!("✅ Connected!\n");
    
    // Navigate to a website
    println!("Navigating to example.com...");
    driver.navigate("https://example.com").await?;
    println!("✅ Navigated\n");
    
    // Get page title
    let title = driver.title().await?;
    println!("Page title: {}\n", title);
    
    // Get current URL
    let url = driver.current_url().await?;
    println!("Current URL: {}\n", url);
    
    // Find an element
    println!("Finding h1 element...");
    let mut h1 = driver.find_element("h1").await?;
    let h1_text = h1.text().await?;
    println!("H1 text: {}\n", h1_text);
    
    // Find all paragraphs
    println!("Finding all paragraphs...");
    let paragraphs = driver.find_elements("p").await?;
    println!("Found {} paragraphs\n", paragraphs.len());
    
    // Get page source
    println!("Getting page source...");
    let source = driver.page_source().await?;
    println!("Page source length: {} bytes\n", source.len());
    
    // Execute JavaScript
    println!("Executing JavaScript...");
    let result = driver.execute_script("return document.title", vec![]).await?;
    println!("JS result: {:?}\n", result);
    
    // Take a screenshot
    println!("Taking screenshot...");
    driver.screenshot("/tmp/safari_demo.png").await?;
    println!("✅ Screenshot saved to /tmp/safari_demo.png\n");
    
    // Close the browser
    println!("Closing browser...");
    driver.quit().await?;
    println!("✅ Done!");
    
    Ok(())
}

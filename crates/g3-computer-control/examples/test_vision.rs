use g3_computer_control::ocr::{OCREngine, DefaultOCR};
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    println!("🧪 Testing Apple Vision OCR");
    println!("===========================\n");
    
    // Initialize OCR engine
    println!("📦 Initializing OCR engine...");
    let ocr = DefaultOCR::new()?;
    println!("✅ OCR engine: {}\n", ocr.name());
    
    // Check if test image exists
    let test_image = "/tmp/safari_test.png";
    if !std::path::Path::new(test_image).exists() {
        println!("⚠️  Test image not found: {}", test_image);
        println!("   Creating a screenshot...");
        
        let status = std::process::Command::new("screencapture")
            .arg("-x")
            .arg("-R")
            .arg("0,0,1200,800")
            .arg(test_image)
            .status()?;
        
        if !status.success() {
            anyhow::bail!("Failed to create screenshot");
        }
        
        println!("✅ Screenshot created\n");
    }
    
    // Run OCR
    println!("🔍 Running Apple Vision OCR on {}...", test_image);
    let start = std::time::Instant::now();
    let locations = ocr.extract_text_with_locations(test_image).await?;
    let duration = start.elapsed();
    
    println!("✅ OCR completed in {:.3}s\n", duration.as_secs_f64());
    
    // Display results
    println!("📊 Results:");
    println!("   Found {} text elements\n", locations.len());
    
    if locations.is_empty() {
        println!("⚠️  No text found in image");
    } else {
        println!("   Top 20 results:");
        println!("   {:<4} {:<40} {:<15} {:<12} {:<8}", "#", "Text", "Position", "Size", "Conf");
        println!("   {}", "-".repeat(85));
        
        for (i, loc) in locations.iter().take(20).enumerate() {
            let text = if loc.text.len() > 37 {
                format!("{}...", &loc.text[..37])
            } else {
                loc.text.clone()
            };
            
            println!("   {:<4} {:<40} ({:>4},{:>4})    {:>4}x{:<4}  {:.2}",
                i + 1,
                text,
                loc.x,
                loc.y,
                loc.width,
                loc.height,
                loc.confidence
            );
        }
        
        if locations.len() > 20 {
            println!("\n   ... and {} more", locations.len() - 20);
        }
        
        // Performance comparison
        println!("\n📈 Performance:");
        println!("   OCR Speed: {:.3}s", duration.as_secs_f64());
        println!("   Text elements: {}", locations.len());
        println!("   Avg per element: {:.1}ms", duration.as_millis() as f64 / locations.len() as f64);
    }
    
    println!("\n✅ Test complete!");
    
    Ok(())
}

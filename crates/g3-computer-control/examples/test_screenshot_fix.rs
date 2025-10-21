use core_graphics::display::CGDisplay;
use image::{ImageBuffer, RgbaImage};
use std::path::Path;

fn main() {
    let display = CGDisplay::main();
    let image = display.image().expect("Failed to capture screen");
    
    let width = image.width() as u32;
    let height = image.height() as u32;
    let bytes_per_row = image.bytes_per_row() as usize;
    let data = image.data();
    
    println!("Testing screenshot fix...");
    println!("Image: {}x{}, bytes_per_row: {}", width, height, bytes_per_row);
    println!("Expected bytes per row: {}", width * 4);
    println!("Padding per row: {} bytes", bytes_per_row - (width as usize * 4));
    
    // OLD METHOD (broken) - treating data as continuous
    println!("\n=== OLD METHOD (BROKEN) ===");
    let mut old_rgba = Vec::with_capacity(data.len() as usize);
    for chunk in data.chunks_exact(4) {
        old_rgba.push(chunk[2]); // R
        old_rgba.push(chunk[1]); // G
        old_rgba.push(chunk[0]); // B
        old_rgba.push(chunk[3]); // A
    }
    println!("Converted {} pixels", old_rgba.len() / 4);
    println!("Expected {} pixels", width * height);
    
    // NEW METHOD (fixed) - handling row padding
    println!("\n=== NEW METHOD (FIXED) ===");
    let mut new_rgba = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height as usize {
        let row_start = row * bytes_per_row;
        let row_end = row_start + (width as usize * 4);
        
        for chunk in data[row_start..row_end].chunks_exact(4) {
            new_rgba.push(chunk[2]); // R
            new_rgba.push(chunk[1]); // G
            new_rgba.push(chunk[0]); // B
            new_rgba.push(chunk[3]); // A
        }
    }
    println!("Converted {} pixels", new_rgba.len() / 4);
    println!("Expected {} pixels", width * height);
    
    // Save a small crop from both methods
    let crop_size = 200;
    
    // Old method crop
    let old_crop: Vec<u8> = old_rgba.iter().take((crop_size * crop_size * 4) as usize).copied().collect();
    if let Some(old_img) = ImageBuffer::from_raw(crop_size, crop_size, old_crop) {
        let old_img: RgbaImage = old_img;
        old_img.save("/tmp/screenshot_old_method.png").unwrap();
        println!("\nSaved OLD method crop to: /tmp/screenshot_old_method.png");
    }
    
    // New method crop
    let new_crop: Vec<u8> = new_rgba.iter().take((crop_size * crop_size * 4) as usize).copied().collect();
    if let Some(new_img) = ImageBuffer::from_raw(crop_size, crop_size, new_crop) {
        let new_img: RgbaImage = new_img;
        new_img.save("/tmp/screenshot_new_method.png").unwrap();
        println!("Saved NEW method crop to: /tmp/screenshot_new_method.png");
    }
    
    println!("\nOpen both images to compare:");
    println!("  open /tmp/screenshot_old_method.png /tmp/screenshot_new_method.png");
}

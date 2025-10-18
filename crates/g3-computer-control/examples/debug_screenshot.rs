use core_graphics::display::CGDisplay;

fn main() {
    let display = CGDisplay::main();
    let image = display.image().expect("Failed to capture screen");
    
    println!("CGImage properties:");
    println!("  Width: {}", image.width());
    println!("  Height: {}", image.height());
    println!("  Bits per component: {}", image.bits_per_component());
    println!("  Bits per pixel: {}", image.bits_per_pixel());
    println!("  Bytes per row: {}", image.bytes_per_row());
    
    let data = image.data();
    let expected_size = image.width() * image.height() * 4;
    println!("  Data length: {}", data.len());
    println!("  Expected (w*h*4): {}", expected_size);
    
    // Check if there's padding in rows
    let bytes_per_row = image.bytes_per_row();
    let width = image.width();
    let expected_bytes_per_row = width * 4;
    println!("\nRow alignment:");
    println!("  Actual bytes per row: {}", bytes_per_row);
    println!("  Expected (width * 4): {}", expected_bytes_per_row);
    println!("  Padding per row: {}", bytes_per_row - expected_bytes_per_row);
    
    // Sample some pixels from different locations
    println!("\nFirst 3 pixels (raw bytes):");
    for i in 0..3 {
        let offset = i * 4;
        println!("  Pixel {}: [{:3}, {:3}, {:3}, {:3}]", 
                 i, data[offset], data[offset+1], data[offset+2], data[offset+3]);
    }
    
    // Check a pixel from the middle
    let mid_row = image.height() / 2;
    let mid_col = image.width() / 2;
    let mid_offset = (mid_row * bytes_per_row + mid_col * 4) as usize;
    println!("\nMiddle pixel (row {}, col {}):", mid_row, mid_col);
    println!("  Offset: {}", mid_offset);
    if mid_offset + 3 < data.len() as usize {
        println!("  Bytes: [{:3}, {:3}, {:3}, {:3}]", 
                 data[mid_offset], data[mid_offset+1], data[mid_offset+2], data[mid_offset+3]);
    }
}

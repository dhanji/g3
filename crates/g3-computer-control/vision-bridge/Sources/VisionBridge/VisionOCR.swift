import Foundation
import Vision
import AppKit
import CoreGraphics

// MARK: - C Bridge Functions

@_cdecl("vision_recognize_text")
public func vision_recognize_text(
    _ imagePath: UnsafePointer<CChar>,
    _ imagePathLen: UInt32,
    _ outBoxes: UnsafeMutablePointer<UnsafeMutableRawPointer?>,
    _ outCount: UnsafeMutablePointer<UInt32>
) -> Bool {
    // Convert C string to Swift String
    guard let pathData = Data(bytes: imagePath, count: Int(imagePathLen)).withUnsafeBytes({
        String(bytes: $0, encoding: .utf8)
    }) else {
        return false
    }
    
    let path = pathData.trimmingCharacters(in: .whitespaces)
    
    // Load image
    guard let image = NSImage(contentsOfFile: path),
          let cgImage = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
        return false
    }
    
    // Perform OCR
    var textBoxes: [CTextBox] = []
    let semaphore = DispatchSemaphore(value: 0)
    var success = false
    
    let request = VNRecognizeTextRequest { request, error in
        defer { semaphore.signal() }
        
        if let error = error {
            print("Vision OCR error: \(error.localizedDescription)")
            return
        }
        
        guard let observations = request.results as? [VNRecognizedTextObservation] else {
            return
        }
        
        let imageSize = CGSize(width: cgImage.width, height: cgImage.height)
        
        for observation in observations {
            guard let candidate = observation.topCandidates(1).first else { continue }
            
            let text = candidate.string
            let boundingBox = observation.boundingBox
            
            // Convert normalized coordinates (bottom-left origin) to pixel coordinates (top-left origin)
            let x = Int32(boundingBox.origin.x * imageSize.width)
            let y = Int32((1.0 - boundingBox.origin.y - boundingBox.height) * imageSize.height)
            let width = Int32(boundingBox.width * imageSize.width)
            let height = Int32(boundingBox.height * imageSize.height)
            
            // Allocate C string for text
            let cString = strdup(text)
            
            textBoxes.append(CTextBox(
                text: cString,
                text_len: UInt32(text.utf8.count),
                x: x,
                y: y,
                width: width,
                height: height,
                confidence: observation.confidence
            ))
        }
        
        success = true
    }
    
    // Configure request for best accuracy
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = true
    request.recognitionLanguages = ["en-US"]
    
    // Perform request
    let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
    do {
        try handler.perform([request])
    } catch {
        print("Vision request failed: \(error.localizedDescription)")
        return false
    }
    
    // Wait for completion
    semaphore.wait()
    
    if !success {
        return false
    }
    
    // Allocate array for results
    let boxesPtr = UnsafeMutablePointer<CTextBox>.allocate(capacity: textBoxes.count)
    for (index, box) in textBoxes.enumerated() {
        boxesPtr[index] = box
    }
    
    outBoxes.pointee = UnsafeMutableRawPointer(boxesPtr)
    outCount.pointee = UInt32(textBoxes.count)
    
    return true
}

@_cdecl("vision_free_boxes")
public func vision_free_boxes(
    _ boxes: UnsafeMutableRawPointer,
    _ count: UInt32
) {
    let typedBoxes = boxes.assumingMemoryBound(to: CTextBox.self)
    for i in 0..<Int(count) {
        if let text = typedBoxes[i].text {
            free(UnsafeMutableRawPointer(mutating: text))
        }
    }
    typedBoxes.deallocate()
}

// MARK: - C-Compatible Structure

public struct CTextBox {
    public let text: UnsafePointer<CChar>?
    public let text_len: UInt32
    public let x: Int32
    public let y: Int32
    public let width: Int32
    public let height: Int32
    public let confidence: Float
    
    public init(text: UnsafePointer<CChar>?, text_len: UInt32, x: Int32, y: Int32, width: Int32, height: Int32, confidence: Float) {
        self.text = text
        self.text_len = text_len
        self.x = x
        self.y = y
        self.width = width
        self.height = height
        self.confidence = confidence
    }
}

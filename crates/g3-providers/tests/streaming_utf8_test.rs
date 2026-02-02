/// UTF-8 Streaming Tests for Multi-Byte Character Handling
///
/// Tests verify that streaming providers (OpenAI, Anthropic) correctly handle
/// multi-byte UTF-8 characters that may be split across chunk boundaries.
/// This is critical for preserving emoji, Chinese characters, and other
/// non-ASCII text in streaming responses.

use g3_providers::decode_utf8_streaming;

#[test]
fn test_emoji_split_across_chunks() {
    // 🎭 (U+1F3AD) = [F0 9F 8E AD] (4-byte UTF-8)
    // Split into two chunks: incomplete then complete

    let mut byte_buffer = Vec::new();

    // Chunk 1: First 3 bytes (incomplete)
    byte_buffer.extend_from_slice(&[0xF0, 0x9F, 0x8E]);
    let result1 = decode_utf8_streaming(&mut byte_buffer);
    assert_eq!(result1, None, "Incomplete UTF-8 sequence should return None");
    assert_eq!(byte_buffer.len(), 3, "Incomplete bytes should remain in buffer");

    // Chunk 2: Final byte (completes emoji)
    byte_buffer.extend_from_slice(&[0xAD]);
    let result2 = decode_utf8_streaming(&mut byte_buffer);
    assert_eq!(result2, Some("🎭".to_string()), "Complete UTF-8 should decode to emoji");
    assert_eq!(byte_buffer.len(), 0, "Buffer should be empty after successful decode");
}

#[test]
fn test_chinese_split_across_chunks() {
    // 中 (U+4E2D) = [E4 B8 AD] (3-byte UTF-8)
    // Split into two chunks

    let mut byte_buffer = Vec::new();

    // Chunk 1: First 2 bytes
    byte_buffer.extend_from_slice(&[0xE4, 0xB8]);
    let result1 = decode_utf8_streaming(&mut byte_buffer);
    assert_eq!(result1, None);
    assert_eq!(byte_buffer.len(), 2);

    // Chunk 2: Final byte + start of next character
    byte_buffer.extend_from_slice(&[0xAD, 0xE6, 0x96]);
    let result2 = decode_utf8_streaming(&mut byte_buffer);
    assert_eq!(result2, Some("中".to_string()));
    assert_eq!(byte_buffer.len(), 2, "Incomplete bytes of next char should remain");

    // Chunk 3: Complete second character 文 (U+6587) = [E6 96 87]
    byte_buffer.extend_from_slice(&[0x87]);
    let result3 = decode_utf8_streaming(&mut byte_buffer);
    assert_eq!(result3, Some("文".to_string()));
    assert_eq!(byte_buffer.len(), 0);
}

#[test]
fn test_mixed_content_with_multibyte() {
    // "Hello 👑 world 你好 🎯"
    // Split at arbitrary boundaries

    let text = "Hello 👑 world 你好 🎯";
    let bytes = text.as_bytes();

    let mut byte_buffer = Vec::new();
    let mut decoded = String::new();

    // Process in small chunks (3 bytes at a time to force splits)
    for chunk in bytes.chunks(3) {
        byte_buffer.extend_from_slice(chunk);

        if let Some(decoded_chunk) = decode_utf8_streaming(&mut byte_buffer) {
            decoded.push_str(&decoded_chunk);
        }
    }

    // Flush any remaining bytes
    if !byte_buffer.is_empty() {
        if let Some(final_chunk) = decode_utf8_streaming(&mut byte_buffer) {
            decoded.push_str(&final_chunk);
        }
    }

    assert_eq!(decoded, text, "Mixed content should be perfectly reassembled");
}

#[test]
fn test_ascii_unchanged() {
    // Regression test: ASCII-only content should work exactly as before

    let text = "Hello world! This is ASCII only.";
    let mut byte_buffer = Vec::new();

    byte_buffer.extend_from_slice(text.as_bytes());
    let result = decode_utf8_streaming(&mut byte_buffer);

    assert_eq!(result, Some(text.to_string()));
    assert_eq!(byte_buffer.len(), 0);
}

#[test]
fn test_persona_emoji_stream() {
    // GB-specific: Persona responses with heavy emoji
    // "Regina 👑 says: That's so fetch! 💖✨"

    let text = "Regina 👑 says: That's so fetch! 💖✨";
    let bytes = text.as_bytes();

    let mut byte_buffer = Vec::new();
    let mut decoded = String::new();

    // Simulate streaming in very small chunks (2 bytes) to maximize splitting
    for chunk in bytes.chunks(2) {
        byte_buffer.extend_from_slice(chunk);

        if let Some(decoded_chunk) = decode_utf8_streaming(&mut byte_buffer) {
            decoded.push_str(&decoded_chunk);
        }
    }

    // Flush remaining
    if !byte_buffer.is_empty() {
        if let Some(final_chunk) = decode_utf8_streaming(&mut byte_buffer) {
            decoded.push_str(&final_chunk);
        }
    }

    assert_eq!(decoded, text, "Persona emoji must be preserved in streaming");
    assert!(decoded.contains("👑"), "Crown emoji must be present");
    assert!(decoded.contains("💖"), "Heart emoji must be present");
    assert!(decoded.contains("✨"), "Sparkles emoji must be present");
}

#[test]
fn test_multiple_emoji_sequence() {
    // Test edge case: Multiple 4-byte emoji in sequence
    // "🎭🎯👑💖✨"

    let text = "🎭🎯👑💖✨";
    let bytes = text.as_bytes();

    let mut byte_buffer = Vec::new();
    let mut decoded = String::new();

    // Process in 5-byte chunks (will split 4-byte emoji)
    for chunk in bytes.chunks(5) {
        byte_buffer.extend_from_slice(chunk);

        if let Some(decoded_chunk) = decode_utf8_streaming(&mut byte_buffer) {
            decoded.push_str(&decoded_chunk);
        }
    }

    // Flush any remaining bytes
    if !byte_buffer.is_empty() {
        if let Some(final_chunk) = decode_utf8_streaming(&mut byte_buffer) {
            decoded.push_str(&final_chunk);
        }
    }

    assert_eq!(decoded, text);
}

#[test]
fn test_partial_emoji_at_stream_end() {
    // Edge case: Stream ends with incomplete multi-byte sequence
    // This should NOT happen in normal API usage but tests robustness

    let mut byte_buffer = Vec::new();

    // Complete character followed by incomplete
    byte_buffer.extend_from_slice("Hello 🎭".as_bytes());
    byte_buffer.push(0xF0); // Start of 4-byte emoji
    byte_buffer.push(0x9F); // Second byte

    let result = decode_utf8_streaming(&mut byte_buffer);

    // Should return "Hello 🎭" and leave incomplete bytes in buffer
    assert!(result.is_some());
    let decoded = result.unwrap();
    assert_eq!(decoded, "Hello 🎭");
    assert_eq!(byte_buffer.len(), 2, "Incomplete emoji bytes should remain");
}

#[test]
fn test_empty_buffer_returns_empty_string() {
    let mut byte_buffer = Vec::new();
    let result = decode_utf8_streaming(&mut byte_buffer);
    assert_eq!(result, Some("".to_string()), "Empty buffer should return Some(\"\")");
}

#[test]
fn test_single_byte_ascii() {
    let mut byte_buffer = Vec::new();
    byte_buffer.push(b'A');

    let result = decode_utf8_streaming(&mut byte_buffer);
    assert_eq!(result, Some("A".to_string()));
    assert_eq!(byte_buffer.len(), 0);
}

#[test]
fn test_consecutive_multibyte_sequences() {
    // Real-world scenario: Multiple multi-byte chars in quick succession
    // "OMG bestie! 😭💖 The code is like, literally SO fetch! 🎯✨👑"

    let text = "OMG bestie! 😭💖 The code is like, literally SO fetch! 🎯✨👑";
    let bytes = text.as_bytes();

    let mut byte_buffer = Vec::new();
    let mut decoded = String::new();

    // Simulate realistic chunking (chunks of 16 bytes)
    for chunk in bytes.chunks(16) {
        byte_buffer.extend_from_slice(chunk);

        if let Some(decoded_chunk) = decode_utf8_streaming(&mut byte_buffer) {
            decoded.push_str(&decoded_chunk);
        }
    }

    // Flush any remaining
    if !byte_buffer.is_empty() {
        if let Some(final_chunk) = decode_utf8_streaming(&mut byte_buffer) {
            decoded.push_str(&final_chunk);
        }
    }

    assert_eq!(decoded, text);
    assert_eq!(decoded.matches('😭').count(), 1);
    assert_eq!(decoded.matches('💖').count(), 1);
    assert_eq!(decoded.matches('🎯').count(), 1);
    assert_eq!(decoded.matches('✨').count(), 1);
    assert_eq!(decoded.matches('👑').count(), 1);
}

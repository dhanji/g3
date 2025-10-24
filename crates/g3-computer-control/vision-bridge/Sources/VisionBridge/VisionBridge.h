#ifndef VisionBridge_h
#define VisionBridge_h

#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

// Text box structure for FFI
typedef struct {
    const char* text;
    uint32_t text_len;
    int32_t x;
    int32_t y;
    int32_t width;
    int32_t height;
    float confidence;
} VisionTextBox;

// Recognize text in an image and return bounding boxes
// Returns true on success, false on failure
// Caller must free the returned boxes using vision_free_boxes
bool vision_recognize_text(
    const char* image_path,
    uint32_t image_path_len,
    VisionTextBox** out_boxes,
    uint32_t* out_count
);

// Free memory allocated by vision_recognize_text
void vision_free_boxes(VisionTextBox* boxes, uint32_t count);

#ifdef __cplusplus
}
#endif

#endif /* VisionBridge_h */

#pragma once

#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

/** P4X-EYE wiring used by the ESP-Video camera driver. */
typedef struct {
    int32_t sccb_i2c_port;
    int32_t sccb_clock_pin;
    int32_t sccb_data_pin;
    int32_t camera_enable_pin;
    int32_t reset_pin;
    int32_t xclk_pin;
    uint32_t xclk_hz;
} camera_bridge_config_t;

/** A completed V4L2 RGB565 camera buffer. It remains valid until release_frame is called. */
typedef struct {
    const uint8_t *data;
    uint32_t width;
    uint32_t height;
    uint32_t bytes_per_line;
    size_t length;
} camera_bridge_frame_t;

/** A JPEG bitstream allocated by the bridge. Release it with release_jpeg. */
typedef struct {
    const uint8_t *data;
    size_t length;
} camera_bridge_jpeg_t;

/** A decoded RGB888 JPEG image allocated by the bridge. Release it with release_image. */
typedef struct {
    const uint8_t *data;
    uint32_t width;
    uint32_t height;
    uint32_t bytes_per_line;
    size_t length;
} camera_bridge_image_t;

/** One COCO detection in the coordinate system of the RGB565 camera frame. */
typedef struct {
    int32_t left;
    int32_t top;
    int32_t right;
    int32_t bottom;
    int32_t category;
    float score;
} camera_bridge_detection_t;

esp_err_t camera_bridge_start(const camera_bridge_config_t *config);
esp_err_t camera_bridge_next_frame(camera_bridge_frame_t *frame);
esp_err_t camera_bridge_release_frame(void);
esp_err_t camera_bridge_encode_jpeg(const camera_bridge_frame_t *frame, camera_bridge_jpeg_t *jpeg);
void camera_bridge_release_jpeg(camera_bridge_jpeg_t *jpeg);
esp_err_t camera_bridge_decode_jpeg(const uint8_t *jpeg, size_t jpeg_length,
                                    camera_bridge_image_t *image);
void camera_bridge_release_image(camera_bridge_image_t *image);
esp_err_t camera_bridge_release_jpeg_decoder(void);

/** Create the lazy 320×320 COCO detector. The model loads on its first inference. */
esp_err_t camera_bridge_detector_start(void);
/** Detect up to `detection_capacity` objects in one camera frame. */
esp_err_t camera_bridge_detector_run(const camera_bridge_frame_t *frame,
                                     camera_bridge_detection_t *detections,
                                     size_t detection_capacity,
                                     size_t *detection_count);
/** Release the detector and its model/tensor allocations. */
void camera_bridge_detector_stop(void);
esp_err_t camera_bridge_stop(void);

#ifdef __cplusplus
}
#endif

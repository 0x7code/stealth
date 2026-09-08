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

esp_err_t camera_bridge_start(const camera_bridge_config_t *config);
esp_err_t camera_bridge_next_frame(camera_bridge_frame_t *frame);
esp_err_t camera_bridge_release_frame(void);
esp_err_t camera_bridge_stop(void);

#ifdef __cplusplus
}
#endif

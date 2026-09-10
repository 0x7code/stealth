#include "camera_bridge.h"

#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#include "esp_cam_sensor_xclk.h"
#include "esp_log.h"
#include "esp_video_device.h"
#include "esp_video_init.h"
#include "driver/jpeg_encode.h"
#include "linux/videodev2.h"

#define CAMERA_BUFFER_COUNT 2
static const char *TAG = "camera_bridge";
static jpeg_encoder_handle_t jpeg_encoder;

typedef struct {
    uint8_t *data;
    size_t length;
} capture_buffer_t;

static struct {
    int fd;
    capture_buffer_t buffers[CAMERA_BUFFER_COUNT];
    struct v4l2_buffer dequeued;
    uint32_t width;
    uint32_t height;
    uint32_t bytes_per_line;
    bool frame_is_dequeued;
    bool video_initialized;
    esp_cam_sensor_xclk_handle_t xclk;
} camera = {
    .fd = -1,
};

static void stop_xclk(void)
{
    if (camera.xclk != NULL) {
        esp_cam_sensor_xclk_stop(camera.xclk);
        esp_cam_sensor_xclk_free(camera.xclk);
        camera.xclk = NULL;
    }
}

static void stop_jpeg_encoder(void)
{
    if (jpeg_encoder != NULL) {
        jpeg_del_encoder_engine(jpeg_encoder);
        jpeg_encoder = NULL;
    }
}

static esp_err_t start_jpeg_encoder(void)
{
    if (jpeg_encoder != NULL) {
        return ESP_OK;
    }

    const jpeg_encode_engine_cfg_t config = {
        .timeout_ms = 500,
    };
    return jpeg_new_encoder_engine(&config, &jpeg_encoder);
}

static void close_capture(void)
{
    if (camera.fd < 0) {
        return;
    }

    if (camera.frame_is_dequeued) {
        ioctl(camera.fd, VIDIOC_QBUF, &camera.dequeued);
        camera.frame_is_dequeued = false;
    }

    int type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    ioctl(camera.fd, VIDIOC_STREAMOFF, &type);

    for (size_t i = 0; i < CAMERA_BUFFER_COUNT; ++i) {
        if (camera.buffers[i].data != NULL) {
            munmap(camera.buffers[i].data, camera.buffers[i].length);
            camera.buffers[i].data = NULL;
            camera.buffers[i].length = 0;
        }
    }

    close(camera.fd);
    camera.fd = -1;
    camera.width = 0;
    camera.height = 0;
    camera.bytes_per_line = 0;
}

static esp_err_t deinitialize_video(void)
{
    if (!camera.video_initialized) {
        return ESP_OK;
    }

    esp_err_t err = esp_video_deinit();
    if (err == ESP_OK) {
        camera.video_initialized = false;
    }
    return err;
}

static esp_err_t start_xclk(const camera_bridge_config_t *hardware)
{
    esp_err_t err = esp_cam_sensor_xclk_allocate(
        ESP_CAM_SENSOR_XCLK_ESP_CLOCK_ROUTER, &camera.xclk);
    if (err != ESP_OK) {
        return err;
    }

    const esp_cam_sensor_xclk_config_t xclk_config = {
        .esp_clock_router_cfg = {
            .xclk_pin = hardware->xclk_pin,
            .xclk_freq_hz = hardware->xclk_hz,
        },
    };
    err = esp_cam_sensor_xclk_start(camera.xclk, &xclk_config);
    if (err != ESP_OK) {
        stop_xclk();
    }
    return err;
}

static esp_err_t open_capture(void)
{
    camera.fd = open(ESP_VIDEO_MIPI_CSI_DEVICE_NAME, O_RDONLY);
    if (camera.fd < 0) {
        ESP_LOGE(TAG, "open %s failed", ESP_VIDEO_MIPI_CSI_DEVICE_NAME);
        return ESP_FAIL;
    }

    struct v4l2_format format = {
        .type = V4L2_BUF_TYPE_VIDEO_CAPTURE,
    };
    if (ioctl(camera.fd, VIDIOC_G_FMT, &format) != 0) {
        ESP_LOGE(TAG, "VIDIOC_G_FMT failed");
        return ESP_FAIL;
    }
    ESP_LOGI(TAG, "camera default: %" PRIu32 "x%" PRIu32 ", format=0x%08" PRIx32,
             format.fmt.pix.width, format.fmt.pix.height, format.fmt.pix.pixelformat);

    // ESP-Video selects RGB565 by default for the OV2710 + ISP. Do not send a redundant
    // S_FMT request: the driver only accepts format changes when they differ from its current
    // sensor/pipeline configuration.
    if (format.fmt.pix.pixelformat != V4L2_PIX_FMT_RGB565) {
        format.fmt.pix.pixelformat = V4L2_PIX_FMT_RGB565;
        if (ioctl(camera.fd, VIDIOC_S_FMT, &format) != 0) {
            ESP_LOGE(TAG, "VIDIOC_S_FMT(RGB565) failed");
            return ESP_FAIL;
        }
    }
    camera.width = format.fmt.pix.width;
    camera.height = format.fmt.pix.height;
    camera.bytes_per_line = format.fmt.pix.bytesperline;
    // ESP-Video leaves this optional V4L2 field as zero for its packed RGB565 output.
    // RGB565 is always two bytes per pixel, so derive the tightly packed stride ourselves.
    if (camera.bytes_per_line == 0) {
        if (camera.width > UINT32_MAX / 2) {
            return ESP_ERR_INVALID_SIZE;
        }
        camera.bytes_per_line = camera.width * 2;
    }

    struct v4l2_requestbuffers request = {
        .count = CAMERA_BUFFER_COUNT,
        .type = V4L2_BUF_TYPE_VIDEO_CAPTURE,
        .memory = V4L2_MEMORY_MMAP,
    };
    if (ioctl(camera.fd, VIDIOC_REQBUFS, &request) != 0 || request.count < CAMERA_BUFFER_COUNT) {
        ESP_LOGE(TAG, "VIDIOC_REQBUFS failed");
        return ESP_FAIL;
    }

    for (uint32_t index = 0; index < CAMERA_BUFFER_COUNT; ++index) {
        struct v4l2_buffer buffer = {
            .type = V4L2_BUF_TYPE_VIDEO_CAPTURE,
            .memory = V4L2_MEMORY_MMAP,
            .index = index,
        };
        if (ioctl(camera.fd, VIDIOC_QUERYBUF, &buffer) != 0) {
            ESP_LOGE(TAG, "VIDIOC_QUERYBUF failed");
            return ESP_FAIL;
        }

        camera.buffers[index].data = mmap(
            NULL, buffer.length, PROT_READ | PROT_WRITE, MAP_SHARED, camera.fd, buffer.m.offset);
        if (camera.buffers[index].data == NULL) {
            camera.buffers[index].data = NULL;
            ESP_LOGE(TAG, "mmap failed");
            return ESP_ERR_NO_MEM;
        }
        camera.buffers[index].length = buffer.length;

        if (ioctl(camera.fd, VIDIOC_QBUF, &buffer) != 0) {
            ESP_LOGE(TAG, "VIDIOC_QBUF failed");
            return ESP_FAIL;
        }
    }

    int type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    if (ioctl(camera.fd, VIDIOC_STREAMON, &type) != 0) {
        ESP_LOGE(TAG, "VIDIOC_STREAMON failed");
        return ESP_FAIL;
    }

    ESP_LOGI(TAG, "streaming %" PRIu32 "x%" PRIu32 " RGB565 frames (stride=%" PRIu32 ")",
             camera.width, camera.height, camera.bytes_per_line);
    return ESP_OK;
}

esp_err_t camera_bridge_start(const camera_bridge_config_t *hardware)
{
    if (hardware == NULL || camera.fd >= 0) {
        return ESP_ERR_INVALID_STATE;
    }

    esp_err_t err = start_xclk(hardware);
    if (err != ESP_OK) {
        return err;
    }

    const esp_video_init_csi_config_t csi = {
        .sccb_config = {
            .init_sccb = true,
            .i2c_config = {
                .port = hardware->sccb_i2c_port,
                .scl_pin = hardware->sccb_clock_pin,
                .sda_pin = hardware->sccb_data_pin,
            },
            .freq = 100000,
        },
        .reset_pin = hardware->reset_pin,
        .pwdn_pin = hardware->camera_enable_pin,
    };
    const esp_video_init_config_t video_config = {
        .csi = &csi,
    };

    err = esp_video_init(&video_config);
    if (err != ESP_OK) {
        stop_xclk();
        return err;
    }
    camera.video_initialized = true;

    err = open_capture();
    if (err != ESP_OK) {
        close_capture();
        esp_err_t deinit_err = deinitialize_video();
        stop_xclk();
        if (deinit_err != ESP_OK) {
            return deinit_err;
        }
    }
    return err;
}

esp_err_t camera_bridge_next_frame(camera_bridge_frame_t *frame)
{
    if (frame == NULL || camera.fd < 0 || camera.frame_is_dequeued) {
        return ESP_ERR_INVALID_STATE;
    }

    memset(&camera.dequeued, 0, sizeof(camera.dequeued));
    camera.dequeued.type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    camera.dequeued.memory = V4L2_MEMORY_MMAP;
    if (ioctl(camera.fd, VIDIOC_DQBUF, &camera.dequeued) != 0 ||
        camera.dequeued.index >= CAMERA_BUFFER_COUNT) {
        ESP_LOGE(TAG, "VIDIOC_DQBUF failed");
        return ESP_FAIL;
    }

    const capture_buffer_t *buffer = &camera.buffers[camera.dequeued.index];
    if (buffer->data == NULL || buffer->length == 0) {
        ioctl(camera.fd, VIDIOC_QBUF, &camera.dequeued);
        return ESP_ERR_INVALID_STATE;
    }

    size_t payload_length = camera.dequeued.bytesused;
    // ESP-Video's CSI driver reports zero bytesused for a complete MMAP frame.
    if (payload_length == 0) {
        payload_length = buffer->length;
    }
    if (payload_length > buffer->length) {
        ESP_LOGE(TAG, "camera payload exceeds its mapped buffer");
        ioctl(camera.fd, VIDIOC_QBUF, &camera.dequeued);
        return ESP_ERR_INVALID_SIZE;
    }

    *frame = (camera_bridge_frame_t) {
        .data = buffer->data,
        .width = camera.width,
        .height = camera.height,
        .bytes_per_line = camera.bytes_per_line,
        .length = payload_length,
    };
    camera.frame_is_dequeued = true;
    return ESP_OK;
}

esp_err_t camera_bridge_release_frame(void)
{
    if (camera.fd < 0 || !camera.frame_is_dequeued) {
        return ESP_ERR_INVALID_STATE;
    }

    if (ioctl(camera.fd, VIDIOC_QBUF, &camera.dequeued) != 0) {
        ESP_LOGE(TAG, "VIDIOC_QBUF after display failed");
        return ESP_FAIL;
    }
    camera.frame_is_dequeued = false;
    return ESP_OK;
}

esp_err_t camera_bridge_encode_jpeg(const camera_bridge_frame_t *frame, camera_bridge_jpeg_t *jpeg)
{
    if (frame == NULL || jpeg == NULL || frame->data == NULL || frame->width == 0 ||
        frame->height == 0 || frame->width > UINT32_MAX / 2 ||
        frame->bytes_per_line != frame->width * 2) {
        return ESP_ERR_INVALID_ARG;
    }

    const uint32_t row_bytes = frame->width * 2;
    if (frame->height > UINT32_MAX / row_bytes) {
        return ESP_ERR_INVALID_SIZE;
    }
    const uint32_t input_size = row_bytes * frame->height;
    if (frame->length < input_size) {
        return ESP_ERR_INVALID_SIZE;
    }

    esp_err_t err = start_jpeg_encoder();
    if (err != ESP_OK) {
        return err;
    }

    const jpeg_encode_memory_alloc_cfg_t allocation = {
        .buffer_direction = JPEG_ENC_ALLOC_OUTPUT_BUFFER,
    };
    size_t output_capacity = 0;
    uint8_t *output = jpeg_alloc_encoder_mem(input_size, &allocation, &output_capacity);
    if (output == NULL || output_capacity > UINT32_MAX) {
        free(output);
        return ESP_ERR_NO_MEM;
    }

    const jpeg_encode_cfg_t config = {
        .width = frame->width,
        .height = frame->height,
        .src_type = JPEG_ENCODE_IN_FORMAT_RGB565,
        .sub_sample = JPEG_DOWN_SAMPLING_YUV420,
        .image_quality = 85,
    };
    uint32_t output_size = 0;
    err = jpeg_encoder_process(jpeg_encoder, &config, frame->data, input_size, output,
                               output_capacity, &output_size);
    if (err != ESP_OK) {
        free(output);
        return err;
    }

    jpeg->data = output;
    jpeg->length = output_size;
    return ESP_OK;
}

void camera_bridge_release_jpeg(camera_bridge_jpeg_t *jpeg)
{
    if (jpeg != NULL) {
        free((void *)jpeg->data);
        jpeg->data = NULL;
        jpeg->length = 0;
    }
}

esp_err_t camera_bridge_stop(void)
{
    close_capture();
    stop_jpeg_encoder();
    esp_err_t err = deinitialize_video();
    stop_xclk();
    return err;
}

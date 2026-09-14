#include "camera_bridge.h"

#include <algorithm>
#include <limits>
#include <new>

#include "coco_detect.hpp"
#include "esp_log.h"

namespace {

COCODetect *detector = nullptr;
constexpr const char *TAG = "camera_bridge";

} // namespace

extern "C" esp_err_t camera_bridge_detector_start(void)
{
    if (detector != nullptr) {
        return ESP_OK;
    }

    // COCODetect is deliberately lazy: construction is cheap, while its first `run` loads the
    // 320×320 model from /sdcard/models/p4. This lets Photo mode retain all available PSRAM.
    detector = new (std::nothrow) COCODetect(COCODetect::YOLO11N_320_S8_V1, true);
    if (detector == nullptr) {
        return ESP_ERR_NO_MEM;
    }
    return ESP_OK;
}

extern "C" esp_err_t camera_bridge_detector_run(const camera_bridge_frame_t *frame,
                                                   camera_bridge_detection_t *detections,
                                                   size_t detection_capacity,
                                                   size_t *detection_count)
{
    if (detector == nullptr) {
        return ESP_ERR_INVALID_STATE;
    }
    if (frame == nullptr || frame->data == nullptr || detections == nullptr || detection_count == nullptr ||
        frame->width == 0 || frame->height == 0 || frame->width > UINT16_MAX || frame->height > UINT16_MAX) {
        return ESP_ERR_INVALID_ARG;
    }

    const dl::image::img_t image = {
        .data = const_cast<uint8_t *>(frame->data),
        .width = static_cast<uint16_t>(frame->width),
        .height = static_cast<uint16_t>(frame->height),
        .pix_type = dl::image::DL_IMAGE_PIX_TYPE_RGB565LE,
    };
    const auto &results = detector->run(image);
    const size_t count = std::min(results.size(), detection_capacity);
    auto result = results.begin();
    for (size_t index = 0; index < count; ++index, ++result) {
        // ESP-DL's YOLO postprocessor always returns the four coordinates documented by result_t.
        detections[index] = {
            .left = result->box[0],
            .top = result->box[1],
            .right = result->box[2],
            .bottom = result->box[3],
            .category = result->category,
            .score = result->score,
        };
    }
    *detection_count = count;
    ESP_LOGI(TAG, "ESP-DL found %u object(s)", static_cast<unsigned>(count));
    return ESP_OK;
}

extern "C" void camera_bridge_detector_stop(void)
{
    delete detector;
    detector = nullptr;
}

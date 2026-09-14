idf_component_register(
    SRCS "camera_bridge.c" "espdl_bridge.cpp"
    INCLUDE_DIRS "include"
    REQUIRES esp_video esp_cam_sensor esp_driver_jpeg coco_detect
)

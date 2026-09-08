idf_component_register(
    SRCS "camera_bridge.c"
    INCLUDE_DIRS "include"
    REQUIRES esp_video esp_cam_sensor
)

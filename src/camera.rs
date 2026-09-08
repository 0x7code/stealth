//! MIPI-CSI camera capture for the ESP32-P4X-EYE's OV2710 sensor.
//!
//! ESP-Video owns the PSRAM DMA buffers. [`CameraFrame`] temporarily borrows one completed
//! buffer and returns it to the driver when dropped, so callers never allocate a framebuffer.

use crate::board::CameraHardware;
use anyhow::Context;
use esp_idf_svc::sys;

/// A running OV2710 video stream.
pub struct Camera;

/// One completed camera buffer borrowed from ESP-Video.
pub struct CameraFrame<'camera> {
    raw: sys::camera_bridge_frame_t,
    _camera: &'camera mut Camera,
}

impl Camera {
    /// Start the P4X-EYE's OV2710 MIPI-CSI camera in RGB565 mode.
    pub fn start(hardware: CameraHardware) -> anyhow::Result<Self> {
        let config = sys::camera_bridge_config_t {
            sccb_i2c_port: hardware.sccb_i2c_port,
            sccb_clock_pin: hardware.sccb_clock_pin,
            sccb_data_pin: hardware.sccb_data_pin,
            camera_enable_pin: hardware.camera_enable_pin,
            reset_pin: hardware.reset_pin,
            xclk_pin: hardware.xclk_pin,
            xclk_hz: hardware.xclk_hz,
        };
        sys::EspError::convert(unsafe { sys::camera_bridge_start(&config) })
            .context("failed to initialize the OV2710 camera")?;

        Ok(Self)
    }

    /// Wait for a completed camera frame.
    ///
    /// The returned value must be dropped before requesting another frame. Its `Drop`
    /// implementation requeues the buffer, including when LCD drawing returns an error.
    pub fn next_frame(&mut self) -> anyhow::Result<CameraFrame<'_>> {
        let mut raw = sys::camera_bridge_frame_t::default();
        sys::EspError::convert(unsafe { sys::camera_bridge_next_frame(&mut raw) })
            .context("failed to dequeue camera frame")?;

        Ok(CameraFrame { raw, _camera: self })
    }
}

impl CameraFrame<'_> {
    /// Packed, little-endian RGB565 pixels owned by the camera driver.
    pub fn bytes(&self) -> &[u8] {
        // `camera_bridge_next_frame` validates the pointer and length before constructing this
        // object. The buffer stays queued out of the driver until this frame is dropped.
        unsafe { std::slice::from_raw_parts(self.raw.data.cast(), self.raw.length) }
    }

    pub fn width(&self) -> u32 {
        self.raw.width
    }

    pub fn height(&self) -> u32 {
        self.raw.height
    }

    pub fn stride(&self) -> u32 {
        self.raw.bytes_per_line
    }
}

impl Drop for CameraFrame<'_> {
    fn drop(&mut self) {
        // There is no useful recovery path here. Logging preserves the original LCD error,
        // while still making the driver failure visible.
        if let Err(error) = sys::EspError::convert(unsafe { sys::camera_bridge_release_frame() }) {
            log::error!("failed to return camera buffer: {error}");
        }
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        let _ = unsafe { sys::camera_bridge_stop() };
    }
}

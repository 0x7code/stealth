//! MIPI-CSI camera capture for the ESP32-P4X-EYE's OV2710 sensor.
//!
//! ESP-Video owns the PSRAM DMA buffers. [`CameraFrame`] temporarily borrows one completed
//! buffer and returns it to the driver when dropped, so callers never allocate a framebuffer.

use crate::board::CameraHardware;
use anyhow::Context;
use esp_idf_svc::sys;

const MAX_DETECTIONS: usize = 10;
/// Square input expected by the selected ESP-DL COCO model.
pub const DETECTION_INPUT_SIZE: u32 = 320;

/// A running OV2710 video stream.
pub struct Camera;

/// One completed camera buffer borrowed from ESP-Video.
pub struct CameraFrame<'camera> {
    raw: sys::camera_bridge_frame_t,
    _camera: &'camera mut Camera,
}

/// A hardware-encoded JPEG owned by the camera bridge.
pub struct EncodedJpeg {
    raw: sys::camera_bridge_jpeg_t,
}

/// A hardware-decoded RGB888 JPEG image owned by the camera bridge.
pub struct DecodedJpeg {
    raw: sys::camera_bridge_image_t,
}

/// An object found by the ESP-DL COCO detector.
#[derive(Clone, Copy, Debug)]
pub struct Detection {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub category: i32,
    pub score: f32,
}

/// A compact center crop prepared for one detector-worker request.
///
/// The owned 320×320 RGB565 buffer remains valid after its source camera frame has returned to
/// ESP-Video. This lets inference run without holding up the live preview's DMA buffers.
pub struct DetectionInput {
    pixels: Vec<u8>,
    crop_x: u32,
    crop_y: u32,
    crop_size: u32,
    source_width: u32,
    source_height: u32,
}

/// The lazily loaded ESP-DL COCO detector.
///
/// It owns the C++ detector singleton and releases its PSRAM allocations when dropped.
pub struct Detector;

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

    /// Make a 320×320 center crop for asynchronous detection.
    ///
    /// This uses the same square crop as the LCD preview. Sampling down before handing the data
    /// to the worker keeps a queued request to 200 KiB rather than retaining a 4 MiB camera DMA
    /// buffer for the duration of inference.
    pub fn detection_input(&self) -> anyhow::Result<DetectionInput> {
        let width = self.width();
        let height = self.height();
        let stride = self.stride();
        let minimum_stride = width
            .checked_mul(2)
            .ok_or_else(|| anyhow::anyhow!("camera width overflows RGB565 stride"))?;
        let required_bytes = usize::try_from(stride)
            .ok()
            .and_then(|stride| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| stride.checked_mul(height))
            })
            .ok_or_else(|| anyhow::anyhow!("camera frame dimensions overflow"))?;
        if width == 0
            || height == 0
            || stride < minimum_stride
            || self.bytes().len() < required_bytes
        {
            anyhow::bail!("camera returned an invalid RGB565 frame");
        }

        let crop_size = width.min(height);
        let crop_x = (width - crop_size) / 2;
        let crop_y = (height - crop_size) / 2;
        let stride = usize::try_from(stride).expect("validated camera stride fits usize");
        let mut pixels = Vec::with_capacity(
            usize::try_from(DETECTION_INPUT_SIZE * DETECTION_INPUT_SIZE * 2)
                .expect("detector input size fits usize"),
        );
        let source = self.bytes();

        for y in 0..DETECTION_INPUT_SIZE {
            let source_y = crop_y + y * crop_size / DETECTION_INPUT_SIZE;
            for x in 0..DETECTION_INPUT_SIZE {
                let source_x = crop_x + x * crop_size / DETECTION_INPUT_SIZE;
                let offset = usize::try_from(source_y).expect("source coordinate fits usize")
                    * stride
                    + usize::try_from(source_x).expect("source coordinate fits usize") * 2;
                pixels.extend_from_slice(&source[offset..offset + 2]);
            }
        }

        Ok(DetectionInput {
            pixels,
            crop_x,
            crop_y,
            crop_size,
            source_width: width,
            source_height: height,
        })
    }

    /// Compress this RGB565 frame with the ESP32-P4 JPEG hardware encoder.
    pub fn encode_jpeg(&self) -> anyhow::Result<EncodedJpeg> {
        let mut raw = sys::camera_bridge_jpeg_t::default();
        sys::EspError::convert(unsafe { sys::camera_bridge_encode_jpeg(&self.raw, &mut raw) })
            .context("failed to encode camera frame as JPEG")?;

        Ok(EncodedJpeg { raw })
    }
}

impl Detector {
    /// Prepare the 320×320 COCO detector for its first frame.
    ///
    /// The model itself is loaded on the first [`Self::detect`] call from the configured MicroSD
    /// path. This allows entering Photo mode without reserving the detector's tensor arena.
    pub fn start() -> anyhow::Result<Self> {
        sys::EspError::convert(unsafe { sys::camera_bridge_detector_start() })
            .context("failed to create ESP-DL COCO detector")?;
        Ok(Self)
    }

    /// Run object detection on a compact RGB565 crop.
    pub fn detect(&mut self, input: &DetectionInput) -> anyhow::Result<Vec<Detection>> {
        let frame = sys::camera_bridge_frame_t {
            data: input.pixels.as_ptr(),
            width: DETECTION_INPUT_SIZE,
            height: DETECTION_INPUT_SIZE,
            bytes_per_line: DETECTION_INPUT_SIZE * 2,
            length: input.pixels.len(),
        };
        let mut raw = [sys::camera_bridge_detection_t::default(); MAX_DETECTIONS];
        let mut count = 0;
        sys::EspError::convert(unsafe {
            sys::camera_bridge_detector_run(&frame, raw.as_mut_ptr(), raw.len(), &mut count)
        })
        .context("ESP-DL COCO inference failed")?;

        let count = count.min(raw.len());
        Ok(raw[..count]
            .iter()
            .map(|detection| Detection {
                left: detection.left,
                top: detection.top,
                right: detection.right,
                bottom: detection.bottom,
                category: detection.category,
                score: detection.score,
            })
            .map(|detection| input.to_source_coordinates(detection))
            .collect())
    }
}

impl DetectionInput {
    fn to_source_coordinates(&self, mut detection: Detection) -> Detection {
        detection.left = self.source_x(detection.left);
        detection.right = self.source_x(detection.right);
        detection.top = self.source_y(detection.top);
        detection.bottom = self.source_y(detection.bottom);
        detection
    }

    fn source_x(&self, coordinate: i32) -> i32 {
        scale_coordinate(coordinate, self.crop_x, self.crop_size, self.source_width)
    }

    fn source_y(&self, coordinate: i32) -> i32 {
        scale_coordinate(coordinate, self.crop_y, self.crop_size, self.source_height)
    }
}

fn scale_coordinate(coordinate: i32, offset: u32, scale: u32, limit: u32) -> i32 {
    let coordinate = i64::from(coordinate.clamp(0, i32::try_from(DETECTION_INPUT_SIZE).unwrap()));
    let scaled =
        i64::from(offset) + coordinate * i64::from(scale) / i64::from(DETECTION_INPUT_SIZE);
    scaled.clamp(0, i64::from(limit)).try_into().unwrap()
}

impl Detection {
    /// The standard COCO class label for this detection, when the model reported a valid class.
    pub fn label(&self) -> &'static str {
        COCO_LABELS
            .get(usize::try_from(self.category).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or("object")
    }
}

impl EncodedJpeg {
    pub fn bytes(&self) -> &[u8] {
        // The C bridge allocates and validates this buffer, which remains valid until Drop.
        unsafe { std::slice::from_raw_parts(self.raw.data.cast(), self.raw.length) }
    }
}

/// Decode a saved JPEG with the ESP32-P4 hardware codec for Gallery rendering.
pub fn decode_jpeg(jpeg: &[u8]) -> anyhow::Result<DecodedJpeg> {
    u32::try_from(jpeg.len()).map_err(|_| anyhow::anyhow!("JPEG is too large"))?;
    let mut raw = sys::camera_bridge_image_t::default();
    sys::EspError::convert(unsafe {
        sys::camera_bridge_decode_jpeg(jpeg.as_ptr(), jpeg.len(), &mut raw)
    })
    .context("failed to decode JPEG for Gallery")?;

    Ok(DecodedJpeg { raw })
}

/// Release the Gallery-only hardware decoder and its driver allocations.
///
/// This does not stop the camera stream or affect the hardware JPEG encoder used for captures.
pub fn release_gallery_decoder() -> anyhow::Result<()> {
    sys::EspError::convert(unsafe { sys::camera_bridge_release_jpeg_decoder() })
        .context("failed to release Gallery JPEG decoder")
}

impl DecodedJpeg {
    pub fn rgb888_bytes(&self) -> &[u8] {
        // The bridge allocates and validates this buffer, which remains valid until Drop.
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

impl Drop for EncodedJpeg {
    fn drop(&mut self) {
        unsafe { sys::camera_bridge_release_jpeg(&mut self.raw) };
    }
}

impl Drop for DecodedJpeg {
    fn drop(&mut self) {
        unsafe { sys::camera_bridge_release_image(&mut self.raw) };
    }
}

impl Drop for Detector {
    fn drop(&mut self) {
        unsafe { sys::camera_bridge_detector_stop() };
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

const COCO_LABELS: [&str; 80] = [
    "person",
    "bicycle",
    "car",
    "motorcycle",
    "airplane",
    "bus",
    "train",
    "truck",
    "boat",
    "traffic light",
    "fire hydrant",
    "stop sign",
    "parking meter",
    "bench",
    "bird",
    "cat",
    "dog",
    "horse",
    "sheep",
    "cow",
    "elephant",
    "bear",
    "zebra",
    "giraffe",
    "backpack",
    "umbrella",
    "handbag",
    "tie",
    "suitcase",
    "frisbee",
    "skis",
    "snowboard",
    "sports ball",
    "kite",
    "baseball bat",
    "baseball glove",
    "skateboard",
    "surfboard",
    "tennis racket",
    "bottle",
    "wine glass",
    "cup",
    "fork",
    "knife",
    "spoon",
    "bowl",
    "banana",
    "apple",
    "sandwich",
    "orange",
    "broccoli",
    "carrot",
    "hot dog",
    "pizza",
    "donut",
    "cake",
    "chair",
    "couch",
    "potted plant",
    "bed",
    "dining table",
    "toilet",
    "tv",
    "laptop",
    "mouse",
    "remote",
    "keyboard",
    "cell phone",
    "microwave",
    "oven",
    "toaster",
    "sink",
    "refrigerator",
    "book",
    "clock",
    "vase",
    "scissors",
    "teddy bear",
    "hair drier",
    "toothbrush",
];

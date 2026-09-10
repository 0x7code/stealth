//! ESP32-P4X-EYE board wiring.
//!
//! This is the one place that maps named hardware features to ESP32-P4 peripherals and GPIOs.

use crate::{
    buttons::ButtonsHardware, display::DisplayHardware, rotary::RotaryHardware,
    sd_card::SdCardHardware,
};
use esp_idf_hal::peripherals::Peripherals;

/// The P4X-EYE hardware resources divided by feature.
pub struct P4xEye {
    display: DisplayHardware,
    sd_card: SdCardHardware,
    camera: CameraHardware,
    buttons: ButtonsHardware,
    rotary: RotaryHardware,
}

/// P4X-EYE camera wiring consumed by the ESP-Video bridge.
///
/// These are numeric settings rather than `PinDriver`s because ESP-Video configures the MIPI
/// controller, SCCB bus, XCLK generator, reset, and shared camera-enable line itself.
pub struct CameraHardware {
    pub(crate) sccb_i2c_port: i32,
    pub(crate) sccb_clock_pin: i32,
    pub(crate) sccb_data_pin: i32,
    pub(crate) camera_enable_pin: i32,
    pub(crate) reset_pin: i32,
    pub(crate) xclk_pin: i32,
    pub(crate) xclk_hz: u32,
}

impl P4xEye {
    /// Claim the ESP32-P4 peripherals and assign them to the P4X-EYE features they serve.
    pub fn take() -> anyhow::Result<Self> {
        let peripherals = Peripherals::take()?;
        let pins = peripherals.pins;

        Ok(Self {
            display: DisplayHardware {
                spi2: peripherals.spi2,
                lcd_enable_pin: pins.gpio12,
                backlight_pin: pins.gpio20,
                reset_pin: pins.gpio15,
                dc_pin: pins.gpio19,
                clock_pin: pins.gpio17,
                mosi_pin: pins.gpio16,
                chip_select_pin: pins.gpio18,
            },
            sd_card: SdCardHardware {
                host: peripherals.sdmmc0,
                ldo4: peripherals.ldo4,
                card_enable_pin: pins.gpio46,
            },
            camera: CameraHardware {
                sccb_i2c_port: 0,
                sccb_clock_pin: 13,
                sccb_data_pin: 14,
                // GPIO12 is the board's camera-enable line. The display initializes it high
                // before this driver briefly pulses it as part of camera power-on.
                camera_enable_pin: 12,
                reset_pin: 26,
                xclk_pin: 11,
                xclk_hz: 24_000_000,
            },
            buttons: ButtonsHardware {
                previous_pin: pins.gpio4,
                next_pin: pins.gpio5,
                enter_pin: pins.gpio3,
            },
            rotary: RotaryHardware {
                switch_pin: pins.gpio2,
                phase_a_pin: pins.gpio48,
                phase_b_pin: pins.gpio47,
            },
        })
    }

    /// Split the board resources so each feature can own its hardware for its full lifetime.
    pub fn into_parts(
        self,
    ) -> (
        DisplayHardware,
        SdCardHardware,
        CameraHardware,
        ButtonsHardware,
        RotaryHardware,
    ) {
        (
            self.display,
            self.sd_card,
            self.camera,
            self.buttons,
            self.rotary,
        )
    }
}

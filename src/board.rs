//! ESP32-P4X-EYE board wiring.
//!
//! This is the one place that maps named hardware features to ESP32-P4 peripherals and GPIOs.

use crate::{display::DisplayHardware, sd_card::SdCardHardware};
use esp_idf_hal::peripherals::Peripherals;

/// The P4X-EYE hardware resources divided by feature.
pub struct P4xEye {
    display: DisplayHardware,
    sd_card: SdCardHardware,
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
        })
    }

    /// Split the board resources so each feature can own its hardware for its full lifetime.
    pub fn into_parts(self) -> (DisplayHardware, SdCardHardware) {
        (self.display, self.sd_card)
    }
}

use esp_idf_hal::gpio::{Gpio3, Gpio4, Gpio5, Input, PinDriver, Pull};

/// P4X-EYE GPIOs used by the rear navigation buttons.
///
/// `board::P4xEye` creates this so it remains the sole owner of `Peripherals`.
pub struct ButtonsHardware {
    pub(crate) previous_pin: Gpio4<'static>,
    pub(crate) next_pin: Gpio5<'static>,
    pub(crate) enter_pin: Gpio3<'static>,
}

/// The three navigation buttons on the rear of the P4X-EYE.
#[derive(Clone, Copy, Debug)]
pub enum Button {
    Previous,
    Next,
    Enter,
}

impl Button {
    fn from_index(index: usize) -> Self {
        match index {
            0 => Self::Previous,
            1 => Self::Next,
            2 => Self::Enter,
            _ => unreachable!("invalid button index"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Previous => "previous",
            Self::Next => "next",
            Self::Enter => "enter",
        }
    }
}

/// Polling driver with press-edge detection and release debouncing.
pub struct Buttons {
    pins: [PinDriver<'static, Input>; 3],
    pressed: [bool; 3],
    released_samples: [u8; 3],
}

impl Buttons {
    // With the 20 ms polling interval in main, a button must stay released for about 60 ms
    // before another press can register. This filters switch bounce but also merges faster taps.
    const RELEASE_SAMPLES: u8 = 3;

    /// Configure the P4X-EYE's rear navigation buttons.
    ///
    /// The board maps previous, next, and enter to GPIO4, GPIO5, and GPIO3 respectively.
    pub fn new(hardware: ButtonsHardware) -> anyhow::Result<Self> {
        Ok(Self {
            pins: [
                PinDriver::input(hardware.previous_pin, Pull::Up)?,
                PinDriver::input(hardware.next_pin, Pull::Up)?,
                PinDriver::input(hardware.enter_pin, Pull::Up)?,
            ],
            pressed: [false; 3],
            released_samples: [0; 3],
        })
    }

    /// Return a button once for each press. Call this regularly (the main loop uses 20 ms).
    pub fn pressed(&mut self) -> Option<Button> {
        for index in 0..self.pins.len() {
            if self.pins[index].is_low() {
                self.released_samples[index] = 0;
                if !self.pressed[index] {
                    self.pressed[index] = true;
                    return Some(Button::from_index(index));
                }
            } else if self.pressed[index] {
                self.released_samples[index] += 1;
                if self.released_samples[index] >= Self::RELEASE_SAMPLES {
                    self.pressed[index] = false;
                }
            }
        }

        None
    }
}

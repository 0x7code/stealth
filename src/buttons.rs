use esp_idf_hal::gpio::{Input, InputPin, PinDriver, Pull};

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
    const RELEASE_SAMPLES: u8 = 3;

    /// Configure the P4X-EYE's rear navigation buttons.
    ///
    /// The board maps previous, next, and enter to GPIO4, GPIO5, and GPIO3 respectively.
    pub fn new(
        previous: impl InputPin + 'static,
        next: impl InputPin + 'static,
        enter: impl InputPin + 'static,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            pins: [
                PinDriver::input(previous, Pull::Up)?,
                PinDriver::input(next, Pull::Up)?,
                PinDriver::input(enter, Pull::Up)?,
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

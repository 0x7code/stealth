use std::sync::mpsc::{self, Receiver, TrySendError};

use anyhow::Context;
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{Gpio2, Gpio47, Gpio48, Input, PinDriver, Pull};

const POLL_INTERVAL_MS: u32 = 2;
const RELEASE_SAMPLES: u8 = 3;

/// P4X-EYE GPIOs used by the rear rotary encoder.
pub struct RotaryHardware {
    pub(crate) switch_pin: Gpio2<'static>,
    pub(crate) phase_a_pin: Gpio48<'static>,
    pub(crate) phase_b_pin: Gpio47<'static>,
}

/// One user action from the rotary encoder.
#[derive(Clone, Copy, Debug)]
pub enum RotaryEvent {
    Pressed,
    Clockwise,
    CounterClockwise,
}

/// A queue of rotary events sampled independently of the camera and LCD work.
pub struct RotaryEvents {
    receiver: Receiver<RotaryEvent>,
}

struct Rotary {
    switch: PinDriver<'static, Input>,
    phase_a: PinDriver<'static, Input>,
    phase_b: PinDriver<'static, Input>,
    previous_state: u8,
    transition_count: i8,
    switch_pressed: bool,
    released_samples: u8,
}

impl RotaryEvents {
    /// Configure the encoder and begin sampling it at a fixed 2 ms interval.
    pub fn start(hardware: RotaryHardware) -> anyhow::Result<Self> {
        let mut rotary = Rotary::new(hardware)?;
        let (sender, receiver) = mpsc::sync_channel(8);

        std::thread::Builder::new()
            .name("rotary".into())
            .stack_size(4096)
            .spawn(move || loop {
                if let Some(event) = rotary.poll() {
                    match sender.try_send(event) {
                        Ok(()) | Err(TrySendError::Full(_)) => {}
                        Err(TrySendError::Disconnected(_)) => break,
                    }
                }
                // Unlike std::thread::sleep on this target, FreeRTOS yields the CPU to the
                // idle task instead of busy-waiting, which keeps the task watchdog fed.
                FreeRtos::delay_ms(POLL_INTERVAL_MS);
            })
            .context("failed to start rotary encoder task")?;

        Ok(Self { receiver })
    }

    /// Return the next queued press or turn, if any.
    pub fn poll(&self) -> Option<RotaryEvent> {
        self.receiver.try_recv().ok()
    }
}

impl Rotary {
    fn new(hardware: RotaryHardware) -> anyhow::Result<Self> {
        let switch = PinDriver::input(hardware.switch_pin, Pull::Up)?;
        let phase_a = PinDriver::input(hardware.phase_a_pin, Pull::Up)?;
        let phase_b = PinDriver::input(hardware.phase_b_pin, Pull::Up)?;
        let previous_state = pin_state(&phase_a, &phase_b);

        Ok(Self {
            switch,
            phase_a,
            phase_b,
            previous_state,
            transition_count: 0,
            switch_pressed: false,
            released_samples: 0,
        })
    }

    fn poll(&mut self) -> Option<RotaryEvent> {
        if self.switch.is_low() {
            self.released_samples = 0;
            if !self.switch_pressed {
                self.switch_pressed = true;
                return Some(RotaryEvent::Pressed);
            }
        } else if self.switch_pressed {
            self.released_samples += 1;
            if self.released_samples >= RELEASE_SAMPLES {
                self.switch_pressed = false;
            }
        }

        let state = pin_state(&self.phase_a, &self.phase_b);
        let transition = (self.previous_state << 2) | state;
        self.previous_state = state;
        self.transition_count += match transition {
            0b0001 | 0b0111 | 0b1110 | 0b1000 => 1,
            0b0010 | 0b0100 | 0b1101 | 0b1011 => -1,
            _ => 0,
        };

        if self.transition_count >= 4 {
            self.transition_count = 0;
            Some(RotaryEvent::Clockwise)
        } else if self.transition_count <= -4 {
            self.transition_count = 0;
            Some(RotaryEvent::CounterClockwise)
        } else {
            None
        }
    }
}

fn pin_state(phase_a: &PinDriver<'static, Input>, phase_b: &PinDriver<'static, Input>) -> u8 {
    (u8::from(phase_a.is_high()) << 1) | u8::from(phase_b.is_high())
}

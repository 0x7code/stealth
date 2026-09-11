//! Application startup and the camera live-view loop.

use std::time::Instant;

use crate::{
    board, buttons,
    camera::{self, CameraFrame},
    display,
    live_view::{ControlEvent, LiveView, LiveViewAction},
    rotary, sd_card,
};

/// The running application and the hardware it owns for its full lifetime.
pub struct App {
    display: display::Display,
    buttons: buttons::Buttons,
    rotary: rotary::RotaryEvents,
    sd_card: sd_card::SdCard,
    camera: camera::Camera,
    live_view: LiveView,
}

impl App {
    /// Initialize the P4X-EYE peripherals and start the camera preview.
    pub fn boot() -> anyhow::Result<Self> {
        let (
            display_hardware,
            sd_card_hardware,
            camera_hardware,
            buttons_hardware,
            rotary_hardware,
        ) = board::P4xEye::take()?.into_parts();

        log::info!("starting P4X-EYE display");
        let mut buttons = buttons::Buttons::new(buttons_hardware)?;
        let rotary = rotary::RotaryEvents::start(rotary_hardware)?;
        let mut display = display::Display::init(display_hardware)?;
        display.run_boot_animation()?;

        let mut sd_card = sd_card::SdCard::new(sd_card_hardware)?;
        initialize_sd_card(&mut sd_card);

        display.show_message("starting camera")?;
        let camera = match camera::Camera::start(camera_hardware) {
            Ok(camera) => camera,
            Err(error) => show_error_forever(&mut display, &mut buttons, "camera error", error),
        };
        log::info!("camera streaming to the LCD");

        Ok(Self {
            display,
            buttons,
            rotary,
            sd_card,
            camera,
            live_view: LiveView::new(),
        })
    }

    /// Stream camera frames to the LCD and react to user input forever.
    pub fn run(mut self) -> ! {
        let Self {
            display,
            buttons,
            rotary,
            sd_card,
            camera,
            live_view,
        } = &mut self;

        loop {
            // Keep the frame borrow local: it must be returned to ESP-Video before requesting
            // the next frame, while the other fields remain available for capture and drawing.
            let frame = match camera.next_frame() {
                Ok(frame) => frame,
                Err(error) => show_error_forever(display, buttons, "capture error", error),
            };

            let now = Instant::now();
            if let Some(event) = poll_control(buttons, rotary) {
                handle_control(live_view, sd_card, event, &frame, now);
            }

            let zoom = live_view.zoom(now);
            let confirmation = live_view.confirmation(now);
            let draw_result = display.draw_rgb565_scaled(
                frame.bytes(),
                frame.width(),
                frame.height(),
                frame.stride(),
                zoom,
                confirmation,
            );

            // Return the DMA buffer before handling an error that may keep this task alive.
            drop(frame);
            if let Err(error) = draw_result {
                show_error_forever(display, buttons, "frame error", error);
            }
        }
    }
}

fn initialize_sd_card(sd_card: &mut sd_card::SdCard) {
    match sd_card.mount() {
        Ok(()) => match sd_card
            .append_log("stealth boot complete")
            .and_then(|()| sd_card.read_log())
        {
            Ok(log) => log::info!(
                "MicroSD mounted; wrote and read /sdcard/stealth.log ({} bytes)",
                log.len()
            ),
            Err(error) => log::warn!("MicroSD mounted but log verification failed: {error:#}"),
        },
        Err(error) => {
            log::warn!("MicroSD unavailable at boot; insert one and press capture: {error:#}");
        }
    }
}

fn poll_control(
    buttons: &mut buttons::Buttons,
    rotary: &rotary::RotaryEvents,
) -> Option<ControlEvent> {
    let button = buttons.pressed();
    let rotary_event = rotary.poll();

    if matches!(button, Some(buttons::Button::Enter))
        || matches!(rotary_event, Some(rotary::RotaryEvent::Pressed))
    {
        return Some(ControlEvent::Capture);
    }

    match (button, rotary_event) {
        (Some(buttons::Button::Previous), _) => Some(ControlEvent::Previous),
        (Some(buttons::Button::Next), _) => Some(ControlEvent::Next),
        (_, Some(rotary::RotaryEvent::Clockwise)) => Some(ControlEvent::ZoomIn),
        (_, Some(rotary::RotaryEvent::CounterClockwise)) => Some(ControlEvent::ZoomOut),
        _ => None,
    }
}

fn handle_control(
    live_view: &mut LiveView,
    sd_card: &mut sd_card::SdCard,
    event: ControlEvent,
    frame: &CameraFrame<'_>,
    now: Instant,
) {
    log::info!("control: {event:?}");
    if live_view.handle(event, now) == LiveViewAction::Capture {
        live_view.show_confirmation(capture_frame(sd_card, frame), now);
    }
}

fn capture_frame(sd_card: &mut sd_card::SdCard, frame: &CameraFrame<'_>) -> &'static str {
    match sd_card.mount() {
        Err(error) => {
            log::warn!("cannot capture: MicroSD is unavailable: {error:#}");
            "no sd card"
        }
        Ok(()) => match frame
            .encode_jpeg()
            .and_then(|jpeg| sd_card.save_jpeg(jpeg.bytes()))
        {
            Ok(path) => {
                log::info!("saved camera frame to {path}");
                "saved"
            }
            Err(error) => {
                log::error!("failed to save camera frame: {error:#}");
                "save error"
            }
        },
    }
}

/// Log an unrecoverable runtime failure and leave it visible without rebooting the board.
fn show_error_forever(
    display: &mut display::Display,
    buttons: &mut buttons::Buttons,
    message: &str,
    error: anyhow::Error,
) -> ! {
    log::error!("{message}: {error:#}");
    if let Err(display_error) = display.show_message(message) {
        log::error!("failed to show {message} on the LCD: {display_error:#}");
    }

    loop {
        if let Some(button) = buttons.pressed() {
            log::info!("button: {}", button.label());
            display
                .show_message(button.label())
                .expect("failed to show button confirmation");
        }

        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

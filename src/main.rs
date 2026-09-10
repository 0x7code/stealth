mod board;
mod buttons;
mod camera;
mod display;
mod rotary;
mod sd_card;

const BUTTON_CONFIRMATION_DURATION: std::time::Duration = std::time::Duration::from_millis(500);
const FOCUS_ASSIST_DURATION: std::time::Duration = std::time::Duration::from_secs(3);
const MIN_ZOOM: u8 = 1;
const MAX_ZOOM: u8 = 3;

fn main() {
    // It is necessary to call this function once. Otherwise, some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    let (display_hardware, sd_card_hardware, camera_hardware, buttons_hardware, rotary_hardware) =
        board::P4xEye::take()
            .expect("failed to acquire P4X-EYE peripherals")
            .into_parts();

    log::info!("starting P4X-EYE display");
    let mut buttons =
        buttons::Buttons::new(buttons_hardware).expect("button initialization failed");
    let rotary =
        rotary::RotaryEvents::start(rotary_hardware).expect("rotary encoder initialization failed");
    let mut display =
        display::Display::init(display_hardware).expect("display initialization failed");
    display
        .run_boot_animation()
        .expect("display boot animation failed");

    let mut sd_card =
        sd_card::SdCard::new(sd_card_hardware).expect("failed to initialize MicroSD controller");
    match sd_card.mount() {
        Ok(()) => {
            sd_card
                .append_log("stealth boot complete")
                .expect("failed to write SD card log");
            let log_bytes = sd_card
                .read_log()
                .expect("failed to read SD card log back")
                .len();
            log::info!("MicroSD mounted; wrote and read /sdcard/stealth.log ({log_bytes} bytes)");
        }
        Err(error) => {
            log::warn!("MicroSD unavailable at boot; insert one and press capture: {error:#}");
        }
    }

    display
        .show_message("starting camera")
        .expect("failed to show camera status");
    let mut camera = match camera::Camera::start(camera_hardware) {
        Ok(camera) => camera,
        Err(error) => show_error_forever(&mut display, &mut buttons, "camera error", error),
    };
    log::info!("camera streaming to the LCD");

    // Keep all hardware owners in scope. A camera frame borrows a driver-owned DMA buffer
    // only until it has been sent to the LCD, then it is immediately returned to the camera.
    let mut button_confirmation = None;
    let mut zoom = MIN_ZOOM;
    let mut focus_assist_until = None;
    loop {
        let frame = match camera.next_frame() {
            Ok(frame) => frame,
            Err(error) => show_error_forever(&mut display, &mut buttons, "capture error", error),
        };
        let button = buttons.pressed();
        let rotary_event = rotary.poll();
        let capture_requested = matches!(button, Some(buttons::Button::Enter))
            || matches!(rotary_event, Some(rotary::RotaryEvent::Pressed));
        let button_message = if capture_requested {
            match sd_card.mount() {
                Err(error) => {
                    log::warn!("cannot capture: MicroSD is unavailable: {error:#}");
                    Some("no sd card")
                }
                Ok(()) => match frame
                    .encode_jpeg()
                    .and_then(|jpeg| sd_card.save_jpeg(jpeg.bytes()))
                {
                    Ok(path) => {
                        log::info!("saved camera frame to {path}");
                        Some("saved")
                    }
                    Err(error) => {
                        log::error!("failed to save camera frame: {error:#}");
                        Some("save error")
                    }
                },
            }
        } else if let Some(button) = button {
            {
                log::info!("button: {}", button.label());
                Some(button.label())
            }
        } else {
            match rotary_event {
                Some(rotary::RotaryEvent::Clockwise) => {
                    zoom = (zoom + 1).min(MAX_ZOOM);
                    focus_assist_until = (zoom > MIN_ZOOM)
                        .then(|| std::time::Instant::now() + FOCUS_ASSIST_DURATION);
                    Some(zoom_label(zoom))
                }
                Some(rotary::RotaryEvent::CounterClockwise) => {
                    zoom = zoom.saturating_sub(1).max(MIN_ZOOM);
                    focus_assist_until = (zoom > MIN_ZOOM)
                        .then(|| std::time::Instant::now() + FOCUS_ASSIST_DURATION);
                    Some(zoom_label(zoom))
                }
                Some(rotary::RotaryEvent::Pressed) | None => None,
            }
        };
        if let Some(message) = button_message {
            button_confirmation = Some((message, std::time::Instant::now()));
        }
        let confirmation = match button_confirmation {
            Some((message, started)) if started.elapsed() < BUTTON_CONFIRMATION_DURATION => {
                Some(message)
            }
            Some(_) => {
                button_confirmation = None;
                None
            }
            None => None,
        };
        let active_zoom = match focus_assist_until {
            Some(until) if std::time::Instant::now() < until => zoom,
            Some(_) => {
                zoom = MIN_ZOOM;
                focus_assist_until = None;
                MIN_ZOOM
            }
            None => MIN_ZOOM,
        };
        let draw_result = display.draw_rgb565_scaled(
            frame.bytes(),
            frame.width(),
            frame.height(),
            frame.stride(),
            active_zoom,
            confirmation,
        );
        // Return the DMA buffer before handling an error that may keep this task alive forever.
        drop(frame);
        if let Err(error) = draw_result {
            show_error_forever(&mut display, &mut buttons, "frame error", error);
        }
    }
}

fn zoom_label(zoom: u8) -> &'static str {
    match zoom {
        1 => "focus 1x",
        2 => "focus 2x",
        3 => "focus 3x",
        _ => unreachable!("zoom is clamped to its supported range"),
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

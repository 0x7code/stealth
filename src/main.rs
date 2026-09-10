mod board;
mod camera;
mod display;
mod sd_card;

fn main() {
    // It is necessary to call this function once. Otherwise, some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    let (display_hardware, sd_card_hardware, camera_hardware) = board::P4xEye::take()
        .expect("failed to acquire P4X-EYE peripherals")
        .into_parts();

    log::info!("starting P4X-EYE display");
    let mut display =
        display::Display::init(display_hardware).expect("display initialization failed");
    display
        .run_boot_animation()
        .expect("display boot animation failed");

    let _sd_card = match sd_card::SdCard::mount(sd_card_hardware) {
        Ok(card) => {
            card.append_log("stealth boot complete")
                .expect("failed to write SD card log");
            let log_bytes = card
                .read_log()
                .expect("failed to read SD card log back")
                .len();
            log::info!("MicroSD mounted; wrote and read /sdcard/stealth.log ({log_bytes} bytes)");
            Some(card)
        }
        Err(error) => {
            log::warn!("MicroSD unavailable: {error:#}");
            None
        }
    };

    display
        .show_message("starting camera")
        .expect("failed to show camera status");
    let mut camera = match camera::Camera::start(camera_hardware) {
        Ok(camera) => camera,
        Err(error) => show_error_forever(&mut display, "camera error", error),
    };
    log::info!("camera streaming to the LCD");

    // Keep all hardware owners in scope. A camera frame borrows a driver-owned DMA buffer
    // only until it has been sent to the LCD, then it is immediately returned to the camera.
    loop {
        let frame = match camera.next_frame() {
            Ok(frame) => frame,
            Err(error) => show_error_forever(&mut display, "capture error", error),
        };
        let draw_result = display.draw_rgb565_scaled(
            frame.bytes(),
            frame.width(),
            frame.height(),
            frame.stride(),
        );
        // Return the DMA buffer before handling an error that may keep this task alive forever.
        drop(frame);
        if let Err(error) = draw_result {
            show_error_forever(&mut display, "frame error", error);
        }
    }
}

/// Log an unrecoverable runtime failure and leave it visible without rebooting the board.
fn show_error_forever(display: &mut display::Display, message: &str, error: anyhow::Error) -> ! {
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

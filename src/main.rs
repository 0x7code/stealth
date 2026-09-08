mod board;
mod display;
mod sd_card;

fn main() {
    // It is necessary to call this function once. Otherwise, some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    let (display_hardware, sd_card_hardware) = board::P4xEye::take()
        .expect("failed to acquire P4X-EYE peripherals")
        .into_parts();

    log::info!("starting P4X-EYE display");
    let mut display =
        display::Display::init(display_hardware).expect("display initialization failed");
    display
        .run_boot_animation()
        .expect("display boot animation failed");

    let sd_card = match sd_card::SdCard::mount(sd_card_hardware) {
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

    // Keep both drivers in scope: they own the display and MicroSD hardware resources.
    loop {
        let _ = &sd_card;
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

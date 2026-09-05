mod display;

fn main() {
    // It is necessary to call this function once. Otherwise, some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("starting P4X-EYE display");
    let mut display = display::Display::init().expect("display initialization failed");
    display.run_color_test().expect("display color test failed");

    // Keep `display` in scope: it owns the LCD power, backlight, SPI, and controller drivers.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

mod buttons;
mod display;

fn main() {
    // It is necessary to call this function once. Otherwise, some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("starting P4X-EYE display");
    let peripherals =
        esp_idf_hal::peripherals::Peripherals::take().expect("failed to take ESP32-P4 peripherals");
    let mut buttons = buttons::Buttons::new(
        peripherals.pins.gpio4,
        peripherals.pins.gpio5,
        peripherals.pins.gpio3,
    )
    .expect("button initialization failed");
    let mut display = display::Display::init(
        peripherals.spi2,
        peripherals.pins.gpio12,
        peripherals.pins.gpio20,
        peripherals.pins.gpio15,
        peripherals.pins.gpio19,
        peripherals.pins.gpio17,
        peripherals.pins.gpio16,
        peripherals.pins.gpio18,
    )
    .expect("display initialization failed");
    display.run_color_test().expect("display color test failed");

    // Keep `display` in scope: it owns the LCD power, backlight, SPI, and controller drivers.
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

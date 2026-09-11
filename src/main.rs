mod app;
mod board;
mod buttons;
mod camera;
mod display;
mod live_view;
mod rotary;
mod sd_card;

fn main() {
    // It is necessary to call this function once. Otherwise, some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    app::App::boot()
        .expect("failed to initialize P4X-EYE")
        .run();
}

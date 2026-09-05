use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};
use esp_idf_hal::{
    delay::{Ets, FreeRtos},
    gpio::PinDriver,
    peripherals::Peripherals,
    spi::{config::Config as SpiConfig, SpiDeviceDriver, SpiDriverConfig},
    units::FromValueType,
};
use mipidsi::{
    interface::SpiInterface,
    models::ST7789,
    options::{ColorInversion, ColorOrder},
    Builder,
};

const LCD_WIDTH: u16 = 240;
const LCD_HEIGHT: u16 = 240;

/// Initialize the P4X-EYE ST7789 LCD and show red, green, then blue with a message.
///
/// The final blue frame with "ready" stays on-screen while the firmware runs.
pub fn run_color_test() -> anyhow::Result<()> {
    let peripherals = Peripherals::take()?;

    // GPIO12 supplies the LCD power path. Keep this driver alive until all frames are sent.
    let mut lcd_enable = PinDriver::output(peripherals.pins.gpio12)?;
    lcd_enable.set_high()?;

    // The board's LCD backlight is active-low.
    let mut backlight = PinDriver::output(peripherals.pins.gpio20)?;
    backlight.set_low()?;

    let reset = PinDriver::output(peripherals.pins.gpio15)?;
    let dc = PinDriver::output(peripherals.pins.gpio19)?;

    let spi_config = SpiConfig::new().baudrate(40.MHz().into());
    let spi = SpiDeviceDriver::new_single(
        peripherals.spi2,
        peripherals.pins.gpio17,
        peripherals.pins.gpio16,
        None::<esp_idf_hal::gpio::AnyInputPin>,
        Some(peripherals.pins.gpio18),
        &SpiDriverConfig::new(),
        &spi_config,
    )?;

    // This is an SPI write buffer, not a framebuffer. Full-screen fills are streamed in chunks.
    let mut transfer_buffer = [0_u8; 512];
    let interface = SpiInterface::new(spi, dc, &mut transfer_buffer);
    let mut delay = Ets;
    let mut display = Builder::new(ST7789, interface)
        .reset_pin(reset)
        .display_size(LCD_WIDTH, LCD_HEIGHT)
        .color_order(ColorOrder::Rgb)
        .invert_colors(ColorInversion::Inverted)
        .init(&mut delay)
        .map_err(|err| anyhow::anyhow!("ST7789 initialization failed: {err:?}"))?;

    for (name, color) in [
        ("red", Rgb565::RED),
        ("green", Rgb565::GREEN),
        ("blue", Rgb565::BLUE),
    ] {
        log::info!("display: {name}");
        display
            .clear(color)
            .map_err(|err| anyhow::anyhow!("failed to draw {name}: {err:?}"))?;
        FreeRtos::delay_ms(1_000);
    }

    let text_style = TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Middle)
        .build();
    Text::with_text_style(
        "ready",
        Point::new(i32::from(LCD_WIDTH) / 2, i32::from(LCD_HEIGHT) / 2),
        MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
        text_style,
    )
    .draw(&mut display)
    .map_err(|err| anyhow::anyhow!("failed to draw message: {err:?}"))?;

    log::info!("display: ready");

    // Keep the LCD power, backlight, reset, DC, and SPI drivers alive. Dropping these objects
    // returns their pins to the default state, which turns the screen off on this board.
    loop {
        FreeRtos::delay_ms(1_000);
    }
}

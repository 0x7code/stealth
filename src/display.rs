use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::Rectangle,
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};
use esp_idf_hal::{
    delay::{Ets, FreeRtos},
    gpio::{Output, PinDriver},
    peripherals::Peripherals,
    spi::{config::Config as SpiConfig, SpiDeviceDriver, SpiDriver, SpiDriverConfig},
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
const BOOT_ANIMATION_FRAMES: u32 = 20;
const BOOT_ANIMATION_DURATION: std::time::Duration = std::time::Duration::from_secs(2);

// Eight fixed segments form a ring around the boot title. They are stored in flash, not RAM.
const SPINNER_SEGMENTS: [(i32, i32, u32, u32); 8] = [
    (108, 24, 24, 44),
    (164, 48, 44, 24),
    (184, 108, 32, 24),
    (164, 168, 44, 24),
    (108, 172, 24, 44),
    (32, 168, 44, 24),
    (24, 108, 32, 24),
    (32, 48, 44, 24),
];

const SPINNER_COLORS: [Rgb565; 6] = [
    Rgb565::RED,
    Rgb565::YELLOW,
    Rgb565::GREEN,
    Rgb565::CYAN,
    Rgb565::BLUE,
    Rgb565::MAGENTA,
];

type St7789Interface =
    SpiInterface<'static, SpiDeviceDriver<'static, SpiDriver<'static>>, PinDriver<'static, Output>>;
type St7789Panel = mipidsi::Display<St7789Interface, ST7789, PinDriver<'static, Output>>;

/// The initialized P4X-EYE LCD and the GPIO drivers that keep it powered.
pub struct Display {
    // These drivers must outlive the panel. Dropping them resets the pins and turns the LCD off.
    _lcd_enable: PinDriver<'static, Output>,
    _backlight: PinDriver<'static, Output>,
    panel: St7789Panel,
}

impl Display {
    /// Enable, reset, and initialize the P4X-EYE's ST7789 display.
    pub fn init() -> anyhow::Result<Self> {
        let peripherals = Peripherals::take()?;

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

        // This is an SPI write buffer, not a framebuffer. The display interface borrows it.
        let transfer_buffer = Box::leak(Box::new([0_u8; 512]));
        let interface = SpiInterface::new(spi, dc, transfer_buffer);
        let mut delay = Ets;
        let panel = Builder::new(ST7789, interface)
            .reset_pin(reset)
            .display_size(LCD_WIDTH, LCD_HEIGHT)
            .color_order(ColorOrder::Rgb)
            .invert_colors(ColorInversion::Inverted)
            .init(&mut delay)
            .map_err(|err| anyhow::anyhow!("ST7789 initialization failed: {err:?}"))?;

        Ok(Self {
            _lcd_enable: lcd_enable,
            _backlight: backlight,
            panel,
        })
    }

    /// Fill the entire screen with one color.
    pub fn clear(&mut self, color: Rgb565) -> anyhow::Result<()> {
        self.panel
            .clear(color)
            .map_err(|err| anyhow::anyhow!("failed to clear display: {err:?}"))
    }

    /// Fill one rectangular region without allocating a framebuffer.
    pub fn fill_rectangle(&mut self, area: Rectangle, color: Rgb565) -> anyhow::Result<()> {
        self.panel
            .fill_solid(&area, color)
            .map_err(|err| anyhow::anyhow!("failed to fill display region: {err:?}"))
    }

    /// Draw any embedded-graphics drawable, such as text, shapes, or an image.
    pub fn draw<D>(&mut self, drawable: &D) -> anyhow::Result<D::Output>
    where
        D: Drawable<Color = Rgb565>,
    {
        drawable
            .draw(&mut self.panel)
            .map_err(|err| anyhow::anyhow!("failed to draw to display: {err:?}"))
    }

    /// Clear the screen and show a single centered status message.
    pub fn show_message(&mut self, message: &str) -> anyhow::Result<()> {
        self.clear(Rgb565::BLACK)?;
        self.draw(&Text::with_text_style(
            message,
            Point::new(i32::from(LCD_WIDTH) / 2, i32::from(LCD_HEIGHT) / 2),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            TextStyleBuilder::new()
                .alignment(Alignment::Center)
                .baseline(Baseline::Middle)
                .build(),
        ))?;

        Ok(())
    }

    /// Play a two-second, asset-free boot animation and leave the display ready for the app.
    pub fn run_boot_animation(&mut self) -> anyhow::Result<()> {
        let started = std::time::Instant::now();
        let frame_duration = BOOT_ANIMATION_DURATION / BOOT_ANIMATION_FRAMES;

        self.clear(Rgb565::BLACK)?;
        for &(x, y, width, height) in &SPINNER_SEGMENTS {
            // self.fill_rectangle(
            //     Rectangle::new(Point::new(x, y), Size::new(width, height)),
            //     Rgb565::BLUE,
            // )?;
        }
        self.draw(&Text::with_text_style(
            "booting up...",
            Point::new(i32::from(LCD_WIDTH) / 2, i32::from(LCD_HEIGHT) / 2),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            TextStyleBuilder::new()
                .alignment(Alignment::Center)
                .baseline(Baseline::Middle)
                .build(),
        ))?;

        for frame in 0..BOOT_ANIMATION_FRAMES {
            let active_segment = (frame as usize) % SPINNER_SEGMENTS.len();

            // Each frame changes only the previous and current pulse: two small SPI
            // transfers, rather than repainting the full screen and causing a black flash.
            if frame != 0 {
                let previous_segment =
                    (active_segment + SPINNER_SEGMENTS.len() - 1) % SPINNER_SEGMENTS.len();
                let (x, y, width, height) = SPINNER_SEGMENTS[previous_segment];
                // self.fill_rectangle(
                //     Rectangle::new(Point::new(x, y), Size::new(width, height)),
                //     Rgb565::BLUE,
                // )?;
            }

            let (x, y, width, height) = SPINNER_SEGMENTS[active_segment];
            self.fill_rectangle(
                Rectangle::new(Point::new(x, y), Size::new(width, height)),
                SPINNER_COLORS[(frame as usize) % SPINNER_COLORS.len()],
            )?;

            let next_frame = started + frame_duration * (frame + 1);
            if let Some(wait) = next_frame.checked_duration_since(std::time::Instant::now()) {
                std::thread::sleep(wait);
            }
        }

        self.show_message("ready")
    }
}

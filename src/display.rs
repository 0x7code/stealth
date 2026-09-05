use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};
use esp_idf_hal::{
    delay::{Ets, FreeRtos},
    gpio::{Output, OutputPin, PinDriver},
    spi::{config::Config as SpiConfig, SpiDeviceDriver, SpiDriver, SpiDriverConfig, SPI2},
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
    pub fn init(
        spi2: SPI2<'static>,
        lcd_enable_pin: impl OutputPin + 'static,
        backlight_pin: impl OutputPin + 'static,
        reset_pin: impl OutputPin + 'static,
        dc_pin: impl OutputPin + 'static,
        clock_pin: impl OutputPin + 'static,
        data_pin: impl OutputPin + 'static,
        chip_select_pin: impl OutputPin + 'static,
    ) -> anyhow::Result<Self> {
        let mut lcd_enable = PinDriver::output(lcd_enable_pin)?;
        lcd_enable.set_high()?;

        // The board's LCD backlight is active-low.
        let mut backlight = PinDriver::output(backlight_pin)?;
        backlight.set_low()?;

        let reset = PinDriver::output(reset_pin)?;
        let dc = PinDriver::output(dc_pin)?;

        let spi_config = SpiConfig::new().baudrate(40.MHz().into());
        let spi = SpiDeviceDriver::new_single(
            spi2,
            clock_pin,
            data_pin,
            None::<esp_idf_hal::gpio::AnyInputPin>,
            Some(chip_select_pin),
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

    /// Draw any embedded-graphics drawable, such as text, shapes, or an image.
    pub fn draw<D>(&mut self, drawable: &D) -> anyhow::Result<D::Output>
    where
        D: Drawable<Color = Rgb565>,
    {
        drawable
            .draw(&mut self.panel)
            .map_err(|err| anyhow::anyhow!("failed to draw to display: {err:?}"))
    }

    /// Replace the current screen with a centered white confirmation message.
    pub fn show_message(&mut self, message: &str) -> anyhow::Result<()> {
        self.clear(Rgb565::BLACK)?;

        let text_style = TextStyleBuilder::new()
            .alignment(Alignment::Center)
            .baseline(Baseline::Middle)
            .build();
        self.draw(&Text::with_text_style(
            message,
            Point::new(i32::from(LCD_WIDTH) / 2, i32::from(LCD_HEIGHT) / 2),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            text_style,
        ))?;

        Ok(())
    }

    /// Temporary hardware test: show red, green, blue, then centered white text.
    pub fn run_color_test(&mut self) -> anyhow::Result<()> {
        for (name, color) in [
            ("red", Rgb565::RED),
            ("green", Rgb565::GREEN),
            ("blue", Rgb565::BLUE),
        ] {
            log::info!("display: {name}");
            self.clear(color)?;
            FreeRtos::delay_ms(1_000);
        }

        self.show_message("ready")?;

        log::info!("display: ready");
        Ok(())
    }
}

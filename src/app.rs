//! Application startup and the camera's Photo, Menu, and Gallery modes.

use std::time::Instant;

use crate::{
    board, buttons,
    camera::{self, CameraFrame},
    display,
    live_view::{ControlEvent, LiveView, LiveViewAction},
    rotary, sd_card,
};
use esp_idf_hal::delay::FreeRtos;

/// The running application and the hardware it owns for its full lifetime.
pub struct App {
    display: display::Display,
    buttons: buttons::Buttons,
    rotary: rotary::RotaryEvents,
    sd_card: sd_card::SdCard,
    camera: camera::Camera,
    mode: Mode,
}

enum Mode {
    Photo(LiveView),
    Menu(Menu),
    Gallery(Gallery),
}

struct Menu {
    gallery_selected: bool,
}

struct Gallery {
    captures: Vec<sd_card::Capture>,
    selected: usize,
    delete_pending: bool,
}

/// A physical control action before it is interpreted by the current application mode.
#[derive(Clone, Copy, Debug)]
enum InputEvent {
    Previous,
    Next,
    Enter,
    RotaryPressed,
    Clockwise,
    CounterClockwise,
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
            mode: Mode::Photo(LiveView::new()),
        })
    }

    /// Run the selected mode forever, while keeping the camera stream ready in the background.
    pub fn run(mut self) -> ! {
        let Self {
            display,
            buttons,
            rotary,
            sd_card,
            camera,
            mode,
        } = &mut self;

        loop {
            // Even static screens dequeue and return a frame. This avoids stalling the MIPI-CSI
            // pipeline while Menu or Gallery is open, and keeps Photo immediately responsive.
            let frame = match camera.next_frame() {
                Ok(frame) => frame,
                Err(error) => show_error_forever(display, buttons, "capture error", error),
            };
            let input = poll_input(buttons, rotary);

            let next_mode = match mode {
                Mode::Photo(live_view) => {
                    run_photo_frame(display, sd_card, live_view, input, &frame, Instant::now())
                }
                Mode::Menu(menu) => {
                    drop(frame);
                    run_menu(display, sd_card, menu, input)
                }
                Mode::Gallery(gallery) => {
                    drop(frame);
                    run_gallery(display, sd_card, gallery, input)
                }
            };

            match next_mode {
                Ok(Some(next_mode)) => *mode = next_mode,
                Ok(None) => {}
                Err(error) => show_error_forever(display, buttons, "screen error", error),
            }
        }
    }
}

impl Menu {
    fn photo() -> Self {
        Self {
            gallery_selected: false,
        }
    }

    fn gallery() -> Self {
        Self {
            gallery_selected: true,
        }
    }

    fn toggle(&mut self) {
        self.gallery_selected = !self.gallery_selected;
    }
}

impl Gallery {
    fn load(sd_card: &mut sd_card::SdCard) -> anyhow::Result<Self> {
        Ok(Self {
            captures: sd_card.captures()?,
            selected: 0,
            delete_pending: false,
        })
    }

    fn selected_capture(&self) -> Option<&sd_card::Capture> {
        self.captures.get(self.selected)
    }

    fn move_selection(&mut self, forward: bool) {
        if self.captures.is_empty() {
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % self.captures.len()
        } else {
            (self.selected + self.captures.len() - 1) % self.captures.len()
        };
    }

    fn remove_selected(&mut self) {
        self.captures.remove(self.selected);
        if self.selected == self.captures.len() && !self.captures.is_empty() {
            self.selected -= 1;
        }
        self.delete_pending = false;
    }
}

fn run_photo_frame(
    display: &mut display::Display,
    sd_card: &mut sd_card::SdCard,
    live_view: &mut LiveView,
    input: Option<InputEvent>,
    frame: &CameraFrame<'_>,
    now: Instant,
) -> anyhow::Result<Option<Mode>> {
    if matches!(input, Some(InputEvent::Previous | InputEvent::Next)) {
        display.show_mode_menu(false)?;
        return Ok(Some(Mode::Menu(Menu::photo())));
    }

    if let Some(event) = input.and_then(photo_control) {
        handle_photo_control(live_view, sd_card, event, frame, now);
    }

    display.draw_rgb565_scaled(
        frame.bytes(),
        frame.width(),
        frame.height(),
        frame.stride(),
        live_view.zoom(now),
        live_view.confirmation(now),
    )?;
    Ok(None)
}

fn run_menu(
    display: &mut display::Display,
    sd_card: &mut sd_card::SdCard,
    menu: &mut Menu,
    input: Option<InputEvent>,
) -> anyhow::Result<Option<Mode>> {
    let Some(input) = input else {
        FreeRtos::delay_ms(20);
        return Ok(None);
    };

    match input {
        InputEvent::Previous
        | InputEvent::CounterClockwise
        | InputEvent::Next
        | InputEvent::Clockwise => {
            menu.toggle();
            display.show_mode_menu(menu.gallery_selected)?;
        }
        InputEvent::Enter | InputEvent::RotaryPressed if !menu.gallery_selected => {
            return Ok(Some(Mode::Photo(LiveView::new())));
        }
        InputEvent::Enter | InputEvent::RotaryPressed => match Gallery::load(sd_card) {
            Ok(gallery) => {
                render_gallery(display, sd_card, &gallery)?;
                return Ok(Some(Mode::Gallery(gallery)));
            }
            Err(error) => {
                log::warn!("cannot open gallery: {error:#}");
                display.show_message("no sd card")?;
            }
        },
    }
    Ok(None)
}

fn run_gallery(
    display: &mut display::Display,
    sd_card: &mut sd_card::SdCard,
    gallery: &mut Gallery,
    input: Option<InputEvent>,
) -> anyhow::Result<Option<Mode>> {
    let Some(input) = input else {
        FreeRtos::delay_ms(20);
        return Ok(None);
    };

    // The encoder press is the Gallery back action, except while a deletion is awaiting an
    // explicit confirmation. This leaves Enter as the dedicated delete/confirm control.
    if matches!(input, InputEvent::RotaryPressed) && !gallery.delete_pending {
        display.show_mode_menu(true)?;
        return Ok(Some(Mode::Menu(Menu::gallery())));
    }

    if gallery.delete_pending {
        if matches!(input, InputEvent::Enter | InputEvent::RotaryPressed) {
            let capture = gallery
                .selected_capture()
                .expect("a delete confirmation always has a selected capture")
                .clone();
            match sd_card.delete_capture(&capture) {
                Ok(()) => {
                    log::info!("deleted camera frame {}", capture.name());
                    gallery.remove_selected();
                }
                Err(error) => log::error!("failed to delete {}: {error:#}", capture.name()),
            }
        } else {
            // Previous, Next, and either turn all cancel; no accidental delete is possible.
            gallery.delete_pending = false;
        }
        render_gallery(display, sd_card, gallery)?;
        return Ok(None);
    }

    match input {
        InputEvent::Previous | InputEvent::CounterClockwise => gallery.move_selection(false),
        InputEvent::Next | InputEvent::Clockwise => gallery.move_selection(true),
        InputEvent::Enter => {
            if gallery.selected_capture().is_some() {
                gallery.delete_pending = true;
            }
        }
        InputEvent::RotaryPressed => unreachable!("handled before deletion confirmation"),
    }
    render_gallery(display, sd_card, gallery)?;
    Ok(None)
}

fn render_gallery(
    display: &mut display::Display,
    sd_card: &mut sd_card::SdCard,
    gallery: &Gallery,
) -> anyhow::Result<()> {
    let Some(capture) = gallery.selected_capture() else {
        return display.show_message("gallery empty");
    };

    let jpeg = sd_card.read_capture(capture)?;
    let image = camera::decode_jpeg(&jpeg)?;
    let caption;
    let label = if gallery.delete_pending {
        "delete? enter=yes"
    } else {
        caption = format!("{} del=enter", capture.name());
        &caption
    };
    display.draw_rgb888_scaled(
        image.rgb888_bytes(),
        image.width(),
        image.height(),
        image.stride(),
        Some(label),
    )
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

fn poll_input(buttons: &mut buttons::Buttons, rotary: &rotary::RotaryEvents) -> Option<InputEvent> {
    let button = buttons.pressed();
    let rotary_event = rotary.poll();

    match (button, rotary_event) {
        (Some(buttons::Button::Enter), _) => Some(InputEvent::Enter),
        (_, Some(rotary::RotaryEvent::Pressed)) => Some(InputEvent::RotaryPressed),
        (Some(buttons::Button::Previous), _) => Some(InputEvent::Previous),
        (Some(buttons::Button::Next), _) => Some(InputEvent::Next),
        (_, Some(rotary::RotaryEvent::Clockwise)) => Some(InputEvent::Clockwise),
        (_, Some(rotary::RotaryEvent::CounterClockwise)) => Some(InputEvent::CounterClockwise),
        _ => None,
    }
}

fn photo_control(input: InputEvent) -> Option<ControlEvent> {
    match input {
        InputEvent::Enter | InputEvent::RotaryPressed => Some(ControlEvent::Capture),
        InputEvent::Clockwise => Some(ControlEvent::ZoomIn),
        InputEvent::CounterClockwise => Some(ControlEvent::ZoomOut),
        InputEvent::Previous | InputEvent::Next => None,
    }
}

fn handle_photo_control(
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

        FreeRtos::delay_ms(20);
    }
}

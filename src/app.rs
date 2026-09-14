//! Application startup and the camera's Photo, Menu, and Gallery modes.

use std::time::Instant;

use crate::{
    board, buttons,
    camera::{self, CameraFrame},
    display,
    live_view::{ControlEvent, LiveView, LiveViewAction},
    memory, rotary, sd_card,
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
    Detect(Detect),
}

struct Menu {
    selected: MenuItem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuItem {
    Photo,
    Gallery,
    Detect,
}

struct Gallery {
    captures: Vec<sd_card::Capture>,
    selected: usize,
    delete_pending: bool,
}

struct Detect {
    detector: camera::Detector,
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
        memory::log_free("Photo ready");

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
                Mode::Detect(detect) => run_detect_frame(display, detect, input, &frame),
            };

            match next_mode {
                Ok(Some(next_mode)) => {
                    let leaving_gallery = matches!(mode, Mode::Gallery(_));
                    let leaving_detection = matches!(mode, Mode::Detect(_));
                    *mode = next_mode;
                    if leaving_gallery {
                        memory::log_free("Gallery exited");
                    }
                    if leaving_detection {
                        memory::log_free("Detection exited");
                    }
                }
                Ok(None) => {}
                Err(error) => show_error_forever(display, buttons, "screen error", error),
            }
        }
    }
}

impl Menu {
    fn photo() -> Self {
        Self {
            selected: MenuItem::Photo,
        }
    }

    fn gallery() -> Self {
        Self {
            selected: MenuItem::Gallery,
        }
    }

    fn detect() -> Self {
        Self {
            selected: MenuItem::Detect,
        }
    }

    fn toggle(&mut self) {
        self.selected = match self.selected {
            MenuItem::Photo => MenuItem::Gallery,
            MenuItem::Gallery => MenuItem::Detect,
            MenuItem::Detect => MenuItem::Photo,
        };
    }

    fn selected_index(&self) -> usize {
        match self.selected {
            MenuItem::Photo => 0,
            MenuItem::Gallery => 1,
            MenuItem::Detect => 2,
        }
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
        display.show_mode_menu(Menu::photo().selected_index())?;
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
        display::CameraRenderOptions {
            zoom: live_view.zoom(now),
            confirmation: live_view.confirmation(now),
            overlays: &[],
        },
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
            display.show_mode_menu(menu.selected_index())?;
        }
        InputEvent::Enter | InputEvent::RotaryPressed => match menu.selected {
            MenuItem::Photo => return Ok(Some(Mode::Photo(LiveView::new()))),
            MenuItem::Gallery => match Gallery::load(sd_card) {
                Ok(gallery) => {
                    render_gallery(display, sd_card, &gallery)?;
                    return Ok(Some(Mode::Gallery(gallery)));
                }
                Err(error) => {
                    log::warn!("cannot open gallery: {error:#}");
                    display.show_message("no sd card")?;
                }
            },
            MenuItem::Detect => match start_detector(sd_card) {
                Ok(detector) => {
                    display.show_message("detecting")?;
                    return Ok(Some(Mode::Detect(Detect { detector })));
                }
                Err(error) => {
                    log::warn!("cannot enter detection mode: {error:#}");
                    display.show_message("add coco model")?;
                }
            },
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
        camera::release_gallery_decoder()?;
        display.show_mode_menu(Menu::gallery().selected_index())?;
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

fn run_detect_frame(
    display: &mut display::Display,
    detect: &mut Detect,
    input: Option<InputEvent>,
    frame: &CameraFrame<'_>,
) -> anyhow::Result<Option<Mode>> {
    if matches!(
        input,
        Some(InputEvent::Previous | InputEvent::Next | InputEvent::RotaryPressed)
    ) {
        display.show_mode_menu(Menu::detect().selected_index())?;
        return Ok(Some(Mode::Menu(Menu::detect())));
    }

    let started = Instant::now();
    let detections = detect.detector.detect(frame)?;
    log::info!(
        "ESP-DL detection: {} result(s) in {} ms",
        detections.len(),
        started.elapsed().as_millis()
    );
    if let Some(detection) = detections.first() {
        log::info!(
            "ESP-DL best match: {} ({:.0}%)",
            detection.label(),
            detection.score * 100.0
        );
    }
    let overlays: Vec<display::OverlayBox> = detections
        .iter()
        .map(|detection| display::OverlayBox {
            left: detection.left,
            top: detection.top,
            right: detection.right,
            bottom: detection.bottom,
        })
        .collect();
    let label = detections
        .first()
        .map(camera::Detection::label)
        .unwrap_or("no objects");
    display.draw_rgb565_scaled(
        frame.bytes(),
        frame.width(),
        frame.height(),
        frame.stride(),
        display::CameraRenderOptions {
            zoom: 1,
            confirmation: Some(label),
            overlays: &overlays,
        },
    )?;
    Ok(None)
}

fn start_detector(sd_card: &mut sd_card::SdCard) -> anyhow::Result<camera::Detector> {
    if !sd_card.has_coco_detector_model()? {
        anyhow::bail!("missing /sdcard/models/p4/coco_detect_yolo11n_320_s8_v1.espdl");
    }
    camera::Detector::start()
}

fn render_gallery(
    display: &mut display::Display,
    sd_card: &mut sd_card::SdCard,
    gallery: &Gallery,
) -> anyhow::Result<()> {
    let Some(capture) = gallery.selected_capture() else {
        return display.show_message("gallery empty");
    };

    memory::log_free("Gallery before JPEG decode");
    let jpeg = sd_card.read_capture(capture)?;
    let image = camera::decode_jpeg(&jpeg)?;
    drop(jpeg);
    memory::log_free("Gallery JPEG decoded");
    let caption;
    let label = if gallery.delete_pending {
        "delete? enter=yes"
    } else {
        caption = format!("{} del=enter", capture.name());
        &caption
    };
    let draw_result = display.draw_rgb888_scaled(
        image.rgb888_bytes(),
        image.width(),
        image.height(),
        image.stride(),
        Some(label),
    );
    drop(image);
    memory::log_free("Gallery JPEG released");
    draw_result
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

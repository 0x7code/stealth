//! State for the camera live-view user interface.
//!
//! This module is deliberately independent of GPIO and camera drivers. It translates
//! application-level control events into a capture request or changes to the view state.

use std::time::{Duration, Instant};

const CONFIRMATION_DURATION: Duration = Duration::from_millis(500);
const FOCUS_ASSIST_DURATION: Duration = Duration::from_secs(3);
const MIN_ZOOM: u8 = 1;
const MAX_ZOOM: u8 = 3;

/// A normalized control action, independent of the physical button or encoder that produced it.
#[derive(Clone, Copy, Debug)]
pub enum ControlEvent {
    Previous,
    Next,
    Capture,
    ZoomIn,
    ZoomOut,
}

/// Work the application must do after the live view processes a control event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveViewAction {
    None,
    Capture,
}

/// The transient state shown over the camera stream.
pub struct LiveView {
    confirmation: Option<(&'static str, Instant)>,
    zoom: u8,
    focus_assist_until: Option<Instant>,
}

impl LiveView {
    pub fn new() -> Self {
        Self {
            confirmation: None,
            zoom: MIN_ZOOM,
            focus_assist_until: None,
        }
    }

    /// Update the visible state and report whether the application should save a frame.
    pub fn handle(&mut self, event: ControlEvent, now: Instant) -> LiveViewAction {
        match event {
            ControlEvent::Previous => self.show_confirmation("previous", now),
            ControlEvent::Next => self.show_confirmation("next", now),
            ControlEvent::Capture => return LiveViewAction::Capture,
            ControlEvent::ZoomIn => {
                self.zoom = (self.zoom + 1).min(MAX_ZOOM);
                self.refresh_focus_assist(now);
                self.show_confirmation(zoom_label(self.zoom), now);
            }
            ControlEvent::ZoomOut => {
                self.zoom = self.zoom.saturating_sub(1).max(MIN_ZOOM);
                self.refresh_focus_assist(now);
                self.show_confirmation(zoom_label(self.zoom), now);
            }
        }

        LiveViewAction::None
    }

    /// Show a short status message over the next frames.
    pub fn show_confirmation(&mut self, message: &'static str, now: Instant) {
        self.confirmation = Some((message, now));
    }

    /// Return the currently visible status message, clearing it after its display duration.
    pub fn confirmation(&mut self, now: Instant) -> Option<&'static str> {
        match self.confirmation {
            Some((message, started)) if now.duration_since(started) < CONFIRMATION_DURATION => {
                Some(message)
            }
            Some(_) => {
                self.confirmation = None;
                None
            }
            None => None,
        }
    }

    /// Return the active focus-assist zoom, returning to 1x after a short idle period.
    pub fn zoom(&mut self, now: Instant) -> u8 {
        match self.focus_assist_until {
            Some(until) if now < until => self.zoom,
            Some(_) => {
                self.zoom = MIN_ZOOM;
                self.focus_assist_until = None;
                MIN_ZOOM
            }
            None => MIN_ZOOM,
        }
    }

    fn refresh_focus_assist(&mut self, now: Instant) {
        self.focus_assist_until = (self.zoom > MIN_ZOOM).then(|| now + FOCUS_ASSIST_DURATION);
    }
}

fn zoom_label(zoom: u8) -> &'static str {
    match zoom {
        1 => "focus 1x",
        2 => "focus 2x",
        3 => "focus 3x",
        _ => unreachable!("zoom is clamped to its supported range"),
    }
}

//! Controller input, turned into the same actions the keyboard produces.
//!
//! A thread blocks on the controller so the window only wakes when a button
//! or stick actually does something. Otherwise a connected controller would
//! mean polling every frame, which kept the idle app at 20% of a core.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use gilrs::{Axis, Button, EventType, Gilrs};

use crate::model::{Action, Direction};

/// How long the reader sleeps with nothing held. Hotplug and input both wake
/// it early, so this only bounds how fast it notices being asked to stop.
const IDLE_WAIT: Duration = Duration::from_millis(500);
const STICK_THRESHOLD: f32 = 0.5;
const STICK_FIRST_REPEAT: Duration = Duration::from_millis(350);
const STICK_REPEAT: Duration = Duration::from_millis(120);

pub struct Gamepad {
    actions: Option<mpsc::Receiver<Action>>,
}

/// A stick pushed past the threshold, repeating like a held key.
struct Held {
    direction: Direction,
    next: Instant,
}

impl Gamepad {
    /// Starts the reader thread; `ctx` is woken whenever it has actions.
    pub fn new(ctx: egui::Context) -> Self {
        let gilrs = match Gilrs::new() {
            Ok(gilrs) => gilrs,
            Err(error) => {
                log::warn!("no gamepad support: {error}");
                return Self { actions: None };
            }
        };
        for (_, pad) in gilrs.gamepads() {
            log::info!("gamepad: {}", pad.name());
        }
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("gamepad".into())
            .spawn(move || read_loop(gilrs, &tx, &ctx));
        if let Err(error) = spawned {
            log::warn!("no gamepad support: spawning reader: {error}");
            return Self { actions: None };
        }
        Self { actions: Some(rx) }
    }

    /// Moves the controller's actions since the last frame into `actions`.
    /// Returns whether there were any, so the interface can show controller
    /// glyphs. An unfocused window drops them: the controller is driving
    /// whatever is in front, and nothing should fire on coming back.
    pub fn poll(&mut self, focused: bool, actions: &mut Vec<Action>) -> bool {
        let Some(rx) = &self.actions else {
            return false;
        };
        if !focused {
            rx.try_iter().for_each(drop);
            return false;
        }
        let before = actions.len();
        actions.extend(rx.try_iter());
        actions.len() > before
    }
}

/// Runs until the interface drops its receiver.
fn read_loop(mut gilrs: Gilrs, tx: &mpsc::Sender<Action>, ctx: &egui::Context) {
    let mut stick: Option<Held> = None;
    loop {
        let wait = stick.as_ref().map_or(IDLE_WAIT, |held| {
            held.next.saturating_duration_since(Instant::now())
        });
        let mut sent = false;
        if let Some(event) = gilrs.next_event_blocking(Some(wait)) {
            match event.event {
                EventType::ButtonPressed(button, _) | EventType::ButtonRepeated(button, _) => {
                    if let Some(action) = button_action(button) {
                        if tx.send(action).is_err() {
                            return;
                        }
                        sent = true;
                    }
                }
                EventType::Connected => {
                    let name = gilrs.gamepad(event.id).name().to_string();
                    log::info!("gamepad connected: {name}");
                }
                EventType::Disconnected => log::info!("gamepad disconnected"),
                _ => {}
            }
        }
        // Stick state is read after every wake, whether an axis event or the
        // repeat deadline caused it.
        if let Some(direction) = poll_stick(&gilrs, &mut stick) {
            if tx.send(Action::MoveFocus(direction)).is_err() {
                return;
            }
            sent = true;
        }
        if sent {
            ctx.request_repaint();
        }
    }
}

/// The focus move a deflected stick calls for right now, if any, tracking
/// the hold so it repeats like a held key.
fn poll_stick(gilrs: &Gilrs, stick: &mut Option<Held>) -> Option<Direction> {
    // The first controller with a deflected stick wins; a couch usually
    // has one in use at a time.
    let direction = gilrs.gamepads().find_map(|(_, pad)| {
        let x = pad.value(Axis::LeftStickX);
        let y = pad.value(Axis::LeftStickY);
        stick_direction(x, y)
    });
    let now = Instant::now();
    match (direction, stick.as_mut()) {
        (None, _) => {
            *stick = None;
            None
        }
        (Some(direction), Some(held)) if held.direction == direction => {
            if now >= held.next {
                held.next = now + STICK_REPEAT;
                Some(direction)
            } else {
                None
            }
        }
        (Some(direction), _) => {
            *stick = Some(Held {
                direction,
                next: now + STICK_FIRST_REPEAT,
            });
            Some(direction)
        }
    }
}

fn button_action(button: Button) -> Option<Action> {
    Some(match button {
        Button::DPadUp => Action::MoveFocus(Direction::Up),
        Button::DPadDown => Action::MoveFocus(Direction::Down),
        Button::DPadLeft => Action::MoveFocus(Direction::Left),
        Button::DPadRight => Action::MoveFocus(Direction::Right),
        // South is A on Xbox and Cross on PlayStation; East is B / Circle.
        Button::South => Action::Activate,
        Button::East => Action::Back,
        Button::North => Action::FocusSearch,
        // gilrs names the bumpers LeftTrigger/RightTrigger; the triggers
        // proper are the *2 variants.
        Button::LeftTrigger => Action::CycleTab(-1),
        Button::RightTrigger => Action::CycleTab(1),
        Button::LeftTrigger2 => Action::CycleFilter(-1),
        Button::RightTrigger2 => Action::CycleFilter(1),
        _ => return None,
    })
}

/// The dominant axis once the stick leaves the dead zone. Up is positive y.
fn stick_direction(x: f32, y: f32) -> Option<Direction> {
    if x.abs() < STICK_THRESHOLD && y.abs() < STICK_THRESHOLD {
        return None;
    }
    Some(if x.abs() > y.abs() {
        if x > 0.0 {
            Direction::Right
        } else {
            Direction::Left
        }
    } else if y > 0.0 {
        Direction::Up
    } else {
        Direction::Down
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_zone_is_quiet() {
        assert_eq!(stick_direction(0.2, -0.3), None);
    }

    #[test]
    fn dominant_axis_wins() {
        assert_eq!(stick_direction(0.9, 0.6), Some(Direction::Right));
        assert_eq!(stick_direction(-0.6, 0.9), Some(Direction::Up));
        assert_eq!(stick_direction(0.1, -0.8), Some(Direction::Down));
    }
}

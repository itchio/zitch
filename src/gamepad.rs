//! Controller input, turned into the same actions the keyboard produces.
//!
//! egui only repaints on window input, so while a controller is connected
//! the app asks for a repaint every frame to keep polling it.

use std::time::{Duration, Instant};

use gilrs::{Axis, Button, EventType, Gilrs};

use crate::model::{Action, Direction};

const POLL: Duration = Duration::from_millis(16);
const STICK_THRESHOLD: f32 = 0.5;
const STICK_FIRST_REPEAT: Duration = Duration::from_millis(350);
const STICK_REPEAT: Duration = Duration::from_millis(120);

pub struct Gamepad {
    gilrs: Option<Gilrs>,
    stick: Option<Held>,
}

/// A stick pushed past the threshold, repeating like a held key.
struct Held {
    direction: Direction,
    next: Instant,
}

impl Gamepad {
    pub fn new() -> Self {
        let gilrs = match Gilrs::new() {
            Ok(gilrs) => {
                for (_, pad) in gilrs.gamepads() {
                    log::info!("gamepad: {}", pad.name());
                }
                Some(gilrs)
            }
            Err(error) => {
                log::warn!("no gamepad support: {error}");
                None
            }
        };
        Self { gilrs, stick: None }
    }

    /// Drains controller events into `actions`. Returns whether the
    /// controller produced any action this frame, so the interface can show
    /// controller glyphs; repaints keep coming while one is connected.
    pub fn poll(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) -> bool {
        let before = actions.len();
        let Some(gilrs) = self.gilrs.as_mut() else {
            return false;
        };
        while let Some(event) = gilrs.next_event() {
            match event.event {
                EventType::ButtonPressed(button, _) | EventType::ButtonRepeated(button, _) => {
                    if let Some(action) = button_action(button) {
                        actions.push(action);
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

        let connected = gilrs.gamepads().next().is_some();
        if connected {
            self.poll_stick(actions);
            ctx.request_repaint_after(POLL);
        } else {
            self.stick = None;
        }
        actions.len() > before
    }

    fn poll_stick(&mut self, actions: &mut Vec<Action>) {
        let Some(gilrs) = self.gilrs.as_ref() else {
            return;
        };
        // The first controller with a deflected stick wins; a couch usually
        // has one in use at a time.
        let direction = gilrs.gamepads().find_map(|(_, pad)| {
            let x = pad.value(Axis::LeftStickX);
            let y = pad.value(Axis::LeftStickY);
            stick_direction(x, y)
        });
        let now = Instant::now();
        match (direction, self.stick.as_mut()) {
            (None, _) => self.stick = None,
            (Some(direction), Some(held)) if held.direction == direction => {
                if now >= held.next {
                    actions.push(Action::MoveFocus(direction));
                    held.next = now + STICK_REPEAT;
                }
            }
            (Some(direction), _) => {
                actions.push(Action::MoveFocus(direction));
                self.stick = Some(Held {
                    direction,
                    next: now + STICK_FIRST_REPEAT,
                });
            }
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
        Button::LeftTrigger => Action::CycleFilter(-1),
        Button::RightTrigger => Action::CycleFilter(1),
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

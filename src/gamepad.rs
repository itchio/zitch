//! Controller input, turned into the same actions the keyboard produces.
//!
//! On the desktop a thread blocks on gilrs so the window only wakes when a
//! button or stick actually does something. Otherwise a connected controller
//! would mean polling every frame, which kept the idle app at 20% of a core.
//! A host that reads controllers itself (the SDL one) feeds the same channel
//! through [`Gamepad::external`] and maps its buttons with [`button_action`].

use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::model::{Action, Direction};

const STICK_THRESHOLD: f32 = 0.5;
const STICK_FIRST_REPEAT: Duration = Duration::from_millis(350);
const STICK_REPEAT: Duration = Duration::from_millis(120);

/// A controller button by position, whichever library read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadButton {
    /// A on Xbox, Cross on PlayStation.
    South,
    /// B / Circle.
    East,
    /// Y / Triangle.
    North,
    LeftBumper,
    RightBumper,
    LeftTrigger,
    RightTrigger,
    Guide,
    Start,
}

pub fn button_action(button: PadButton) -> Option<Action> {
    Some(match button {
        PadButton::South => Action::Activate,
        PadButton::East => Action::Back,
        PadButton::North => Action::FocusSearch,
        PadButton::LeftBumper => Action::CycleTab(-1),
        PadButton::RightBumper => Action::CycleTab(1),
        PadButton::LeftTrigger | PadButton::RightTrigger => return None,
        PadButton::Guide | PadButton::Start => Action::Menu,
    })
}

/// What a controller reader tells the interface.
pub enum PadEvent {
    /// A controller is present, at startup or plugged in since.
    Connected,
    Action(Action),
}

/// The reader's end of the channel: presses and connections.
#[derive(Clone)]
pub struct PadSender(mpsc::Sender<PadEvent>);

impl PadSender {
    pub fn send(&self, action: Action) -> Result<(), mpsc::SendError<PadEvent>> {
        self.0.send(PadEvent::Action(action))
    }

    pub fn connected(&self) -> Result<(), mpsc::SendError<PadEvent>> {
        self.0.send(PadEvent::Connected)
    }
}

/// What a frame's poll found.
#[derive(Default)]
pub struct Poll {
    /// A press or stick move landed in the actions.
    pub pressed: bool,
    /// A controller announced itself; a hint to show its glyphs before
    /// anything is pressed.
    pub connected: bool,
}

pub struct Gamepad {
    events: Option<mpsc::Receiver<PadEvent>>,
}

impl Gamepad {
    /// Starts the gilrs reader thread; `ctx` is woken whenever it has
    /// actions. Without gilrs there is no controller unless the host
    /// provides one through [`Self::external`].
    pub fn new(ctx: egui::Context) -> Self {
        #[cfg(feature = "gilrs")]
        {
            reader::start(ctx)
        }
        #[cfg(not(feature = "gilrs"))]
        {
            let _ = ctx;
            Self { events: None }
        }
    }

    /// A gamepad fed by the host: whatever it sends arrives at the next
    /// [`Self::poll`].
    pub fn external() -> (Self, PadSender) {
        let (tx, rx) = mpsc::channel();
        (Self { events: Some(rx) }, PadSender(tx))
    }

    /// Moves the controller's actions since the last frame into `actions`
    /// and reports what arrived, so the interface can show controller
    /// glyphs. An unfocused window drops presses: the controller is driving
    /// whatever is in front, and nothing should fire on coming back.
    pub fn poll(&mut self, focused: bool, actions: &mut Vec<Action>) -> Poll {
        let mut poll = Poll::default();
        let Some(rx) = &self.events else {
            return poll;
        };
        for event in rx.try_iter() {
            match event {
                PadEvent::Connected => poll.connected = true,
                // Only the Guide button reaches an unfocused window: it is
                // the way back from a running game.
                PadEvent::Action(action) if focused || matches!(action, Action::Menu) => {
                    actions.push(action);
                    poll.pressed = true;
                }
                PadEvent::Action(_) => {}
            }
        }
        poll
    }
}

/// A stick (or d-pad) pushed past the dead zone, repeating like a held key.
#[derive(Default)]
pub struct Stick {
    held: Option<Held>,
}

struct Held {
    direction: Direction,
    next: Instant,
}

impl Stick {
    /// The focus move the position `(x, y)` calls for right now, if any.
    /// Up is positive y. Call it after every input and again when
    /// [`Self::deadline`] passes.
    pub fn update(&mut self, x: f32, y: f32) -> Option<Direction> {
        let direction = stick_direction(x, y);
        let now = Instant::now();
        match (direction, self.held.as_mut()) {
            (None, _) => {
                self.held = None;
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
                self.held = Some(Held {
                    direction,
                    next: now + STICK_FIRST_REPEAT,
                });
                Some(direction)
            }
        }
    }

    /// When the next repeat is due, while the stick is held.
    pub fn deadline(&self) -> Option<Instant> {
        self.held.as_ref().map(|held| held.next)
    }
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

#[cfg(feature = "gilrs")]
mod reader {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use gilrs::{Axis, Button, EventType, Gilrs};

    use super::{Gamepad, PadButton, PadSender, Stick, button_action};
    use crate::model::{Action, Direction};

    /// How long the reader sleeps with nothing held. Hotplug and input both
    /// wake it early, so this only bounds how fast it notices being asked
    /// to stop.
    const IDLE_WAIT: Duration = Duration::from_millis(500);

    pub fn start(ctx: egui::Context) -> Gamepad {
        let gilrs = match Gilrs::new() {
            Ok(gilrs) => gilrs,
            Err(error) => {
                log::warn!("no gamepad support: {error}");
                return Gamepad { events: None };
            }
        };
        let (tx, rx) = mpsc::channel();
        let tx = PadSender(tx);
        for (_, pad) in gilrs.gamepads() {
            log::info!("gamepad: {}", pad.name());
            let _ = tx.connected();
        }
        let spawned = std::thread::Builder::new()
            .name("gamepad".into())
            .spawn(move || read_loop(gilrs, &tx, &ctx));
        if let Err(error) = spawned {
            log::warn!("no gamepad support: spawning reader: {error}");
            return Gamepad { events: None };
        }
        Gamepad { events: Some(rx) }
    }

    /// Runs until the interface drops its receiver.
    fn read_loop(mut gilrs: Gilrs, tx: &PadSender, ctx: &egui::Context) {
        let mut stick = Stick::default();
        loop {
            let wait = stick.deadline().map_or(IDLE_WAIT, |next| {
                next.saturating_duration_since(Instant::now())
            });
            let mut sent = false;
            if let Some(event) = gilrs.next_event_blocking(Some(wait)) {
                match event.event {
                    EventType::ButtonPressed(button, _) | EventType::ButtonRepeated(button, _) => {
                        if let Some(action) = action_for(button) {
                            if tx.send(action).is_err() {
                                return;
                            }
                            sent = true;
                        }
                    }
                    EventType::Connected => {
                        let name = gilrs.gamepad(event.id).name().to_string();
                        log::info!("gamepad connected: {name}");
                        if tx.connected().is_err() {
                            return;
                        }
                        sent = true;
                    }
                    EventType::Disconnected => log::info!("gamepad disconnected"),
                    _ => {}
                }
            }
            // Stick state is read after every wake, whether an axis event or
            // the repeat deadline caused it. The first controller with a
            // deflected stick wins; a couch usually has one in use at a time.
            let (x, y) = gilrs
                .gamepads()
                .map(|(_, pad)| (pad.value(Axis::LeftStickX), pad.value(Axis::LeftStickY)))
                .find(|(x, y)| {
                    x.abs() >= super::STICK_THRESHOLD || y.abs() >= super::STICK_THRESHOLD
                })
                .unwrap_or((0.0, 0.0));
            if let Some(direction) = stick.update(x, y) {
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

    fn action_for(button: Button) -> Option<Action> {
        let pad = match button {
            Button::DPadUp => return Some(Action::MoveFocus(Direction::Up)),
            Button::DPadDown => return Some(Action::MoveFocus(Direction::Down)),
            Button::DPadLeft => return Some(Action::MoveFocus(Direction::Left)),
            Button::DPadRight => return Some(Action::MoveFocus(Direction::Right)),
            Button::South => PadButton::South,
            Button::East => PadButton::East,
            Button::North => PadButton::North,
            // gilrs names the bumpers LeftTrigger/RightTrigger; the triggers
            // proper are the *2 variants.
            Button::LeftTrigger => PadButton::LeftBumper,
            Button::RightTrigger => PadButton::RightBumper,
            Button::LeftTrigger2 => PadButton::LeftTrigger,
            Button::RightTrigger2 => PadButton::RightTrigger,
            Button::Mode => PadButton::Guide,
            Button::Start => PadButton::Start,
            _ => return None,
        };
        button_action(pad)
    }
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

    #[test]
    fn held_stick_repeats_after_a_pause() {
        let mut stick = Stick::default();
        assert_eq!(stick.update(1.0, 0.0), Some(Direction::Right));
        assert_eq!(stick.update(1.0, 0.0), None);
        assert!(stick.deadline().is_some());
        assert_eq!(stick.update(0.0, 0.0), None);
        assert!(stick.deadline().is_none());
        assert_eq!(stick.update(0.0, 1.0), Some(Direction::Up));
    }
}

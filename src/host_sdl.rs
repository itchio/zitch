//! An SDL2 window with GLES2 drawing, for machines without a window system.
//! Handheld firmware (muOS and the like) has no X11 or Wayland; its own SDL2
//! build knows how to reach the framebuffer and the Mali driver, so this
//! host links against that SDL2 and lets it open the screen. Controllers
//! come through SDL too, mapped by the firmware's game controller database.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use sdl2::controller::{Axis, Button, GameController};
use sdl2::event::{Event, WindowEvent};
use sdl2::keyboard::{Keycode, Mod};
use sdl2::mouse::MouseButton;

use crate::app::{App, Options, Shot};
use crate::backend::{Backend, Waker};
use crate::gamepad::{Gamepad, PadButton, Stick, button_action};
use crate::images::CoverLoader;
use crate::model::Action;

pub struct Window {
    pub size: (f32, f32),
    pub fullscreen: bool,
}

/// SDL treats a trigger as an axis; past this it counts as pressed.
const TRIGGER_PRESSED: i16 = 16_000;
/// egui asks for a repaint "never" as a very long delay; keep the
/// arithmetic on it finite.
const LONGEST_WAIT: Duration = Duration::from_secs(3600);
/// How often the idle loop pumps SDL for input. SDL's own timed wait can
/// only block on drivers with a window system; on the framebuffer drivers
/// this host exists for it falls back to polling every millisecond, which
/// cost 5% of a core doing nothing. A frame's worth of latency on input
/// is not felt in a menu, and brings idle down to 0.6%.
const POLL: Duration = Duration::from_millis(16);

/// Dark enough not to flash, and unlike any tile the interface draws.
const SCRUB_COLOR: [f32; 4] = [0.02, 0.0, 0.03, 1.0];

/// Wakes the frame loop from another thread, between input polls.
#[derive(Default)]
struct Wake {
    flag: Mutex<bool>,
    ready: Condvar,
}

impl Wake {
    fn signal(&self) {
        *self.flag.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.ready.notify_one();
    }

    /// Sleeps until signalled or `timeout` passes. Whether it was signalled,
    /// now or since the last call.
    fn wait(&self, timeout: Duration) -> bool {
        let mut flag = self.flag.lock().unwrap_or_else(|p| p.into_inner());
        if !*flag {
            flag = self
                .ready
                .wait_timeout(flag, timeout)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        std::mem::take(&mut *flag)
    }
}

/// Runs the interface until the window closes.
pub fn run(
    backend: Backend,
    covers: CoverLoader,
    waker: Waker,
    mut options: Options,
    shot: Option<Shot>,
    window: Window,
) -> Result<()> {
    let sdl = sdl2::init().map_err(|e| anyhow!("initialising SDL: {e}"))?;
    let video = sdl.video().map_err(|e| anyhow!("SDL video: {e}"))?;
    let controllers = sdl
        .game_controller()
        .map_err(|e| anyhow!("SDL controllers: {e}"))?;
    log::info!("SDL video driver: {}", video.current_video_driver());
    {
        let attr = video.gl_attr();
        attr.set_context_profile(sdl2::video::GLProfile::GLES);
        attr.set_context_version(2, 0);
        attr.set_double_buffer(true);
        attr.set_depth_size(0);
    }
    let mut builder = video.window("zitch", window.size.0 as u32, window.size.1 as u32);
    builder.opengl().position_centered();
    if window.fullscreen {
        builder.fullscreen_desktop();
    }
    let sdl_window = builder.build().context("opening the window")?;
    let gl_context = sdl_window
        .gl_create_context()
        .map_err(|e| anyhow!("creating a GLES2 context: {e}"))?;
    sdl_window
        .gl_make_current(&gl_context)
        .map_err(|e| anyhow!("making the GL context current: {e}"))?;
    if let Err(error) = video.gl_set_swap_interval(1) {
        log::warn!("no vsync: {error}");
    }
    let gl = unsafe {
        egui_glow::glow::Context::from_loader_function(|name| {
            video.gl_get_proc_address(name) as *const _
        })
    };
    let mut painter = egui_glow::Painter::new(Arc::new(gl), "", None, false)
        .map_err(|e| anyhow!("setting up the egui painter: {e}"))?;
    video.text_input().start();

    let ctx = egui::Context::default();
    // The backend and the cover loader wake the frame loop through `wake`;
    // a delayed request is served by the loop's own timeout.
    let wake = Arc::new(Wake::default());
    let waker_wake = Arc::clone(&wake);
    ctx.set_request_repaint_callback(move |info| {
        if info.delay.is_zero() {
            waker_wake.signal();
        }
    });
    waker.attach(&ctx);

    let (gamepad, pad_tx) = Gamepad::external();
    options.gamepad = Some(gamepad);
    let mut app = App::new(backend, covers, &ctx, options, shot);

    let mut pads = Pads::new();
    let mut event_pump = sdl
        .event_pump()
        .map_err(|e| anyhow!("SDL event pump: {e}"))?;
    let mut input = Input::default();
    let started = Instant::now();
    let mut wait = Duration::ZERO;
    let mut shots: Vec<egui::UserData> = Vec::new();
    // Set while a game has the screen: the interface keeps running but
    // draws nothing and lets the controller through to the game. Minimize
    // is the closest thing egui has for a window that owns the screen.
    let mut hidden = false;
    // Set when the window comes back from behind a game. The game drew to
    // the same screen memory, and Mali's transaction elimination skips
    // tiles it thinks are unchanged since its last frame there, so the
    // first frame back only lands where the interface itself changed.
    // Two frames of a colour nothing else uses, one per buffer, make every
    // tile different from what the GPU remembers.
    let mut scrub = false;

    'frames: loop {
        let deadline = pads.stick.deadline().or(pads.dpad.deadline());
        if let Some(deadline) = deadline {
            wait = wait.min(deadline.saturating_duration_since(Instant::now()));
        }
        let due = Instant::now() + wait.min(LONGEST_WAIT);
        let mut sdl_events: Vec<Event> = event_pump.poll_iter().collect();
        while sdl_events.is_empty() {
            let now = Instant::now();
            if now >= due || wake.wait(POLL.min(due - now)) {
                break;
            }
            sdl_events = event_pump.poll_iter().collect();
        }
        let (w, _) = sdl_window.size();
        let (pw, ph) = sdl_window.drawable_size();
        let native_ppp = if w > 0 { pw as f32 / w as f32 } else { 1.0 };
        let ppp = ctx.pixels_per_point();
        // SDL reports the pointer in window units; egui wants points.
        let to_points = native_ppp / ppp;
        for event in sdl_events {
            match event {
                Event::Quit { .. } => break 'frames,
                Event::ControllerDeviceAdded { which, .. } => {
                    if pads.open(&controllers, which) {
                        let _ = pad_tx.connected();
                    }
                }
                Event::ControllerDeviceRemoved { which, .. } => pads.close(which),
                _ if hidden => {}
                Event::ControllerButtonDown { button, .. } => {
                    if let Some(action) = pads.press(button) {
                        let _ = pad_tx.send(action);
                    }
                }
                Event::ControllerButtonUp { button, .. } => pads.release(button),
                Event::ControllerAxisMotion { axis, value, .. } => {
                    if let Some(action) = pads.axis(axis, value) {
                        let _ = pad_tx.send(action);
                    }
                }
                other => input.translate(other, to_points),
            }
        }
        for direction in pads.repeat() {
            if !hidden {
                let _ = pad_tx.send(Action::MoveFocus(direction));
            }
        }

        let screen =
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(pw as f32, ph as f32) / ppp);
        let viewport = egui::ViewportInfo {
            native_pixels_per_point: Some(native_ppp),
            inner_rect: Some(screen),
            outer_rect: Some(screen),
            focused: Some(true),
            fullscreen: Some(window.fullscreen),
            ..Default::default()
        };
        let raw = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            viewports: std::iter::once((egui::ViewportId::ROOT, viewport)).collect(),
            screen_rect: Some(screen),
            max_texture_side: Some(painter.max_texture_side()),
            time: Some(started.elapsed().as_secs_f64()),
            events: std::mem::take(&mut input.events),
            focused: true,
            ..Default::default()
        };
        let output = ctx.run_ui(raw, |ui| {
            app.update_logic(ui.ctx());
            app.update_ui(ui);
        });
        let egui::FullOutput {
            platform_output,
            mut textures_delta,
            shapes,
            pixels_per_point,
            viewport_output,
        } = output;
        wait = Duration::MAX;
        let mut close = false;
        if let Some(root) = viewport_output.get(&egui::ViewportId::ROOT) {
            wait = root.repaint_delay;
            for command in &root.commands {
                match command {
                    egui::ViewportCommand::Close => close = true,
                    egui::ViewportCommand::Screenshot(data) => shots.push(data.clone()),
                    egui::ViewportCommand::Minimized(minimized) => {
                        if hidden && !*minimized {
                            scrub = true;
                            wait = Duration::ZERO;
                        }
                        hidden = *minimized;
                    }
                    // Focus and the rest mean nothing on a single-window
                    // screen.
                    _ => {}
                }
            }
        }
        for command in platform_output.commands {
            if let egui::OutputCommand::OpenUrl(url) = command
                && let Err(error) = open::that_detached(&url.url)
            {
                log::warn!("opening {}: {error}", url.url);
            }
        }
        if hidden && !close {
            // Nothing is drawn, but egui's textures move on (the font
            // atlas grows, covers arrive) and the painter has to follow or
            // the next visible frame draws with stale ones.
            for (id, deltas) in &textures_delta.set {
                for delta in deltas {
                    painter.set_texture(*id, delta);
                }
            }
            for id in &textures_delta.free {
                painter.free_texture(*id);
            }
            continue;
        }
        let primitives = ctx.tessellate(shapes, pixels_per_point);
        if std::mem::take(&mut scrub) {
            for _ in 0..2 {
                painter.clear([pw, ph], SCRUB_COLOR);
                sdl_window.gl_swap_window();
            }
        }
        painter.clear([pw, ph], [0.0, 0.0, 0.0, 1.0]);
        painter.paint_and_update_textures(
            [pw, ph],
            pixels_per_point,
            &primitives,
            &mut textures_delta,
        );
        for data in shots.drain(..) {
            let image = painter.read_screen_rgba([pw, ph]);
            input.events.push(egui::Event::Screenshot {
                viewport_id: egui::ViewportId::ROOT,
                user_data: data,
                image: Arc::new(image),
            });
            wait = Duration::ZERO;
        }
        sdl_window.gl_swap_window();
        if close {
            break;
        }
    }
    app.on_close();
    painter.destroy();
    Ok(())
}

/// Keyboard and mouse state between frames, as egui events.
#[derive(Default)]
struct Input {
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
    pointer: egui::Pos2,
}

impl Input {
    fn translate(&mut self, event: Event, to_points: f32) {
        match event {
            Event::KeyDown {
                keycode: Some(keycode),
                keymod,
                repeat,
                ..
            } => {
                self.modifiers = modifiers(keymod);
                if let Some(key) = key(keycode) {
                    self.events.push(egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat,
                        modifiers: self.modifiers,
                    });
                }
            }
            Event::KeyUp {
                keycode: Some(keycode),
                keymod,
                ..
            } => {
                self.modifiers = modifiers(keymod);
                if let Some(key) = key(keycode) {
                    self.events.push(egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: false,
                        repeat: false,
                        modifiers: self.modifiers,
                    });
                }
            }
            Event::TextInput { text, .. } => {
                if !text.is_empty() {
                    self.events.push(egui::Event::Text(text));
                }
            }
            Event::MouseMotion { x, y, .. } => {
                self.pointer = egui::pos2(x as f32, y as f32) * to_points;
                self.events.push(egui::Event::PointerMoved(self.pointer));
            }
            Event::MouseButtonDown {
                mouse_btn, x, y, ..
            }
            | Event::MouseButtonUp {
                mouse_btn, x, y, ..
            } => {
                let pressed = matches!(event, Event::MouseButtonDown { .. });
                let button = match mouse_btn {
                    MouseButton::Left => egui::PointerButton::Primary,
                    MouseButton::Right => egui::PointerButton::Secondary,
                    MouseButton::Middle => egui::PointerButton::Middle,
                    MouseButton::X1 => egui::PointerButton::Extra1,
                    MouseButton::X2 => egui::PointerButton::Extra2,
                    MouseButton::Unknown => return,
                };
                self.pointer = egui::pos2(x as f32, y as f32) * to_points;
                self.events.push(egui::Event::PointerButton {
                    pos: self.pointer,
                    button,
                    pressed,
                    modifiers: self.modifiers,
                });
            }
            Event::MouseWheel {
                precise_x,
                precise_y,
                ..
            } => self.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(precise_x, precise_y),
                phase: egui::TouchPhase::Move,
                modifiers: self.modifiers,
            }),
            Event::Window {
                win_event: WindowEvent::Leave,
                ..
            } => self.events.push(egui::Event::PointerGone),
            _ => {}
        }
    }
}

fn modifiers(keymod: Mod) -> egui::Modifiers {
    let ctrl = keymod.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
    egui::Modifiers {
        alt: keymod.intersects(Mod::LALTMOD | Mod::RALTMOD),
        ctrl,
        shift: keymod.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD),
        mac_cmd: false,
        command: ctrl,
    }
}

fn key(keycode: Keycode) -> Option<egui::Key> {
    use egui::Key;
    Some(match keycode {
        Keycode::Up => Key::ArrowUp,
        Keycode::Down => Key::ArrowDown,
        Keycode::Left => Key::ArrowLeft,
        Keycode::Right => Key::ArrowRight,
        Keycode::Return | Keycode::KpEnter => Key::Enter,
        Keycode::Escape => Key::Escape,
        Keycode::Tab => Key::Tab,
        Keycode::Backspace => Key::Backspace,
        Keycode::Delete => Key::Delete,
        Keycode::Space => Key::Space,
        Keycode::Home => Key::Home,
        Keycode::End => Key::End,
        Keycode::PageUp => Key::PageUp,
        Keycode::PageDown => Key::PageDown,
        // Letters and digits share egui's names.
        other => Key::from_name(&other.name())?,
    })
}

/// Open controllers and their held state. Buttons that mean a direction
/// repeat while held, the same as a stick.
struct Pads {
    open: Vec<GameController>,
    stick: Stick,
    dpad: Stick,
    stick_pos: (f32, f32),
    dpad_pos: (f32, f32),
    left_trigger: bool,
    right_trigger: bool,
}

impl Pads {
    /// SDL announces the controllers already attached with device-added
    /// events at startup, so they are all opened from the event loop.
    fn new() -> Self {
        Self {
            open: Vec::new(),
            stick: Stick::default(),
            dpad: Stick::default(),
            stick_pos: (0.0, 0.0),
            dpad_pos: (0.0, 0.0),
            left_trigger: false,
            right_trigger: false,
        }
    }

    /// Opens a newly seen controller; true when it is one and is new.
    fn open(&mut self, controllers: &sdl2::GameControllerSubsystem, index: u32) -> bool {
        if !controllers.is_game_controller(index) {
            log::info!("joystick {index} has no controller mapping");
            return false;
        }
        match controllers.open(index) {
            Ok(pad)
                if self
                    .open
                    .iter()
                    .any(|p| p.instance_id() == pad.instance_id()) =>
            {
                false
            }
            Ok(pad) => {
                log::info!("gamepad: {}", pad.name());
                self.open.push(pad);
                true
            }
            Err(error) => {
                log::warn!("opening controller {index}: {error}");
                false
            }
        }
    }

    fn close(&mut self, id: u32) {
        self.open.retain(|pad| pad.instance_id() != id);
        log::info!("gamepad disconnected");
    }

    fn press(&mut self, button: Button) -> Option<Action> {
        let pad = match button {
            Button::DPadUp => return self.dpad_set(0.0, 1.0),
            Button::DPadDown => return self.dpad_set(0.0, -1.0),
            Button::DPadLeft => return self.dpad_set(-1.0, 0.0),
            Button::DPadRight => return self.dpad_set(1.0, 0.0),
            Button::A => PadButton::South,
            Button::B => PadButton::East,
            Button::Y => PadButton::North,
            Button::LeftShoulder => PadButton::LeftBumper,
            Button::RightShoulder => PadButton::RightBumper,
            Button::Guide => PadButton::Guide,
            Button::Start => PadButton::Start,
            _ => return None,
        };
        button_action(pad)
    }

    fn release(&mut self, button: Button) {
        if matches!(
            button,
            Button::DPadUp | Button::DPadDown | Button::DPadLeft | Button::DPadRight
        ) {
            self.dpad_set(0.0, 0.0);
        }
    }

    fn dpad_set(&mut self, x: f32, y: f32) -> Option<Action> {
        self.dpad_pos = (x, y);
        self.dpad.update(x, y).map(Action::MoveFocus)
    }

    fn axis(&mut self, axis: Axis, value: i16) -> Option<Action> {
        let unit = value as f32 / i16::MAX as f32;
        match axis {
            Axis::LeftX => self.stick_pos.0 = unit,
            // SDL's y grows downward; the stick logic wants up positive.
            Axis::LeftY => self.stick_pos.1 = -unit,
            Axis::TriggerLeft | Axis::TriggerRight => {
                let held = if axis == Axis::TriggerLeft {
                    &mut self.left_trigger
                } else {
                    &mut self.right_trigger
                };
                let pressed = value > TRIGGER_PRESSED;
                let edge = pressed && !*held;
                *held = pressed;
                if edge {
                    return button_action(if axis == Axis::TriggerLeft {
                        PadButton::LeftTrigger
                    } else {
                        PadButton::RightTrigger
                    });
                }
                return None;
            }
            _ => return None,
        }
        self.stick
            .update(self.stick_pos.0, self.stick_pos.1)
            .map(Action::MoveFocus)
    }

    /// Repeats due now for anything still held.
    fn repeat(&mut self) -> Vec<crate::model::Direction> {
        let mut moves = Vec::new();
        if self.stick.deadline().is_some() {
            moves.extend(self.stick.update(self.stick_pos.0, self.stick_pos.1));
        }
        if self.dpad.deadline().is_some() {
            moves.extend(self.dpad.update(self.dpad_pos.0, self.dpad_pos.1));
        }
        moves
    }
}

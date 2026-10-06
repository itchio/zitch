//! The boot animation: the screen powers on like a CRT, the logo snaps
//! into focus, then flies to its place in the header while the page comes
//! in. Quitting powers the screen back off.

use std::time::Instant;

use egui::{Color32, Mesh, Rect, TextureHandle, pos2, vec2};

/// Milliseconds. The beam, then the logo until `LOGO`, then the handoff.
const BEAM: f32 = 110.0;
const LOGO: f32 = 600.0;
const TOTAL: f32 = 1240.0;
/// Longest step one frame may take, so a slow first frame skips nothing.
const MAX_STEP: f32 = 50.0;
/// Power off: the screen closes to a line, the line to a dot, the dot fades.
const CLOSE: f32 = 160.0;
const SHRINK: f32 = 140.0;
const FADE: f32 = 80.0;
const STRIPS: usize = 10;
/// Additive, so the two copies make white where they overlap.
const RED: Color32 = Color32::from_rgb_additive(255, 42, 58);
const CYAN: Color32 = Color32::from_rgb_additive(32, 240, 255);

/// Milliseconds of animation played so far.
struct Clock {
    at: f32,
    last: Option<Instant>,
}

impl Clock {
    fn starting_at(at: f32) -> Self {
        Self { at, last: None }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last {
            self.at += ((now - last).as_secs_f32() * 1000.0).min(MAX_STEP);
        }
        self.last = Some(now);
    }
}

fn entrance_id() -> egui::Id {
    egui::Id::new("intro-entrance")
}

/// How far a piece of the page is into its entrance, 0 to 1, when its
/// turn starts `delay` ms into the handoff and takes `length` ms.
pub fn entered(ctx: &egui::Context, delay: f32, length: f32) -> f32 {
    match ctx.data(|data| data.get_temp::<f32>(entrance_id())) {
        Some(since) => out_cubic(span(since, delay, length)),
        None => 1.0,
    }
}

pub struct Intro {
    clock: Clock,
    /// Where the header draws the logo.
    home: Option<Rect>,
}

impl Intro {
    pub fn new(play: bool) -> Self {
        Self {
            clock: Clock::starting_at(if play { 0.0 } else { TOTAL }),
            home: None,
        }
    }

    pub fn running(&self) -> bool {
        self.clock.at < TOTAL
    }

    pub fn skip(&mut self) {
        self.clock.at = TOTAL;
    }

    pub fn set_home(&mut self, home: Option<Rect>) {
        self.home = home;
    }

    /// Advances the animation; call before the page draws.
    pub fn tick(&mut self, ctx: &egui::Context) {
        if !self.running() {
            ctx.data_mut(|data| data.remove::<f32>(entrance_id()));
            return;
        }
        self.clock.tick();
        ctx.data_mut(|data| data.insert_temp(entrance_id(), self.clock.at - LOGO));
        ctx.request_repaint();
    }

    pub fn draw(&self, ctx: &egui::Context, screen: Rect, logo: Option<&TextureHandle>) {
        if !self.running() {
            return;
        }
        let t = self.clock.at;
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("intro"));
        let painter = ctx.layer_painter(layer).with_clip_rect(screen);
        // Sizes are in 480ths of the screen height.
        let unit = screen.height() / 480.0;
        let middle = screen.center();

        let cover = 1.0 - span(t, LOGO, 150.0);
        painter.rect_filled(screen, 0.0, crate::ui::BG.gamma_multiply(cover));

        if let Some(texture) = logo {
            let size = texture.size_vec2();
            let width = screen.width() * 0.5625;
            let center = Rect::from_center_size(middle, vec2(width, width * size.y / size.x));
            if t >= LOGO {
                let k = in_out_quart(span(t, LOGO, 320.0));
                let rect = center.lerp_towards(&self.home.unwrap_or(center), k);
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                painter.image(texture.id(), rect, uv, Color32::WHITE);
            } else if t >= BEAM {
                painter.add(split_logo(texture, center, t, unit));
            }
        }

        if t < BEAM {
            let width = (4.0 + out_cubic(t / BEAM) * 640.0) * unit;
            let beam = |height: f32| Rect::from_center_size(middle, vec2(width, height * unit));
            painter.rect_filled(beam(11.0), 0.0, Color32::from_white_alpha(40));
            painter.rect_filled(beam(3.0), 0.0, Color32::WHITE);
        } else if t < 300.0 {
            let k = out_cubic(span(t, BEAM, 190.0));
            let height = egui::lerp(3.0 * unit..=screen.height(), k);
            let flash = Rect::from_center_size(middle, vec2(screen.width(), height));
            painter.rect_filled(flash, 0.0, crate::ui::TEXT.gamma_multiply(0.85 * (1.0 - k)));
        }

        let lines = 0.45 * (1.0 - span(t, 300.0, 300.0));
        if lines > 0.0 {
            let color = Color32::from_black_alpha((lines * 255.0) as u8);
            let mut y = screen.top();
            while y < screen.bottom() {
                let row = Rect::from_min_size(pos2(screen.left(), y), vec2(screen.width(), unit));
                painter.rect_filled(row, 0.0, color);
                y += 3.0 * unit;
            }
        }
    }
}

/// The screen switching off, drawn over everything while the app quits.
pub struct PowerOff {
    clock: Clock,
}

impl PowerOff {
    pub fn new(play: bool) -> Self {
        let at = if play { 0.0 } else { CLOSE + SHRINK + FADE };
        Self {
            clock: Clock::starting_at(at),
        }
    }

    /// Draws the next frame. True once the screen is black.
    pub fn draw(&mut self, ctx: &egui::Context, screen: Rect) -> bool {
        self.clock.tick();
        let t = self.clock.at;
        let layer = egui::LayerId::new(egui::Order::Debug, egui::Id::new("power-off"));
        let painter = ctx.layer_painter(layer).with_clip_rect(screen);
        let unit = screen.height() / 480.0;
        let middle = screen.center();
        let line = 3.0 * unit;

        if t < CLOSE {
            // Black closes in from above and below as the picture whites out.
            let k = span(t, 0.0, CLOSE).powi(3);
            let gap = egui::lerp(screen.height()..=line, k);
            let picture = Rect::from_center_size(middle, vec2(screen.width(), gap));
            let above = Rect::from_min_max(screen.min, picture.right_top());
            let below = Rect::from_min_max(picture.left_bottom(), screen.max);
            painter.rect_filled(above, 0.0, Color32::BLACK);
            painter.rect_filled(below, 0.0, Color32::BLACK);
            painter.rect_filled(picture, 0.0, Color32::WHITE.gamma_multiply(k));
            return false;
        }
        painter.rect_filled(screen, 0.0, Color32::BLACK);
        let done = t >= CLOSE + SHRINK + FADE;
        if !done {
            let width = egui::lerp(
                screen.width()..=2.0 * line,
                out_cubic(span(t, CLOSE, SHRINK)),
            );
            let fade = 1.0 - span(t, CLOSE + SHRINK, FADE);
            let glow = Rect::from_center_size(middle, vec2(width + 8.0 * unit, 11.0 * unit));
            painter.rect_filled(glow, 0.0, Color32::from_white_alpha((40.0 * fade) as u8));
            painter.rect_filled(
                Rect::from_center_size(middle, vec2(width, line)),
                0.0,
                Color32::WHITE.gamma_multiply(fade),
            );
        }
        done
    }
}

/// The logo unfolding from the beam as red and cyan copies that converge,
/// cut into strips that jump sideways on glitch frames.
fn split_logo(texture: &TextureHandle, center: Rect, t: f32, unit: f32) -> Mesh {
    let open = out_cubic(span(t, BEAM, 150.0)).max(0.01);
    let converged = out_cubic(span(t, BEAM, 300.0));
    let frame = (t / 40.0).floor();
    let glitch = t < 400.0 && noise(frame) < 0.45;
    let jitter = if glitch {
        (noise(frame + 7.0) - 0.5) * 22.0
    } else {
        0.0
    };
    let split = (34.0 * (1.0 - converged) + jitter) * unit;

    let mut mesh = Mesh::with_texture(texture.id());
    for strip in 0..STRIPS {
        let v0 = strip as f32 / STRIPS as f32;
        let v1 = (strip + 1) as f32 / STRIPS as f32;
        let shift = if glitch {
            (noise(frame * 13.0 + strip as f32) - 0.5) * 70.0 * (1.0 - converged) * unit
        } else {
            0.0
        };
        let y = |v: f32| center.center().y + (v - 0.5) * center.height() * open;
        let uv = Rect::from_min_max(pos2(0.0, v0), pos2(1.0, v1));
        for (color, dx) in [(RED, shift - split), (CYAN, shift + split)] {
            let rect = Rect::from_min_max(
                pos2(center.left() + dx, y(v0)),
                pos2(center.right() + dx, y(v1)),
            );
            mesh.add_rect_with_uv(rect, uv, color);
        }
    }
    mesh
}

/// How far `t` is through the `length` ms that start at `start`, 0 to 1.
fn span(t: f32, start: f32, length: f32) -> f32 {
    ((t - start) / length).clamp(0.0, 1.0)
}

fn out_cubic(k: f32) -> f32 {
    1.0 - (1.0 - k).powi(3)
}

fn in_out_quart(k: f32) -> f32 {
    if k < 0.5 {
        8.0 * k.powi(4)
    } else {
        1.0 - (-2.0 * k + 2.0).powi(4) / 2.0
    }
}

/// The same 0 to 1 value for the same seed.
fn noise(seed: f32) -> f32 {
    ((seed * 127.1 + 311.7).sin() * 43758.547).rem_euclid(1.0)
}

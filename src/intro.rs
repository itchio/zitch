//! The boot animation: the screen powers on like a CRT, the logo snaps
//! into focus, then flies to its place in the header.

use std::time::Instant;

use egui::{Color32, Mesh, Rect, TextureHandle, pos2, vec2};

/// Milliseconds. The beam, then the logo until `LOGO`, then the handoff.
const BEAM: f32 = 110.0;
const LOGO: f32 = 600.0;
const TOTAL: f32 = 1080.0;
/// Longest step one frame may take, so a slow first frame skips nothing.
const MAX_STEP: f32 = 50.0;
const STRIPS: usize = 10;
/// Additive, so the two copies make white where they overlap.
const RED: Color32 = Color32::from_rgb_additive(255, 42, 58);
const CYAN: Color32 = Color32::from_rgb_additive(32, 240, 255);

pub struct Intro {
    at: f32,
    last: Option<Instant>,
    /// Where the header draws the logo.
    home: Option<Rect>,
}

impl Intro {
    pub fn new(play: bool) -> Self {
        Self {
            at: if play { 0.0 } else { TOTAL },
            last: None,
            home: None,
        }
    }

    pub fn running(&self) -> bool {
        self.at < TOTAL
    }

    pub fn skip(&mut self) {
        self.at = TOTAL;
    }

    pub fn set_home(&mut self, home: Option<Rect>) {
        self.home = home;
    }

    pub fn draw(&mut self, ctx: &egui::Context, screen: Rect, logo: Option<&TextureHandle>) {
        if !self.running() {
            return;
        }
        let now = Instant::now();
        if let Some(last) = self.last {
            self.at += ((now - last).as_secs_f32() * 1000.0).min(MAX_STEP);
        }
        self.last = Some(now);
        ctx.request_repaint();

        let t = self.at;
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("intro"));
        let painter = ctx.layer_painter(layer).with_clip_rect(screen);
        // Sizes are in 480ths of the screen height.
        let unit = screen.height() / 480.0;
        let middle = screen.center();

        let cover = 1.0 - span(t, LOGO, 250.0);
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

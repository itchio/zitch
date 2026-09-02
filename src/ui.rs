//! Drawing. Views read the app state and never mutate it.

use std::sync::Arc;
use std::time::Instant;

use egui::{Color32, CornerRadius, FontId, Rect, Sense, Stroke, TextureHandle, Ui, pos2, vec2};

use crate::images::{Animation, CoverLoader};
use crate::model::{Action, Cave, Game, InstallState, Page, UploadExt};

pub const BG: Color32 = Color32::from_rgb(0x14, 0x12, 0x1a);
const TILE_BG: Color32 = Color32::from_rgb(0x24, 0x21, 0x2e);
const TILE_HOVER: Color32 = Color32::from_rgb(0x34, 0x30, 0x42);
const TEXT: Color32 = Color32::from_gray(0xee);
const ACCENT: Color32 = Color32::from_rgb(0xfa, 0x5c, 0x5c);
const DIM: Color32 = Color32::from_gray(0x99);
const GREEN: Color32 = Color32::from_rgb(0x4c, 0xc9, 0x6b);

/// itch.io covers are 315x250; tiles keep that shape.
const COVER_ASPECT: f32 = 315.0 / 250.0;
const TILE_WIDTH: f32 = 170.0;
const GAP: f32 = 14.0;
const TITLE_HEIGHT: f32 = 26.0;

/// Where the library grid is and what it points at. Drawing fills in
/// `columns` and `scroll`; actions move `focus`.
#[derive(Default)]
pub struct Grid {
    pub focus: usize,
    /// Scroll so the focused tile is in view on the next frame.
    pub follow: bool,
    pub columns: usize,
    scroll: f32,
    last_pointer: Option<egui::Pos2>,
    /// The focused tile's animated cover, while it has one.
    playing: Option<Playing>,
}

struct Playing {
    url: String,
    animation: Arc<Animation>,
    started: Instant,
    /// Uploaded the first time each frame is shown, so starting playback
    /// never sends the whole gif to the GPU in one frame.
    textures: Vec<Option<TextureHandle>>,
}

impl Game {
    /// The animated cover, when the game has one distinct from its still.
    fn animated_cover(&self) -> Option<&str> {
        let cover = self.cover_url.as_deref()?;
        let is_gif = cover
            .rsplit('.')
            .next()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gif"));
        (is_gif && self.still_cover_url.as_deref() != Some(cover)).then_some(cover)
    }
}

pub fn library(
    ui: &mut Ui,
    games: &[Game],
    installed: &std::collections::HashSet<i64>,
    installs: &std::collections::HashMap<i64, InstallState>,
    grid: &mut Grid,
    covers: &CoverLoader,
    actions: &mut Vec<Action>,
) {
    let available = ui.available_width();
    let columns = ((available + GAP) / (TILE_WIDTH + GAP)).floor().max(1.0) as usize;
    let tile_width = (available - GAP * (columns as f32 - 1.0)) / columns as f32;
    let cover_height = tile_width / COVER_ASPECT;
    let row_height = cover_height + TITLE_HEIGHT + GAP;
    let rows = games.len().div_ceil(columns);
    grid.columns = columns;

    let viewport = ui.available_height();
    let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
    if std::mem::take(&mut grid.follow) {
        let row = grid.focus / columns;
        let top = row as f32 * row_height;
        let bottom = top + row_height;
        let margin = GAP;
        let mut offset = grid.scroll;
        if top - margin < offset {
            offset = top - margin;
        } else if bottom + margin > offset + viewport {
            offset = bottom + margin - viewport;
        }
        area = area.vertical_scroll_offset(offset.max(0.0));
    }
    // Only a pointer that moved between two frames takes focus, so the
    // keyboard keeps it while the mouse rests on a tile, and a window that
    // opens under the cursor does not start focused on whatever is beneath.
    let pointer = ui.input(|input| input.pointer.latest_pos());
    let pointer_moved = matches!((grid.last_pointer, pointer), (Some(a), Some(b)) if a != b);
    grid.last_pointer = pointer;

    // Playback follows focus: the focused game's animation, or none.
    let wanted = games.get(grid.focus).and_then(Game::animated_cover);
    if grid.playing.as_ref().map(|p| p.url.as_str()) != wanted {
        grid.playing = None;
    }
    if let Some(url) = wanted
        && grid.playing.is_none()
        && let Some(animation) = covers.animation(ui.ctx(), url)
    {
        let textures = vec![None; animation.frames.len()];
        log::debug!("playing {} frames of {url}", animation.frames.len());
        grid.playing = Some(Playing {
            url: url.to_string(),
            animation,
            started: Instant::now(),
            textures,
        });
    }
    let mut playing = grid.playing.take();

    let output = area.show_rows(ui, row_height, rows, |ui, range| {
        for row in range {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for column in 0..columns {
                    let index = row * columns + column;
                    let Some(game) = games.get(index) else {
                        break;
                    };
                    let (rect, response) = ui.allocate_exact_size(
                        vec2(tile_width, cover_height + TITLE_HEIGHT),
                        Sense::click(),
                    );
                    if response.hovered() && pointer_moved {
                        actions.push(Action::FocusIndex(index));
                    }
                    if response.clicked() {
                        actions.push(Action::FocusIndex(index));
                        actions.push(Action::Activate);
                    }
                    if ui.is_rect_visible(rect) {
                        let focused = index == grid.focus;
                        let animation = if focused { playing.as_mut() } else { None };
                        let tile = Tile {
                            game,
                            focused,
                            installed: installed.contains(&game.id),
                            install: installs.get(&game.id),
                        };
                        draw_tile(ui, rect, cover_height, tile, animation);
                    }
                }
            });
            ui.add_space(GAP - ui.spacing().item_spacing.y);
        }
    });
    grid.scroll = output.state.offset.y;
    grid.playing = playing;
}

struct Tile<'a> {
    game: &'a Game,
    focused: bool,
    installed: bool,
    install: Option<&'a InstallState>,
}

fn draw_tile(ui: &Ui, rect: Rect, cover_height: f32, tile: Tile, playing: Option<&mut Playing>) {
    let Tile {
        game,
        focused,
        installed,
        install,
    } = tile;
    let cover = Rect::from_min_size(rect.min, vec2(rect.width(), cover_height));
    let radius = CornerRadius::same(6);
    let painted = match playing {
        Some(playing) => {
            paint_frame(ui, playing, cover, radius);
            true
        }
        None => {
            // stillCoverUrl is the static frame of an animated cover; those
            // gifs run to megabytes and only play while focused.
            let url = game
                .still_cover_url
                .as_deref()
                .or(game.cover_url.as_deref());
            url.is_some_and(|url| paint_cover(ui, url, cover, radius))
        }
    };
    if !painted {
        let fill = if focused { TILE_HOVER } else { TILE_BG };
        ui.painter().rect_filled(cover, radius, fill);
        // No art: the title stands in for it, wrapped inside the cover.
        let galley = ui.painter().layout(
            game.title.clone(),
            FontId::proportional(15.0),
            DIM,
            cover.width() - 24.0,
        );
        let pos = cover.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, DIM);
    }
    if let Some(install) = install {
        let bar = Rect::from_min_max(
            pos2(cover.left(), cover.bottom() - 6.0),
            cover.right_bottom(),
        );
        progress_bar(ui, bar, install.progress as f32);
    } else if installed {
        badge(
            ui,
            pos2(cover.left() + 8.0, cover.bottom() - 8.0),
            "INSTALLED",
        );
    }
    if focused {
        ui.painter().rect_stroke(
            cover.expand(2.0),
            CornerRadius::same(8),
            Stroke::new(3.0, ACCENT),
            egui::StrokeKind::Outside,
        );
    }
    let title_rect = Rect::from_min_max(
        pos2(rect.left(), cover.bottom() + 6.0),
        pos2(rect.right(), rect.bottom()),
    );
    let galley = ui.painter().layout(
        game.title.clone(),
        FontId::proportional(13.5),
        if focused { TEXT } else { DIM },
        f32::INFINITY,
    );
    ui.painter()
        .with_clip_rect(title_rect)
        .galley(title_rect.left_top(), galley, DIM);
}

/// Paints the current frame of a playing animation and schedules the next.
fn paint_frame(ui: &Ui, playing: &mut Playing, rect: Rect, radius: CornerRadius) {
    let (index, until_next) = playing.animation.frame_at(playing.started.elapsed());
    let texture = playing.textures[index].get_or_insert_with(|| {
        ui.ctx().load_texture(
            format!("{}#{index}", playing.url),
            Arc::clone(&playing.animation.frames[index]),
            egui::TextureOptions::LINEAR,
        )
    });
    paint_texture(
        ui,
        egui::load::SizedTexture::from_handle(texture),
        rect,
        radius,
    );
    if playing.animation.frames.len() > 1 {
        ui.ctx().request_repaint_after(until_next);
    }
}

/// Paints the cover cropped to fill `rect`, or returns false while it is
/// still loading or has failed.
fn paint_cover(ui: &Ui, url: &str, rect: Rect, radius: CornerRadius) -> bool {
    let image = egui::Image::new(url);
    let Ok(egui::load::TexturePoll::Ready { texture }) = image.load_for_size(ui.ctx(), rect.size())
    else {
        return false;
    };
    paint_texture(ui, texture, rect, radius);
    true
}

fn paint_texture(ui: &Ui, texture: egui::load::SizedTexture, rect: Rect, radius: CornerRadius) {
    let image_aspect = texture.size.x / texture.size.y;
    let rect_aspect = rect.width() / rect.height();
    let uv = if image_aspect > rect_aspect {
        let visible = rect_aspect / image_aspect;
        let inset = (1.0 - visible) / 2.0;
        Rect::from_min_max(pos2(inset, 0.0), pos2(1.0 - inset, 1.0))
    } else {
        let visible = image_aspect / rect_aspect;
        let inset = (1.0 - visible) / 2.0;
        Rect::from_min_max(pos2(0.0, inset), pos2(1.0, 1.0 - inset))
    };
    egui::Image::new(texture)
        .uv(uv)
        .corner_radius(radius)
        .paint_at(ui, rect);
}

pub fn heading(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(FontId::proportional(30.0))
            .color(TEXT),
    );
}

pub fn subtle(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(FontId::proportional(14.0))
            .color(DIM),
    );
}

pub fn error(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(FontId::proportional(14.0))
            .color(Color32::from_rgb(0xff, 0x6e, 0x6e)),
    );
}

pub fn centered_spinner(ui: &mut Ui) {
    let rect = ui.available_rect_before_wrap();
    let center = rect.center();
    let mut child = ui.new_child(
        egui::UiBuilder::new().max_rect(Rect::from_center_size(center, vec2(40.0, 40.0))),
    );
    child.add(egui::Spinner::new().size(32.0).color(DIM));
}

/// A small label anchored by its bottom-left corner.
fn badge(ui: &Ui, bottom_left: egui::Pos2, text: &str) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(9.5), BG);
    let size = galley.size() + vec2(12.0, 6.0);
    let rect = Rect::from_min_size(bottom_left - vec2(0.0, size.y), size);
    ui.painter().rect_filled(rect, CornerRadius::same(3), GREEN);
    ui.painter().galley(rect.min + vec2(6.0, 3.0), galley, BG);
}

/// What the detail page offers for a game, in button order.
pub fn game_buttons(
    game: &Game,
    caves: &[&Cave],
    install: Option<&InstallState>,
) -> Vec<(&'static str, Action)> {
    if let Some(install) = install {
        if install.cancelling {
            return Vec::new();
        }
        if install.error.is_some() {
            return vec![
                ("Retry", Action::RetryInstall { game_id: game.id }),
                ("Dismiss", Action::CancelInstall { game_id: game.id }),
            ];
        }
        return vec![("Cancel", Action::CancelInstall { game_id: game.id })];
    }
    match caves.first() {
        Some(cave) => vec![
            (
                "Play",
                Action::Play {
                    cave_id: cave.id.clone(),
                },
            ),
            (
                "Uninstall",
                Action::Uninstall {
                    cave_id: cave.id.clone(),
                },
            ),
        ],
        None => vec![("Install", Action::Install { game_id: game.id })],
    }
}

pub fn game_detail(
    ui: &mut Ui,
    game: &Game,
    caves: &[&Cave],
    install: Option<&InstallState>,
    focused_button: usize,
    actions: &mut Vec<Action>,
) {
    let buttons = game_buttons(game, caves, install);
    let width = ui.available_width();
    let cover_width = (width * 0.42).min(420.0);
    let cover_height = cover_width / COVER_ASPECT;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 28.0;
        let (cover, _) = ui.allocate_exact_size(vec2(cover_width, cover_height), Sense::hover());
        let radius = CornerRadius::same(8);
        let url = game
            .still_cover_url
            .as_deref()
            .or(game.cover_url.as_deref());
        if !url.is_some_and(|url| paint_cover(ui, url, cover, radius)) {
            ui.painter().rect_filled(cover, radius, TILE_BG);
        }
        ui.vertical(|ui| {
            ui.set_max_width(width - cover_width - 28.0);
            ui.label(
                egui::RichText::new(&game.title)
                    .font(FontId::proportional(26.0))
                    .color(TEXT),
            );
            if let Some(text) = game.short_text.as_deref().filter(|t| !t.is_empty()) {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(text)
                        .font(FontId::proportional(14.0))
                        .color(DIM),
                );
            }
            ui.add_space(12.0);
            match (install, caves.first()) {
                (Some(install), _) => {
                    let line = if let Some(error) = &install.error {
                        format!("Failed: {error}")
                    } else if install.cancelling {
                        "Cancelling".to_string()
                    } else if install.bps > 0.0 {
                        format!(
                            "{}, {:.0}%, {}/s, {} left",
                            install.stage,
                            install.progress * 100.0,
                            human_size(install.bps as i64),
                            human_duration_seconds(install.eta_seconds as i64),
                        )
                    } else {
                        format!("{}, {:.0}%", install.stage, install.progress * 100.0)
                    };
                    ui.label(
                        egui::RichText::new(line)
                            .font(FontId::proportional(13.0))
                            .color(ACCENT),
                    );
                    ui.add_space(8.0);
                    let (bar, _) = ui.allocate_exact_size(
                        vec2(ui.available_width().min(420.0), 8.0),
                        Sense::hover(),
                    );
                    progress_bar(ui, bar, install.progress as f32);
                }
                (None, Some(cave)) => {
                    let mut line = String::from("Installed");
                    if let Some(info) = &cave.install_info {
                        line.push_str(&format!(", {}", human_size(info.installed_size)));
                    }
                    if let Some(upload) = &cave.upload {
                        line.push_str(&format!(", {}", upload.name()));
                    }
                    ui.label(
                        egui::RichText::new(line)
                            .font(FontId::proportional(13.0))
                            .color(GREEN),
                    );
                    if let Some(stats) = &cave.stats
                        && stats.seconds_run > 0
                    {
                        subtle(ui, &format!("Played {}", human_duration(stats.seconds_run)));
                    }
                }
                (None, None) => subtle(ui, "Not installed"),
            }
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                for (index, (label, action)) in buttons.iter().enumerate() {
                    let response = pill(ui, label, index == focused_button, index == 0);
                    if response.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                        actions.push(Action::FocusButton(index));
                    }
                    if response.clicked() {
                        actions.push(action.clone());
                    }
                }
            });
            ui.add_space(16.0);
            subtle(ui, "Esc or B to go back");
        });
    });
}

fn pill(ui: &mut Ui, label: &str, focused: bool, primary: bool) -> egui::Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), FontId::proportional(16.0), TEXT);
    let size = galley.size() + vec2(40.0, 18.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = match (primary, focused) {
        (true, _) => ACCENT,
        (false, true) => TILE_HOVER,
        (false, false) => TILE_BG,
    };
    let radius = CornerRadius::same(6);
    ui.painter().rect_filled(rect, radius, fill);
    if focused {
        ui.painter().rect_stroke(
            rect.expand(3.0),
            CornerRadius::same(9),
            Stroke::new(2.5, TEXT),
            egui::StrokeKind::Outside,
        );
    }
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, TEXT);
    response
}

pub fn human_size(bytes: i64) -> String {
    let bytes = bytes.max(0) as f64;
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Short remaining-time text for progress lines.
pub fn human_duration_seconds(seconds: i64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

fn progress_bar(ui: &Ui, rect: Rect, fraction: f32) {
    let radius = CornerRadius::same(3);
    ui.painter().rect_filled(rect, radius, TILE_HOVER);
    let filled = Rect::from_min_size(
        rect.min,
        vec2(rect.width() * fraction.clamp(0.0, 1.0), rect.height()),
    );
    if filled.width() > 0.0 {
        ui.painter().rect_filled(filled, radius, ACCENT);
    }
}

pub fn human_duration(seconds: i64) -> String {
    let minutes = seconds / 60;
    if minutes < 60 {
        format!("{minutes} min")
    } else {
        format!("{}h {:02}m", minutes / 60, minutes % 60)
    }
}

impl Page {
    pub fn is_library(&self) -> bool {
        matches!(self, Page::Library)
    }
}

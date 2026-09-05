//! Drawing. Views read the app state and never mutate it.

use std::sync::Arc;
use std::time::Instant;

use egui::{Color32, CornerRadius, FontId, Rect, Sense, Stroke, TextureHandle, Ui, pos2, vec2};

use crate::glyphs::{Glyph, Glyphs, InputMode};
use crate::images::{Animation, CoverLoader, Variant};
use crate::model::{
    Action, Cave, Direction, Game, GameUpdate, InstallState, Page, Prompt, Tab, UploadExt,
};

pub const BG: Color32 = Color32::from_rgb(0x14, 0x12, 0x1a);
const TILE_BG: Color32 = Color32::from_rgb(0x24, 0x21, 0x2e);
const TILE_HOVER: Color32 = Color32::from_rgb(0x34, 0x30, 0x42);
const TEXT: Color32 = Color32::from_gray(0xee);
const ACCENT: Color32 = Color32::from_rgb(0xfa, 0x5c, 0x5c);
const DIM: Color32 = Color32::from_gray(0x99);
const GREEN: Color32 = Color32::from_rgb(0x4c, 0xc9, 0x6b);
const AMBER: Color32 = Color32::from_rgb(0xf5, 0xb8, 0x3d);

/// itch.io covers are 315x250; tiles keep that shape.
const COVER_ASPECT: f32 = 315.0 / 250.0;

/// Sizes for the screen being drawn, recomputed every frame. The design
/// was drawn on a 800x450 canvas and scales with the screen's height, so a
/// TV across the room and a handheld in the hands get the same layout at
/// their own size. Text has floors that matter at 1x density.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub scale: f32,
    /// Space between the page edge and content.
    pub margin: f32,
    pub tile_width: f32,
    /// Space between tiles in a row.
    pub gap: f32,
    /// Room around a strip for the focus ring, which is painted outside the
    /// cover and would otherwise be clipped at the strip's edges.
    pub ring: f32,
    /// Height of the title line under a tile's cover.
    pub title_height: f32,
    pub header_height: f32,
    pub section_gap: f32,
    pub heading: f32,
    pub title: f32,
    pub dialog: f32,
    pub section: f32,
    pub button: f32,
    pub label: f32,
    pub body: f32,
    pub caption: f32,
    pub badge: f32,
}

impl Metrics {
    const DESIGN_HEIGHT: f32 = 450.0;
    /// Tiles shrink below the design size rather than show fewer than this
    /// across, so a narrow screen still reads as a carousel.
    const MIN_COLUMNS: f32 = 3.5;

    pub fn for_screen(screen: Rect) -> Self {
        let scale = (screen.height() / Self::DESIGN_HEIGHT).clamp(0.8, 2.4);
        // Whole points keep widget edges on pixels at 1x density.
        let space = |base: f32| (base * scale).round();
        let margin = (screen.width() * 0.03).clamp(12.0, 48.0).round();
        let gap = space(14.0);
        let usable = screen.width() - 2.0 * margin;
        let tile_width = space(170.0)
            .min((usable - Self::MIN_COLUMNS * gap) / Self::MIN_COLUMNS)
            .round();
        let font = |base: f32, floor: f32| (base * scale).max(floor);
        Self {
            scale,
            margin,
            tile_width,
            gap,
            ring: space(6.0),
            title_height: space(26.0),
            header_height: space(34.0),
            section_gap: space(22.0),
            heading: font(30.0, 20.0),
            title: font(26.0, 18.0),
            dialog: font(22.0, 16.0),
            section: font(18.0, 14.0),
            button: font(16.0, 13.0),
            label: font(15.0, 12.0),
            body: font(14.0, 12.0),
            caption: font(13.0, 12.0),
            badge: font(9.5, 10.0),
        }
    }

    /// A length from the design canvas, scaled to this screen in whole
    /// points.
    pub fn space(&self, base: f32) -> f32 {
        (base * self.scale).round()
    }
}

/// One finger on the home screen. egui's own drag-to-scroll would give the
/// gesture to whichever row it started on and drop the vertical part, so
/// the list decides the axis itself from the first few points of motion.
struct Swipe {
    axis: Option<usize>,
    row: Option<usize>,
    travel: egui::Vec2,
}

/// Off for comparison against egui's own drag-to-scroll; flip to try the
/// axis-locked swipe again.
const CUSTOM_SWIPE: bool = true;
const SWIPE_LOCK: f32 = 8.0;
const FLING_FRICTION: f32 = 1000.0;
const FLING_STOP: f32 = 20.0;

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

/// One carousel: a title and the game ids it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    pub games: Vec<i64>,
}

/// The home screen's rows of carousels and which tile has focus. Drawing
/// records scroll positions; actions move the focus.
#[derive(Default)]
pub struct Rows {
    pub sections: Vec<Section>,
    pub row: usize,
    /// Where focus sits in each row, so moving down and back up returns to
    /// the same tile, the way console home screens behave.
    cols: Vec<usize>,
    /// Scroll so the focused tile is in view on the next frame.
    pub follow: bool,
    vscroll: f32,
    hscroll: Vec<f32>,
    /// How far each area can scroll, as last laid out, so a swipe stops at
    /// the ends instead of overshooting and snapping back.
    vmax: f32,
    hmax: Vec<f32>,
    /// Each row's top and height as last laid out, relative to the list's
    /// top, so follow-scrolling uses real measurements.
    row_spans: Vec<(f32, f32)>,
    last_pointer: Option<egui::Pos2>,
    /// A touch drag in progress, once it has picked an axis.
    swipe: Option<Swipe>,
    /// Velocity left over from a swipe, in points per second, and the row it
    /// applies to when horizontal.
    fling: egui::Vec2,
    fling_row: Option<usize>,
    /// The focused tile's animated cover, while it has one.
    playing: Option<Playing>,
}

impl Rows {
    pub fn set_sections(&mut self, sections: Vec<Section>) {
        if sections == self.sections {
            return;
        }
        let focused = self.focused_game();
        self.sections = sections;
        self.cols.resize(self.sections.len(), 0);
        self.hscroll.resize(self.sections.len(), 0.0);
        self.hmax.resize(self.sections.len(), 0.0);
        self.row_spans.resize(self.sections.len(), (0.0, 0.0));
        self.row = self.row.min(self.sections.len().saturating_sub(1));
        for (row, section) in self.sections.iter().enumerate() {
            self.cols[row] = self.cols[row].min(section.games.len().saturating_sub(1));
        }
        // Keep pointing at the same game when the rows reshuffle around it.
        if let Some(id) = focused
            && self.focused_game() != Some(id)
        {
            self.focus_game(id);
        }
    }

    pub fn col(&self) -> usize {
        self.cols.get(self.row).copied().unwrap_or(0)
    }

    /// The game id under focus.
    pub fn focused_game(&self) -> Option<i64> {
        self.sections.get(self.row)?.games.get(self.col()).copied()
    }

    pub fn focus_tile(&mut self, row: usize, col: usize) {
        if let Some(section) = self.sections.get(row)
            && col < section.games.len()
        {
            self.row = row;
            self.cols[row] = col;
        }
    }

    /// Focuses a game by id, preferring the current row.
    pub fn focus_game(&mut self, id: i64) {
        let in_current = self
            .sections
            .get(self.row)
            .and_then(|s| s.games.iter().position(|&g| g == id))
            .map(|col| (self.row, col));
        let anywhere = || {
            self.sections
                .iter()
                .enumerate()
                .find_map(|(row, s)| s.games.iter().position(|&g| g == id).map(|c| (row, c)))
        };
        if let Some((row, col)) = in_current.or_else(anywhere) {
            self.focus_tile(row, col);
            self.follow = true;
        }
    }

    pub fn move_focus(&mut self, direction: Direction) {
        if self.sections.is_empty() {
            return;
        }
        match direction {
            Direction::Left => self.cols[self.row] = self.col().saturating_sub(1),
            Direction::Right => {
                let len = self.sections[self.row].games.len();
                self.cols[self.row] = (self.col() + 1).min(len.saturating_sub(1));
            }
            Direction::Up => self.row = self.row.saturating_sub(1),
            Direction::Down => self.row = (self.row + 1).min(self.sections.len() - 1),
        }
        self.follow = true;
    }
}

/// Everything the home screen reads while drawing.
pub struct LibraryView<'a> {
    pub games: &'a std::collections::HashMap<i64, Game>,
    pub installed: &'a std::collections::HashSet<i64>,
    pub installs: &'a std::collections::HashMap<i64, InstallState>,
    pub updatable: &'a std::collections::HashSet<i64>,
    pub covers: &'a CoverLoader,
}

pub fn library(
    ui: &mut Ui,
    m: &Metrics,
    view: LibraryView,
    rows: &mut Rows,
    actions: &mut Vec<Action>,
) {
    let LibraryView {
        games,
        installed,
        installs,
        updatable,
        covers,
    } = view;
    let Metrics {
        tile_width,
        gap,
        ring,
        ..
    } = *m;
    let cover_height = tile_width / COVER_ASPECT;
    let tile_height = cover_height + m.title_height;
    let stride = tile_width + gap;
    let follow = std::mem::take(&mut rows.follow);

    let viewport_height = ui.available_height();
    let list_rect = ui.available_rect_before_wrap();
    let no_drag = if CUSTOM_SWIPE {
        egui::scroll_area::ScrollSource {
            drag: egui::scroll_area::DragScroll::Never,
            ..Default::default()
        }
    } else {
        egui::scroll_area::ScrollSource::default()
    };
    let (set_vscroll, set_hscroll) = swipe(ui, rows, list_rect);
    let mut area = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .scroll_source(no_drag)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden);
    if let Some(offset) = set_vscroll {
        area = area.vertical_scroll_offset(offset);
    } else if follow && let Some(&(top, height)) = rows.row_spans.get(rows.row) {
        let bottom = top + height;
        let mut offset = rows.vscroll;
        if top < offset {
            offset = top;
        } else if bottom > offset + viewport_height {
            offset = bottom - viewport_height;
        }
        area = area.vertical_scroll_offset(offset.max(0.0));
    }

    // Only a pointer that moved between two frames takes focus, so the
    // keyboard keeps it while the mouse rests on a tile, and a window that
    // opens under the cursor does not start focused on whatever is beneath.
    let pointer = ui.input(|input| input.pointer.latest_pos());
    let pointer_moved = matches!((rows.last_pointer, pointer), (Some(a), Some(b)) if a != b)
        && rows.swipe.is_none()
        && rows.fling == egui::Vec2::ZERO;
    rows.last_pointer = pointer;

    // Playback follows focus: the focused game's animation, or none.
    let focused_game = rows.focused_game();
    let wanted = focused_game
        .and_then(|id| games.get(&id))
        .and_then(Game::animated_cover);
    if rows.playing.as_ref().map(|p| p.url.as_str()) != wanted {
        rows.playing = None;
    }
    if let Some(url) = wanted
        && rows.playing.is_none()
        && let Some(animation) = covers.animation(ui.ctx(), url)
    {
        let textures = vec![None; animation.frames.len()];
        log::debug!("playing {} frames of {url}", animation.frames.len());
        rows.playing = Some(Playing {
            url: url.to_string(),
            animation,
            started: Instant::now(),
            textures,
        });
    }
    let mut playing = rows.playing.take();

    let output = area.show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let list_top = ui.min_rect().top();
        for row in 0..rows.sections.len() {
            let row_top = ui.cursor().top() - list_top;
            let section = &rows.sections[row];
            let focused_col = rows.cols[row];
            let is_focused_row = row == rows.row;
            ui.allocate_ui(vec2(ui.available_width(), m.header_height), |ui| {
                ui.label(
                    egui::RichText::new(&section.title)
                        .font(FontId::proportional(m.section))
                        .color(if is_focused_row { TEXT } else { DIM }),
                );
            });

            let mut strip = egui::ScrollArea::horizontal()
                .id_salt(("row", row))
                .auto_shrink([false, false])
                .scroll_source(no_drag)
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .max_height(tile_height + 2.0 * ring);
            if let Some((swiped, offset)) = set_hscroll
                && swiped == row
            {
                strip = strip.horizontal_scroll_offset(offset);
            } else if follow && is_focused_row {
                let left = ring + focused_col as f32 * stride;
                let right = left + tile_width;
                let width = ui.available_width() + 2.0 * ring;
                let mut offset = rows.hscroll[row];
                if left - gap < offset {
                    offset = left - gap;
                } else if right + gap > offset + width {
                    offset = right + gap - width;
                }
                // egui shows an offset past the end for a frame before
                // clamping it, which reads as a shake at the ends of the row.
                let total = section.games.len() as f32 * stride - gap + 2.0 * ring;
                strip = strip.horizontal_scroll_offset(offset.clamp(0.0, (total - width).max(0.0)));
            }
            // The strip's clip region reaches into the page margin on both
            // sides, and the tiles are indented back by the same amount, so
            // they line up with the header while the ring has room.
            let strip_area = ui.available_rect_before_wrap().expand2(vec2(ring, 0.0));
            let mut strip_ui = ui.new_child(egui::UiBuilder::new().max_rect(strip_area));
            let out = strip.show_viewport(&mut strip_ui, |ui, viewport| {
                let count = section.games.len();
                let total = count as f32 * stride - gap + 2.0 * ring;
                let (strip_rect, _) = ui.allocate_exact_size(
                    vec2(total.max(0.0), tile_height + 2.0 * ring),
                    Sense::hover(),
                );
                // Only tiles inside the viewport get drawn; a row can hold
                // the whole library.
                let first = ((viewport.min.x - ring) / stride).floor().max(0.0) as usize;
                let last = (((viewport.max.x - ring) / stride).ceil() as usize).min(count);
                for col in first..last {
                    let Some(game) = games.get(&section.games[col]) else {
                        continue;
                    };
                    let rect = Rect::from_min_size(
                        strip_rect.min + vec2(ring + col as f32 * stride, ring),
                        vec2(tile_width, tile_height),
                    );
                    let response =
                        ui.interact(rect, ui.id().with(("tile", row, col)), Sense::click());
                    if response.hovered() && pointer_moved {
                        actions.push(Action::FocusTile { row, col });
                    }
                    if response.clicked() {
                        actions.push(Action::FocusTile { row, col });
                        actions.push(Action::Activate);
                    }
                    let focused = is_focused_row && col == focused_col;
                    let animation = if focused { playing.as_mut() } else { None };
                    let tile = Tile {
                        game,
                        focused,
                        installed: installed.contains(&game.id),
                        install: installs.get(&game.id),
                        updatable: updatable.contains(&game.id),
                    };
                    draw_tile(ui, m, covers, rect, cover_height, tile, animation);
                }
            });
            // The strip lives in a child ui; move the parent's cursor past it.
            ui.add_space(tile_height + 2.0 * ring);
            rows.hscroll[row] = out.state.offset.x;
            rows.hmax[row] = (out.content_size.x - out.inner_rect.width()).max(0.0);
            rows.row_spans[row] = (row_top, ui.cursor().top() - list_top - row_top);
            ui.add_space(m.section_gap - m.space(6.0));
        }
    });
    rows.vscroll = output.state.offset.y;
    rows.vmax = (output.content_size.y - output.inner_rect.height()).max(0.0);
    rows.playing = playing;
}

/// Reads this frame's touch drag and fling, returning the vertical offset
/// and the (row, offset) to force on the scroll areas, if any.
fn swipe(ui: &mut Ui, rows: &mut Rows, list_rect: Rect) -> (Option<f32>, Option<(usize, f32)>) {
    if !CUSTOM_SWIPE || !ui.input(|i| i.has_touch_screen()) {
        return (None, None);
    }
    let mut set_vscroll = None;
    let mut set_hscroll = None;
    // Sensed before the tiles are added so they still receive their clicks.
    let drag = ui.interact(list_rect, ui.id().with("home-swipe"), Sense::drag());
    if drag.drag_started() {
        let row = drag.interact_pointer_pos().and_then(|pos| {
            let y = pos.y - list_rect.top() + rows.vscroll;
            rows.row_spans
                .iter()
                .position(|&(top, height)| y >= top && y < top + height)
        });
        rows.swipe = Some(Swipe {
            axis: None,
            row,
            travel: egui::Vec2::ZERO,
        });
        rows.fling = egui::Vec2::ZERO;
    }
    if drag.dragged()
        && let Some(swipe) = rows.swipe.as_mut()
    {
        let delta = drag.drag_delta();
        if swipe.axis.is_none() {
            swipe.travel += delta;
            if swipe.travel.length() > SWIPE_LOCK {
                swipe.axis = Some(if swipe.travel.x.abs() > swipe.travel.y.abs() {
                    0
                } else {
                    1
                });
            }
        }
        match (swipe.axis, swipe.row) {
            (Some(1), _) => {
                rows.vscroll = (rows.vscroll - delta.y).clamp(0.0, rows.vmax);
                set_vscroll = Some(rows.vscroll);
            }
            (Some(0), Some(row)) => {
                rows.hscroll[row] = (rows.hscroll[row] - delta.x).clamp(0.0, rows.hmax[row]);
                set_hscroll = Some((row, rows.hscroll[row]));
            }
            _ => {}
        }
    }
    if drag.drag_stopped()
        && let Some(swipe) = rows.swipe.take()
    {
        let velocity = ui.input(|i| i.pointer.velocity());
        rows.fling = match swipe.axis {
            Some(0) => vec2(velocity.x, 0.0),
            Some(1) => vec2(0.0, velocity.y),
            _ => egui::Vec2::ZERO,
        };
        rows.fling_row = swipe.row;
    }
    if rows.fling != egui::Vec2::ZERO {
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        for d in 0..2 {
            let v = &mut rows.fling[d];
            let friction = FLING_FRICTION * dt;
            if friction > v.abs() || v.abs() < FLING_STOP {
                *v = 0.0;
            } else {
                *v -= friction * v.signum();
            }
        }
        if rows.fling.y != 0.0 {
            let next = rows.vscroll - rows.fling.y * dt;
            rows.vscroll = next.clamp(0.0, rows.vmax);
            if rows.vscroll != next {
                rows.fling.y = 0.0;
            }
            set_vscroll = Some(rows.vscroll);
        }
        if rows.fling.x != 0.0
            && let Some(row) = rows.fling_row
        {
            let next = rows.hscroll[row] - rows.fling.x * dt;
            rows.hscroll[row] = next.clamp(0.0, rows.hmax[row]);
            if rows.hscroll[row] != next {
                rows.fling.x = 0.0;
            }
            set_hscroll = Some((row, rows.hscroll[row]));
        }
        ui.ctx().request_repaint();
    }
    (set_vscroll, set_hscroll)
}

struct Tile<'a> {
    game: &'a Game,
    focused: bool,
    installed: bool,
    install: Option<&'a InstallState>,
    updatable: bool,
}

fn draw_tile(
    ui: &Ui,
    m: &Metrics,
    covers: &CoverLoader,
    rect: Rect,
    cover_height: f32,
    tile: Tile,
    playing: Option<&mut Playing>,
) {
    let Tile {
        game,
        focused,
        installed,
        install,
        updatable,
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
            url.is_some_and(|url| paint_cover(ui, covers, url, Variant::Thumb, cover, radius))
        }
    };
    if !painted {
        let fill = if focused { TILE_HOVER } else { TILE_BG };
        ui.painter().rect_filled(cover, radius, fill);
        // No art: the title stands in for it, wrapped inside the cover.
        let galley = ui.painter().layout(
            game.title.clone(),
            FontId::proportional(m.label),
            DIM,
            cover.width() - m.space(24.0),
        );
        let pos = cover.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, DIM);
    }
    if let Some(install) = install {
        let bar = Rect::from_min_max(
            pos2(cover.left(), cover.bottom() - m.space(6.0)),
            cover.right_bottom(),
        );
        progress_bar(ui, bar, install.progress as f32);
    } else if updatable {
        badge(
            ui,
            m,
            pos2(cover.left(), cover.bottom()) + m.space(8.0) * vec2(1.0, -1.0),
            "UPDATE",
            AMBER,
        );
    } else if installed {
        badge(
            ui,
            m,
            pos2(cover.left(), cover.bottom()) + m.space(8.0) * vec2(1.0, -1.0),
            "INSTALLED",
            GREEN,
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
        pos2(rect.left(), cover.bottom() + m.space(6.0)),
        pos2(rect.right(), rect.bottom()),
    );
    let galley = ui.painter().layout(
        game.title.clone(),
        FontId::proportional(m.caption),
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
fn paint_cover(
    ui: &Ui,
    covers: &CoverLoader,
    url: &str,
    variant: Variant,
    rect: Rect,
    radius: CornerRadius,
) -> bool {
    let Some(texture) = covers.texture(ui.ctx(), url, variant) else {
        return false;
    };
    paint_texture(
        ui,
        egui::load::SizedTexture::from_handle(&texture),
        rect,
        radius,
    );
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

pub fn subtle(ui: &mut Ui, m: &Metrics, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(FontId::proportional(m.body))
            .color(DIM),
    );
}

pub fn offline(ui: &mut Ui, m: &Metrics) {
    ui.label(
        egui::RichText::new("Offline")
            .font(FontId::proportional(m.body))
            .color(AMBER),
    );
}

pub fn error(ui: &mut Ui, m: &Metrics, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(FontId::proportional(m.body))
            .color(Color32::from_rgb(0xff, 0x6e, 0x6e)),
    );
}

pub fn centered_spinner(ui: &mut Ui, m: &Metrics) {
    let rect = ui.available_rect_before_wrap();
    let center = rect.center();
    let size = m.space(40.0);
    let mut child = ui.new_child(
        egui::UiBuilder::new().max_rect(Rect::from_center_size(center, vec2(size, size))),
    );
    child.add(egui::Spinner::new().size(m.space(32.0)).color(DIM));
}

/// A small label anchored by its bottom-left corner.
fn badge(ui: &Ui, m: &Metrics, bottom_left: egui::Pos2, text: &str, fill: Color32) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(m.badge), BG);
    let pad = m.space(6.0) * vec2(1.0, 0.5);
    let size = galley.size() + 2.0 * pad;
    let rect = Rect::from_min_size(bottom_left - vec2(0.0, size.y), size);
    ui.painter().rect_filled(rect, CornerRadius::same(3), fill);
    ui.painter().galley(rect.min + pad, galley, BG);
}

/// What the detail page offers for a game, in button order.
pub fn game_buttons(
    game: &Game,
    caves: &[&Cave],
    install: Option<&InstallState>,
    running: bool,
    update: Option<&GameUpdate>,
    online: bool,
) -> Vec<(&'static str, Action)> {
    if running {
        return Vec::new();
    }
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
        Some(cave) => {
            let mut buttons = vec![(
                "Play",
                Action::Play {
                    cave_id: cave.id.clone(),
                },
            )];
            if update.is_some() && online {
                buttons.push((
                    "Update",
                    Action::Update {
                        cave_id: cave.id.clone(),
                    },
                ));
            }
            buttons.push((
                "Uninstall",
                Action::Uninstall {
                    cave_id: cave.id.clone(),
                },
            ));
            buttons
        }
        None if online => vec![("Install", Action::Install { game_id: game.id })],
        None => Vec::new(),
    }
}

/// Everything the detail page reads while drawing.
pub struct GameView<'a> {
    pub game: &'a Game,
    pub covers: &'a CoverLoader,
    pub caves: &'a [&'a Cave],
    pub install: Option<&'a InstallState>,
    pub running: bool,
    pub update: Option<&'a GameUpdate>,
    pub online: bool,
    pub focused_button: usize,
}

pub fn game_detail(ui: &mut Ui, m: &Metrics, view: GameView, actions: &mut Vec<Action>) {
    let GameView {
        game,
        covers,
        caves,
        install,
        running,
        update,
        online,
        focused_button,
    } = view;
    let buttons = game_buttons(game, caves, install, running, update, online);
    let width = ui.available_width();
    let cover_width = (width * 0.42).min(m.space(420.0));
    let cover_height = cover_width / COVER_ASPECT;
    let column_gap = m.space(28.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = column_gap;
        let (cover, _) = ui.allocate_exact_size(vec2(cover_width, cover_height), Sense::hover());
        let radius = CornerRadius::same(8);
        let url = game
            .still_cover_url
            .as_deref()
            .or(game.cover_url.as_deref());
        if !url.is_some_and(|url| paint_cover(ui, covers, url, Variant::Detail, cover, radius)) {
            ui.painter().rect_filled(cover, radius, TILE_BG);
        }
        ui.vertical(|ui| {
            ui.set_max_width(width - cover_width - column_gap);
            ui.label(
                egui::RichText::new(&game.title)
                    .font(FontId::proportional(m.title))
                    .color(TEXT),
            );
            if let Some(text) = game.short_text.as_deref().filter(|t| !t.is_empty()) {
                ui.add_space(m.space(4.0));
                ui.label(
                    egui::RichText::new(text)
                        .font(FontId::proportional(m.body))
                        .color(DIM),
                );
            }
            ui.add_space(m.space(12.0));
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
                            .font(FontId::proportional(m.caption))
                            .color(ACCENT),
                    );
                    ui.add_space(m.space(8.0));
                    let (bar, _) = ui.allocate_exact_size(
                        vec2(ui.available_width().min(420.0), 8.0),
                        Sense::hover(),
                    );
                    progress_bar(ui, bar, install.progress as f32);
                }
                (None, Some(_)) if running => {
                    ui.label(
                        egui::RichText::new("Running")
                            .font(FontId::proportional(m.caption))
                            .color(GREEN),
                    );
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
                            .font(FontId::proportional(m.caption))
                            .color(GREEN),
                    );
                    if let Some(stats) = &cave.stats
                        && stats.seconds_run > 0
                    {
                        subtle(
                            ui,
                            m,
                            &format!("Played {}", human_duration(stats.seconds_run)),
                        );
                    }
                    if let Some(update) = update {
                        let name = update
                            .choices
                            .first()
                            .and_then(|c| c.upload.as_ref())
                            .map_or("newer version", UploadExt::name);
                        let line = if update.direct {
                            format!("Update available: {name}")
                        } else if update.choices.len() > 1 {
                            format!("{} newer uploads available", update.choices.len())
                        } else {
                            format!("Newer upload available: {name}")
                        };
                        ui.label(
                            egui::RichText::new(line)
                                .font(FontId::proportional(m.caption))
                                .color(AMBER),
                        );
                    }
                }
                (None, None) if online => subtle(ui, m, "Not installed"),
                (None, None) => subtle(ui, m, "Not installed; offline"),
            }
            ui.add_space(m.space(20.0));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = m.space(12.0);
                for (index, (label, action)) in buttons.iter().enumerate() {
                    let response = pill(ui, m, label, index == focused_button, index == 0);
                    if response.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                        actions.push(Action::FocusButton(index));
                    }
                    if response.clicked() {
                        actions.push(action.clone());
                    }
                }
            });
        });
    });
}

fn pill(ui: &mut Ui, m: &Metrics, label: &str, focused: bool, primary: bool) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), FontId::proportional(m.button), TEXT);
    let size = galley.size() + m.space(1.0) * vec2(40.0, 18.0);
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

/// A modal question over the whole window. Keyboard and controller focus
/// go to it while it is up; the mouse can also pick a button.
/// Draws the modal over `screen`, which is the whole window unless a
/// smaller display is being emulated.
pub fn prompt(
    ctx: &egui::Context,
    m: &Metrics,
    screen: Rect,
    prompt: &Prompt,
    actions: &mut Vec<Action>,
) {
    egui::Area::new(egui::Id::new("prompt-dim"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .interactable(true)
        .show(ctx, |ui| {
            ui.allocate_rect(screen, Sense::click());
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(170));
        });
    let width = (screen.width() * 0.6).clamp(m.space(320.0), m.space(560.0));
    egui::Area::new(egui::Id::new("prompt"))
        .order(egui::Order::Foreground)
        .anchor(
            egui::Align2::CENTER_CENTER,
            screen.center() - ctx.content_rect().center(),
        )
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(TILE_BG)
                .corner_radius(CornerRadius::same(10))
                .stroke(Stroke::new(1.0, TILE_HOVER))
                .inner_margin(m.space(24.0))
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.label(
                        egui::RichText::new(&prompt.title)
                            .font(FontId::proportional(m.dialog))
                            .color(TEXT),
                    );
                    if !prompt.body.is_empty() {
                        ui.add_space(m.space(10.0));
                        egui::ScrollArea::vertical()
                            .max_height(screen.height() * 0.4)
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(&prompt.body)
                                        .font(FontId::proportional(m.caption))
                                        .color(DIM),
                                );
                            });
                    }
                    ui.add_space(m.space(18.0));
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = m.space(1.0) * vec2(12.0, 10.0);
                        for (index, label) in prompt.choices.iter().enumerate() {
                            let response = pill(ui, m, label, index == prompt.focus, index == 0);
                            if response.hovered()
                                && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO)
                            {
                                actions.push(Action::PromptFocus(index));
                            }
                            if response.clicked() {
                                actions.push(Action::Answer {
                                    prompt: prompt.id,
                                    choice: Some(index),
                                });
                            }
                        }
                    });
                });
        });
}

/// The hint bar along the bottom: a glyph and a word for each thing the
/// current page lets the user do.
pub fn footer(
    ui: &mut Ui,
    m: &Metrics,
    glyphs: &Glyphs,
    mode: InputMode,
    hints: &[(Vec<Glyph>, String)],
) {
    egui::Panel::bottom("footer")
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(BG)
                .inner_margin(egui::Margin::symmetric(m.margin as i8, m.space(10.0) as i8)),
        )
        .show_separator_line(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = m.space(8.0);
                for (keys, label) in hints {
                    for glyph in keys {
                        if let Some(texture) = glyphs.get(mode, *glyph) {
                            let size = m.space(22.0);
                            ui.add(
                                egui::Image::new(egui::load::SizedTexture::from_handle(texture))
                                    .fit_to_exact_size(vec2(size, size)),
                            );
                        }
                    }
                    ui.label(
                        egui::RichText::new(label)
                            .font(FontId::proportional(m.caption))
                            .color(DIM),
                    );
                    ui.add_space(m.space(14.0));
                }
            });
        });
}

/// A round back button with a painted chevron, sized for a fingertip.
pub fn back_button(ui: &mut Ui, m: &Metrics) -> egui::Response {
    let size = m.space(40.0);
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let fill = if response.hovered() {
        TILE_HOVER
    } else {
        TILE_BG
    };
    ui.painter().circle_filled(rect.center(), size / 2.0, fill);
    let c = rect.center();
    let arm = m.space(7.0);
    let points = [
        pos2(c.x + arm * 0.5, c.y - arm),
        pos2(c.x - arm * 0.5, c.y),
        pos2(c.x + arm * 0.5, c.y + arm),
    ];
    ui.painter()
        .add(egui::Shape::line(points.to_vec(), Stroke::new(3.0, TEXT)));
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small filter toggle for the header.
pub fn chip(ui: &mut Ui, m: &Metrics, label: &str, selected: bool) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), FontId::proportional(m.caption), TEXT);
    let size = galley.size() + m.space(1.0) * vec2(20.0, 10.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if selected {
        ACCENT
    } else if response.hovered() {
        TILE_HOVER
    } else {
        TILE_BG
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same((size.y / 2.0) as u8), fill);
    let color = if selected { BG } else { DIM };
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The tab bar along the top, with the bumper glyphs that switch tabs at
/// each end. Returns a tab the pointer picked.
pub fn tab_strip(
    ui: &mut Ui,
    m: &Metrics,
    glyphs: &Glyphs,
    mode: InputMode,
    active: Tab,
    downloading: usize,
) -> Option<Tab> {
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = m.space(18.0);
        let glyph = |ui: &mut Ui, glyph: Glyph| {
            if let Some(texture) = glyphs.get(mode, glyph) {
                let size = m.space(20.0);
                ui.add(
                    egui::Image::new(egui::load::SizedTexture::from_handle(texture))
                        .fit_to_exact_size(vec2(size, size)),
                );
            }
        };
        glyph(ui, Glyph::TabLeft);
        for tab in Tab::ALL {
            let selected = tab == active;
            let color = if selected { TEXT } else { DIM };
            let galley = ui.painter().layout_no_wrap(
                tab.label().to_string(),
                FontId::proportional(m.section),
                color,
            );
            let count = (tab == Tab::Downloads && downloading > 0).then(|| {
                ui.painter().layout_no_wrap(
                    downloading.to_string(),
                    FontId::proportional(m.caption),
                    BG,
                )
            });
            let pad = m.space(3.0);
            let count_width = count
                .as_ref()
                .map_or(0.0, |c| c.size().x + m.space(12.0) + m.space(8.0));
            let size = vec2(
                galley.size().x + 2.0 * pad + count_width,
                galley.size().y + m.space(12.0),
            );
            let (rect, response) = ui.allocate_exact_size(size, Sense::click());
            let text_pos = pos2(rect.left() + pad, rect.top());
            let text_height = galley.size().y;
            let text_right = text_pos.x + galley.size().x;
            ui.painter().galley(text_pos, galley, color);
            if let Some(count) = count {
                let height = text_height * 0.8;
                let pill = Rect::from_min_size(
                    pos2(
                        text_right + m.space(8.0),
                        rect.top() + (text_height - height) / 2.0,
                    ),
                    vec2(count.size().x + m.space(12.0), height),
                );
                ui.painter()
                    .rect_filled(pill, CornerRadius::same((height / 2.0) as u8), ACCENT);
                ui.painter()
                    .galley(pill.center() - count.size() / 2.0, count, BG);
            }
            if selected {
                let line = Rect::from_min_max(
                    pos2(rect.left(), rect.bottom() - m.space(3.0)),
                    rect.right_bottom(),
                );
                ui.painter()
                    .rect_filled(line, CornerRadius::same(2), ACCENT);
            }
            if response.clicked() {
                picked = Some(tab);
            }
            response.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
        glyph(ui, Glyph::TabRight);
    });
    picked
}

/// A quiet line in the middle of an otherwise empty page.
pub fn placeholder(ui: &mut Ui, m: &Metrics, text: &str) {
    let rect = ui.available_rect_before_wrap();
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        FontId::proportional(m.dialog),
        DIM,
    );
}

/// One entry on the Downloads tab, already worded by the app.
pub struct DownloadRow<'a> {
    pub game: Option<&'a Game>,
    pub title: String,
    pub detail: String,
    /// 0 to 1 while butler is working on it.
    pub progress: Option<f32>,
    pub failed: bool,
    pub buttons: Vec<(&'static str, Action)>,
}

pub struct DownloadsView<'a> {
    pub rows: &'a [DownloadRow<'a>],
    pub covers: &'a CoverLoader,
    /// Row and button with controller focus.
    pub focus: (usize, usize),
}

pub fn downloads(ui: &mut Ui, m: &Metrics, view: DownloadsView, actions: &mut Vec<Action>) {
    if view.rows.is_empty() {
        placeholder(ui, m, "Nothing downloading");
        return;
    }
    let thumb_width = m.space(110.0);
    let thumb_height = (thumb_width / COVER_ASPECT).round();
    let pad = m.space(12.0);
    let row_height = thumb_height + 2.0 * pad;
    let radius = CornerRadius::same(6);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = m.space(10.0);
            // Room for the focus ring, which is painted outside the row.
            ui.add_space(m.ring);
            for (index, row) in view.rows.iter().enumerate() {
                let focused_row = index == view.focus.0;
                let width = ui.available_width() - 2.0 * m.ring;
                let (rect, _) = ui.allocate_exact_size(vec2(width, row_height), Sense::hover());
                let rect = rect.translate(vec2(m.ring, 0.0));
                if focused_row {
                    ui.scroll_to_rect(rect.expand(m.ring), None);
                    ui.painter().rect_stroke(
                        rect.expand(2.0),
                        CornerRadius::same(8),
                        Stroke::new(3.0, ACCENT),
                        egui::StrokeKind::Outside,
                    );
                }
                ui.painter().rect_filled(rect, radius, TILE_BG);

                let thumb = Rect::from_min_size(
                    rect.left_top() + vec2(pad, pad),
                    vec2(thumb_width, thumb_height),
                );
                let url = row.game.and_then(|game| {
                    game.still_cover_url
                        .as_deref()
                        .or(game.cover_url.as_deref())
                });
                if !url.is_some_and(|url| {
                    paint_cover(ui, view.covers, url, Variant::Thumb, thumb, radius)
                }) {
                    ui.painter().rect_filled(thumb, radius, TILE_HOVER);
                }

                // Buttons hug the right edge; the first one is leftmost.
                let button_rect = Rect::from_min_max(
                    pos2(rect.right() - m.space(320.0), rect.top()),
                    pos2(rect.right() - pad, rect.bottom()),
                );
                let mut buttons_left = button_rect.right();
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(button_rect)
                        .layout(egui::Layout::right_to_left(egui::Align::Center)),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = m.space(10.0);
                        for (button, (label, action)) in row.buttons.iter().enumerate().rev() {
                            let focused = focused_row && button == view.focus.1;
                            let response = pill(ui, m, label, focused, false);
                            buttons_left = buttons_left.min(response.rect.left());
                            if response.clicked() {
                                actions.push(action.clone());
                            }
                        }
                    },
                );

                let text_left = thumb.right() + m.space(16.0);
                let text_right = buttons_left - m.space(16.0);
                let text_width = (text_right - text_left).max(0.0);
                let mut job = egui::text::LayoutJob::simple_singleline(
                    row.title.clone(),
                    FontId::proportional(m.title),
                    TEXT,
                );
                job.wrap = egui::text::TextWrapping::truncate_at_width(text_width);
                let title = ui.painter().layout_job(job);
                let detail_color = if row.failed { ACCENT } else { DIM };
                let detail = ui.painter().layout(
                    row.detail.clone(),
                    FontId::proportional(m.body),
                    detail_color,
                    text_width,
                );
                let bar_height = if row.progress.is_some() {
                    m.space(6.0) + m.space(10.0)
                } else {
                    0.0
                };
                let block = title.size().y + m.space(6.0) + detail.size().y + bar_height;
                let mut y = rect.top() + (rect.height() - block) / 2.0;
                ui.painter().galley(pos2(text_left, y), title.clone(), TEXT);
                y += title.size().y + m.space(6.0);
                ui.painter()
                    .galley(pos2(text_left, y), detail.clone(), detail_color);
                y += detail.size().y + m.space(10.0);
                if let Some(progress) = row.progress {
                    let bar =
                        Rect::from_min_size(pos2(text_left, y), vec2(text_width, m.space(6.0)));
                    progress_bar(ui, bar, progress);
                }
            }
            ui.add_space(m.ring);
        });
}

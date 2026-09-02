//! Drawing. Views read the app state and never mutate it.

use egui::{Color32, CornerRadius, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};

use crate::model::Game;

pub const BG: Color32 = Color32::from_rgb(0x14, 0x12, 0x1a);
const TILE_BG: Color32 = Color32::from_rgb(0x24, 0x21, 0x2e);
const TILE_HOVER: Color32 = Color32::from_rgb(0x34, 0x30, 0x42);
const TEXT: Color32 = Color32::from_gray(0xee);
const DIM: Color32 = Color32::from_gray(0x99);

/// itch.io covers are 315x250; tiles keep that shape.
const COVER_ASPECT: f32 = 315.0 / 250.0;
const TILE_WIDTH: f32 = 170.0;
const GAP: f32 = 14.0;
const TITLE_HEIGHT: f32 = 26.0;

pub fn library(ui: &mut Ui, games: &[Game]) {
    let available = ui.available_width();
    let columns = ((available + GAP) / (TILE_WIDTH + GAP)).floor().max(1.0) as usize;
    let tile_width = (available - GAP * (columns as f32 - 1.0)) / columns as f32;
    let cover_height = tile_width / COVER_ASPECT;
    let row_height = cover_height + TITLE_HEIGHT + GAP;
    let rows = games.len().div_ceil(columns);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, row_height, rows, |ui, range| {
            for row in range {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    for column in 0..columns {
                        let Some(game) = games.get(row * columns + column) else {
                            break;
                        };
                        let (rect, response) = ui.allocate_exact_size(
                            vec2(tile_width, cover_height + TITLE_HEIGHT),
                            Sense::click(),
                        );
                        if ui.is_rect_visible(rect) {
                            tile(ui, rect, cover_height, game, response.hovered());
                        }
                    }
                });
                ui.add_space(GAP - ui.spacing().item_spacing.y);
            }
        });
}

fn tile(ui: &Ui, rect: Rect, cover_height: f32, game: &Game, hovered: bool) {
    let cover = Rect::from_min_size(rect.min, vec2(rect.width(), cover_height));
    let radius = CornerRadius::same(6);
    let url = game
        .cover_url
        .as_deref()
        .or(game.still_cover_url.as_deref());
    let painted = url.is_some_and(|url| paint_cover(ui, url, cover, radius));
    if !painted {
        let fill = if hovered { TILE_HOVER } else { TILE_BG };
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
    if hovered {
        ui.painter().rect_stroke(
            cover,
            radius,
            Stroke::new(2.0, TEXT),
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
        if hovered { TEXT } else { DIM },
        f32::INFINITY,
    );
    ui.painter()
        .with_clip_rect(title_rect)
        .galley(title_rect.left_top(), galley, DIM);
}

/// Paints the cover cropped to fill `rect`, or returns false while it is
/// still loading or has failed.
fn paint_cover(ui: &Ui, url: &str, rect: Rect, radius: CornerRadius) -> bool {
    let image = egui::Image::new(url);
    let Ok(egui::load::TexturePoll::Ready { texture }) = image.load_for_size(ui.ctx(), rect.size())
    else {
        return false;
    };
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
    true
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

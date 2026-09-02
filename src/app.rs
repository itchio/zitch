//! Application state and the window that draws it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Color32, FontId, RichText};

use crate::backend::{Backend, Event};
use crate::model::{Game, Loadable, Profile};

pub struct App {
    backend: Backend,
    status: String,
    profile: Option<Profile>,
    games: Loadable<Vec<Game>>,
    error: Option<String>,
    shot: Option<Shot>,
}

/// A debugging capture: write the window to a PNG once the library has
/// settled, or after a deadline, then quit.
pub struct Shot {
    pub path: PathBuf,
    pub deadline: Instant,
    pub asked: bool,
}

impl Shot {
    pub fn new(path: PathBuf, wait: Duration) -> Self {
        Self {
            path,
            deadline: Instant::now() + wait,
            asked: false,
        }
    }
}

impl App {
    pub fn new(backend: Backend, ctx: &egui::Context, shot: Option<Shot>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(0x14, 0x12, 0x1a);
        ctx.set_visuals(visuals);
        // Big picture: read from across the room.
        ctx.set_zoom_factor(1.6);
        Self {
            backend,
            status: String::new(),
            profile: None,
            games: Loadable::Loading,
            error: None,
            shot,
        }
    }

    fn drive_shot(&mut self, ctx: &egui::Context) {
        let Some(shot) = self.shot.as_mut() else {
            return;
        };
        let settled = !matches!(self.games, Loadable::Loading) || self.error.is_some();
        if !shot.asked && (settled || Instant::now() >= shot.deadline) {
            shot.asked = true;
            // One more frame so the settled state is what gets captured.
            ctx.request_repaint();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        if !shot.asked {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        let image = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = image else {
            return;
        };
        let [width, height] = image.size;
        let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
        match image::save_buffer(
            &shot.path,
            &rgba,
            width as u32,
            height as u32,
            image::ColorType::Rgba8,
        ) {
            Ok(()) => log::info!("wrote {width}x{height} to {}", shot.path.display()),
            Err(error) => log::error!("writing {}: {error}", shot.path.display()),
        }
        self.shot = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn handle_events(&mut self) {
        for event in self.backend.poll() {
            match event {
                Event::Status(text) => self.status = text,
                Event::SignedIn(profile) => self.profile = Some(profile),
                Event::OwnedGames(games) => self.games = Loadable::Loaded(games),
                Event::Error(message) => {
                    if self.games.get().is_none() {
                        self.games = Loadable::Failed(message.clone());
                    }
                    self.error = Some(message);
                }
            }
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_events();
        self.drive_shot(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("zitch")
                        .font(FontId::proportional(32.0))
                        .strong(),
                );
                if let Some(profile) = &self.profile {
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new(profile.user.name())
                            .font(FontId::proportional(18.0))
                            .color(Color32::from_gray(160)),
                    );
                }
            });
            ui.label(RichText::new(&self.status).color(Color32::from_gray(140)));
            if let Some(error) = &self.error {
                ui.colored_label(Color32::from_rgb(0xff, 0x6e, 0x6e), error);
            }
            ui.add_space(16.0);
            match &self.games {
                Loadable::NotLoaded | Loadable::Loading => {
                    ui.spinner();
                }
                Loadable::Failed(_) => {}
                Loadable::Loaded(games) => {
                    ui.label(
                        RichText::new(format!("{} games", games.len()))
                            .font(FontId::proportional(20.0)),
                    );
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for game in games {
                                ui.label(
                                    RichText::new(&game.title).font(FontId::proportional(18.0)),
                                );
                            }
                        });
                }
            }
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.backend.shutdown();
    }
}

//! Application state and the window that draws it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::backend::{Backend, Event};
use crate::gamepad::Gamepad;
use crate::images::CoverLoader;
use crate::model::{Action, Cave, CaveExt, Direction, Game, Loadable, Page, Profile, UserExt};
use crate::ui;

pub struct App {
    backend: Backend,
    covers: CoverLoader,
    gamepad: Gamepad,
    status: String,
    profile: Option<Profile>,
    games: Loadable<Vec<Game>>,
    caves: Vec<Cave>,
    installed: std::collections::HashSet<i64>,
    page: Page,
    error: Option<String>,
    /// Something the user just did, shown in the header.
    notice: Option<String>,
    pub actions: Vec<Action>,
    pub grid: ui::Grid,
    shot: Option<Shot>,
}

/// A debugging capture: write the window to a PNG once the library has
/// settled, or after a deadline, then quit.
pub struct Shot {
    pub path: PathBuf,
    pub deadline: Instant,
    /// When the library finished loading; covers get a moment after that.
    pub settled_at: Option<Instant>,
    /// Scripted input to play once the library is loaded, one per frame.
    pub script: std::collections::VecDeque<Action>,
    pub asked: bool,
}

const COVER_GRACE: Duration = Duration::from_secs(3);

impl Shot {
    pub fn new(path: PathBuf, wait: Duration, script: Vec<Action>) -> Self {
        Self {
            path,
            deadline: Instant::now() + wait,
            settled_at: None,
            script: script.into(),
            asked: false,
        }
    }
}

/// Parses `down,down,right,enter` into actions for `--screenshot-script`.
pub fn parse_script(text: &str) -> Result<Vec<Action>, String> {
    text.split(',')
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(|word| match word {
            "up" => Ok(Action::MoveFocus(Direction::Up)),
            "down" => Ok(Action::MoveFocus(Direction::Down)),
            "left" => Ok(Action::MoveFocus(Direction::Left)),
            "right" => Ok(Action::MoveFocus(Direction::Right)),
            "enter" => Ok(Action::Activate),
            "back" => Ok(Action::Back),
            other => Err(format!("unknown script step {other:?}")),
        })
        .collect()
}

impl App {
    pub fn new(
        backend: Backend,
        covers: CoverLoader,
        ctx: &egui::Context,
        shot: Option<Shot>,
    ) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = ui::BG;
        ctx.set_visuals(visuals);
        covers.install(ctx);
        // Big picture: read from across the room.
        ctx.set_zoom_factor(1.6);
        Self {
            backend,
            covers,
            gamepad: Gamepad::new(),
            status: String::new(),
            profile: None,
            games: Loadable::Loading,
            caves: Vec::new(),
            installed: Default::default(),
            page: Page::Library,
            error: None,
            notice: None,
            actions: Vec::new(),
            grid: ui::Grid::default(),
            shot,
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        ctx.input_mut(|input| {
            let mut key = |key: Key, action: Action| {
                if input.consume_key(Modifiers::NONE, key) {
                    self.actions.push(action);
                }
            };
            key(Key::ArrowUp, Action::MoveFocus(Direction::Up));
            key(Key::ArrowDown, Action::MoveFocus(Direction::Down));
            key(Key::ArrowLeft, Action::MoveFocus(Direction::Left));
            key(Key::ArrowRight, Action::MoveFocus(Direction::Right));
            key(Key::Enter, Action::Activate);
            key(Key::Escape, Action::Back);
        });
    }

    fn apply_actions(&mut self) {
        let actions = std::mem::take(&mut self.actions);
        for action in actions {
            self.apply(action);
        }
    }

    fn caves_for(&self, game_id: i64) -> Vec<&Cave> {
        self.caves
            .iter()
            .filter(|cave| cave.game_id() == Some(game_id))
            .collect()
    }

    fn game_at(&self, index: usize) -> Option<&Game> {
        self.games.get().and_then(|games| games.get(index))
    }

    fn apply(&mut self, action: Action) {
        let count = self.games.get().map_or(0, Vec::len);
        match action {
            Action::MoveFocus(direction) => match self.page.clone() {
                Page::Library => self.move_grid_focus(direction, count),
                Page::Game { index, button } => {
                    let Some(game) = self.game_at(index) else {
                        return;
                    };
                    let mut buttons = ui::game_buttons(&self.caves_for(game.id)).len();
                    if buttons == 0 {
                        buttons = 1;
                    }
                    let button = match direction {
                        Direction::Left => button.saturating_sub(1),
                        Direction::Right => (button + 1).min(buttons - 1),
                        _ => button,
                    };
                    self.page = Page::Game { index, button };
                }
            },
            Action::FocusIndex(index) => {
                if index < count {
                    self.grid.focus = index;
                }
            }
            Action::FocusButton(button) => {
                if let Page::Game { index, .. } = self.page {
                    self.page = Page::Game { index, button };
                }
            }
            Action::Activate => match self.page.clone() {
                Page::Library => {
                    if self.grid.focus < count {
                        self.actions.push(Action::Open(Page::Game {
                            index: self.grid.focus,
                            button: 0,
                        }));
                    }
                }
                Page::Game { index, button } => {
                    let Some(game) = self.game_at(index) else {
                        return;
                    };
                    let mut buttons = ui::game_buttons(&self.caves_for(game.id));
                    if buttons.is_empty() {
                        buttons.push(("Install", Action::Install { game_id: game.id }));
                    }
                    if let Some((_, action)) = buttons.get(button) {
                        self.actions.push(action.clone());
                    }
                }
            },
            Action::Back => match self.page {
                Page::Library => self.notice = None,
                Page::Game { index, .. } => {
                    self.grid.focus = index;
                    self.grid.follow = true;
                    self.page = Page::Library;
                }
            },
            Action::Open(page) => {
                self.notice = None;
                self.page = page;
            }
            // Placeholders until the install and launch flows exist.
            Action::Play { cave_id } => {
                log::info!("play cave {cave_id}");
                self.notice = Some("Launching is not wired up yet".into());
            }
            Action::Install { game_id } => {
                log::info!("install game {game_id}");
                self.notice = Some("Installing is not wired up yet".into());
            }
            Action::Uninstall { cave_id } => {
                log::info!("uninstall cave {cave_id}");
                self.notice = Some("Uninstalling is not wired up yet".into());
            }
        }
    }

    fn move_grid_focus(&mut self, direction: Direction, count: usize) {
        if count == 0 {
            return;
        }
        let columns = self.grid.columns.max(1);
        let focus = self.grid.focus;
        let next = match direction {
            Direction::Left => focus.saturating_sub(1),
            Direction::Right => (focus + 1).min(count - 1),
            Direction::Up => focus.checked_sub(columns).unwrap_or(focus),
            // Stop at the last row, on the last tile if the row is short.
            Direction::Down if focus + columns < count => focus + columns,
            Direction::Down if focus / columns < (count - 1) / columns => count - 1,
            Direction::Down => focus,
        };
        self.grid.focus = next;
        self.grid.follow = true;
    }

    fn drive_shot(&mut self, ctx: &egui::Context) {
        let Some(shot) = self.shot.as_mut() else {
            return;
        };
        let settled = !matches!(self.games, Loadable::Loading) || self.error.is_some();
        if settled && let Some(action) = shot.script.pop_front() {
            self.actions.push(action);
            ctx.request_repaint();
            return;
        }
        if settled && shot.settled_at.is_none() {
            shot.settled_at = Some(Instant::now());
        }
        let now = Instant::now();
        let ready = shot.settled_at.is_some_and(|at| now >= at + COVER_GRACE);
        if !shot.asked && (ready || now >= shot.deadline) {
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
                Event::Caves(caves) => {
                    self.installed = caves.iter().filter_map(CaveExt::game_id).collect();
                    self.caves = caves;
                }
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
        self.handle_keys(ctx);
        self.gamepad.poll(ctx, &mut self.actions);
        self.drive_shot(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui::BG).inner_margin(24.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if self.page.is_library() {
                        ui::heading(ui, "Library");
                    } else {
                        ui::heading(ui, "‹ Library");
                    }
                    if let Some(user) = self.profile.as_ref().and_then(|p| p.user.as_ref()) {
                        ui.add_space(12.0);
                        ui::subtle(ui, user.name());
                    }
                    if let Loadable::Loaded(games) = &self.games {
                        ui.add_space(12.0);
                        ui::subtle(ui, &format!("{} games", games.len()));
                    }
                });
                match (&self.games, &self.notice) {
                    (Loadable::Loaded(_), Some(notice)) => ui::subtle(ui, notice),
                    (Loadable::Loaded(_), None) => ui::subtle(ui, ""),
                    _ => ui::subtle(ui, &self.status),
                }
                if let Some(error) = &self.error {
                    ui::error(ui, error);
                }
                ui.add_space(16.0);
                match (&self.games, self.page.clone()) {
                    (Loadable::NotLoaded | Loadable::Loading, _) => ui::centered_spinner(ui),
                    (Loadable::Failed(_), _) => {}
                    (Loadable::Loaded(games), Page::Library) => ui::library(
                        ui,
                        games,
                        &self.installed,
                        &mut self.grid,
                        &self.covers,
                        &mut self.actions,
                    ),
                    (Loadable::Loaded(games), Page::Game { index, button }) => {
                        match games.get(index) {
                            Some(game) => {
                                let caves: Vec<&Cave> = self
                                    .caves
                                    .iter()
                                    .filter(|cave| cave.game_id() == Some(game.id))
                                    .collect();
                                ui::game_detail(ui, game, &caves, button, &mut self.actions);
                            }
                            None => self.actions.push(Action::Back),
                        }
                    }
                }
            });
        self.apply_actions();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.backend.shutdown();
    }
}

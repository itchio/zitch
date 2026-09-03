//! Application state and the window that draws it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::backend::{Backend, Command, Event};
use crate::gamepad::Gamepad;
use crate::glyphs::{Glyph, Glyphs, InputMode};
use crate::images::CoverLoader;
use crate::model::{
    Action, Cave, CaveExt, Direction, Download, DownloadProgress, Filter, Game, GameUpdate,
    InstallState, Loadable, Page, Profile, Prompt, UserExt,
};
use crate::ui;

pub struct App {
    backend: Backend,
    covers: CoverLoader,
    gamepad: Gamepad,
    glyphs: Glyphs,
    /// The device the user touched last, which picks the footer's glyphs.
    input_mode: InputMode,
    status: String,
    profile: Option<Profile>,
    games: Loadable<Vec<Game>>,
    caves: Vec<Cave>,
    installed: std::collections::HashSet<i64>,
    /// butler's download queue and the latest progress per download.
    downloads: Vec<Download>,
    progress: std::collections::HashMap<String, DownloadProgress>,
    /// Games the user asked to install that the queue has not listed yet.
    pending_installs: std::collections::HashSet<i64>,
    /// Downloads the user asked to discard that the queue still lists.
    discarding: std::collections::HashSet<String>,
    /// What the interface shows per game, rebuilt from the fields above.
    pub installs: std::collections::HashMap<i64, InstallState>,
    /// Caves with a Launch call in flight.
    running: std::collections::HashSet<String>,
    /// Updates butler found, by cave.
    updates: std::collections::HashMap<String, GameUpdate>,
    /// A question from the backend, shown over everything until answered.
    prompt: Option<Prompt>,
    filter: Filter,
    query: String,
    /// Move keyboard focus into the search box on the next frame.
    focus_search: bool,
    /// Take keyboard focus out of the search box on the next frame.
    blur_search: bool,
    page: Page,
    error: Option<String>,
    /// Something the user just did, shown in the header.
    notice: Option<String>,
    pub actions: Vec<Action>,
    pub rows: ui::Rows,
    shot: Option<Shot>,
}

/// A debugging capture: write the window to a PNG once the library has
/// settled, or after a deadline, then quit.
pub struct Shot {
    pub path: PathBuf,
    pub deadline: Instant,
    /// When the library finished loading; covers get a moment after that.
    pub settled_at: Option<Instant>,
    /// Scripted steps to play once the library is loaded, one per frame.
    pub script: std::collections::VecDeque<Step>,
    wait_until: Option<Instant>,
    /// A screenshot was requested and its pixels have not arrived yet.
    capture_pending: bool,
    /// Whether a `capture` step already wrote the file, so the run ends
    /// without a second capture.
    captured: bool,
}

/// One step of `--screenshot-script`.
#[derive(Debug, Clone)]
pub enum Step {
    Act(Action),
    Wait(Duration),
    /// Write the screenshot now, mid-script, instead of at the end.
    Capture,
    /// Type into the search box.
    Search(String),
}

const COVER_GRACE: Duration = Duration::from_secs(3);

impl Shot {
    pub fn new(path: PathBuf, wait: Duration, script: Vec<Step>) -> Self {
        Self {
            path,
            deadline: Instant::now() + wait,
            settled_at: None,
            script: script.into(),
            wait_until: None,
            capture_pending: false,
            captured: false,
        }
    }
}

/// Parses `focus:12,enter,wait:2000,capture` for `--screenshot-script`.
pub fn parse_script(text: &str) -> Result<Vec<Step>, String> {
    text.split(',')
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(|word| match word {
            "up" => Ok(Step::Act(Action::MoveFocus(Direction::Up))),
            "down" => Ok(Step::Act(Action::MoveFocus(Direction::Down))),
            "left" => Ok(Step::Act(Action::MoveFocus(Direction::Left))),
            "right" => Ok(Step::Act(Action::MoveFocus(Direction::Right))),
            "enter" => Ok(Step::Act(Action::Activate)),
            "back" => Ok(Step::Act(Action::Back)),
            "capture" => Ok(Step::Capture),
            "tab" => Ok(Step::Act(Action::CycleFilter(1))),
            // A stand-in question, to look at the modal without a game that
            // asks one.
            "prompt" => Ok(Step::Act(Action::Answer {
                prompt: 0,
                choice: None,
            })),
            other => {
                if let Some(index) = other.strip_prefix("focus:").and_then(|n| n.parse().ok()) {
                    Ok(Step::Act(Action::FocusIndex(index)))
                } else if let Some(ms) = other.strip_prefix("wait:").and_then(|n| n.parse().ok()) {
                    Ok(Step::Wait(Duration::from_millis(ms)))
                } else if let Some(text) = other.strip_prefix("search:") {
                    Ok(Step::Search(text.to_string()))
                } else {
                    Err(format!("unknown script step {other:?}"))
                }
            }
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
            glyphs: Glyphs::load(ctx),
            input_mode: InputMode::Keyboard,
            status: String::new(),
            profile: None,
            games: Loadable::Loading,
            caves: Vec::new(),
            installed: Default::default(),
            downloads: Vec::new(),
            progress: Default::default(),
            pending_installs: Default::default(),
            discarding: Default::default(),
            installs: Default::default(),
            running: Default::default(),
            updates: Default::default(),
            prompt: None,
            filter: Filter::default(),
            query: String::new(),
            focus_search: false,
            blur_search: false,
            page: Page::Library,
            error: None,
            notice: None,
            actions: Vec::new(),
            rows: ui::Rows::default(),
            shot,
        }
    }

    fn search_id() -> egui::Id {
        egui::Id::new("search")
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let typing = ctx.memory(|m| m.has_focus(Self::search_id()));
        ctx.input_mut(|input| {
            let mut key = |modifiers: Modifiers, key: Key, action: Action| {
                if input.consume_key(modifiers, key) {
                    self.actions.push(action);
                }
            };
            if typing {
                // The text box owns the arrows and letters; only leaving it
                // is ours.
                key(Modifiers::NONE, Key::Enter, Action::SearchDone);
                key(Modifiers::NONE, Key::Escape, Action::ClearSearch);
                key(Modifiers::NONE, Key::ArrowDown, Action::SearchDone);
                return;
            }
            key(
                Modifiers::NONE,
                Key::ArrowUp,
                Action::MoveFocus(Direction::Up),
            );
            key(
                Modifiers::NONE,
                Key::ArrowDown,
                Action::MoveFocus(Direction::Down),
            );
            key(
                Modifiers::NONE,
                Key::ArrowLeft,
                Action::MoveFocus(Direction::Left),
            );
            key(
                Modifiers::NONE,
                Key::ArrowRight,
                Action::MoveFocus(Direction::Right),
            );
            key(Modifiers::NONE, Key::Enter, Action::Activate);
            key(Modifiers::NONE, Key::Escape, Action::Back);
            key(Modifiers::NONE, Key::Slash, Action::FocusSearch);
            key(Modifiers::NONE, Key::Tab, Action::CycleFilter(1));
            key(Modifiers::SHIFT, Key::Tab, Action::CycleFilter(-1));
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
        if let (None, Action::Answer { prompt: 0, .. }) = (&self.prompt, &action) {
            self.prompt = Some(Prompt {
                id: 0,
                title: "License agreement".into(),
                body: "This is a sample license shown by the screenshot script. ".repeat(12),
                choices: vec!["Accept".into(), "Decline".into()],
                focus: 0,
            });
            return;
        }
        if let Some(prompt) = self.prompt.as_mut() {
            match action {
                Action::MoveFocus(Direction::Left) => prompt.focus = prompt.focus.saturating_sub(1),
                Action::MoveFocus(Direction::Right) => {
                    prompt.focus = (prompt.focus + 1).min(prompt.choices.len().saturating_sub(1))
                }
                Action::PromptFocus(index) if index < prompt.choices.len() => prompt.focus = index,
                Action::Activate => {
                    let answer = Action::Answer {
                        prompt: prompt.id,
                        choice: Some(prompt.focus),
                    };
                    self.actions.push(answer);
                }
                Action::Back => {
                    let answer = Action::Answer {
                        prompt: prompt.id,
                        choice: None,
                    };
                    self.actions.push(answer);
                }
                Action::Answer { prompt: id, choice } if id == prompt.id => {
                    self.prompt = None;
                    self.backend.send(Command::Answer { prompt: id, choice });
                }
                _ => {}
            }
            return;
        }
        match action {
            Action::MoveFocus(direction) => match self.page.clone() {
                Page::Library => self.rows.move_focus(direction),
                Page::Game { index, button } => {
                    let Some(game) = self.game_at(index) else {
                        return;
                    };
                    let buttons = ui::game_buttons(
                        game,
                        &self.caves_for(game.id),
                        self.installs.get(&game.id),
                        self.is_running(game.id),
                        self.update_for(game.id),
                    )
                    .len()
                    .max(1);
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
                    self.rows.focus_game(index);
                }
            }
            Action::FocusTile { row, col } => self.rows.focus_tile(row, col),
            Action::FocusButton(button) => {
                if let Page::Game { index, .. } = self.page {
                    self.page = Page::Game { index, button };
                }
            }
            Action::Activate => match self.page.clone() {
                Page::Library => {
                    if let Some(index) = self.rows.focused_game().filter(|&i| i < count) {
                        self.actions
                            .push(Action::Open(Page::Game { index, button: 0 }));
                    }
                }
                Page::Game { index, button } => {
                    let Some(game) = self.game_at(index) else {
                        return;
                    };
                    let buttons = ui::game_buttons(
                        game,
                        &self.caves_for(game.id),
                        self.installs.get(&game.id),
                        self.is_running(game.id),
                        self.update_for(game.id),
                    );
                    if let Some((_, action)) = buttons.get(button) {
                        self.actions.push(action.clone());
                    }
                }
            },
            Action::SetFilter(filter) => {
                if self.filter != filter {
                    self.filter = filter;
                    self.rebuild_sections();
                }
            }
            Action::CycleFilter(step) => {
                if self.page.is_library() {
                    self.actions.push(Action::SetFilter(self.filter.next(step)));
                }
            }
            Action::FocusSearch => {
                if self.page.is_library() {
                    self.focus_search = true;
                }
            }
            Action::SearchDone => {
                self.blur_search = true;
                self.rows.follow = true;
            }
            Action::ClearSearch => {
                self.blur_search = true;
                if !self.query.is_empty() {
                    self.query.clear();
                    self.rebuild_sections();
                }
            }
            Action::Back => match self.page {
                Page::Library if !self.query.is_empty() => self.actions.push(Action::ClearSearch),
                Page::Library => self.notice = None,
                Page::Game { index, .. } => {
                    self.rows.focus_game(index);
                    self.page = Page::Library;
                }
            },
            Action::Open(page) => {
                self.notice = None;
                self.page = page;
            }
            Action::Update { cave_id } => {
                if let Some(update) = self.updates.get(&cave_id) {
                    let title = update.game.as_ref().map_or("game", |g| g.title.as_str());
                    self.notice = Some(format!("Updating {title}"));
                    self.backend.send(Command::Update {
                        update: Box::new(update.clone()),
                    });
                }
            }
            Action::Play { cave_id } => {
                if self.running.insert(cave_id.clone()) {
                    self.notice = Some("Launching".into());
                    self.backend.send(Command::Launch { cave_id });
                }
            }
            // Only meaningful while a prompt is open, handled above.
            Action::Answer { .. } | Action::PromptFocus(_) => {}
            Action::Install { game_id } => {
                let Some(game) = self
                    .games
                    .get()
                    .and_then(|g| g.iter().find(|g| g.id == game_id))
                else {
                    return;
                };
                if self.installs.contains_key(&game_id) {
                    return;
                }
                // Shown as installing from this instant; the queue listing
                // that follows replaces it.
                self.pending_installs.insert(game_id);
                self.notice = Some(format!("Installing {}", game.title));
                self.backend.send(Command::Install {
                    game: Box::new(game.clone()),
                });
                self.rebuild_installs();
            }
            Action::CancelInstall { game_id } => {
                let Some(download_id) = self.download_for(game_id).map(|d| d.id.clone()) else {
                    return;
                };
                self.discarding.insert(download_id.clone());
                self.backend.send(Command::Discard { download_id });
                self.rebuild_installs();
            }
            Action::RetryInstall { game_id } => {
                let Some(download_id) = self.download_for(game_id).map(|d| d.id.clone()) else {
                    return;
                };
                self.backend.send(Command::Retry { download_id });
            }
            Action::Uninstall { cave_id } => {
                self.notice = Some("Uninstalling".into());
                self.backend.send(Command::Uninstall { cave_id });
            }
        }
    }

    /// Games with an update waiting, for the grid's badges.
    fn updatable(&self) -> std::collections::HashSet<i64> {
        self.caves
            .iter()
            .filter(|cave| self.updates.contains_key(&cave.id))
            .filter_map(CaveExt::game_id)
            .collect()
    }

    fn update_for(&self, game_id: i64) -> Option<&GameUpdate> {
        self.caves
            .iter()
            .filter(|cave| cave.game_id() == Some(game_id))
            .find_map(|cave| self.updates.get(&cave.id))
    }

    fn is_running(&self, game_id: i64) -> bool {
        self.caves
            .iter()
            .any(|cave| cave.game_id() == Some(game_id) && self.running.contains(&cave.id))
    }

    /// Lays the home screen out as carousels: what you can play now first,
    /// then what you played last, then anything with an update, then all.
    fn rebuild_sections(&mut self) {
        let Some(games) = self.games.get() else {
            return;
        };
        let filter = self.filter;
        let query = self.query.trim().to_lowercase();
        if !query.is_empty() {
            // Searching narrows the whole screen to one row of matches.
            let matches: Vec<usize> = games
                .iter()
                .enumerate()
                .filter(|(_, g)| filter.matches(g) && g.title.to_lowercase().contains(&query))
                .map(|(i, _)| i)
                .collect();
            let title = match matches.len() {
                0 => "No matches".to_string(),
                1 => "1 match".to_string(),
                n => format!("{n} matches"),
            };
            self.rows.set_sections(vec![ui::Section {
                title,
                games: matches,
            }]);
            return;
        }
        let index_of: std::collections::HashMap<i64, usize> =
            games.iter().enumerate().map(|(i, g)| (g.id, i)).collect();
        let mut sections = Vec::new();

        let mut installed: Vec<(&Cave, usize)> = self
            .caves
            .iter()
            .filter_map(|cave| Some((cave, *index_of.get(&cave.game_id()?)?)))
            .collect();
        // Newest install first.
        installed.sort_by(|a, b| {
            let at = |c: &Cave| c.stats.as_ref().and_then(|s| s.installed_at.clone());
            at(b.0).cmp(&at(a.0))
        });
        let mut seen = std::collections::HashSet::new();
        let installed_games: Vec<usize> = installed
            .iter()
            .map(|(_, i)| *i)
            .filter(|i| seen.insert(*i))
            .collect();
        if !installed_games.is_empty() {
            sections.push(ui::Section {
                title: "Installed".into(),
                games: installed_games,
            });
        }

        let mut played: Vec<(&Cave, usize)> = installed
            .iter()
            .copied()
            .filter(|(cave, _)| {
                cave.stats
                    .as_ref()
                    .is_some_and(|s| s.last_touched_at.is_some() && s.seconds_run > 0)
            })
            .collect();
        played.sort_by(|a, b| {
            let at = |c: &Cave| c.stats.as_ref().and_then(|s| s.last_touched_at.clone());
            at(b.0).cmp(&at(a.0))
        });
        let mut seen = std::collections::HashSet::new();
        let played_games: Vec<usize> = played
            .iter()
            .map(|(_, i)| *i)
            .filter(|i| seen.insert(*i))
            .take(12)
            .collect();
        if !played_games.is_empty() {
            sections.push(ui::Section {
                title: "Recently played".into(),
                games: played_games,
            });
        }

        let updatable = self.updatable();
        let update_games: Vec<usize> = games
            .iter()
            .enumerate()
            .filter(|(_, g)| updatable.contains(&g.id))
            .map(|(i, _)| i)
            .collect();
        if !update_games.is_empty() {
            sections.push(ui::Section {
                title: "Updates".into(),
                games: update_games,
            });
        }

        sections.push(ui::Section {
            title: match filter {
                Filter::All => "All games".to_string(),
                other => other.label().to_string(),
            },
            games: games
                .iter()
                .enumerate()
                .filter(|(_, g)| filter.matches(g))
                .map(|(i, _)| i)
                .collect(),
        });
        self.rows.set_sections(sections);
    }

    fn download_for(&self, game_id: i64) -> Option<&Download> {
        self.downloads
            .iter()
            .find(|d| d.game.as_ref().is_some_and(|g| g.id == game_id))
    }

    /// Derives what each game's tile and page show from the queue.
    fn rebuild_installs(&mut self) {
        let mut installs = std::collections::HashMap::new();
        for download in &self.downloads {
            let Some(game_id) = download.game.as_ref().map(|g| g.id) else {
                continue;
            };
            if download.finished_at.is_some() && download.error.is_none() {
                continue;
            }
            let progress = self.progress.get(&download.id);
            let error = download
                .error_message
                .clone()
                .or_else(|| download.error.clone());
            installs.insert(
                game_id,
                InstallState {
                    download_id: download.id.clone(),
                    progress: progress.map_or(0.0, |p| p.progress),
                    bps: progress.map_or(0.0, |p| p.bps),
                    eta_seconds: progress.map_or(0.0, |p| p.eta),
                    stage: match progress {
                        Some(p) if !p.stage.is_empty() => capitalize(&p.stage),
                        _ if download.started_at.is_some() => "Starting".to_string(),
                        _ => "Queued".to_string(),
                    },
                    cancelling: self.discarding.contains(&download.id),
                    error,
                },
            );
        }
        for game_id in &self.pending_installs {
            installs.entry(*game_id).or_insert_with(|| InstallState {
                stage: "Queueing".into(),
                ..Default::default()
            });
        }
        self.installs = installs;
    }

    fn drive_shot(&mut self, ctx: &egui::Context) {
        let Some(shot) = self.shot.as_mut() else {
            return;
        };
        let now = Instant::now();
        ctx.request_repaint_after(Duration::from_millis(100));

        if shot.capture_pending {
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
            shot.capture_pending = false;
            shot.captured = true;
            if shot.script.is_empty() && self.installs.is_empty() && self.running.is_empty() {
                self.shot = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }

        let loaded = !matches!(self.games, Loadable::Loading) || self.error.is_some();
        if let Some(until) = shot.wait_until {
            if now < until {
                return;
            }
            shot.wait_until = None;
        }
        if loaded && let Some(step) = shot.script.pop_front() {
            match step {
                Step::Act(action) => self.actions.push(action),
                Step::Search(text) => {
                    self.query = text;
                    self.rebuild_sections();
                }
                Step::Wait(duration) => shot.wait_until = Some(now + duration),
                Step::Capture => {
                    shot.capture_pending = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                        egui::UserData::default(),
                    ));
                }
            }
            ctx.request_repaint();
            return;
        }

        // An install the script started runs to the end before the window
        // closes; closing would kill the daemon under it.
        let settled = loaded && self.installs.is_empty() && self.running.is_empty();
        if settled && shot.settled_at.is_none() {
            shot.settled_at = Some(now);
        }
        let ready = shot.settled_at.is_some_and(|at| now >= at + COVER_GRACE);
        let busy = !self.installs.is_empty() || !self.running.is_empty();
        if ready || (now >= shot.deadline && !busy) {
            if shot.captured {
                self.shot = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                shot.capture_pending = true;
                ctx.request_repaint();
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
        }
    }

    fn handle_events(&mut self) {
        for event in self.backend.poll() {
            match event {
                Event::Status(text) => self.status = text,
                Event::SignedIn(profile) => self.profile = Some(profile),
                Event::OwnedGames(games) => {
                    self.games = Loadable::Loaded(games);
                    self.rebuild_sections();
                }
                Event::Caves(caves) => {
                    self.installed = caves.iter().filter_map(CaveExt::game_id).collect();
                    self.caves = caves;
                    self.rebuild_sections();
                }
                Event::Downloads(downloads) => {
                    let listed: std::collections::HashSet<String> =
                        downloads.iter().map(|d| d.id.clone()).collect();
                    self.progress.retain(|id, _| listed.contains(id));
                    self.discarding.retain(|id| listed.contains(id));
                    for download in &downloads {
                        if let Some(game) = &download.game {
                            self.pending_installs.remove(&game.id);
                        }
                    }
                    // A queue attempt that produced no download is over.
                    if downloads.is_empty() {
                        self.pending_installs.clear();
                    }
                    self.downloads = downloads;
                    self.rebuild_installs();
                }
                Event::DownloadProgress {
                    download_id,
                    progress,
                } => {
                    self.progress.insert(download_id, progress);
                    self.rebuild_installs();
                }
                Event::DownloadFinished(download) => {
                    let title = download.game.as_ref().map_or("game", |g| g.title.as_str());
                    let updated = self.updates.remove(&download.cave_id).is_some();
                    self.notice = Some(if updated {
                        format!("Updated {title}")
                    } else {
                        format!("Installed {title}")
                    });
                }
                Event::Updates(updates) => {
                    self.updates = updates
                        .into_iter()
                        .map(|u| (u.cave_id.clone(), u))
                        .collect();
                    self.rebuild_sections();
                }
                Event::DownloadErrored(download) => {
                    let title = download.game.as_ref().map_or("game", |g| g.title.as_str());
                    let error = download
                        .error_message
                        .as_deref()
                        .or(download.error.as_deref())
                        .unwrap_or("unknown error");
                    self.notice = Some(format!("Install of {title} failed: {error}"));
                }
                Event::LaunchRunning { .. } => self.notice = Some("Running".into()),
                Event::LaunchFinished { cave_id, result } => {
                    self.running.remove(&cave_id);
                    self.notice = Some(match result {
                        Ok(()) => "Game exited".to_string(),
                        Err(error) => format!("Couldn't launch: {error}"),
                    });
                }
                Event::Prompt(prompt) => self.prompt = Some(prompt),
                Event::PromptClosed(id) => {
                    if self.prompt.as_ref().is_some_and(|p| p.id == id) {
                        self.prompt = None;
                    }
                }
                Event::UninstallFinished { result, .. } => {
                    self.notice = Some(match result {
                        Ok(()) => "Uninstalled".to_string(),
                        Err(error) => format!("Uninstall failed: {error}"),
                    });
                }
                Event::Error(message) => {
                    // A failed queue attempt has no download to report on.
                    self.pending_installs.clear();
                    self.rebuild_installs();
                    if self.games.get().is_none() {
                        self.games = Loadable::Failed(message.clone());
                    }
                    self.error = Some(message);
                }
            }
        }
    }
}

impl App {
    /// What the footer offers on the current page, in reading order.
    fn hints(&self) -> Vec<(Glyph, String)> {
        if let Some(prompt) = &self.prompt {
            let mut hints = Vec::new();
            if prompt.choices.len() > 1 {
                hints.push((Glyph::NavigateHorizontal, "Choose".to_string()));
            }
            if let Some(choice) = prompt.choices.get(prompt.focus) {
                hints.push((Glyph::Confirm, choice.clone()));
            }
            hints.push((Glyph::Back, "Dismiss".to_string()));
            return hints;
        }
        match self.page.clone() {
            Page::Library => vec![
                (Glyph::Navigate, "Browse".to_string()),
                (Glyph::Confirm, "Open".to_string()),
                (Glyph::Tab, "Filter".to_string()),
                (Glyph::Search, "Search".to_string()),
            ],
            Page::Game { index, button } => {
                let mut hints = Vec::new();
                if let Some(game) = self.game_at(index) {
                    let buttons = ui::game_buttons(
                        game,
                        &self.caves_for(game.id),
                        self.installs.get(&game.id),
                        self.is_running(game.id),
                        self.update_for(game.id),
                    );
                    if buttons.len() > 1 {
                        hints.push((Glyph::NavigateHorizontal, "Choose".to_string()));
                    }
                    if let Some((label, _)) = buttons.get(button) {
                        hints.push((Glyph::Confirm, label.to_string()));
                    }
                }
                hints.push((Glyph::Back, "Back".to_string()));
                hints
            }
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_events();
        let (keys, touches) = ctx.input(|i| {
            (
                i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Key { .. })),
                i.any_touches(),
            )
        });
        self.handle_keys(ctx);
        let pad = self.gamepad.poll(ctx, &mut self.actions);
        if pad {
            self.input_mode = InputMode::Gamepad;
        } else if keys {
            self.input_mode = InputMode::Keyboard;
        } else if touches {
            self.input_mode = InputMode::Touch;
        }
        self.drive_shot(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.input_mode != InputMode::Touch && self.games.get().is_some() {
            let hints = self.hints();
            ui::footer(ui, &self.glyphs, self.input_mode, &hints);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui::BG).inner_margin(24.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if self.page.is_library() {
                        ui::heading(ui, "Library");
                    } else {
                        // The heading doubles as the way back for touch.
                        let back = ui::back_button(ui);
                        ui.add_space(6.0);
                        let label = ui
                            .add(
                                egui::Label::new(
                                    egui::RichText::new("Library")
                                        .font(egui::FontId::proportional(30.0))
                                        .color(egui::Color32::from_gray(0xee)),
                                )
                                .sense(egui::Sense::click()),
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if back.clicked() || label.clicked() {
                            self.actions.push(Action::Back);
                        }
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
                if self.page.is_library() && self.games.get().is_some() {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        for filter in Filter::ALL {
                            if ui::chip(ui, filter.label(), filter == self.filter).clicked() {
                                self.actions.push(Action::SetFilter(filter));
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let id = Self::search_id();
                            if std::mem::take(&mut self.blur_search) {
                                ui.memory_mut(|m| m.surrender_focus(id));
                            }
                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut self.query)
                                    .id(id)
                                    .hint_text("Search")
                                    .desired_width(200.0)
                                    .font(egui::FontId::proportional(15.0)),
                            );
                            if std::mem::take(&mut self.focus_search) {
                                edit.request_focus();
                            }
                            if edit.changed() {
                                self.rebuild_sections();
                            }
                        });
                    });
                }
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
                        ui::LibraryView {
                            games,
                            installed: &self.installed,
                            installs: &self.installs,
                            updatable: &self.updatable(),
                            covers: &self.covers,
                        },
                        &mut self.rows,
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
                                let running = self.is_running(game.id);
                                let update = self.update_for(game.id).cloned();
                                ui::game_detail(
                                    ui,
                                    ui::GameView {
                                        game,
                                        caves: &caves,
                                        install: self.installs.get(&game.id),
                                        running,
                                        update: update.as_ref(),
                                        focused_button: button,
                                    },
                                    &mut self.actions,
                                );
                            }
                            None => self.actions.push(Action::Back),
                        }
                    }
                }
            });
        if let Some(prompt) = &self.prompt {
            ui::prompt(ui.ctx(), prompt, &mut self.actions);
        }
        self.apply_actions();
        if !self.installs.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.backend.shutdown();
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

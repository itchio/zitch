//! Application state and the window that draws it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::backend::{Backend, Command, Event};
use crate::gamepad::Gamepad;
use crate::glyphs::{Glyph, Glyphs, InputMode};
use crate::images::CoverLoader;
use crate::model::{
    Action, Cave, CaveExt, CollectionGames, Direction, Download, DownloadProgress, Game,
    GameUpdate, InstallState, Kind, LaunchFailure, Loadable, Page, Profile, Prompt, Tab, UserExt,
    playable_here,
};
use crate::ui;

/// How the window presents itself, from the command line.
pub struct Options {
    /// Extra magnification on top of the screen-derived layout.
    pub zoom: f32,
    /// Lay out for a display of this many points and letterbox it.
    pub emulate: Option<(f32, f32)>,
    pub low_spec: Option<bool>,
    pub minimize_while_playing: bool,
}

pub struct App {
    backend: Backend,
    covers: CoverLoader,
    gamepad: Gamepad,
    glyphs: Glyphs,
    /// The device the user touched last, which picks the footer's glyphs.
    input_mode: InputMode,
    status: String,
    profile: Option<Profile>,
    /// Games the profile has a key for, in butler's order (newest first).
    owned: Loadable<Vec<Game>>,
    caves: Vec<Cave>,
    collections: Loadable<Vec<CollectionGames>>,
    /// Show only installed games on the Collections tab.
    collections_installed_only: bool,
    /// Installed games per collection from butler's filter, or `None` while
    /// unknown. Cleared whenever the installs change.
    collection_installed: Option<std::collections::HashMap<i64, Vec<Game>>>,
    /// Collections with a page request in flight.
    collection_loading: std::collections::HashSet<i64>,
    /// The Collections tab's carousels, one per collection.
    pub collection_rows: ui::Rows,
    /// Every game the screen can show, owned or installed, by id.
    catalog: std::collections::HashMap<i64, Game>,
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
    /// Games in flight, by cave id, with when they were launched.
    running: std::collections::HashMap<String, Instant>,
    /// Why the last launch of a cave failed, until it is launched again.
    launch_failures: std::collections::HashMap<String, LaunchFailure>,
    /// Updates butler found, by cave.
    updates: std::collections::HashMap<String, GameUpdate>,
    /// A question from the backend, shown over everything until answered.
    prompt: Option<Prompt>,
    /// Questions that arrived while another was showing, oldest first.
    prompt_queue: std::collections::VecDeque<Prompt>,
    /// Whether butler can reach itch.io; installs and updates need it.
    online: bool,
    tab: Tab,
    /// Row and button with focus on the Downloads tab.
    downloads_focus: (usize, usize),
    /// Downloads that completed this session. butler drops them from its
    /// queue as soon as they finish, so the tab remembers them itself.
    finished: Vec<Download>,
    /// Hide games with no upload for this computer, on every tab.
    playable_only: bool,
    query: String,
    /// Move keyboard focus into the search box on the next frame.
    focus_search: bool,
    /// Take keyboard focus out of the search box on the next frame.
    blur_search: bool,
    page: Page,
    error: Option<String>,
    /// Something the user just did, shown in the header.
    pub actions: Vec<Action>,
    pub rows: ui::Rows,
    shot: Option<Shot>,
    /// Pretend the display is this many points, whatever the window size.
    emulate: Option<(f32, f32)>,
    /// Force the cover policy instead of picking it by screen size.
    low_spec: Option<bool>,
    minimize_while_playing: bool,
    /// For window commands raised from events, outside a frame.
    ctx: egui::Context,
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
            "home" => Ok(Step::Act(Action::MoveFocus(Direction::Home))),
            "end" => Ok(Step::Act(Action::MoveFocus(Direction::End))),
            "enter" => Ok(Step::Act(Action::Activate)),
            "back" => Ok(Step::Act(Action::Back)),
            "capture" => Ok(Step::Capture),
            "tab" => Ok(Step::Act(Action::ToggleFilter)),
            "nexttab" => Ok(Step::Act(Action::CycleTab(1))),
            "prevtab" => Ok(Step::Act(Action::CycleTab(-1))),
            // A stand-in question, to look at the modal without a game that
            // asks one.
            "guide" => Ok(Step::Act(Action::ToggleOverlay)),
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
        options: Options,
        shot: Option<Shot>,
    ) -> Self {
        let Options {
            zoom,
            emulate,
            low_spec,
            minimize_while_playing,
        } = options;
        ui::install_fonts(ctx);
        ctx.set_visuals(ui::visuals());
        ctx.set_zoom_factor(zoom);
        Self {
            backend,
            covers,
            gamepad: Gamepad::new(ctx.clone()),
            glyphs: Glyphs::load(ctx),
            input_mode: InputMode::Keyboard,
            status: String::new(),
            profile: None,
            owned: Loadable::Loading,
            caves: Vec::new(),
            catalog: Default::default(),
            installed: Default::default(),
            downloads: Vec::new(),
            progress: Default::default(),
            pending_installs: Default::default(),
            discarding: Default::default(),
            installs: Default::default(),
            running: Default::default(),
            launch_failures: Default::default(),
            updates: Default::default(),
            prompt: None,
            prompt_queue: Default::default(),
            online: true,
            tab: Tab::default(),
            collections: Loadable::default(),
            collections_installed_only: false,
            collection_installed: None,
            collection_loading: Default::default(),
            collection_rows: ui::Rows::default(),
            downloads_focus: (0, 0),
            finished: Vec::new(),
            playable_only: false,
            query: String::new(),
            focus_search: false,
            blur_search: false,
            page: Page::Library,
            error: None,
            actions: Vec::new(),
            rows: ui::Rows::default(),
            shot,
            emulate,
            low_spec,
            minimize_while_playing,
            ctx: ctx.clone(),
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
            key(
                Modifiers::NONE,
                Key::Home,
                Action::MoveFocus(Direction::Home),
            );
            key(Modifiers::NONE, Key::End, Action::MoveFocus(Direction::End));
            key(Modifiers::NONE, Key::Enter, Action::Activate);
            key(Modifiers::NONE, Key::Escape, Action::Back);
            key(Modifiers::NONE, Key::Slash, Action::FocusSearch);
            key(Modifiers::NONE, Key::Tab, Action::ToggleFilter);
            key(Modifiers::SHIFT, Key::Tab, Action::ToggleFilter);
            key(Modifiers::NONE, Key::Q, Action::CycleTab(-1));
            key(Modifiers::NONE, Key::E, Action::CycleTab(1));
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

    fn game(&self, id: i64) -> Option<&Game> {
        self.catalog.get(&id)
    }

    /// Owned games first, so their fresher records win over the copy each
    /// cave carries; then installed games with no key.
    /// The carousel rows the current tab shows, if it has any.
    /// Brings the window to the front. Wayland compositors that refuse
    /// ignore the request.
    fn raise_window(&self) {
        self.ctx
            .send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Gets out of the game's way by minimizing, which every window system
    /// answers by focusing what was behind.
    fn hide_window(&self) {
        self.ctx
            .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }

    fn active_rows(&mut self) -> Option<&mut ui::Rows> {
        match self.tab {
            Tab::Library => Some(&mut self.rows),
            Tab::Collections => Some(&mut self.collection_rows),
            Tab::Downloads => None,
        }
    }

    fn rebuild_catalog(&mut self) {
        let mut catalog = std::collections::HashMap::new();
        for game in self.owned.get().into_iter().flatten() {
            catalog.insert(game.id, game.clone());
        }
        for game in self.caves.iter().filter_map(|cave| cave.game.as_ref()) {
            catalog.entry(game.id).or_insert_with(|| game.clone());
        }
        let collection_games = self
            .collections
            .get()
            .into_iter()
            .flatten()
            .flat_map(|c| &c.games);
        let installed_games = self
            .collection_installed
            .iter()
            .flat_map(|m| m.values())
            .flatten();
        for game in collection_games.chain(installed_games) {
            catalog.entry(game.id).or_insert_with(|| game.clone());
        }
        self.catalog = catalog;
    }

    fn apply(&mut self, action: Action) {
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
                    self.prompt = self.prompt_queue.pop_front();
                    self.backend.send(Command::Answer { prompt: id, choice });
                }
                Action::ToggleOverlay => self.raise_window(),
                _ => {}
            }
            return;
        }
        match action {
            Action::MoveFocus(direction) => match self.page.clone() {
                Page::Library => match self.tab {
                    Tab::Library | Tab::Collections => {
                        if let Some(rows) = self.active_rows() {
                            rows.move_focus(direction);
                        }
                    }
                    Tab::Downloads => {
                        let rows = self.download_rows();
                        let (row, button) = self.downloads_focus_in(&rows);
                        let row = match direction {
                            Direction::Up => row.saturating_sub(1),
                            Direction::Down => (row + 1).min(rows.len().saturating_sub(1)),
                            Direction::Home => 0,
                            Direction::End => rows.len().saturating_sub(1),
                            _ => row,
                        };
                        let buttons = rows.get(row).map_or(0, |r| r.buttons.len());
                        let button = match direction {
                            Direction::Left => button.saturating_sub(1),
                            Direction::Right => button + 1,
                            _ => button,
                        }
                        .min(buttons.saturating_sub(1));
                        self.downloads_focus = (row, button);
                    }
                },
                Page::Game { id, button } => {
                    let Some(game) = self.game(id) else {
                        return;
                    };
                    let buttons = ui::game_buttons(
                        game,
                        &self.caves_for(game.id),
                        self.installs.get(&game.id),
                        self.is_running(game.id),
                        self.update_for(game.id),
                        self.online,
                    )
                    .len()
                    .max(1);
                    let button = match direction {
                        Direction::Left => button.saturating_sub(1),
                        Direction::Right => (button + 1).min(buttons - 1),
                        _ => button,
                    };
                    self.page = Page::Game { id, button };
                }
            },
            Action::FocusIndex(index) => {
                if let Some(game) = self.owned.get().and_then(|games| games.get(index)) {
                    self.rows.focus_game(game.id);
                }
            }
            Action::FocusTile { row, col } => {
                if let Some(rows) = self.active_rows() {
                    rows.focus_tile(row, col);
                }
            }
            Action::FocusButton(button) => {
                if let Page::Game { id, .. } = self.page {
                    self.page = Page::Game { id, button };
                }
            }
            Action::Activate => match self.page.clone() {
                Page::Library => match self.tab {
                    Tab::Library | Tab::Collections => {
                        if let Some(id) = self
                            .active_rows()
                            .and_then(|rows| rows.focused_game())
                            .filter(|id| self.catalog.contains_key(id))
                        {
                            self.actions
                                .push(Action::Open(Page::Game { id, button: 0 }));
                        }
                    }
                    Tab::Downloads => {
                        let rows = self.download_rows();
                        let (row, button) = self.downloads_focus_in(&rows);
                        if let Some((_, action)) = rows.get(row).and_then(|r| r.buttons.get(button))
                        {
                            self.actions.push(action.clone());
                        }
                    }
                },
                Page::Game { id, button } => {
                    let Some(game) = self.game(id) else {
                        return;
                    };
                    let buttons = ui::game_buttons(
                        game,
                        &self.caves_for(game.id),
                        self.installs.get(&game.id),
                        self.is_running(game.id),
                        self.update_for(game.id),
                        self.online,
                    );
                    if let Some((_, action)) = buttons.get(button) {
                        self.actions.push(action.clone());
                    }
                }
            },
            Action::SetPlayableOnly(on) => {
                if self.playable_only != on {
                    self.playable_only = on;
                    self.rebuild_sections();
                    self.rebuild_collection_sections();
                    self.rows.follow = true;
                    self.collection_rows.follow = true;
                }
            }
            Action::ToggleFilter => match (self.page.is_library(), self.tab) {
                (true, Tab::Library) => {
                    self.actions
                        .push(Action::SetPlayableOnly(!self.playable_only));
                }
                (true, Tab::Collections) => {
                    self.actions.push(Action::SetCollectionsInstalledOnly(
                        !self.collections_installed_only,
                    ));
                }
                _ => {}
            },
            Action::SetCollectionsInstalledOnly(on) => {
                if self.collections_installed_only != on {
                    self.collections_installed_only = on;
                    self.request_collection_installed();
                    self.rebuild_collection_sections();
                    self.collection_rows.follow = true;
                }
            }
            Action::ToggleOverlay => self.raise_window(),
            Action::BackToGame => {
                if !self.running.is_empty() {
                    self.hide_window();
                }
            }
            Action::MoreGames { row } => {
                let Some(id) = self
                    .collection_rows
                    .sections
                    .get(row)
                    .and_then(|s| s.collection)
                else {
                    return;
                };
                let cursor = self
                    .collections
                    .get()
                    .into_iter()
                    .flatten()
                    .find(|c| c.collection.id == id)
                    .and_then(|c| c.next_cursor.clone());
                if let Some(cursor) = cursor
                    && self.collection_loading.insert(id)
                {
                    self.backend.send(Command::CollectionPage {
                        collection_id: id,
                        cursor,
                    });
                }
            }
            Action::SetTab(tab) => {
                if self.page.is_library() {
                    self.tab = tab;
                    self.error = None;
                    self.blur_search = true;
                    self.rows.follow = true;
                    self.collection_rows.follow = true;
                }
            }
            Action::CycleTab(step) => {
                if self.page.is_library() {
                    self.actions.push(Action::SetTab(self.tab.next(step)));
                }
            }
            Action::FocusSearch => {
                if self.page.is_library() && self.tab == Tab::Library {
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
                Page::Library if self.tab != Tab::Library => {
                    self.actions.push(Action::SetTab(Tab::Library))
                }
                Page::Library if !self.query.is_empty() => self.actions.push(Action::ClearSearch),
                Page::Library => self.error = None,
                Page::Game { id, .. } => {
                    if let Some(rows) = self.active_rows() {
                        rows.focus_game(id);
                    }
                    self.page = Page::Library;
                }
            },
            Action::Open(page) => {
                self.error = None;
                self.page = page;
            }
            Action::Update { cave_id } => {
                if let Some(update) = self.updates.get(&cave_id) {
                    self.backend.send(Command::Update {
                        update: Box::new(update.clone()),
                    });
                }
            }
            Action::QuitGame { cave_id } => {
                if self.running.contains_key(&cave_id) {
                    self.backend.send(Command::QuitGame { cave_id });
                }
            }
            Action::Play { cave_id } => {
                if !self.running.contains_key(&cave_id) {
                    self.running.insert(cave_id.clone(), Instant::now());
                    self.launch_failures.remove(&cave_id);
                    self.backend.send(Command::Launch { cave_id });
                }
            }
            // Only meaningful while a prompt is open, handled above.
            Action::Answer { .. } | Action::PromptFocus(_) => {}
            Action::Install { game_id } => {
                let Some(game) = self.game(game_id).cloned() else {
                    return;
                };
                if self.installs.contains_key(&game_id) {
                    return;
                }
                // Shown as installing from this instant; the queue listing
                // that follows replaces it.
                self.pending_installs.insert(game_id);
                self.backend.send(Command::Install {
                    game: Box::new(game),
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
                let title = self
                    .caves
                    .iter()
                    .find(|cave| cave.id == cave_id)
                    .and_then(|cave| cave.game.as_ref())
                    .map_or_else(|| "this game".to_string(), |game| game.title.clone());
                self.backend.send(Command::Uninstall { cave_id, title });
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
            .any(|cave| cave.game_id() == Some(game_id) && self.running.contains_key(&cave.id))
    }

    /// Lays the home screen out as carousels, the way the itch app's
    /// Library tab does: what is installed, then what was played last,
    /// anything with an update, and everything owned.
    fn rebuild_sections(&mut self) {
        if self.owned.get().is_none() {
            return;
        }
        let query = self.query.trim().to_lowercase();
        if !query.is_empty() {
            // Searching narrows the whole screen to one row of matches,
            // owned first then installed-only, each in its usual order.
            let mut seen = std::collections::HashSet::new();
            let matches: Vec<i64> = self
                .owned_ids()
                .into_iter()
                .chain(self.installed_ids())
                .filter(|id| seen.insert(*id))
                .filter(|id| {
                    self.catalog
                        .get(id)
                        .is_some_and(|g| self.passes(g) && g.title.to_lowercase().contains(&query))
                })
                .collect();
            let title = match matches.len() {
                0 => "No matches".to_string(),
                1 => "1 match".to_string(),
                n => format!("{n} matches"),
            };
            self.rows.set_sections(vec![ui::Section {
                title,
                games: matches,
                note: None,
                more: false,
                collection: None,
            }]);
            return;
        }
        let mut sections = Vec::new();

        sections.extend(self.section(|_| "Installed".into(), self.installed_ids()));

        let mut played: Vec<&Cave> = self
            .caves
            .iter()
            .filter(|cave| {
                cave.stats
                    .as_ref()
                    .is_some_and(|s| s.last_touched_at.is_some() && s.seconds_run > 0)
            })
            .collect();
        played.sort_by_key(|cave| std::cmp::Reverse(last_touched(cave)));
        let mut seen = std::collections::HashSet::new();
        let played: Vec<i64> = played
            .iter()
            .filter_map(|cave| cave.game_id())
            .filter(|id| seen.insert(*id))
            .take(12)
            .collect();
        sections.extend(self.section(|_| "Recently played".into(), played));

        let updatable = self.updatable();
        let mut seen = std::collections::HashSet::new();
        let updates: Vec<i64> = self
            .caves
            .iter()
            .filter_map(|cave| cave.game_id())
            .filter(|id| updatable.contains(id) && seen.insert(*id))
            .collect();
        sections.extend(self.section(|_| "Updates".into(), updates));

        // The owned library, one row per kind of thing.
        for kind in Kind::ALL {
            let games: Vec<i64> = self
                .owned_ids()
                .into_iter()
                .filter(|id| self.catalog.get(id).is_some_and(|g| kind.matches(g)))
                .collect();
            sections.extend(self.section(|n| format!("{} · {n}", kind.label()), games));
        }
        self.rows.set_sections(sections);
    }

    /// Whether the game clears the page-wide filter.
    fn passes(&self, game: &Game) -> bool {
        !self.playable_only || playable_here(game)
    }

    /// A row after the page-wide filter: none when it was empty anyway, a
    /// note in place of tiles when the filter took everything.
    fn section(&self, title: impl Fn(usize) -> String, games: Vec<i64>) -> Option<ui::Section> {
        if games.is_empty() {
            return None;
        }
        let games: Vec<i64> = games
            .into_iter()
            .filter(|id| self.catalog.get(id).is_none_or(|g| self.passes(g)))
            .collect();
        let note = games
            .is_empty()
            .then(|| "Nothing here runs on this computer".to_string());
        Some(ui::Section {
            title: title(games.len()),
            games,
            note,
            more: false,
            collection: None,
        })
    }

    fn owned_ids(&self) -> Vec<i64> {
        self.owned
            .get()
            .into_iter()
            .flatten()
            .map(|game| game.id)
            .collect()
    }

    /// Installed games, most recently touched first, as the itch app sorts
    /// its Installed stripe.
    fn installed_ids(&self) -> Vec<i64> {
        let mut caves: Vec<&Cave> = self.caves.iter().collect();
        caves.sort_by_key(|cave| std::cmp::Reverse(last_touched(cave)));
        let mut seen = std::collections::HashSet::new();
        caves
            .iter()
            .filter_map(|cave| cave.game_id())
            .filter(|id| seen.insert(*id))
            .collect()
    }

    fn download_for(&self, game_id: i64) -> Option<&Download> {
        self.downloads
            .iter()
            .find(|d| d.game.as_ref().is_some_and(|g| g.id == game_id))
    }

    /// Derives what each game's tile and page show from the queue.
    /// One row per collection. With the installed filter on, collections
    /// with nothing installed sink to the bottom and say so.
    fn rebuild_collection_sections(&mut self) {
        let Some(collections) = self.collections.get() else {
            return;
        };
        let installed_only = self.collections_installed_only;
        let mut sections: Vec<ui::Section> = collections
            .iter()
            .map(|c| {
                // Butler's answer covers the whole collection; until it
                // arrives, the pages fetched so far stand in.
                let exact = installed_only
                    .then(|| self.collection_installed.as_ref()?.get(&c.collection.id))
                    .flatten();
                let listed = exact.unwrap_or(&c.games);
                let games: Vec<i64> = listed
                    .iter()
                    .filter(|g| self.passes(g))
                    .map(|g| g.id)
                    .filter(|id| !installed_only || self.installed.contains(id))
                    .collect();
                let more = exact.is_none() && c.next_cursor.is_some();
                let note = match (games.is_empty(), installed_only) {
                    _ if more => None,
                    _ if c.games.is_empty() => Some("Empty collection".to_string()),
                    (true, true) => Some("Nothing installed from this collection".to_string()),
                    (true, false) => Some("Nothing here runs on this computer".to_string()),
                    _ => None,
                };
                ui::Section {
                    title: format!("{} · {}", c.collection.title, c.collection.games_count),
                    games,
                    note,
                    more,
                    collection: Some(c.collection.id),
                }
            })
            .collect();
        sections.sort_by_key(|s| s.games.is_empty() && !s.more);
        self.collection_rows.set_sections(sections);
    }

    /// Asks butler which games in each collection are installed, when the
    /// filter needs it and the answer is not already known.
    fn request_collection_installed(&mut self) {
        if !self.collections_installed_only || self.collection_installed.is_some() {
            return;
        }
        let Some(collections) = self.collections.get() else {
            return;
        };
        self.backend.send(Command::CollectionsInstalled {
            collection_ids: collections.iter().map(|c| c.collection.id).collect(),
        });
    }

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

        let loaded = !matches!(self.owned, Loadable::Loading) || self.error.is_some();
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
                    self.owned = Loadable::Loaded(games);
                    self.rebuild_catalog();
                    self.rebuild_sections();
                }
                Event::Collections(collections) => {
                    self.collections = Loadable::Loaded(collections);
                    self.collection_loading.clear();
                    self.collection_installed = None;
                    self.request_collection_installed();
                    self.rebuild_catalog();
                    self.rebuild_collection_sections();
                }
                Event::CollectionPage {
                    collection_id,
                    games,
                    next_cursor,
                } => {
                    self.collection_loading.remove(&collection_id);
                    if let Some(c) = self
                        .collections
                        .get_mut()
                        .into_iter()
                        .flatten()
                        .find(|c| c.collection.id == collection_id)
                    {
                        let known: std::collections::HashSet<i64> =
                            c.games.iter().map(|g| g.id).collect();
                        c.games
                            .extend(games.into_iter().filter(|g| !known.contains(&g.id)));
                        c.next_cursor = next_cursor;
                    }
                    self.rebuild_catalog();
                    self.rebuild_collection_sections();
                }
                Event::CollectionPageFailed {
                    collection_id,
                    error,
                } => {
                    log::error!("collection {collection_id} page: {error}");
                    self.collection_loading.remove(&collection_id);
                    // Stop asking; the spinner would otherwise never leave.
                    if let Some(c) = self
                        .collections
                        .get_mut()
                        .into_iter()
                        .flatten()
                        .find(|c| c.collection.id == collection_id)
                    {
                        c.next_cursor = None;
                    }
                    self.error = Some(format!("Couldn't load more: {error}"));
                    self.rebuild_collection_sections();
                }
                Event::CollectionsInstalled(lists) => {
                    self.collection_installed = Some(lists.into_iter().collect());
                    self.rebuild_catalog();
                    self.rebuild_collection_sections();
                }
                Event::Caves(caves) => {
                    self.installed = caves.iter().filter_map(CaveExt::game_id).collect();
                    self.caves = caves;
                    self.collection_installed = None;
                    self.request_collection_installed();
                    self.rebuild_catalog();
                    self.rebuild_sections();
                    self.rebuild_collection_sections();
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
                    self.updates.remove(&download.cave_id);
                    self.finished.retain(|d| d.id != download.id);
                    self.finished.insert(0, download);
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
                    self.error = Some(format!("Install of {title} failed: {error}"));
                }
                Event::LaunchRunning { .. } => {
                    if self.minimize_while_playing {
                        self.hide_window();
                    }
                }
                Event::LaunchFinished { cave_id, result } => {
                    self.running.remove(&cave_id);
                    if let Err(failure) = result {
                        // The game's page shows the failure in full; the
                        // header line is for when the user is elsewhere.
                        let game_id = self
                            .caves
                            .iter()
                            .find(|c| c.id == cave_id)
                            .and_then(CaveExt::game_id);
                        let on_page =
                            matches!(&self.page, Page::Game { id, .. } if Some(*id) == game_id);
                        if !on_page {
                            self.error = Some(format!("Couldn't launch: {}", failure.message));
                        }
                        self.launch_failures.insert(cave_id.clone(), failure);
                    }
                    // Take the screen back. Most window systems already hand
                    // focus to the last focused window when the game's goes
                    // away; this covers the ones that do not, and Wayland
                    // compositors that refuse simply ignore it.
                    self.ctx
                        .send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                Event::Online(online) => self.online = online,
                Event::Prompt(prompt) => {
                    if self.prompt.is_some() {
                        self.prompt_queue.push_back(prompt);
                    } else {
                        self.prompt = Some(prompt);
                    }
                }
                Event::PromptClosed(id) => {
                    self.prompt_queue.retain(|p| p.id != id);
                    if self.prompt.as_ref().is_some_and(|p| p.id == id) {
                        self.prompt = self.prompt_queue.pop_front();
                    }
                }
                Event::UninstallFinished { result, .. } => {
                    if let Err(error) = result {
                        self.error = Some(format!("Uninstall failed: {error}"));
                    }
                }
                Event::SyncFailed(error) => log::warn!("sync: {error}"),
                Event::CollectionsFailed(error) => {
                    log::error!("loading collections: {error}");
                    if self.collections.get().is_none() {
                        self.collections = Loadable::Failed(error);
                    }
                }
                Event::InstallDeclined { game_id } => {
                    self.pending_installs.remove(&game_id);
                    self.rebuild_installs();
                }
                Event::InstallFailed { game_id, error } => {
                    self.pending_installs.remove(&game_id);
                    self.rebuild_installs();
                    let title = self.game(game_id).map_or("game", |g| g.title.as_str());
                    self.error = Some(format!("Couldn't install {title}: {error}"));
                }
                Event::DiscardFailed { download_id, error } => {
                    self.discarding.remove(&download_id);
                    self.rebuild_installs();
                    self.error = Some(format!("Couldn't cancel: {error}"));
                }
                Event::Error(message) => {
                    if self.owned.get().is_none() {
                        self.owned = Loadable::Failed(message.clone());
                    }
                    self.error = Some(message);
                }
            }
        }
    }
}

impl App {
    /// What the footer offers on the current page, in reading order.
    /// What the Downloads tab lists: butler's queue first, in its order,
    /// then what finished this session.
    /// The stored focus, clamped to rows that still exist. The queue changes
    /// underneath the focus, so every reader clamps rather than trusting it.
    fn downloads_focus_in(&self, rows: &[ui::DownloadRow<'_>]) -> (usize, usize) {
        let (row, button) = self.downloads_focus;
        let row = row.min(rows.len().saturating_sub(1));
        let buttons = rows.get(row).map_or(0, |r| r.buttons.len());
        (row, button.min(buttons.saturating_sub(1)))
    }

    fn download_rows(&self) -> Vec<ui::DownloadRow<'_>> {
        let mut rows = Vec::new();
        let mut queue: Vec<&Download> = self.downloads.iter().collect();
        queue.sort_by_key(|d| (d.error.is_some(), d.position));
        for download in queue {
            let game = download.game.as_ref();
            let game_id = game.map(|g| g.id);
            let title = game.map_or_else(|| "Download".to_string(), |g| g.title.clone());
            let updating = self.caves.iter().any(|cave| cave.id == download.cave_id);
            let prefix = if updating { "Update: " } else { "" };
            let error = download
                .error_message
                .as_deref()
                .or(download.error.as_deref());
            let mut buttons = Vec::new();
            let (detail, progress, failed) = if let Some(error) = error {
                if let Some(game_id) = game_id {
                    buttons.push(("Retry", Action::RetryInstall { game_id }));
                    buttons.push(("Dismiss", Action::CancelInstall { game_id }));
                }
                (format!("{prefix}Failed, {error}"), None, true)
            } else {
                if let Some(game_id) = game_id {
                    let label = if self.discarding.contains(&download.id) {
                        "Cancelling"
                    } else {
                        "Cancel"
                    };
                    buttons.push((label, Action::CancelInstall { game_id }));
                }
                match self.progress.get(&download.id) {
                    Some(p) if p.bps > 0.0 => (
                        format!(
                            "{prefix}{}, {:.0}%, {}/s, {} left",
                            capitalize(&p.stage),
                            p.progress * 100.0,
                            ui::human_size(p.bps as i64),
                            ui::human_duration_seconds(p.eta as i64),
                        ),
                        Some(p.progress as f32),
                        false,
                    ),
                    Some(p) if !p.stage.is_empty() => (
                        format!(
                            "{prefix}{}, {:.0}%",
                            capitalize(&p.stage),
                            p.progress * 100.0
                        ),
                        Some(p.progress as f32),
                        false,
                    ),
                    _ if download.started_at.is_some() => {
                        (format!("{prefix}Starting"), Some(0.0), false)
                    }
                    _ => (format!("{prefix}Queued"), None, false),
                }
            };
            rows.push(ui::DownloadRow {
                game,
                title,
                detail,
                progress,
                failed,
                buttons,
            });
        }
        for download in &self.finished {
            let game = download.game.as_ref();
            let mut buttons = Vec::new();
            if let Some(game) = game.filter(|g| self.catalog.contains_key(&g.id)) {
                buttons.push((
                    "Open",
                    Action::Open(Page::Game {
                        id: game.id,
                        button: 0,
                    }),
                ));
            }
            rows.push(ui::DownloadRow {
                game,
                title: game.map_or_else(|| "Download".to_string(), |g| g.title.clone()),
                detail: "Finished".to_string(),
                progress: None,
                failed: false,
                buttons,
            });
        }
        rows
    }

    fn hints(&self) -> Vec<(Vec<Glyph>, String)> {
        if let Some(prompt) = &self.prompt {
            let mut hints = Vec::new();
            if prompt.choices.len() > 1 {
                hints.push((vec![Glyph::NavigateHorizontal], "Choose".to_string()));
            }
            if let Some(choice) = prompt.choices.get(prompt.focus) {
                hints.push((vec![Glyph::Confirm], choice.clone()));
            }
            hints.push((vec![Glyph::Back], "Dismiss".to_string()));
            return hints;
        }
        // The tab strip already shows the bumpers, so no hint repeats them.
        match self.page.clone() {
            Page::Library => match self.tab {
                Tab::Library => vec![
                    (vec![Glyph::Navigate], "Browse".to_string()),
                    (vec![Glyph::Confirm], "Open".to_string()),
                    (
                        vec![Glyph::FilterLeft, Glyph::FilterRight],
                        "Filter".to_string(),
                    ),
                    (vec![Glyph::Search], "Search".to_string()),
                ],
                Tab::Collections => {
                    let mut hints = vec![(vec![Glyph::Navigate], "Browse".to_string())];
                    if self.collection_rows.focused_game().is_some() {
                        hints.push((vec![Glyph::Confirm], "Open".to_string()));
                    }
                    hints.push((
                        vec![Glyph::FilterLeft, Glyph::FilterRight],
                        "Filter".to_string(),
                    ));
                    hints.push((vec![Glyph::Back], "Back".to_string()));
                    hints
                }
                Tab::Downloads => {
                    let rows = self.download_rows();
                    let (row, button) = self.downloads_focus_in(&rows);
                    let mut hints = Vec::new();
                    if rows.len() > 1 {
                        hints.push((vec![Glyph::Navigate], "Browse".to_string()));
                    }
                    if let Some((label, _)) = rows.get(row).and_then(|r| r.buttons.get(button)) {
                        hints.push((vec![Glyph::Confirm], label.to_string()));
                    }
                    hints.push((vec![Glyph::Back], "Back".to_string()));
                    hints
                }
            },
            Page::Game { id, button } => {
                let mut hints: Vec<(Vec<Glyph>, String)> = Vec::new();
                if let Some(game) = self.game(id) {
                    let buttons = ui::game_buttons(
                        game,
                        &self.caves_for(game.id),
                        self.installs.get(&game.id),
                        self.is_running(game.id),
                        self.update_for(game.id),
                        self.online,
                    );
                    if buttons.len() > 1 {
                        hints.push((vec![Glyph::NavigateHorizontal], "Choose".to_string()));
                    }
                    if let Some((label, _)) = buttons.get(button) {
                        hints.push((vec![Glyph::Confirm], label.to_string()));
                    }
                }
                hints.push((vec![Glyph::Back], "Back".to_string()));
                hints
            }
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some((width, height)) = self.emulate {
            // Pick the density that fits the emulated display in the window,
            // so the layout always sees that many points and resizing only
            // changes how many pixels each point gets.
            let ppp = ctx.pixels_per_point();
            let physical = ctx.content_rect().size() * ppp;
            let scale = (physical.x / width).min(physical.y / height);
            if scale.is_finite() && scale > 0.0 && (scale - ppp).abs() > 1e-3 {
                ctx.set_pixels_per_point(scale);
            }
        }
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
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let pad = self.gamepad.poll(focused, &mut self.actions);
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
        let Some((width, height)) = self.emulate else {
            self.draw(ui);
            return;
        };
        // Letterbox: the emulated display sits centered in the window.
        let window = ui.max_rect();
        let screen = egui::Rect::from_center_size(window.center(), egui::vec2(width, height));
        ui.painter().rect_filled(window, 0.0, egui::Color32::BLACK);
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(screen));
        inner.set_clip_rect(screen);
        self.draw(&mut inner);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.backend.shutdown();
    }
}

impl App {
    fn draw(&mut self, ui: &mut egui::Ui) {
        let screen = ui.max_rect();
        let m = ui::Metrics::for_screen(screen);
        self.covers.set_policy(crate::images::Policy::for_screen(
            screen.height(),
            self.low_spec,
        ));
        if self.input_mode != InputMode::Touch && self.owned.get().is_some() {
            let hints = self.hints();
            ui::footer(ui, &m, &self.glyphs, self.input_mode, &hints);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui::BG).inner_margin(m.margin))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui::logo(ui, &m, &self.glyphs);
                    if self.page.is_library() {
                        let downloading =
                            self.downloads.iter().filter(|d| d.error.is_none()).count();
                        if let Some(tab) = ui::tab_strip(
                            ui,
                            &m,
                            &self.glyphs,
                            self.input_mode,
                            self.tab,
                            downloading,
                        ) {
                            self.actions.push(Action::SetTab(tab));
                        }
                    } else if self.input_mode != InputMode::Gamepad {
                        // A pointer has no Back key; the strip's slot holds
                        // the way back. The pad's footer hint covers it.
                        if ui::back_button(ui, &m).clicked() {
                            self.actions.push(Action::Back);
                        }
                    } else {
                        // The strip's slot stays empty at the strip's height,
                        // so the frame around the page does not move.
                        ui.allocate_exact_size(
                            egui::vec2(0.0, ui::tab_strip_height(ui, &m)),
                            egui::Sense::hover(),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = m.space(12.0);
                        if let Some(user) = self.profile.as_ref().and_then(|p| p.user.as_ref()) {
                            ui::subtle(ui, &m, user.name());
                        }
                        if let Loadable::Loaded(games) = &self.owned {
                            ui::subtle(ui, &m, &format!("{} owned", games.len()));
                        }
                        if !self.online {
                            ui::offline(ui, &m);
                        }
                    });
                });
                if self.page.is_library() && self.tab == Tab::Library && self.owned.get().is_some()
                {
                    ui.add_space(m.space(10.0));
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = m.space(8.0);
                        if ui::filter_group(ui, &m, &[("Playable here", self.playable_only)])
                            .is_some()
                        {
                            self.actions
                                .push(Action::SetPlayableOnly(!self.playable_only));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let id = Self::search_id();
                            if std::mem::take(&mut self.blur_search) {
                                ui.memory_mut(|m| m.surrender_focus(id));
                            }
                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut self.query)
                                    .id(id)
                                    .hint_text(
                                        egui::RichText::new("Search")
                                            .font(egui::FontId::proportional(m.label)),
                                    )
                                    .desired_width(m.space(200.0))
                                    .font(egui::FontId::proportional(m.label)),
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
                if self.page.is_library()
                    && self.tab == Tab::Collections
                    && self.collections.get().is_some()
                {
                    ui.add_space(m.space(10.0));
                    ui.horizontal(|ui| {
                        let installed = self.collections_installed_only;
                        if let Some(picked) = ui::filter_group(
                            ui,
                            &m,
                            &[("All", !installed), ("Installed", installed)],
                        ) {
                            self.actions
                                .push(Action::SetCollectionsInstalledOnly(picked == 1));
                        }
                        ui.add_space(m.space(24.0));
                        if ui::filter_group(ui, &m, &[("Playable here", self.playable_only)])
                            .is_some()
                        {
                            self.actions
                                .push(Action::SetPlayableOnly(!self.playable_only));
                        }
                    });
                }
                // One line under the header: progress while loading, then
                // only failures. Its space is kept so the page never jumps.
                match (&self.owned, &self.error) {
                    (_, Some(error)) => ui::error(ui, &m, error),
                    (Loadable::Loaded(_), None) => ui::subtle(ui, &m, ""),
                    _ => ui::subtle(ui, &m, &self.status),
                }
                ui.add_space(m.space(16.0));
                match (&self.owned, self.page.clone()) {
                    (Loadable::NotLoaded | Loadable::Loading, _) => ui::centered_spinner(ui, &m),
                    (Loadable::Failed(_), _) => {}
                    (Loadable::Loaded(_), Page::Library) => match self.tab {
                        Tab::Library => ui::library(
                            ui,
                            &m,
                            ui::LibraryView {
                                games: &self.catalog,
                                installed: &self.installed,
                                installs: &self.installs,
                                updatable: &self.updatable(),
                                covers: &self.covers,
                                scrollbar: self.input_mode == InputMode::Keyboard,
                            },
                            &mut self.rows,
                            &mut self.actions,
                        ),
                        Tab::Collections => match &self.collections {
                            Loadable::Loaded(collections) if collections.is_empty() => {
                                ui::placeholder(ui, &m, "No collections")
                            }
                            Loadable::Failed(_) => {
                                ui::placeholder(ui, &m, "Couldn't load collections")
                            }
                            // Scoped so the rows' scroll state does not share
                            // egui ids with the Library tab's rows.
                            Loadable::Loaded(_) => {
                                ui.push_id("collections", |ui| {
                                    ui::library(
                                        ui,
                                        &m,
                                        ui::LibraryView {
                                            games: &self.catalog,
                                            installed: &self.installed,
                                            installs: &self.installs,
                                            updatable: &self.updatable(),
                                            covers: &self.covers,
                                            scrollbar: self.input_mode == InputMode::Keyboard,
                                        },
                                        &mut self.collection_rows,
                                        &mut self.actions,
                                    );
                                });
                            }
                            _ => ui::centered_spinner(ui, &m),
                        },
                        Tab::Downloads => {
                            let rows = self.download_rows();
                            let (row, button) = self.downloads_focus_in(&rows);
                            let mut actions = Vec::new();
                            ui::downloads(
                                ui,
                                &m,
                                ui::DownloadsView {
                                    rows: &rows,
                                    covers: &self.covers,
                                    focus: (row, button),
                                    scrollbar: self.input_mode == InputMode::Keyboard,
                                },
                                &mut actions,
                            );
                            self.actions.extend(actions);
                        }
                    },
                    (Loadable::Loaded(_), Page::Game { id, button }) => {
                        match self.catalog.get(&id) {
                            Some(game) => {
                                let caves: Vec<&Cave> = self
                                    .caves
                                    .iter()
                                    .filter(|cave| cave.game_id() == Some(game.id))
                                    .collect();
                                let running = self.is_running(game.id);
                                let running_since = caves
                                    .iter()
                                    .find_map(|cave| self.running.get(&cave.id))
                                    .copied();
                                let update = self.update_for(game.id).cloned();
                                let failure = caves
                                    .iter()
                                    .find_map(|cave| self.launch_failures.get(&cave.id));
                                ui::game_detail(
                                    ui,
                                    &m,
                                    ui::GameView {
                                        game,
                                        covers: &self.covers,
                                        caves: &caves,
                                        install: self.installs.get(&game.id),
                                        running,
                                        running_since,
                                        update: update.as_ref(),
                                        online: self.online,
                                        focused_button: button,
                                        failure,
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
            ui::prompt(ui.ctx(), &m, ui.max_rect(), prompt, &mut self.actions);
        }
        self.apply_actions();
        self.covers.end_frame();
        if !self.installs.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
    }
}

/// When the cave was last played or, failing that, installed.
fn last_touched(cave: &Cave) -> Option<String> {
    let stats = cave.stats.as_ref()?;
    stats
        .last_touched_at
        .clone()
        .or_else(|| stats.installed_at.clone())
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

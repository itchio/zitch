//! Application state and the window that draws it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::backend::{Backend, Command, Event};
use crate::gamepad::Gamepad;
use crate::glyphs::{Glyph, Glyphs, InputMode};
use crate::images::CoverLoader;
use crate::model::{
    Action, Cave, CaveExt, CollectionGames, Direction, Download, DownloadProgress, DownloadReason,
    Game, GameUpdate, InstallState, Kind, LaunchFailure, Loadable, Page, Profile, Prompt, Tab,
    UploadExt, UserExt, playable_here,
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
    /// A controller the host reads itself; otherwise gilrs is used.
    pub gamepad: Option<Gamepad>,
}

pub struct App {
    backend: Backend,
    covers: CoverLoader,
    gamepad: Gamepad,
    glyphs: Glyphs,
    /// The device the user touched last, which picks the footer's glyphs.
    input_mode: InputMode,
    /// Something has been pressed or touched this session. Until then a
    /// controller connecting picks the glyphs; after, only input does.
    input_seen: bool,
    /// The focused item while the menu drawer is open.
    menu: Option<usize>,
    /// Frames of the "Quitting" overlay left to show before the window is
    /// asked to close. The backend join that follows the close blocks the
    /// last frame on screen, so it must be one that already says Quitting;
    /// a few frames also let a pending screenshot read back first.
    quitting: Option<u32>,
    /// The backend's latest progress line, shown while the library loads.
    status: String,
    /// A failure with no page of its own, shown briefly above the footer.
    notice: Option<(String, Instant)>,
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
    /// Why a game's last install failed, shown on its page until the next
    /// attempt.
    install_failures: std::collections::HashMap<i64, String>,
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
    /// Per tab, the toolbar stop with controller focus, or none while
    /// focus is in the list below.
    toolbar_focus: [Option<usize>; Tab::ALL.len()],
    /// The row and button with focus on the Downloads list.
    downloads_row: (usize, usize),
    /// Hide games with no upload for this device, on every tab.
    playable_only: bool,
    query: String,
    /// Move keyboard focus into the search box on the next frame.
    focus_search: bool,
    /// Take keyboard focus out of the search box on the next frame.
    blur_search: bool,
    page: Page,
    /// Something the user just did, shown in the header.
    pub actions: Vec<Action>,
    pub rows: ui::Rows,
    shot: Option<Shot>,
    /// Pretend the display is this many points, whatever the window size.
    emulate: Option<(f32, f32)>,
    /// Force the cover policy instead of picking it by screen size.
    low_spec: Option<bool>,
    /// Drawn for a handheld, which has no keyboard: the search box stays
    /// hidden until there is an on-screen one to type into.
    handheld: bool,
    /// An update check the user asked for is still running.
    checking_updates: bool,
    /// When a check the user asked for last came back with nothing; the
    /// button says so for a moment.
    up_to_date_at: Option<Instant>,
    /// A library refresh the user asked for is still running.
    refreshing: bool,
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
    /// Raise a sample failure notice, to look at it.
    Notice,
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
            "notice" => Ok(Step::Notice),
            "nexttab" => Ok(Step::Act(Action::CycleTab(1))),
            "prevtab" => Ok(Step::Act(Action::CycleTab(-1))),
            // A stand-in question, to look at the modal without a game that
            // asks one.
            "guide" => Ok(Step::Act(Action::Menu)),
            "prompt" => Ok(Step::Act(Action::Answer {
                prompt: 0,
                choice: None,
            })),
            other => {
                // `prompt:9`: the stand-in with that many long choices.
                if let Some(count) = other.strip_prefix("prompt:").and_then(|n| n.parse().ok()) {
                    return Ok(Step::Act(Action::Answer {
                        prompt: 0,
                        choice: Some(count),
                    }));
                }
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
    /// Frames the "Quitting" overlay is held before the window closes.
    /// Long enough to paint and to finish a screenshot readback.
    const QUIT_FRAMES: u32 = 3;

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
            gamepad,
        } = options;
        ui::install_fonts(ctx);
        ctx.set_visuals(ui::visuals());
        ctx.set_zoom_factor(zoom);
        Self {
            backend,
            covers,
            gamepad: gamepad.unwrap_or_else(|| Gamepad::new(ctx.clone())),
            glyphs: Glyphs::load(ctx),
            input_mode: InputMode::Keyboard,
            input_seen: false,
            status: String::new(),
            notice: None,
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
            install_failures: Default::default(),
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
            toolbar_focus: [None; Tab::ALL.len()],
            downloads_row: (0, 0),
            playable_only: false,
            query: String::new(),
            focus_search: false,
            blur_search: false,
            page: Page::Library,
            menu: None,
            quitting: None,
            actions: Vec::new(),
            rows: ui::Rows::default(),
            shot,
            emulate,
            low_spec,
            handheld: false,
            checking_updates: false,
            up_to_date_at: None,
            refreshing: false,
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
            key(Modifiers::NONE, Key::Q, Action::CycleTab(-1));
            key(Modifiers::NONE, Key::E, Action::CycleTab(1));
        });
    }

    /// Actions queued while drawing land after the frame was drawn, so it
    /// takes another frame to show them.
    fn apply_actions(&mut self, ctx: &egui::Context) {
        let mut applied = false;
        while !self.actions.is_empty() {
            for action in std::mem::take(&mut self.actions) {
                self.apply(action);
                applied = true;
            }
        }
        if applied {
            ctx.request_repaint();
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

    fn active_rows_ref(&self) -> Option<&ui::Rows> {
        match self.tab {
            Tab::Library => Some(&self.rows),
            Tab::Collections => Some(&self.collection_rows),
            Tab::Downloads => None,
        }
    }

    /// Whether the tab's list has nothing to focus, leaving the toolbar. A
    /// section can be listed with no games, a note in their place; a row
    /// with more to fetch counts as having some.
    fn rows_empty(&self) -> bool {
        let no_tiles =
            |rows: &ui::Rows| rows.sections.iter().all(|s| s.games.is_empty() && !s.more);
        match self.tab {
            Tab::Library => no_tiles(&self.rows),
            Tab::Collections => no_tiles(&self.collection_rows),
            Tab::Downloads => self.download_rows().is_empty(),
        }
    }

    /// The controls above the tab's list, as drawn, and one stop per
    /// button or filter option, in the same order.
    fn toolbar(&self) -> (Vec<ui::ToolbarControl>, Vec<ToolbarStop>) {
        let playable = (
            ui::ToolbarControl::Filters(vec![("Playable here", self.playable_only)]),
            vec![ToolbarStop::new(
                "Playable here",
                Action::SetPlayableOnly(!self.playable_only),
            )],
        );
        let mut controls = Vec::new();
        let mut stops = Vec::new();
        let mut push = |control, mut more: Vec<ToolbarStop>| {
            controls.push(control);
            stops.append(&mut more);
        };
        match self.tab {
            Tab::Library => push(playable.0, playable.1),
            Tab::Collections => {
                let installed = self.collections_installed_only;
                push(
                    ui::ToolbarControl::Filters(vec![
                        ("All", !installed),
                        ("Installed", installed),
                    ]),
                    vec![
                        ToolbarStop::new("All", Action::SetCollectionsInstalledOnly(false)),
                        ToolbarStop::new("Installed", Action::SetCollectionsInstalledOnly(true)),
                    ],
                );
                push(playable.0, playable.1);
            }
            Tab::Downloads => {
                let (label, busy) = if self.checking_updates {
                    ("Checking…", true)
                } else if self
                    .up_to_date_at
                    .is_some_and(|at| at.elapsed() < Self::UP_TO_DATE_FOR)
                {
                    ("Up to date", false)
                } else {
                    ("Check for updates", false)
                };
                push(
                    ui::ToolbarControl::Button { label, busy },
                    vec![ToolbarStop {
                        label,
                        busy,
                        action: Action::CheckUpdates,
                    }],
                );
                let rows = self.download_rows();
                if rows
                    .iter()
                    .filter(|r| r.section == ui::DownloadSection::Updates)
                    .any(|r| r.direct_update)
                {
                    push(
                        ui::ToolbarControl::Button {
                            label: "Update all",
                            busy: false,
                        },
                        vec![ToolbarStop::new("Update all", Action::UpdateAll)],
                    );
                }
                if rows
                    .iter()
                    .any(|r| r.section == ui::DownloadSection::Finished)
                {
                    push(
                        ui::ToolbarControl::Button {
                            label: "Clear all",
                            busy: false,
                        },
                        vec![ToolbarStop::new("Clear all", Action::ClearFinished)],
                    );
                }
            }
        }
        (controls, stops)
    }

    /// The toolbar stop with focus on the current tab, clamped to the
    /// `stops` there are. With nothing in the list the toolbar is all there
    /// is, so focus rests on it.
    fn toolbar_focus_in(&self, stops: usize, rows_empty: bool) -> Option<usize> {
        match self.toolbar_focus[tab_slot(self.tab)] {
            Some(index) => Some(index.min(stops.saturating_sub(1))),
            None if rows_empty && stops > 0 => Some(0),
            None => None,
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
        if self.quitting.is_some() {
            return;
        }
        if let (None, Action::Answer { prompt: 0, choice }) = (&self.prompt, &action) {
            // The screenshot script's stand-in: a license, or with a count,
            // a pick between that many downloads.
            self.prompt = Some(match choice {
                Some(count) => Prompt {
                    id: 0,
                    title: "Which download?".into(),
                    body: "Sample has more than one download for this device.".into(),
                    choices: (1..=*count)
                        .map(|i| format!("Sample - Linux - build {i} (135.9 MB)"))
                        .collect(),
                    focus: 0,
                    primary: None,
                    stacked: true,
                },
                None => Prompt {
                    id: 0,
                    title: "License agreement".into(),
                    body: "This is a sample license shown by the screenshot script. ".repeat(12),
                    choices: vec!["Accept".into(), "Decline".into()],
                    focus: 0,
                    primary: Some(0),
                    stacked: false,
                },
            });
            return;
        }
        if let Some(prompt) = self.prompt.as_mut() {
            match action {
                Action::MoveFocus(direction) => {
                    let last = prompt.choices.len().saturating_sub(1);
                    match (prompt.stacked, direction) {
                        (true, Direction::Up) | (false, Direction::Left) => {
                            prompt.focus = prompt.focus.saturating_sub(1)
                        }
                        (true, Direction::Down) | (false, Direction::Right) => {
                            prompt.focus = (prompt.focus + 1).min(last)
                        }
                        (_, Direction::Home) => prompt.focus = 0,
                        (_, Direction::End) => prompt.focus = last,
                        _ => {}
                    }
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
                Action::Menu => self.raise_window(),
                _ => {}
            }
            return;
        }
        if let Some(focus) = self.menu {
            let items = self.menu_items();
            match action {
                Action::MoveFocus(Direction::Up) => self.menu = Some(focus.saturating_sub(1)),
                Action::MoveFocus(Direction::Down) => {
                    self.menu = Some((focus + 1).min(items.len().saturating_sub(1)))
                }
                Action::MenuFocus(index) if index < items.len() => self.menu = Some(index),
                Action::Activate => {
                    if let Some(action) = items.into_iter().nth(focus).map(|item| item.action) {
                        // Quit keeps the drawer in place under the overlay;
                        // a refresh keeps it to show its progress.
                        if !matches!(action, Action::Quit | Action::RefreshLibrary) {
                            self.menu = None;
                        }
                        self.actions.push(action);
                    }
                }
                Action::Back | Action::Menu => self.menu = None,
                Action::Quit => self.quitting = Some(Self::QUIT_FRAMES),
                Action::RefreshLibrary => self.refresh_library(),
                _ => {}
            }
            return;
        }
        match action {
            Action::MoveFocus(direction) => match self.page.clone() {
                Page::Library => {
                    let stops = self.toolbar().1.len();
                    let rows_empty = self.rows_empty();
                    let at_first_row = match self.tab {
                        Tab::Library => self.rows.row == 0,
                        Tab::Collections => self.collection_rows.row == 0,
                        Tab::Downloads => self.downloads_row_in(&self.download_rows()).0 == 0,
                    };
                    let landing = step_toolbar_focus(
                        self.toolbar_focus_in(stops, rows_empty),
                        direction,
                        stops,
                        rows_empty,
                        at_first_row,
                    );
                    let slot = tab_slot(self.tab);
                    match landing {
                        Landing::Toolbar(index) => self.toolbar_focus[slot] = Some(index),
                        Landing::FirstRow => {
                            self.toolbar_focus[slot] = None;
                            match self.active_rows() {
                                Some(rows) => {
                                    rows.row = 0;
                                    rows.follow = true;
                                }
                                None => self.downloads_row = (0, 0),
                            }
                        }
                        Landing::Rows => {
                            self.toolbar_focus[slot] = None;
                            match self.active_rows() {
                                Some(rows) => rows.move_focus(direction),
                                None => {
                                    let rows = self.download_rows();
                                    self.downloads_row = step_download_row(
                                        self.downloads_row_in(&rows),
                                        direction,
                                        &rows,
                                    );
                                }
                            }
                        }
                    }
                }
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
                self.toolbar_focus[tab_slot(self.tab)] = None;
                if let Some(rows) = self.active_rows() {
                    rows.focus_tile(row, col);
                }
            }
            Action::FocusDownload { row, button } => {
                self.toolbar_focus[tab_slot(self.tab)] = None;
                self.downloads_row = (row, button);
            }
            Action::FocusToolbar(index) => {
                self.toolbar_focus[tab_slot(self.tab)] = Some(index);
            }
            Action::FocusButton(button) => {
                if let Page::Game { id, .. } = self.page {
                    self.page = Page::Game { id, button };
                }
            }
            Action::Activate => match self.page.clone() {
                Page::Library => {
                    let (_, stops) = self.toolbar();
                    if let Some(index) = self.toolbar_focus_in(stops.len(), self.rows_empty()) {
                        if let Some(stop) = stops.get(index)
                            && !stop.busy
                        {
                            self.actions.push(stop.action.clone());
                        }
                        return;
                    }
                    match self.tab {
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
                            let (row, button) = self.downloads_row_in(&rows);
                            if let Some((_, action)) =
                                rows.get(row).and_then(|r| r.buttons.get(button))
                            {
                                self.actions.push(action.clone());
                            }
                        }
                    }
                }
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
            Action::ClearFinished => self.backend.send(Command::ClearFinished),
            Action::RefreshLibrary => self.refresh_library(),
            Action::UpdateAll => {
                for cave_id in self.pending_updates(true) {
                    self.apply(Action::Update { cave_id });
                }
            }
            Action::CheckUpdates => {
                if self.online && !self.checking_updates {
                    self.checking_updates = true;
                    self.up_to_date_at = None;
                    self.backend.send(Command::CheckUpdates);
                }
            }
            Action::SetCollectionsInstalledOnly(on) => {
                if self.collections_installed_only != on {
                    self.collections_installed_only = on;
                    self.request_collection_installed();
                    self.rebuild_collection_sections();
                    self.collection_rows.follow = true;
                }
            }
            Action::Menu => {
                self.raise_window();
                self.menu = Some(0);
            }
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
                if self.page.is_library() && self.tab == Tab::Library && !self.handheld {
                    self.focus_search = true;
                }
            }
            Action::SearchDone => {
                self.blur_search = true;
                // Search hands control to its results, not back to the
                // toolbar button that had focus before typing.
                self.toolbar_focus[tab_slot(Tab::Library)] = None;
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
                Page::Library if self.notice.is_some() => self.notice = None,
                Page::Library => self.actions.push(Action::Menu),
                Page::Game { id, .. } => {
                    if let Some(rows) = self.active_rows() {
                        rows.focus_game(id);
                    }
                    self.page = Page::Library;
                }
            },
            Action::MenuFocus(_) => {}
            Action::Quit => self.quitting = Some(Self::QUIT_FRAMES),
            Action::Open(page) => {
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
                self.install_failures.remove(&game_id);
                self.backend.send(Command::Install {
                    game: Box::new(game),
                });
                self.rebuild_installs();
            }
            Action::CancelInstall { game_id } => {
                let Some(download) = self.download_for(game_id) else {
                    return;
                };
                let download_id = download.id.clone();
                // A live download is worth a question; dismissing a failed
                // one is not.
                let confirm = download.finished_at.is_none().then(|| {
                    download
                        .game
                        .as_ref()
                        .map_or_else(|| "this game".to_string(), |g| g.title.clone())
                });
                if confirm.is_none() {
                    self.discarding.insert(download_id.clone());
                    self.rebuild_installs();
                }
                self.backend.send(Command::Discard {
                    download_id,
                    confirm,
                });
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
    /// Games with a direct update: the installed upload has a newer
    /// version. Indirect updates are guesses, mentioned only on the game
    /// page.
    fn updatable(&self) -> std::collections::HashSet<i64> {
        self.caves
            .iter()
            .filter(|cave| self.updates.get(&cave.id).is_some_and(|u| u.direct))
            .filter_map(CaveExt::game_id)
            .collect()
    }

    /// Caves with an update waiting that nothing is fetching yet, in
    /// library order; `direct_only` leaves out butler's guesses.
    fn pending_updates(&self, direct_only: bool) -> Vec<String> {
        self.caves
            .iter()
            .filter(|cave| {
                self.updates
                    .get(&cave.id)
                    .is_some_and(|u| u.direct || !direct_only)
            })
            .filter(|cave| {
                !self
                    .downloads
                    .iter()
                    .any(|d| d.cave_id == cave.id && d.finished_at.is_none())
            })
            .map(|cave| cave.id.clone())
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
    /// and everything owned.
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
            .then(|| "Nothing here runs on this device".to_string());
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
                    (true, false) => Some("Nothing here runs on this device".to_string()),
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

        let loaded = !matches!(self.owned, Loadable::Loading);
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
                Step::Notice => self.notify(
                    "Couldn't check for updates: a sample failure from the screenshot script"
                        .into(),
                ),
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
                    self.refreshing = false;
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
                    self.notify(format!("Couldn't load more: {error}"));
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
                }
                Event::Updates(updates) => {
                    if self.checking_updates && updates.is_empty() {
                        self.up_to_date_at = Some(Instant::now());
                    }
                    self.checking_updates = false;
                    self.updates = updates
                        .into_iter()
                        .map(|u| (u.cave_id.clone(), u))
                        .collect();
                }
                Event::DownloadErrored(download) => {
                    let title = download.game.as_ref().map_or("game", |g| g.title.as_str());
                    let error = download
                        .error_message
                        .as_deref()
                        .or(download.error.as_deref())
                        .unwrap_or("unknown error");
                    if let Some(game_id) = download.game.as_ref().map(|g| g.id) {
                        self.install_failed(game_id, error.to_string());
                    } else {
                        self.notify(format!("Install of {title} failed: {error}"));
                    }
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
                            self.notify(format!("Couldn't launch: {}", failure.message));
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
                Event::Online(online) => {
                    self.online = online;
                    if !online {
                        self.refreshing = false;
                    }
                }
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
                        self.notify(format!("Uninstall failed: {error}"));
                    }
                }
                Event::SyncFailed(error) => {
                    self.refreshing = false;
                    log::warn!("sync: {error}");
                }
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
                    self.install_failed(game_id, error);
                }
                Event::Discarding { download_id } => {
                    self.discarding.insert(download_id);
                    self.rebuild_installs();
                }
                Event::DiscardFailed { download_id, error } => {
                    self.discarding.remove(&download_id);
                    self.rebuild_installs();
                    self.notify(format!("Couldn't cancel: {error}"));
                }
                Event::Error(message) => {
                    if message.starts_with("Couldn't check for updates") {
                        self.checking_updates = false;
                    }
                    if self.owned.get().is_none() {
                        self.owned = Loadable::Failed(message);
                    } else {
                        self.notify(message);
                    }
                }
            }
        }
    }
}

impl App {
    /// What the footer offers on the current page, in reading order.
    /// The stored focus, clamped to rows that still exist. The queue changes
    /// underneath the focus, so every reader clamps rather than trusting it.
    /// The remembered Downloads focus, clamped to the rows there are: they
    /// come and go as butler works.
    fn downloads_row_in(&self, rows: &[ui::DownloadRow<'_>]) -> (usize, usize) {
        let (row, button) = self.downloads_row;
        let row = row.min(rows.len().saturating_sub(1));
        let buttons = rows.get(row).map_or(0, |r| r.buttons.len());
        (row, button.min(buttons.saturating_sub(1)))
    }

    /// What the Downloads tab lists, split the way the itch app splits it:
    /// butler's pending queue in its order, then everything with a
    /// `finished_at`, done or failed, newest first. Finished entries stay
    /// in butler's queue until the user clears them.
    fn download_rows(&self) -> Vec<ui::DownloadRow<'_>> {
        let mut pending: Vec<&Download> = Vec::new();
        let mut finished: Vec<&Download> = Vec::new();
        for download in &self.downloads {
            if download.finished_at.is_some() {
                finished.push(download);
            } else {
                pending.push(download);
            }
        }
        pending.sort_by_key(|d| d.position);
        finished.sort_by(|a, b| b.finished_at.cmp(&a.finished_at));

        let mut rows = Vec::new();
        if self.online {
            for cave_id in self.pending_updates(false) {
                let Some(update) = self.updates.get(&cave_id) else {
                    continue;
                };
                let game = self
                    .caves
                    .iter()
                    .find(|cave| cave.id == cave_id)
                    .and_then(|cave| cave.game.as_ref())
                    .or(update.game.as_ref());
                let title = game.map_or_else(|| "Game".to_string(), |g| g.title.clone());
                let name = update
                    .choices
                    .first()
                    .and_then(|c| c.upload.as_ref())
                    .map_or("newer version", UploadExt::name);
                // Same wording as the game page: a direct update is the
                // installed upload, newer; an indirect one is a guess.
                let (detail, label) = if update.direct {
                    (format!("Update available: {name}"), "Update")
                } else if update.choices.len() > 1 {
                    (
                        format!("{} newer uploads, maybe replacements", update.choices.len()),
                        "Newer uploads",
                    )
                } else {
                    (
                        format!("Newer upload, maybe a replacement: {name}"),
                        "Newer upload",
                    )
                };
                let mut buttons = vec![(label, Action::Update { cave_id })];
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
                    title,
                    detail,
                    progress: None,
                    failed: false,
                    section: ui::DownloadSection::Updates,
                    direct_update: update.direct,
                    buttons,
                });
            }
        }
        for download in pending {
            let game = download.game.as_ref();
            let game_id = game.map(|g| g.id);
            let title = game.map_or_else(|| "Download".to_string(), |g| g.title.clone());
            let updating = self.caves.iter().any(|cave| cave.id == download.cave_id);
            let prefix = if updating { "Update: " } else { "" };
            let mut buttons = Vec::new();
            if let Some(game_id) = game_id {
                let label = if self.discarding.contains(&download.id) {
                    "Cancelling"
                } else {
                    "Cancel"
                };
                buttons.push((label, Action::CancelInstall { game_id }));
            }
            let (detail, progress) = match self.progress.get(&download.id) {
                Some(p) if p.bps > 0.0 => (
                    format!(
                        "{prefix}{}, {:.0}%, {}/s, {} left",
                        capitalize(&p.stage),
                        p.progress * 100.0,
                        ui::human_size(p.bps as i64),
                        ui::human_duration_seconds(p.eta as i64),
                    ),
                    Some(p.progress as f32),
                ),
                Some(p) if !p.stage.is_empty() => (
                    format!(
                        "{prefix}{}, {:.0}%",
                        capitalize(&p.stage),
                        p.progress * 100.0
                    ),
                    Some(p.progress as f32),
                ),
                _ if download.started_at.is_some() => (format!("{prefix}Starting"), Some(0.0)),
                _ => (format!("{prefix}Queued"), None),
            };
            rows.push(ui::DownloadRow {
                game,
                title,
                detail,
                progress,
                failed: false,
                section: ui::DownloadSection::Queue,
                direct_update: false,
                buttons,
            });
        }
        for download in finished {
            let game = download.game.as_ref();
            let game_id = game.map(|g| g.id);
            let title = game.map_or_else(|| "Download".to_string(), |g| g.title.clone());
            let error = download
                .error_message
                .as_deref()
                .or(download.error.as_deref());
            let mut buttons = Vec::new();
            let (detail, failed) = if let Some(error) = error {
                if let Some(game_id) = game_id {
                    buttons.push(("Retry", Action::RetryInstall { game_id }));
                    buttons.push(("Dismiss", Action::CancelInstall { game_id }));
                }
                (format!("Failed, {error}"), true)
            } else {
                if let Some(game) = game.filter(|g| self.catalog.contains_key(&g.id)) {
                    buttons.push((
                        "Open",
                        Action::Open(Page::Game {
                            id: game.id,
                            button: 0,
                        }),
                    ));
                }
                let outcome = match download.reason {
                    DownloadReason::Install => "Installed",
                    DownloadReason::Update => "Updated",
                    DownloadReason::Reinstall => "Reinstalled",
                    DownloadReason::VersionSwitch => "Switched version",
                    DownloadReason::Unknown => "Finished",
                };
                let when = download
                    .finished_at
                    .as_deref()
                    .and_then(ui::rfc3339_to_unix)
                    .map(ui::human_time_ago);
                (
                    match when {
                        Some(when) => format!("{outcome}, {when}"),
                        None => outcome.to_string(),
                    },
                    false,
                )
            };
            rows.push(ui::DownloadRow {
                game,
                title,
                detail,
                progress: None,
                failed,
                section: ui::DownloadSection::Finished,
                direct_update: false,
                buttons,
            });
        }
        rows
    }

    /// The game's page shows the failure until the next attempt; the
    /// notice is for when the user is elsewhere.
    fn install_failed(&mut self, game_id: i64, error: String) {
        let on_page = matches!(self.page, Page::Game { id, .. } if id == game_id);
        if !on_page {
            let title = self.game(game_id).map_or("game", |g| g.title.as_str());
            self.notify(format!("Couldn't install {title}: {error}"));
        }
        self.install_failures.insert(game_id, error);
    }

    /// Something failed that has no page to show it on.
    fn notify(&mut self, message: String) {
        log::warn!("{message}");
        self.notice = Some((message, Instant::now()));
    }

    fn refresh_library(&mut self) {
        if self.online && !self.refreshing {
            self.refreshing = true;
            self.backend.send(Command::RefreshLibrary);
        }
    }

    /// What the menu drawer offers, top to bottom.
    fn menu_items(&self) -> Vec<ui::MenuItem> {
        vec![
            ui::MenuItem {
                label: if self.refreshing {
                    "Refreshing…"
                } else {
                    "Refresh library"
                },
                action: Action::RefreshLibrary,
                busy: self.refreshing,
            },
            ui::MenuItem {
                label: "Quit",
                action: Action::Quit,
                busy: false,
            },
        ]
    }

    fn hints(&self) -> Vec<(Vec<Glyph>, String)> {
        if let Some(focus) = self.menu {
            let items = self.menu_items();
            let mut hints = Vec::new();
            if items.len() > 1 {
                hints.push((vec![Glyph::Navigate], "Choose".to_string()));
            }
            if let Some(item) = items.get(focus) {
                hints.push((vec![Glyph::Confirm], item.label.to_string()));
            }
            hints.push((vec![Glyph::Back], "Close".to_string()));
            return hints;
        }
        if let Some(prompt) = &self.prompt {
            let mut hints = Vec::new();
            if prompt.choices.len() > 1 {
                let glyph = if prompt.stacked {
                    Glyph::NavigateVertical
                } else {
                    Glyph::NavigateHorizontal
                };
                hints.push((vec![glyph], "Choose".to_string()));
            }
            if let Some(choice) = prompt.choices.get(prompt.focus) {
                hints.push((vec![Glyph::Confirm], choice.clone()));
            }
            hints.push((vec![Glyph::Back], "Dismiss".to_string()));
            return hints;
        }
        // The tab strip already shows the bumpers, so no hint repeats them.
        match self.page.clone() {
            Page::Library => {
                let (_, stops) = self.toolbar();
                let rows_empty = self.rows_empty();
                let on_toolbar = self
                    .toolbar_focus_in(stops.len(), rows_empty)
                    .and_then(|index| stops.get(index))
                    .map(|stop| stop.label);
                let mut hints = Vec::new();
                // Confirm names what the focused control does.
                let confirm = match self.tab {
                    _ if on_toolbar.is_some() => on_toolbar,
                    Tab::Library | Tab::Collections => self
                        .active_rows_ref()
                        .and_then(|rows| rows.focused_game())
                        .map(|_| "Open"),
                    Tab::Downloads => {
                        let rows = self.download_rows();
                        let (row, button) = self.downloads_row_in(&rows);
                        rows.get(row)
                            .and_then(|r| r.buttons.get(button))
                            .map(|(label, _)| *label)
                    }
                };
                if !rows_empty || stops.len() > 1 {
                    hints.push((vec![Glyph::Navigate], "Browse".to_string()));
                }
                if let Some(label) = confirm {
                    hints.push((vec![Glyph::Confirm], label.to_string()));
                }
                match self.tab {
                    Tab::Library => {
                        if !self.handheld {
                            hints.push((vec![Glyph::Search], "Search".to_string()));
                        }
                        hints.push((vec![Glyph::Menu], "Menu".to_string()));
                    }
                    Tab::Collections | Tab::Downloads => {
                        hints.push((vec![Glyph::Back], "Back".to_string()));
                    }
                }
                hints
            }
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

/// One frame, for whichever host drives the window: input and actions
/// first, then drawing into the whole viewport.
impl App {
    pub fn update_logic(&mut self, ctx: &egui::Context) {
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
        if let Some(at) = self.up_to_date_at {
            let left = Self::UP_TO_DATE_FOR.saturating_sub(at.elapsed());
            if left.is_zero() {
                self.up_to_date_at = None;
            } else {
                ctx.request_repaint_after(left);
            }
        }
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
        if pad.pressed {
            self.input_mode = InputMode::Gamepad;
        } else if keys {
            self.input_mode = InputMode::Keyboard;
        } else if touches {
            self.input_mode = InputMode::Touch;
        } else if pad.connected && !self.input_seen {
            // A handheld has a controller before it has a first press.
            self.input_mode = InputMode::Gamepad;
        }
        if pad.pressed || keys || touches {
            self.input_seen = true;
        }
        self.drive_shot(ctx);
        // Before drawing, so the frame that reads a press already shows it.
        self.apply_actions(ctx);
    }

    pub fn update_ui(&mut self, ui: &mut egui::Ui) {
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

    pub fn on_close(&mut self) {
        self.backend.shutdown();
    }
}

#[cfg(feature = "eframe-host")]
impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update_logic(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.update_ui(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.on_close();
    }
}

impl App {
    /// How long a notice stays up; Back dismisses it sooner.
    const NOTICE_FOR: Duration = Duration::from_secs(8);
    /// How long the update button reports a clean check.
    const UP_TO_DATE_FOR: Duration = Duration::from_secs(4);

    fn draw(&mut self, ui: &mut egui::Ui) {
        let screen = ui.max_rect();
        let m = ui::Metrics::for_screen(screen);
        let policy = crate::images::Policy::for_screen(screen.height(), self.low_spec);
        self.handheld = policy.low_spec;
        self.covers.set_policy(policy);
        if self.input_mode != InputMode::Touch && self.owned.get().is_some() {
            let hints = self.hints();
            ui::footer(
                ui,
                &m,
                &self.glyphs,
                self.input_mode,
                &hints,
                self.prompt.is_some() || self.menu.is_some(),
                self.menu.is_some(),
            );
        }
        let page = egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui::BG).inner_margin(egui::Margin {
                left: m.margin as i8,
                right: m.margin as i8,
                top: m.frame(18.0) as i8,
                bottom: m.frame(6.0) as i8,
            }))
            .show(ui, |ui| {
                // A row of the strip's height from the start, so the logo
                // and the right-hand text center on the tabs' line.
                let row = egui::vec2(ui.available_width(), ui::tab_strip_height(ui, &m));
                let centered = egui::Layout::left_to_right(egui::Align::Center);
                ui.allocate_ui_with_layout(row, centered, |ui| {
                    ui::logo(ui, &m, &self.glyphs);
                    if self.page.is_library() {
                        let downloading = self
                            .downloads
                            .iter()
                            .filter(|d| d.finished_at.is_none())
                            .count();
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
                            // Elide rather than wrap: on a 640-wide screen a
                            // long display name meets the tab strip.
                            ui::subtle_truncated(ui, &m, user.name());
                        }
                        if !self.online {
                            ui::offline(ui, &m);
                        }
                    });
                });
                // The Downloads toolbar scrolls with its list instead.
                let toolbar_shown = self.page.is_library()
                    && match self.tab {
                        Tab::Library => self.owned.get().is_some(),
                        Tab::Collections => self.collections.get().is_some(),
                        Tab::Downloads => false,
                    };
                if toolbar_shown {
                    ui.add_space(m.frame(8.0));
                    ui.horizontal(|ui| {
                        let (controls, stops) = self.toolbar();
                        let focused = self.toolbar_focus_in(stops.len(), self.rows_empty());
                        let response = ui::toolbar(ui, &m, &controls, focused);
                        if let Some(index) = response.hovered {
                            self.actions.push(Action::FocusToolbar(index));
                        }
                        if let Some(index) = response.clicked {
                            self.actions.push(stops[index].action.clone());
                        }
                        if self.tab != Tab::Library || self.handheld {
                            return;
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
                // Under a toolbar; or, on Downloads, above the list's own
                // ring space so its toolbar sits where the others do.
                ui.add_space(if self.page.is_library() && self.tab == Tab::Downloads {
                    (m.frame(8.0) - m.ring).max(0.0)
                } else {
                    m.frame(12.0)
                });
                match (&self.owned, self.page.clone()) {
                    (Loadable::NotLoaded | Loadable::Loading, _) => {
                        ui::loading(ui, &m, &self.status)
                    }
                    (Loadable::Failed(message), _) => ui::failed(ui, &m, message),
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
                                focused: self.toolbar_focus[tab_slot(Tab::Library)].is_none(),
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
                                            focused: self.toolbar_focus[tab_slot(Tab::Collections)]
                                                .is_none(),
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
                            let (row, button) = self.downloads_row_in(&rows);
                            let focus = match self
                                .toolbar_focus_in(self.toolbar().1.len(), rows.is_empty())
                            {
                                Some(index) => ui::DownloadFocus::Toolbar(index),
                                None => ui::DownloadFocus::Row { row, button },
                            };
                            let (controls, stops) = self.toolbar();
                            let mut actions = Vec::new();
                            let response = ui::downloads(
                                ui,
                                &m,
                                ui::DownloadsView {
                                    rows: &rows,
                                    covers: &self.covers,
                                    toolbar: &controls,
                                    focus,
                                    scrollbar: self.input_mode == InputMode::Keyboard,
                                },
                                &mut actions,
                            );
                            self.actions.extend(actions);
                            if let Some(index) = response.hovered {
                                self.actions.push(Action::FocusToolbar(index));
                            }
                            if let Some(index) = response.clicked {
                                self.actions.push(stops[index].action.clone());
                            }
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
                                let install_failure =
                                    self.install_failures.get(&game.id).map(String::as_str);
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
                                        install_failure,
                                    },
                                    &mut self.actions,
                                );
                            }
                            None => self.actions.push(Action::Back),
                        }
                    }
                }
            })
            .response
            .rect;
        if let Some((message, since)) = &self.notice {
            if since.elapsed() < Self::NOTICE_FOR {
                ui::notice(ui.ctx(), &m, page, message);
                ui.ctx()
                    .request_repaint_after(Self::NOTICE_FOR - since.elapsed());
            } else {
                self.notice = None;
            }
        }
        if let Some(prompt) = &self.prompt {
            ui::prompt(ui.ctx(), &m, ui.max_rect(), page, prompt, &mut self.actions);
        }
        let items = self.menu_items();
        ui::drawer(
            ui.ctx(),
            &m,
            ui.max_rect(),
            &items,
            self.menu,
            &mut self.actions,
        );
        if let Some(frames) = self.quitting {
            ui::quitting(ui.ctx(), &m, ui.max_rect());
            if frames == 0 {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                self.quitting = Some(frames - 1);
                ui.ctx().request_repaint();
            }
        }
        self.apply_actions(ui.ctx());
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

/// A button on a tab's toolbar and what pressing it does.
struct ToolbarStop {
    label: &'static str,
    /// Its work is under way; pressing it does nothing.
    busy: bool,
    action: Action,
}

impl ToolbarStop {
    fn new(label: &'static str, action: Action) -> Self {
        Self {
            label,
            busy: false,
            action,
        }
    }
}

fn tab_slot(tab: Tab) -> usize {
    Tab::ALL.iter().position(|&t| t == tab).unwrap_or(0)
}

/// Where a controller step lands on a tab with a toolbar above its list.
#[derive(Debug, PartialEq, Eq)]
enum Landing {
    Toolbar(usize),
    /// Down off the toolbar: the list's first row.
    FirstRow,
    /// A move within the list, for the list to apply.
    Rows,
}

/// One controller step: the toolbar's `stops` left to right on top, the
/// list below. `focus` is the toolbar stop with focus, already clamped, or
/// none while focus is in the list.
fn step_toolbar_focus(
    focus: Option<usize>,
    direction: Direction,
    stops: usize,
    rows_empty: bool,
    at_first_row: bool,
) -> Landing {
    let last = stops.saturating_sub(1);
    match (focus, direction) {
        (Some(index), Direction::Left) => Landing::Toolbar(index.saturating_sub(1)),
        (Some(index), Direction::Right) => Landing::Toolbar((index + 1).min(last)),
        (Some(_), Direction::Home) => Landing::Toolbar(0),
        (Some(_), Direction::End) => Landing::Toolbar(last),
        (Some(index), Direction::Up) => Landing::Toolbar(index),
        (Some(index), Direction::Down) if rows_empty => Landing::Toolbar(index),
        (Some(_), Direction::Down) => Landing::FirstRow,
        (None, Direction::Up) if at_first_row && stops > 0 => Landing::Toolbar(0),
        (None, _) => Landing::Rows,
    }
}

/// One step within the Downloads list. Up off the first row is the
/// toolbar's, handled before this.
fn step_download_row(
    (row, button): (usize, usize),
    direction: Direction,
    rows: &[ui::DownloadRow<'_>],
) -> (usize, usize) {
    let last = rows.len().saturating_sub(1);
    let row = match direction {
        Direction::Up => row.saturating_sub(1),
        Direction::Down => (row + 1).min(last),
        Direction::Home => 0,
        Direction::End => last,
        _ => row,
    };
    let buttons = rows.get(row).map_or(0, |r| r.buttons.len());
    let button = match direction {
        Direction::Left => button.saturating_sub(1),
        Direction::Right => button + 1,
        _ => button,
    }
    .min(buttons.saturating_sub(1));
    (row, button)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> ui::DownloadRow<'static> {
        ui::DownloadRow {
            game: None,
            title: String::new(),
            detail: String::new(),
            progress: None,
            failed: false,
            section: ui::DownloadSection::Queue,
            direct_update: false,
            buttons: vec![
                ("Retry", Action::CheckUpdates),
                ("Dismiss", Action::CheckUpdates),
            ],
        }
    }

    #[test]
    fn toolbar_sits_above_the_rows() {
        let step = |focus, direction| step_toolbar_focus(focus, direction, 3, false, false);
        assert_eq!(step(Some(0), Direction::Right), Landing::Toolbar(1));
        assert_eq!(step(Some(2), Direction::Right), Landing::Toolbar(2));
        assert_eq!(step(Some(1), Direction::Left), Landing::Toolbar(0));
        assert_eq!(step(Some(2), Direction::Home), Landing::Toolbar(0));
        assert_eq!(step(Some(0), Direction::End), Landing::Toolbar(2));
        assert_eq!(step(Some(1), Direction::Up), Landing::Toolbar(1));
        assert_eq!(step(Some(1), Direction::Down), Landing::FirstRow);
        assert_eq!(step(None, Direction::Up), Landing::Rows);
        assert_eq!(step(None, Direction::Down), Landing::Rows);
        assert_eq!(step(None, Direction::Home), Landing::Rows);
    }

    #[test]
    fn up_off_the_first_row_reaches_the_toolbar() {
        assert_eq!(
            step_toolbar_focus(None, Direction::Up, 1, false, true),
            Landing::Toolbar(0)
        );
        // No toolbar: the list keeps the move.
        assert_eq!(
            step_toolbar_focus(None, Direction::Up, 0, false, true),
            Landing::Rows
        );
    }

    #[test]
    fn toolbar_is_all_there_is_without_rows() {
        let step = |focus, direction| step_toolbar_focus(focus, direction, 2, true, false);
        assert_eq!(step(Some(0), Direction::Down), Landing::Toolbar(0));
        assert_eq!(step(Some(0), Direction::Right), Landing::Toolbar(1));
    }

    #[test]
    fn download_rows_step_both_ways() {
        let rows = [row(), row()];
        let step = |focus, direction| step_download_row(focus, direction, &rows);
        assert_eq!(step((0, 0), Direction::Right), (0, 1));
        assert_eq!(step((0, 1), Direction::Right), (0, 1));
        assert_eq!(step((0, 1), Direction::Down), (1, 1));
        assert_eq!(step((1, 1), Direction::Down), (1, 1));
        assert_eq!(step((1, 0), Direction::Up), (0, 0));
        assert_eq!(step((1, 0), Direction::Home), (0, 0));
    }
}

//! Application state and the window that draws it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::backend::{Backend, Command, Event};
use crate::battery::Battery;
use crate::gamepad::Gamepad;
use crate::glyphs::{Glyph, Glyphs, InputMode};
use crate::images::CoverLoader;
use crate::model::{
    Action, Cave, CaveExt, CollectionFilter, CollectionGames, Direction, Download,
    DownloadProgress, DownloadReason, Game, GameUpdate, InstallState, Kind, LaunchFailure,
    Launched, Loadable, Mark, Page, Profile, Prompt, PromptOrigin, RatingFilter, Tab, UploadExt,
    UserExt, human_duration_seconds, human_size, human_time_ago, known_playable_here,
    rfc3339_to_unix, wrap_step,
};
use crate::page_info::{Lookup, PageInfoLoader};
use crate::qr::QrCode;
use crate::report::{Rating, Report, Reports, Run, Runs, SavedReport, SavedRun};
use crate::sample;
use crate::self_update::{self, SelfUpdate};
use crate::settings::Settings;
use crate::ui;
use crate::ui::LoginView;

/// How the window presents itself, from the command line.
pub struct Options {
    /// Extra magnification on top of the screen-derived layout.
    pub zoom: f32,
    /// Lay out for a display of this many points and letterbox it.
    pub emulate: Option<(f32, f32)>,
    pub low_spec: Option<bool>,
    pub minimize_while_playing: bool,
    /// Off for a package another tool updates.
    pub self_update: bool,
    /// Nothing of zitch is reachable from Play until the game exits, for
    /// a device that runs one program at a time.
    pub handoff: bool,
    /// A controller the host reads itself; otherwise gilrs is used.
    pub gamepad: Option<Gamepad>,
    /// Where remembered choices are kept.
    pub settings_path: PathBuf,
}

pub struct App {
    backend: Backend,
    covers: CoverLoader,
    /// Screenshots, tags and the like for game pages, while online.
    page_info: PageInfoLoader,
    gamepad: Gamepad,
    glyphs: Glyphs,
    /// The device the user touched last, which picks the footer's glyphs.
    input_mode: InputMode,
    /// Something has been pressed or touched this session. Until then a
    /// controller connecting picks the glyphs; after, only input does.
    input_seen: bool,
    /// The focused item while the menu drawer is open.
    menu: Option<usize>,
    /// Frames of black left to show, once the screen has powered off,
    /// before the window is asked to close. The backend join that follows
    /// the close blocks the last frame on screen, so it must be a black
    /// one; a few frames also let a pending screenshot read back first.
    quitting: Option<u32>,
    /// The backend's latest progress line, shown while the library loads.
    status: String,
    butler_version: Option<String>,
    /// A failure with no page of its own, shown briefly above the footer.
    notice: Option<(String, Instant)>,
    profile: Option<Profile>,
    /// Games the profile has a key for, in butler's order (newest first).
    owned: Loadable<Vec<Arc<Game>>>,
    caves: Vec<Cave>,
    collections: Loadable<Vec<CollectionGames>>,
    /// Show only installed games on the Collections tab.
    collections_installed_only: bool,
    /// butler's answers for the page-wide filters, while one is on.
    collection_filtered: Option<Filtered>,
    /// Numbers each filtered query, so an older answer never replaces a
    /// newer one.
    collection_asks: u64,
    /// The collections on screen, as last sent to the backend.
    collections_wanted: Vec<i64>,
    /// Collections with a page request in flight.
    collection_loading: std::collections::HashSet<i64>,
    /// The Collections tab's carousels, one per collection.
    collection_rows: ui::Rows,
    /// Every game the screen can show, owned or installed, by id. Added to
    /// as lists arrive rather than rebuilt, since a big account has
    /// thousands; cleared at sign-out.
    catalog: std::collections::HashMap<i64, Arc<Game>>,
    /// Which list each catalog entry came from, so a better one wins.
    catalog_sources: std::collections::HashMap<i64, Source>,
    installed: std::collections::HashSet<i64>,
    /// butler's download queue and the latest progress per download.
    downloads: Vec<Download>,
    progress: std::collections::HashMap<String, DownloadProgress>,
    /// Games the user asked to install that the queue has not listed yet,
    /// in the order asked.
    pending_installs: std::collections::HashMap<i64, Pending>,
    /// Downloads the user asked to discard that the queue still lists.
    discarding: std::collections::HashSet<String>,
    /// What the interface shows per game, rebuilt from the fields above.
    pub installs: std::collections::HashMap<i64, InstallState>,
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
    /// Per tab, the toolbar stop with controller focus, or none while
    /// focus is in the list below.
    toolbar_focus: [Option<usize>; Tab::ALL.len()],
    /// The row and button with focus on the Downloads list.
    downloads_row: (usize, usize),
    /// Hide games with no upload for this device, on every tab.
    playable_only: bool,
    /// Types "Playable here" leaves out.
    playable_hidden: Vec<String>,
    /// Where Back goes from the Playable types page: the page it was
    /// opened over.
    /// Where Back goes from Settings and Playable types: the pages they
    /// were opened over, innermost last.
    page_stack: Vec<Page>,
    /// Rating and tried marks on the covers.
    cover_marks: bool,
    /// The toolbar's filter on the player's own ratings. Not saved: it is
    /// for browsing, and starts at Any.
    rating_filter: RatingFilter,
    query: String,
    /// Move keyboard focus into the search box on the next frame.
    focus_search: bool,
    /// Take keyboard focus out of the search box on the next frame.
    blur_search: bool,
    page: Page,
    /// The sign-in code, while there is nothing to sign in with.
    login: Option<LoginView>,
    /// The open game's page as a QR code, over the page.
    qr: Option<ui::QrView>,
    /// A screenshot of the open game, full screen over the page.
    viewer: Option<ui::ScreenshotView>,
    /// Where the game page scrolls to on its next frame, after focus moved.
    detail_scroll: Option<ui::DetailScroll>,
    /// Game pages opened so far; each opening starts at the top.
    detail_visit: u64,
    /// How the open game's page is scrolled.
    page_scroll: ui::PageScroll,
    /// What the game options list does, by choice.
    prompt_options: Vec<Action>,
    /// How a game runs here, while the player fills it in.
    report: Option<ui::ReportView>,
    /// The last run of each cave, for a report filed later.
    runs: Runs,
    /// Where `runs` is kept; none for a screenshot run.
    runs_path: Option<PathBuf>,
    /// The reports sent from this device, by game.
    reports: Reports,
    /// The cover marks by game, from the caves, runs and reports.
    marks: std::collections::HashMap<i64, Mark>,
    reports_path: Option<PathBuf>,
    /// What the frame's input and widgets asked for, applied in order.
    pub actions: Vec<Action>,
    pub rows: ui::Rows,
    shot: Option<Shot>,
    intro: crate::intro::Intro,
    power_off: crate::intro::PowerOff,
    /// Where choices are saved; none for a screenshot run, which reads
    /// them but leaves them as they were.
    settings_path: Option<PathBuf>,
    /// Pretend the display is this many points, whatever the window size.
    emulate: Option<(f32, f32)>,
    /// Force the cover policy instead of picking it by screen size.
    low_spec: Option<bool>,
    /// Drawn for a handheld, which has no keyboard: the search box stays
    /// hidden until there is an on-screen one to type into.
    handheld: bool,
    /// An update check the user asked for is still running.
    checking_updates: bool,
    self_update: Option<SelfUpdate>,
    battery: Battery,
    /// When a check the user asked for last came back with nothing; the
    /// button says so for a moment.
    up_to_date_at: Option<Instant>,
    /// A library refresh the user asked for is still running.
    refreshing: bool,
    /// A background sync is running; the header shows a spinner.
    syncing: bool,
    /// Row rebuilds held back while a batch of events is handled.
    deferred: Option<Rebuilds>,
    minimize_while_playing: bool,
    handoff: bool,
    /// For window commands raised from events, outside a frame.
    ctx: egui::Context,
}

/// A debugging capture: write the window to a PNG once the library has
/// settled, or after a deadline, then quit.
pub struct Shot {
    path: PathBuf,
    deadline: Instant,
    /// When the library finished loading; covers get a moment after that.
    settled_at: Option<Instant>,
    /// Scripted steps to play once the library is loaded, one per frame.
    script: std::collections::VecDeque<Step>,
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
    /// Ask how a sample game ran, to look at the question.
    Report,
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
            "pageup" => Ok(Step::Act(Action::MoveFocus(Direction::PageUp))),
            "pagedown" => Ok(Step::Act(Action::MoveFocus(Direction::PageDown))),
            "top" => Ok(Step::Act(Action::MoveFocus(Direction::Top))),
            "bottom" => Ok(Step::Act(Action::MoveFocus(Direction::Bottom))),
            "enter" => Ok(Step::Act(Action::Activate)),
            "back" => Ok(Step::Act(Action::Back)),
            "capture" => Ok(Step::Capture),
            "notice" => Ok(Step::Notice),
            "report" => Ok(Step::Report),
            "nexttab" => Ok(Step::Act(Action::CycleTab(1))),
            "prevtab" => Ok(Step::Act(Action::CycleTab(-1))),
            "qr" => Ok(Step::Act(Action::ShowQr)),
            "y" => Ok(Step::Act(Action::Secondary)),
            "types" => Ok(Step::Act(Action::Open(Page::PlayableTypes { row: 0 }))),
            "settings" => Ok(Step::Act(Action::Open(Page::Settings { row: 0 }))),
            "guide" => Ok(Step::Act(Action::Menu)),
            // A stand-in question, to look at the modal without a game that
            // asks one.
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
    /// Frames the powered-off screen is held before the window closes.
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
            self_update,
            handoff,
            gamepad,
            settings_path,
        } = options;
        let settings = Settings::load(&settings_path);
        let runs_path = settings_path.with_file_name("runs.json");
        let reports_path = settings_path.with_file_name("reports.json");
        let reports: Reports = crate::report::load(&reports_path);
        // Hiding every type this device has counts as hiding none, as the
        // filter treats it, so the page shows them all on.
        let mut playable_hidden = settings.playable_hidden;
        if crate::model::device_platforms()
            .iter()
            .all(|p| playable_hidden.contains(p))
        {
            playable_hidden.clear();
        }
        ui::install_fonts(ctx);
        ctx.set_visuals(ui::visuals());
        ctx.set_zoom_factor(zoom);
        let mut app = Self {
            backend,
            covers,
            page_info: PageInfoLoader::new(),
            gamepad: gamepad.unwrap_or_else(|| Gamepad::new(ctx.clone())),
            glyphs: Glyphs::load(ctx),
            input_mode: InputMode::Keyboard,
            input_seen: false,
            status: String::new(),
            butler_version: None,
            notice: None,
            profile: None,
            owned: Loadable::Loading,
            caves: Vec::new(),
            catalog: Default::default(),
            catalog_sources: Default::default(),
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
            collections_installed_only: settings.collections_installed_only,
            collection_filtered: None,
            collection_asks: 0,
            collections_wanted: Vec::new(),
            collection_loading: Default::default(),
            collection_rows: ui::Rows::default(),
            toolbar_focus: [None; Tab::ALL.len()],
            downloads_row: (0, 0),
            playable_only: settings.playable_only,
            playable_hidden,
            page_stack: Vec::new(),
            cover_marks: settings.cover_marks,
            rating_filter: RatingFilter::Any,
            query: String::new(),
            focus_search: false,
            blur_search: false,
            page: Page::Library,
            login: None,
            qr: None,
            viewer: None,
            detail_scroll: None,
            detail_visit: 0,
            page_scroll: ui::PageScroll::default(),
            report: None,
            prompt_options: Vec::new(),
            runs: crate::report::load(&runs_path),
            runs_path: shot.is_none().then_some(runs_path),
            reports,
            marks: Default::default(),
            reports_path: shot.is_none().then_some(reports_path),
            menu: None,
            quitting: None,
            actions: Vec::new(),
            rows: ui::Rows::default(),
            settings_path: shot.is_none().then_some(settings_path),
            intro: crate::intro::Intro::new(shot.is_none()),
            power_off: crate::intro::PowerOff::new(shot.is_none()),
            shot,
            emulate,
            low_spec,
            handheld: false,
            checking_updates: false,
            self_update: (self_update && SelfUpdate::supported()).then(|| SelfUpdate::new(ctx)),
            battery: Battery::new(),
            up_to_date_at: None,
            refreshing: false,
            syncing: false,
            deferred: None,
            minimize_while_playing,
            handoff,
            ctx: ctx.clone(),
        };
        app.rebuild_marks();
        app
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
            key(
                Modifiers::NONE,
                Key::PageUp,
                Action::MoveFocus(Direction::PageUp),
            );
            key(
                Modifiers::NONE,
                Key::PageDown,
                Action::MoveFocus(Direction::PageDown),
            );
            key(Modifiers::NONE, Key::Enter, Action::Activate);
            key(Modifiers::NONE, Key::Escape, Action::Back);
            key(Modifiers::NONE, Key::Slash, Action::Search);
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

    fn cave(&self, cave_id: &str) -> Option<&Cave> {
        self.caves.iter().find(|c| c.id == cave_id)
    }

    fn open_menu(&mut self) {
        self.raise_window();
        self.menu = Some(0);
    }

    /// Both carousels follow their focused game on the next frame.
    fn follow_rows(&mut self) {
        self.rows.follow = true;
        self.collection_rows.follow = true;
    }

    /// Rebuilds the rows of both carousel tabs.
    fn rebuild_rows(&mut self) {
        self.rebuild_sections();
        self.rebuild_collection_sections();
    }

    /// What Confirm does on the Library page: the focused toolbar stop,
    /// the focused game, or the focused Downloads button. A busy stop has
    /// a name and nothing to do.
    fn confirm_target(&self) -> Option<(&'static str, Option<Action>)> {
        let (_, stops) = self.toolbar();
        if let Some(index) = self.toolbar_focus_in(stops.len(), self.rows_empty()) {
            let stop = stops.get(index)?;
            return Some((stop.hint, (!stop.busy).then(|| stop.action.clone())));
        }
        match self.tab {
            Tab::Library | Tab::Collections => {
                let id = self
                    .active_rows_ref()
                    .and_then(|rows| rows.focused_game())
                    .filter(|id| self.catalog.contains_key(id))?;
                Some(("Open", Some(Action::Open(Page::game(id)))))
            }
            Tab::Downloads => {
                let rows = self.download_rows();
                let (row, button) = self.downloads_row_in(&rows);
                let (label, action) = rows.get(row)?.buttons.get(button)?;
                Some((label, Some(action.clone())))
            }
        }
    }

    fn caves_for(&self, game_id: i64) -> Vec<&Cave> {
        self.caves
            .iter()
            .filter(|cave| cave.game_id() == Some(game_id))
            .collect()
    }

    fn game(&self, id: i64) -> Option<&Game> {
        self.catalog.get(&id).map(|g| &**g)
    }

    /// The game `step` places along from `id` in the library row its page
    /// was opened from; none from Downloads or past either end.
    fn game_beside(&self, id: i64, step: i32) -> Option<i64> {
        self.active_rows_ref()?
            .beside(id, step)
            .filter(|next| self.catalog.contains_key(next))
    }

    /// Screenshots the game's page shows, once its details are in.
    fn screenshot_count(&self, id: i64) -> usize {
        self.page_info
            .peek(id)
            .map_or(0, |info| info.screenshots.len())
    }

    /// The cave and title under the launch curtain.
    fn handed_off(&self) -> Option<(&str, &str)> {
        if !self.handoff {
            return None;
        }
        let cave_id = self.running.keys().next()?;
        let title = self
            .caves
            .iter()
            .find(|cave| &cave.id == cave_id)
            .and_then(|cave| cave.game.as_ref())
            .map_or("game", |game| game.title.as_str());
        Some((cave_id, title))
    }

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

    /// The carousel rows the current tab shows, if it has any.
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
        let rating = (
            ui::ToolbarControl::Filters(vec![(
                self.rating_filter.label(),
                self.rating_filter != RatingFilter::Any,
            )]),
            vec![ToolbarStop::new("Your rating", Action::RatingFilterMenu)],
        );
        let mut controls = Vec::new();
        let mut stops = Vec::new();
        let mut push = |control, mut more: Vec<ToolbarStop>| {
            controls.push(control);
            stops.append(&mut more);
        };
        match self.tab {
            Tab::Library => {
                push(playable.0, playable.1);
                push(rating.0, rating.1);
            }
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
                push(rating.0, rating.1);
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
                        hint: label,
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

    /// Puts `games` in the catalog, replacing what came from the same or a
    /// lesser list.
    fn catalog_add<'a>(&mut self, source: Source, games: impl IntoIterator<Item = &'a Arc<Game>>) {
        for game in games {
            if self
                .catalog_sources
                .get(&game.id)
                .is_some_and(|known| *known > source)
            {
                continue;
            }
            self.catalog.insert(game.id, Arc::clone(game));
            self.catalog_sources.insert(game.id, source);
        }
    }

    /// An action while a question is up: focus moves among its choices
    /// and Confirm or Back answer it.
    fn apply_in_prompt(&mut self, action: Action) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        match action {
            Action::MoveFocus(direction) => {
                let len = prompt.choices.len();
                let last = len.saturating_sub(1);
                match (prompt.stacked, direction) {
                    (true, Direction::Up | Direction::Down)
                    | (false, Direction::Left | Direction::Right) => {
                        prompt.focus = wrap_step(prompt.focus, len, direction)
                    }
                    // A prompt's list is short; a page reaches its end.
                    (_, Direction::Home | Direction::PageUp | Direction::Top) => prompt.focus = 0,
                    (_, Direction::End | Direction::PageDown | Direction::Bottom) => {
                        prompt.focus = last
                    }
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
            Action::Answer { prompt: id, choice } if id == prompt.id => match prompt.origin {
                PromptOrigin::SelfUpdate => self.answer_self_update(choice),
                PromptOrigin::Options => {
                    self.close_prompt();
                    let options = std::mem::take(&mut self.prompt_options);
                    if let Some(action) = choice.and_then(|c| options.into_iter().nth(c)) {
                        self.actions.push(action);
                    }
                }
                PromptOrigin::Sample => self.close_prompt(),
                PromptOrigin::Backend => {
                    self.close_prompt();
                    self.backend.send(Command::Answer { prompt: id, choice });
                }
                PromptOrigin::UploadPicker { game_id, picks } => {
                    self.close_prompt();
                    self.backend.send(Command::Answer { prompt: id, choice });
                    // Queued once an upload is picked; showing more or
                    // backing out leaves nothing to show until butler says.
                    let state = match choice {
                        Some(c) if c < picks => Pending::Picked,
                        _ => Pending::Starting,
                    };
                    if let Some(pending) = self.pending_installs.get_mut(&game_id) {
                        *pending = state;
                    }
                    self.rebuild_installs();
                }
            },
            Action::Menu => self.raise_window(),
            _ => {}
        }
    }

    /// An action while the compatibility report form is up.
    fn apply_in_report(&mut self, action: Action) {
        let Some(view) = self.report.as_mut() else {
            return;
        };
        match action {
            Action::MoveFocus(direction) => view.step(direction),
            Action::ReportFocus(index) if index < view.rows() => view.focus = index,
            Action::Activate => match view.focused() {
                Some(ui::ReportRow::Rating(_)) => view.pick(),
                Some(ui::ReportRow::Flag(flag)) => view.toggle(flag),
                Some(ui::ReportRow::Send) => self.send_report(),
                Some(ui::ReportRow::Cancel) | None => self.report = None,
            },
            // Start sends from either step, with the rating in focus.
            Action::Menu => self.send_report(),
            Action::Back if view.picked => view.unpick(),
            Action::Back => self.report = None,
            _ => {}
        }
    }

    /// An action while a screenshot fills the screen.
    fn apply_in_viewer(&mut self, action: Action) {
        let Some(view) = self.viewer.as_mut() else {
            return;
        };
        let last = view.urls.len().saturating_sub(1);
        match action {
            Action::MoveFocus(Direction::Left) => view.index = view.index.saturating_sub(1),
            Action::MoveFocus(Direction::Right) => view.index = (view.index + 1).min(last),
            Action::ViewScreenshot(index) if index <= last => view.index = index,
            Action::Activate | Action::Back | Action::CloseScreenshot => self.viewer = None,
            // As over the QR code: the menu comes up, here in its place.
            Action::Menu => {
                self.viewer = None;
                self.open_menu();
            }
            _ => {}
        }
        // The page's focus follows, so closing lands on the same one.
        if let Some(view) = &self.viewer
            && let Page::Game { shot, .. } = &mut self.page
            && shot.is_some()
        {
            *shot = Some(view.index);
        }
    }

    /// An action while the menu drawer is open. Quit and Refresh library
    /// are handled here too: Confirm on those rows keeps the drawer open
    /// and sends them again.
    fn apply_in_menu(&mut self, action: Action) {
        let Some(focus) = self.menu else {
            return;
        };
        let items = self.menu_items();
        match action {
            Action::MoveFocus(direction @ (Direction::Up | Direction::Down)) => {
                self.menu = Some(wrap_step(focus, items.len(), direction))
            }
            Action::MenuFocus(index) if index < items.len() => self.menu = Some(index),
            Action::Activate => {
                if let Some(action) = items.into_iter().nth(focus).map(|item| item.action) {
                    // Quit keeps the drawer in place under the overlay;
                    // a refresh keeps it to show its progress, a
                    // toggle its new state.
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
    }

    /// An action while the QR code is up: nearly anything closes it.
    fn apply_in_qr(&mut self, action: Action) {
        match action {
            Action::Back
            | Action::Activate
            | Action::Secondary
            | Action::Search
            | Action::HideQr => self.qr = None,
            Action::Menu => {
                self.open_menu();
            }
            _ => {}
        }
    }

    /// An action on the sign-in page.
    fn apply_in_login(&mut self, action: Action) {
        let Some(login) = &mut self.login else {
            return;
        };
        match action {
            // Down onto the checkbox, up off it; nothing else to reach.
            Action::MoveFocus(direction) if login.has_checkbox() => {
                login.focused = matches!(
                    direction,
                    Direction::Down
                        | Direction::Right
                        | Direction::End
                        | Direction::PageDown
                        | Direction::Bottom
                );
            }
            Action::Activate if login.focused && login.has_checkbox() => {
                login.share_device_info = !login.share_device_info;
                self.backend
                    .send(Command::SetShareDeviceInfo(login.share_device_info));
            }
            Action::Activate => self.backend.send(Command::RetryLogin),
            Action::SetShareDeviceInfo(on) => {
                if login.share_device_info != on {
                    login.share_device_info = on;
                    self.backend.send(Command::SetShareDeviceInfo(on));
                }
            }
            Action::Back | Action::Quit => self.quitting = Some(Self::QUIT_FRAMES),
            Action::Menu => {
                self.open_menu();
            }
            _ => {}
        }
    }

    /// Focus moving on the Library page: among the toolbar stops, down
    /// into the rows and within them.
    fn move_focus_library(&mut self, direction: Direction) {
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
                        rows.settle_on_game();
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
                        self.downloads_row =
                            step_download_row(self.downloads_row_in(&rows), direction, &rows);
                    }
                }
            }
        }
    }

    /// Focus moving on a game's page: along its buttons, down into the
    /// screenshots and along them, and the page scrolling to follow.
    fn move_focus_game(
        &mut self,
        id: i64,
        button: usize,
        shot: Option<usize>,
        direction: Direction,
    ) {
        let Some(game) = self.game(id) else {
            return;
        };
        let buttons = self.game_buttons(game).len().max(1);
        let shots = self.screenshot_count(id);
        let (button, shot) = match (shot.filter(|&i| i < shots), direction) {
            (None, Direction::Left) => (button.saturating_sub(1), None),
            (None, Direction::Right) => ((button + 1).min(buttons - 1), None),
            (None, Direction::Down) if shots > 0 => {
                self.detail_scroll = Some(ui::DetailScroll::Screenshots);
                (button, Some(0))
            }
            // Nothing more to focus below: the page scrolls to
            // show the rest.
            (_, Direction::Down) => {
                self.detail_scroll = Some(ui::DetailScroll::End);
                (button, shot)
            }
            (_, Direction::Up) => {
                self.detail_scroll = Some(ui::DetailScroll::Top);
                (button, None)
            }
            (Some(i), Direction::Left | Direction::Right) => {
                self.detail_scroll = Some(ui::DetailScroll::Shot);
                let i = if direction == Direction::Left {
                    i.saturating_sub(1)
                } else {
                    (i + 1).min(shots - 1)
                };
                (button, Some(i))
            }
            (shot, _) => (button, shot),
        };
        self.page = Page::Game { id, button, shot };
    }

    /// Stops a game's install: skips one still queueing or choosing its
    /// download, or discards its download, asking first while it is live.
    fn cancel_install(&mut self, game_id: i64) {
        let Some(download) = self.download_for(game_id) else {
            if self.pending_installs.remove(&game_id).is_some() {
                self.backend.send(Command::SkipInstall { game_id });
                // Close its picker so the queue behind it moves on.
                // The skip is sent first so the worker reads the
                // answer as a cancel, not a decline.
                if let Some(prompt) = self.upload_picker_for(game_id) {
                    let id = prompt.id;
                    let choice = Some(prompt.choices.len().saturating_sub(1));
                    self.close_prompt();
                    self.backend.send(Command::Answer { prompt: id, choice });
                }
                self.rebuild_installs();
            }
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

    fn apply(&mut self, action: Action) {
        if self.quitting.is_some() {
            return;
        }
        if let (None, Action::Answer { prompt: 0, choice }) = (&self.prompt, &action) {
            self.prompt = Some(sample::prompt(*choice));
            return;
        }
        if self.prompt.is_some() {
            self.apply_in_prompt(action);
            return;
        }
        if let Some((cave_id, _)) = self.handed_off() {
            // QuitGame cancels a launch butler is still setting up as well.
            if matches!(action, Action::Back) {
                self.backend.send(Command::QuitGame {
                    cave_id: cave_id.to_string(),
                });
            }
            return;
        }
        if self.report.is_some() {
            self.apply_in_report(action);
            return;
        }
        // Only over a game's page; anything that leaves it closes the viewer.
        if !matches!(self.page, Page::Game { .. }) {
            self.viewer = None;
        }
        if self.viewer.is_some() {
            self.apply_in_viewer(action);
            return;
        }
        if self.menu.is_some() {
            self.apply_in_menu(action);
            return;
        }
        if self.qr_shown() {
            self.apply_in_qr(action);
            return;
        }
        if self.login.is_some() {
            self.apply_in_login(action);
            return;
        }
        match action {
            Action::MoveFocus(direction) => match self.page {
                Page::Library => self.move_focus_library(direction),
                Page::Game { id, button, shot } => {
                    self.move_focus_game(id, button, shot, direction)
                }
                Page::PlayableTypes { row } => {
                    // Everything, then each type.
                    let last = crate::model::playable_types().len();
                    let page = 6;
                    let row = match direction {
                        Direction::Up | Direction::Down => wrap_step(row, last + 1, direction),
                        Direction::PageUp => row.saturating_sub(page),
                        Direction::PageDown => (row + page).min(last),
                        Direction::Top | Direction::Home => 0,
                        Direction::Bottom | Direction::End => last,
                        Direction::Left | Direction::Right => row,
                    };
                    self.page = Page::PlayableTypes { row };
                }
                Page::Settings { row } => {
                    let len = self.settings_rows().len();
                    let last = len.saturating_sub(1);
                    let row = match direction {
                        Direction::Up | Direction::Down => wrap_step(row, len, direction),
                        Direction::PageUp | Direction::Top | Direction::Home => 0,
                        Direction::PageDown | Direction::Bottom | Direction::End => last,
                        Direction::Left | Direction::Right => row,
                    };
                    self.page = Page::Settings { row };
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
                    self.page = Page::Game {
                        id,
                        button,
                        shot: None,
                    };
                }
            }
            Action::Activate => match self.page {
                Page::Library => {
                    if let Some((_, Some(action))) = self.confirm_target() {
                        self.actions.push(action);
                    }
                }
                Page::Game {
                    shot: Some(index), ..
                } => self.actions.push(Action::ViewScreenshot(index)),
                Page::Game { id, button, .. } => {
                    let Some(game) = self.game(id) else {
                        return;
                    };
                    let buttons = self.game_buttons(game);
                    if let Some((_, action)) = buttons.get(button) {
                        self.actions.push(action.clone());
                    }
                }
                Page::PlayableTypes { row } => {
                    let action = match crate::model::type_at_row(row) {
                        Some(t) => Action::TogglePlayableType(t.id.clone()),
                        None => Action::AllPlayableTypes,
                    };
                    self.actions.push(action);
                }
                Page::Settings { row } => {
                    if let Some(setting) = self.settings_rows().into_iter().nth(row) {
                        self.actions.push(match setting.kind {
                            ui::SettingKind::Toggle { action, .. } => action,
                            ui::SettingKind::Link(page) => Action::Open(page),
                        });
                    }
                }
            },
            Action::SetPlayableOnly(on) => {
                if self.playable_only != on {
                    self.playable_only = on;
                    self.playable_filter_changed();
                }
            }
            Action::TogglePlayableType(id) => {
                if let Some(index) = self.playable_hidden.iter().position(|h| *h == id) {
                    self.playable_hidden.remove(index);
                } else if self.shown_type_count() > 1 {
                    self.playable_hidden.push(id);
                } else {
                    self.notify("At least one type stays on".to_string());
                    return;
                }
                self.playable_filter_changed();
            }
            Action::OnlyPlayableType(id) => {
                self.playable_hidden = crate::model::playable_types()
                    .iter()
                    .map(|t| t.id.clone())
                    .filter(|t| *t != id)
                    .collect();
                self.playable_filter_changed();
            }
            Action::AllPlayableTypes => {
                if !self.playable_hidden.is_empty() {
                    self.playable_hidden.clear();
                    self.playable_filter_changed();
                }
            }
            Action::FocusTypeRow(row) => {
                if let Page::PlayableTypes { .. } = self.page {
                    self.page = Page::PlayableTypes { row };
                }
            }
            Action::SetCoverMarks(on) => {
                if self.cover_marks != on {
                    self.cover_marks = on;
                    self.save_settings();
                }
            }
            Action::RatingFilterMenu => self.open_rating_filter(),
            Action::SetRatingFilter(filter) => {
                if self.rating_filter != filter {
                    self.rating_filter = filter;
                    self.rebuild_rows();
                    self.follow_rows();
                }
            }
            Action::FocusSettingsRow(row) => {
                if let Page::Settings { .. } = self.page {
                    self.page = Page::Settings { row };
                }
            }
            Action::ClearFinished => self.backend.send(Command::ClearFinished),
            Action::RefreshLibrary => self.refresh_library(),
            Action::ChangeUser => {
                if self.profile.is_some() {
                    self.backend.send(Command::ChangeUser);
                }
            }
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
                    self.save_settings();
                    self.request_collection_filtered(None);
                    self.rebuild_collection_sections();
                    self.collection_rows.follow = true;
                }
            }
            Action::Menu => {
                self.open_menu();
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
                    self.follow_rows();
                }
            }
            Action::CycleTab(step) => match self.page {
                Page::Library => self.actions.push(Action::SetTab(self.tab.next(step))),
                Page::Game { id, .. } => {
                    if let Some(next) = self.game_beside(id, step) {
                        // The row follows, so Back lands on this game.
                        if let Some(rows) = self.active_rows() {
                            rows.focus_game(next);
                        }
                        self.actions.push(Action::Open(Page::game(next)));
                    }
                }
                Page::PlayableTypes { .. } | Page::Settings { .. } => {}
            },
            Action::Search => match self.page {
                Page::Library if self.tab == Tab::Library && !self.handheld => {
                    self.focus_search = true;
                }
                Page::Library | Page::PlayableTypes { .. } | Page::Settings { .. } => {}
                Page::Game { .. } => self.actions.push(Action::ShowQr),
            },
            Action::Secondary => match self.page {
                Page::Library if self.tab != Tab::Downloads => {
                    let stops = self.toolbar().1.len();
                    let rows_empty = self.rows_empty();
                    let slot = tab_slot(self.tab);
                    if self.toolbar_focus_in(stops, rows_empty).is_none() {
                        self.toolbar_focus[slot] = Some(0);
                    } else if !rows_empty {
                        // Back to the game that had focus, which the rows
                        // kept while the filters had it.
                        self.toolbar_focus[slot] = None;
                        if let Some(rows) = self.active_rows() {
                            rows.settle_on_game();
                            rows.follow = true;
                        }
                    }
                }
                Page::Library => {}
                Page::Game { .. } => self.actions.push(Action::ShowQr),
                Page::PlayableTypes { row } => {
                    if let Some(t) = crate::model::type_at_row(row) {
                        self.actions.push(Action::OnlyPlayableType(t.id.clone()));
                    }
                }
                Page::Settings { .. } => {}
            },
            Action::ShowQr => {
                if let Page::Game { id, .. } = self.page
                    && let Some(game) = self.game(id)
                {
                    self.qr = ui::QrView::new(game);
                }
            }
            Action::HideQr => self.qr = None,
            Action::ViewScreenshot(index) => {
                if let Page::Game { id, .. } = self.page
                    && let Some(info) = self.page_info.peek(id)
                    && index < info.screenshots.len()
                {
                    self.viewer = Some(ui::ScreenshotView {
                        urls: info.screenshots.clone(),
                        index,
                    });
                }
            }
            Action::CloseScreenshot => self.viewer = None,
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
                Page::PlayableTypes { .. } | Page::Settings { .. } => {
                    self.page = self.page_stack.pop().unwrap_or(Page::Library);
                }
            },
            Action::MenuFocus(_) => {}
            Action::Quit => self.quitting = Some(Self::QUIT_FRAMES),
            Action::Open(page) => {
                // A list page remembers what it opened over; moving focus
                // within one reopens the same page.
                let list =
                    |p: Page| matches!(p, Page::PlayableTypes { .. } | Page::Settings { .. });
                if list(page) && std::mem::discriminant(&page) != std::mem::discriminant(&self.page)
                {
                    self.page_stack.push(self.page);
                }
                if matches!(page, Page::Game { .. }) {
                    self.detail_visit += 1;
                    self.page_scroll = ui::PageScroll::default();
                }
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
                // One game at a time: the screen, the pad and the panic
                // combo all assume it.
                if !self.running.is_empty() {
                    if !self.running.contains_key(&cave_id) {
                        self.notify("Another game is still running".into());
                    }
                    return;
                }
                self.running.insert(cave_id.clone(), Instant::now());
                self.launch_failures.remove(&cave_id);
                self.refresh_marks();
                self.backend.send(Command::Launch { cave_id });
            }
            // Only meaningful while a prompt is open, handled above.
            Action::Answer { .. } | Action::PromptFocus(_) | Action::ReportFocus(_) => {}
            Action::Report { cave_id } => self.open_report(&cave_id),
            Action::GameOptions { cave_id } => self.open_game_options(&cave_id),
            Action::Install { game_id } => {
                let Some(game) = self.game(game_id).cloned() else {
                    return;
                };
                if self.installs.contains_key(&game_id) {
                    return;
                }
                self.pending_installs.insert(game_id, Pending::Starting);
                self.install_failures.remove(&game_id);
                self.backend.send(Command::Install {
                    game: Box::new(game),
                });
                self.rebuild_installs();
            }
            Action::CancelInstall { game_id } => self.cancel_install(game_id),
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
            // Only the sign-in page has the checkbox, and it is handled above.
            Action::SetShareDeviceInfo(_) => {}
            Action::SelfUpdate => self.open_self_update(),
        }
    }

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

    fn game_buttons(&self, game: &Game) -> Vec<(&'static str, Action)> {
        ui::game_buttons(
            game,
            &self.caves_for(game.id),
            self.installs.get(&game.id),
            self.is_running(game.id),
            self.update_for(game.id),
            self.online,
        )
    }

    fn update_for(&self, game_id: i64) -> Option<&GameUpdate> {
        self.caves
            .iter()
            .filter(|cave| cave.game_id() == Some(game_id))
            .find_map(|cave| self.updates.get(&cave.id))
    }

    /// The QR code is up, over the page of the game it was opened for.
    fn qr_shown(&self) -> bool {
        match (&self.qr, &self.page) {
            (Some(qr), Page::Game { id, .. }) => qr.game_id == *id && self.login.is_none(),
            _ => false,
        }
    }

    /// Whether the game's page shows it running. Under a handoff it never
    /// does: the page sits behind the curtain, as the user will find it
    /// when the game exits.
    fn is_running(&self, game_id: i64) -> bool {
        !self.handoff
            && self
                .caves
                .iter()
                .any(|cave| cave.game_id() == Some(game_id) && self.running.contains_key(&cave.id))
    }

    /// Lays the home screen out as carousels, the way the itch app's
    /// Library tab does: what is installed, then what was played last,
    /// and everything owned.
    fn rebuild_sections(&mut self) {
        if let Some(deferred) = &mut self.deferred {
            deferred.sections = true;
            return;
        }
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

    /// Saves the "Playable here" choices and refilters every tab.
    fn playable_filter_changed(&mut self) {
        self.save_settings();
        self.request_collection_filtered(None);
        self.rebuild_rows();
        self.follow_rows();
    }

    /// How many of the device's types "Playable here" includes.
    fn shown_type_count(&self) -> usize {
        crate::model::device_platforms()
            .iter()
            .filter(|p| !self.playable_hidden.contains(p))
            .count()
    }

    /// Whether the game clears the page-wide filters.
    fn passes(&self, game: &Game) -> bool {
        self.rating_passes(game.id) && self.playable_passes(game)
    }

    /// Whether the game clears "Playable here", which also leaves out a
    /// game the player rated Doesn't run on the build installed now.
    fn playable_passes(&self, game: &Game) -> bool {
        !self.playable_only
            || (known_playable_here(game, &self.playable_hidden)
                && !matches!(
                    self.marks.get(&game.id),
                    Some(Mark::Rated(Rating::WontRun, true))
                ))
    }

    /// Whether the game clears the rating filter, which needs only its id.
    fn rating_passes(&self, game_id: i64) -> bool {
        self.rating_filter
            .matches(self.marks.get(&game_id).copied())
    }

    /// Why a filtered row is empty.
    fn empty_note(&self) -> String {
        if self.rating_filter != RatingFilter::Any {
            "Nothing here matches your rating filter".to_string()
        } else {
            "Nothing here runs on this device".to_string()
        }
    }

    /// A row after the page-wide filter: none when it was empty anyway, a
    /// note in place of tiles when the filter took everything.
    fn section(&self, title: impl Fn(usize) -> String, games: Vec<i64>) -> Option<ui::Section> {
        if games.is_empty() {
            return None;
        }
        let games: Vec<i64> = games
            .into_iter()
            .filter(|id| match self.catalog.get(id) {
                Some(game) => self.passes(game),
                None => self.rating_passes(*id),
            })
            .collect();
        let note = games.is_empty().then(|| self.empty_note());
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

    /// Prompts carry no game id, so the picker is matched by the title in
    /// its body.
    /// The open question of which of the game's uploads to install.
    fn upload_picker_for(&self, game_id: i64) -> Option<&Prompt> {
        self.prompt.as_ref().filter(
            |p| matches!(p.origin, PromptOrigin::UploadPicker { game_id: g, .. } if g == game_id),
        )
    }

    /// Closes the open prompt and shows the next one waiting, if any.
    fn close_prompt(&mut self) {
        self.prompt = self.prompt_queue.pop_front();
    }

    fn download_for(&self, game_id: i64) -> Option<&Download> {
        self.downloads
            .iter()
            .find(|d| d.game.as_ref().is_some_and(|g| g.id == game_id))
    }

    /// One row per collection. With the installed filter on, collections
    /// with nothing installed sink to the bottom and say so.
    fn rebuild_collection_sections(&mut self) {
        if let Some(deferred) = &mut self.deferred {
            deferred.collections = true;
            return;
        }
        let Some(collections) = self.collections.get() else {
            return;
        };
        let installed_only = self.collections_installed_only;
        let filter = self.collection_filter();
        let mut settled = true;
        let mut sections: Vec<ui::Section> = collections
            .iter()
            .map(|c| {
                // With a filter on, butler's answer is the whole row; until
                // it arrives, the first page filtered here stands in. If
                // the query failed, the row pages and filters here instead.
                let id = c.collection.id;
                let filtered = self.collection_filtered.as_ref().filter(|_| filter.any());
                let exact = filtered
                    .filter(|_| !c.refreshing)
                    .and_then(|f| f.games.get(&id));
                let failed = filtered.is_some_and(|f| f.failed.contains(&id));
                let waiting = c.refreshing || (filter.any() && exact.is_none() && !failed);
                settled &= !waiting;
                let listed = exact.unwrap_or(&c.games);
                let games: Vec<i64> = listed
                    .iter()
                    .filter(|g| self.passes(g))
                    .map(|g| g.id)
                    .filter(|id| !installed_only || self.installed.contains(id))
                    .collect();
                // Filtered rows come whole from butler; only unfiltered
                // ones page.
                let more = (!filter.any() || failed) && c.next_cursor.is_some();
                let count = exact.map_or(c.collection.games_count, |_| games.len() as i64);
                let note = match (games.is_empty(), installed_only) {
                    (true, _) if more && self.rating_filter != RatingFilter::Any => {
                        Some("Nothing loaded so far matches your rating filter".to_string())
                    }
                    _ if more => None,
                    (true, _) if waiting => Some("Loading…".to_string()),
                    _ if c.games.is_empty() => Some("Empty collection".to_string()),
                    (true, true) => Some("Nothing installed from this collection".to_string()),
                    (true, false) => Some(self.empty_note()),
                    _ => None,
                };
                ui::Section {
                    title: format!("{} · {count}", c.collection.title),
                    games,
                    note,
                    more,
                    collection: Some(c.collection.id),
                }
            })
            .collect();
        // Rows stay put while they fill in, then empty ones sink all at
        // once.
        if settled {
            sections.sort_by_key(|s| s.games.is_empty() && !s.more);
        }
        self.collection_rows.set_sections(sections);
    }

    /// Clears what a sync left refreshing when it stopped early, and asks
    /// again for filtered rows whose query failed.
    fn finish_collection_refresh(&mut self) {
        let Some(collections) = self.collections.get_mut() else {
            return;
        };
        let mut again = Vec::new();
        for c in collections.iter_mut() {
            if std::mem::take(&mut c.refreshing) {
                again.push(c.collection.id);
            }
        }
        if let Some(filtered) = &self.collection_filtered {
            again.extend(filtered.failed.iter().copied());
        }
        self.request_collection_filtered(Some(again));
        self.rebuild_collection_sections();
    }

    fn save_settings(&self) {
        if let Some(path) = &self.settings_path {
            Settings {
                cover_marks: self.cover_marks,
                playable_only: self.playable_only,
                collections_installed_only: self.collections_installed_only,
                playable_hidden: self.playable_hidden.clone(),
            }
            .save(path);
        }
    }

    fn collection_filter(&self) -> CollectionFilter {
        CollectionFilter {
            installed: self.collections_installed_only,
            playable: self.playable_only,
            hidden: if self.playable_only {
                self.playable_hidden.clone()
            } else {
                Vec::new()
            },
        }
    }

    /// Asks butler which games in each collection pass the filters. With
    /// `only`, just those collections, added to what is known; otherwise
    /// all of them, unless the answer for this filter is already in.
    fn request_collection_filtered(&mut self, only: Option<Vec<i64>>) {
        let filter = self.collection_filter();
        if !filter.any() {
            self.collection_filtered = None;
            return;
        }
        let Some(collections) = self.collections.get() else {
            return;
        };
        let current = self
            .collection_filtered
            .as_ref()
            .is_some_and(|f| f.filter == filter);
        let collection_ids: Vec<i64> = match only {
            Some(ids) if current => ids,
            _ if current => return,
            _ => {
                self.collection_filtered = Some(Filtered {
                    filter: filter.clone(),
                    games: Default::default(),
                    asked: Default::default(),
                    failed: Default::default(),
                });
                collections.iter().map(|c| c.collection.id).collect()
            }
        };
        if collection_ids.is_empty() {
            return;
        }
        self.collection_asks += 1;
        let ask = self.collection_asks;
        if let Some(filtered) = &mut self.collection_filtered {
            for id in &collection_ids {
                filtered.games.remove(id);
                filtered.failed.remove(id);
                filtered.asked.insert(*id, ask);
            }
        }
        self.backend.send(Command::CollectionsFiltered {
            filter,
            ask,
            collection_ids,
        });
    }

    /// Whether `ask` is the latest question about this collection under
    /// this filter; answers to older ones are dropped.
    fn is_latest_ask(&self, filter: &CollectionFilter, ask: u64, collection_id: i64) -> bool {
        self.collection_filtered
            .as_ref()
            .is_some_and(|f| f.filter == *filter && f.asked.get(&collection_id) == Some(&ask))
    }

    /// Tells the backend which collections are on screen, so a refresh
    /// fetches those first.
    fn want_visible_collections(&mut self) {
        let Some(collections) = self.collections.get() else {
            return;
        };
        if !collections.iter().any(|c| c.refreshing) {
            return;
        }
        let rows = &self.collection_rows;
        let wanted: Vec<i64> = rows
            .visible()
            .filter_map(|row| rows.sections.get(row)?.collection)
            .collect();
        if wanted != self.collections_wanted {
            self.collections_wanted = wanted.clone();
            self.backend.send(Command::CollectionsWanted(wanted));
        }
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
        // Queued once an upload is picked; before that butler has said nothing.
        for (game_id, _) in self
            .pending_installs
            .iter()
            .filter(|(_, p)| **p == Pending::Picked)
        {
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

        let loaded = !matches!(self.owned, Loadable::Loading) || self.login.is_some();
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
                Step::Report => self.report = Some(sample::report()),
                Step::Notice => self.notify(sample::NOTICE.into()),
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

    /// Handles what the backend sent since the last frame. A burst of
    /// events rebuilds the rows once, at the end.
    fn handle_events(&mut self) {
        self.deferred = Some(Rebuilds::default());
        self.handle_each_event();
        if let Some(deferred) = self.deferred.take() {
            if deferred.sections {
                self.rebuild_sections();
            }
            if deferred.collections {
                self.rebuild_collection_sections();
            }
        }
    }

    fn handle_each_event(&mut self) {
        for event in self.backend.poll() {
            match event {
                Event::Status(text) => self.status = text,
                Event::ButlerVersion(version) => self.butler_version = Some(version),
                Event::LoginRequired { url, user_code } => {
                    let qr = QrCode::encode(&url);
                    self.login = Some(LoginView {
                        failure: qr
                            .is_none()
                            .then(|| "Couldn't draw the sign-in code".to_string()),
                        qr,
                        user_code: Some(user_code),
                        share_device_info: true,
                        focused: false,
                    });
                }
                Event::LoginFailed(message) => {
                    self.login = Some(LoginView {
                        qr: None,
                        user_code: None,
                        failure: Some(message),
                        share_device_info: true,
                        focused: false,
                    });
                }
                Event::SignedIn(profile) => {
                    self.login = None;
                    self.profile = Some(profile);
                    self.resend_reports();
                }
                Event::SignedOut => self.sign_out(),
                Event::OwnedGames(games) => {
                    self.catalog_add(Source::Owned, &games);
                    drop_later(std::mem::replace(&mut self.owned, Loadable::Loaded(games)));
                    self.rebuild_sections();
                }
                Event::Collections(collections) => {
                    self.catalog_add(
                        Source::Collection,
                        collections.iter().flat_map(|c| &c.games),
                    );
                    drop_later(std::mem::replace(
                        &mut self.collections,
                        Loadable::Loaded(collections),
                    ));
                    self.collection_loading.clear();
                    self.collection_filtered = None;
                    self.request_collection_filtered(None);
                    self.rebuild_collection_sections();
                }
                Event::CollectionRefreshed(shelf) => {
                    self.catalog_add(Source::Collection, &shelf.games);
                    let id = shelf.collection.id;
                    self.collection_loading.remove(&id);
                    if let Some(c) = self
                        .collections
                        .get_mut()
                        .into_iter()
                        .flatten()
                        .find(|c| c.collection.id == id)
                    {
                        *c = shelf;
                    }
                    self.request_collection_filtered(Some(vec![id]));
                    self.rebuild_collection_sections();
                }
                Event::Syncing(syncing) => {
                    self.syncing = syncing;
                    if !syncing {
                        self.refreshing = false;
                        self.finish_collection_refresh();
                    }
                }
                Event::CollectionPage {
                    collection_id,
                    games,
                    next_cursor,
                } => {
                    self.catalog_add(Source::Collection, &games);
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
                Event::CollectionFiltered {
                    filter,
                    ask,
                    collection_id,
                    games,
                } => {
                    if self.is_latest_ask(&filter, ask, collection_id) {
                        self.catalog_add(Source::Collection, &games);
                        if let Some(filtered) = &mut self.collection_filtered {
                            filtered.games.insert(collection_id, games);
                        }
                        self.rebuild_collection_sections();
                    }
                }
                Event::CollectionsFilterFailed {
                    filter,
                    ask,
                    collection_ids,
                } => {
                    let failed: Vec<i64> = collection_ids
                        .into_iter()
                        .filter(|id| self.is_latest_ask(&filter, ask, *id))
                        .collect();
                    if let Some(filtered) = &mut self.collection_filtered
                        && !failed.is_empty()
                    {
                        filtered.failed.extend(failed);
                        self.rebuild_collection_sections();
                    }
                }
                Event::Caves(caves) => {
                    let games: Vec<Arc<Game>> = caves
                        .iter()
                        .filter_map(|c| c.game.clone().map(Arc::new))
                        .collect();
                    self.catalog_add(Source::Install, &games);
                    self.installed = caves.iter().filter_map(CaveExt::game_id).collect();
                    self.caves = caves;
                    self.rebuild_marks();
                    if self.collections_installed_only {
                        self.collection_filtered = None;
                        self.request_collection_filtered(None);
                    }
                    self.rebuild_rows();
                }
                Event::Downloads(downloads) => {
                    let listed: std::collections::HashSet<String> =
                        downloads.iter().map(|d| d.id.clone()).collect();
                    self.progress.retain(|id, _| listed.contains(id));
                    self.discarding.retain(|id| listed.contains(id));
                    self.pending_installs.retain(|id, _| {
                        !downloads
                            .iter()
                            .any(|d| d.game.as_ref().is_some_and(|g| g.id == *id))
                    });
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
                Event::LaunchRunning => {
                    if self.handoff || self.minimize_while_playing {
                        self.hide_window();
                    }
                }
                Event::LaunchFinished {
                    cave_id,
                    outcome,
                    run,
                } => {
                    self.running.remove(&cave_id);
                    if let Some(run) = run {
                        self.save_run(&cave_id, run);
                    }
                    if outcome == Launched::Cancelled {
                        // Nothing ran, so the window never left.
                        continue;
                    }
                    if let Launched::Failed(failure) = outcome {
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
                        self.refresh_marks();
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
                    if online && !self.online {
                        self.resend_reports();
                    }
                    self.online = online;
                    if !online {
                        self.refreshing = false;
                    }
                }
                Event::ReportSent { game_id, result } => match result {
                    Ok(()) => {
                        if let Some(saved) = self.reports.get_mut(&game_id) {
                            saved.sent = true;
                            saved.run = None;
                            self.save_reports();
                        }
                    }
                    Err(error) => {
                        log::warn!("report for {game_id}: {error}");
                        self.notify("Couldn't send the report; it will be sent later".into());
                    }
                },
                Event::Prompt(prompt) => {
                    if let PromptOrigin::UploadPicker { game_id, .. } = prompt.origin {
                        self.pending_installs.insert(game_id, Pending::Choosing);
                        self.rebuild_installs();
                    }
                    if self.prompt.is_some() {
                        self.prompt_queue.push_back(prompt);
                    } else {
                        self.prompt = Some(prompt);
                    }
                }
                Event::PromptClosed(id) => {
                    self.prompt_queue.retain(|p| p.id != id);
                    if self.prompt.as_ref().is_some_and(|p| p.id == id) {
                        self.close_prompt();
                    }
                    self.rebuild_installs();
                }
                Event::UninstallFinished { result } => {
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
                Event::UpdateCheckFailed(error) => {
                    self.checking_updates = false;
                    self.notify(format!("Couldn't check for updates: {error}"));
                }
                Event::Error(message) => {
                    if self.owned.get().is_none() && self.login.is_none() {
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
    /// The remembered Downloads focus, clamped to the rows there are: they
    /// come and go as butler works.
    fn downloads_row_in(&self, rows: &[ui::DownloadRow<'_>]) -> (usize, usize) {
        let (row, button) = self.downloads_row;
        let row = row.min(rows.len().saturating_sub(1));
        let buttons = rows.get(row).map_or(0, |r| r.buttons.len());
        (row, button.min(buttons.saturating_sub(1)))
    }

    /// What the Downloads tab lists, split the way the itch app splits it:
    /// butler's pending queue in its order, then installs asked for that
    /// butler has not listed yet, then everything with a `finished_at`,
    /// done or failed, newest first. Finished entries stay in butler's
    /// queue until the user clears them.
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
                    buttons.push(("Open", Action::Open(Page::game(game.id))));
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
                        human_size(p.bps as i64),
                        human_duration_seconds(p.eta as i64),
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
        for (game_id, pending) in &self.pending_installs {
            if self.download_for(*game_id).is_some() {
                continue;
            }
            let detail = match pending {
                Pending::Starting => continue,
                Pending::Choosing => "Choose a download",
                Pending::Picked => "Queueing",
            };
            let game = self.game(*game_id);
            let title = game.map_or_else(|| "Game".to_string(), |g| g.title.clone());
            rows.push(ui::DownloadRow {
                game,
                title,
                detail: detail.to_string(),
                progress: None,
                failed: false,
                section: ui::DownloadSection::Queue,
                direct_update: false,
                buttons: vec![("Cancel", Action::CancelInstall { game_id: *game_id })],
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
                    buttons.push(("Open", Action::Open(Page::game(game.id))));
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
                    .and_then(rfc3339_to_unix)
                    .map(human_time_ago);
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

    /// Keeps a cave's last run, on disk too, for a report filed later.
    fn save_run(&mut self, cave_id: &str, run: Run) {
        let Some(cave) = self.cave(cave_id) else {
            return;
        };
        let Some(upload) = &cave.upload else {
            return;
        };
        let saved = SavedRun {
            upload_id: upload.id,
            build_id: cave.build.as_ref().map(|b| b.id),
            run,
        };
        self.runs.insert(cave_id.to_string(), saved);
        // Uninstalled caves go; an empty list may just not be loaded.
        if !self.caves.is_empty() {
            let caves = &self.caves;
            self.runs.retain(|id, _| caves.iter().any(|c| &c.id == id));
        }
        if let Some(path) = &self.runs_path {
            crate::report::save(path, &self.runs);
        }
        self.refresh_marks();
    }

    /// Rebuilds the cover marks: the player's rating where there is one,
    /// else whether the installed game was tried. Called whenever the
    /// installs, runs, launch failures or reports change.
    fn rebuild_marks(&mut self) -> bool {
        let mut marks: std::collections::HashMap<i64, Mark> = self
            .caves
            .iter()
            .filter(|cave| self.reportable(cave))
            .filter_map(|cave| Some((cave.game_id()?, Mark::Tried)))
            .collect();
        for &game_id in self.reports.keys() {
            if let Some((saved, current)) = reported(&self.reports, &self.caves, game_id) {
                marks.insert(game_id, Mark::Rated(saved.rating, current));
            }
        }
        if self.marks == marks {
            return false;
        }
        self.marks = marks;
        true
    }

    /// Rebuilds the marks and, when a filter reads them, the rows too.
    fn refresh_marks(&mut self) {
        if self.rebuild_marks() && (self.rating_filter != RatingFilter::Any || self.playable_only) {
            self.rebuild_rows();
        }
    }

    /// Asks which of the player's ratings to show, with how many games
    /// each would be.
    fn open_rating_filter(&mut self) {
        // Counts are of the Library tab's games after "Playable here";
        // collection rows page in, so there they are left off.
        let listed: Vec<i64> = if self.tab == Tab::Library {
            let mut seen = std::collections::HashSet::new();
            self.owned
                .get()
                .into_iter()
                .flatten()
                .map(|g| g.id)
                .chain(self.installed_ids())
                .filter(|id| seen.insert(*id))
                .filter(|id| self.catalog.get(id).is_none_or(|g| self.playable_passes(g)))
                .collect()
        } else {
            Vec::new()
        };
        let choices = RatingFilter::ALL
            .iter()
            .map(|filter| {
                if *filter == RatingFilter::Any || self.tab != Tab::Library {
                    return filter.label().to_string();
                }
                let count = listed
                    .iter()
                    .filter(|id| filter.matches(self.marks.get(id).copied()))
                    .count();
                format!("{} ({count})", filter.label())
            })
            .collect();
        let prompt = Prompt {
            id: 0,
            origin: PromptOrigin::Options,
            title: "Your rating".to_string(),
            body: String::new(),
            choices,
            focus: RatingFilter::ALL
                .iter()
                .position(|f| *f == self.rating_filter)
                .unwrap_or(0),
            primary: None,
            stacked: true,
            details: Vec::new(),
            progress: None,
        };
        if let Some(open) = self.prompt.replace(prompt) {
            self.prompt_queue.push_front(open);
        }
        self.prompt_options = RatingFilter::ALL
            .iter()
            .map(|f| Action::SetRatingFilter(*f))
            .collect();
    }

    /// Whether the cave was launched, so there is something to report on.
    fn reportable(&self, cave: &Cave) -> bool {
        self.runs.contains_key(&cave.id)
            || self.launch_failures.contains_key(&cave.id)
            || cave
                .stats
                .as_ref()
                .is_some_and(|s| s.local_seconds_run.unwrap_or(0) > 0 || s.seconds_run > 0)
    }

    /// Asks how the game in `cave_id` runs. The cave's last run goes with
    /// the answer while it was of the build installed now.
    fn open_report(&mut self, cave_id: &str) {
        let Some(cave) = self.cave(cave_id) else {
            return;
        };
        let (Some(game), Some(upload)) = (&cave.game, &cave.upload) else {
            return;
        };
        let build_id = cave.build.as_ref().map(|b| b.id);
        let run = self
            .runs
            .get(cave_id)
            .filter(|saved| saved.upload_id == upload.id && saved.build_id == build_id)
            .map(|saved| saved.run.clone())
            .unwrap_or_default();
        // An earlier report on this build starts the answers.
        let earlier = self
            .reports
            .get(&game.id)
            .filter(|saved| saved.upload_id == upload.id && saved.build_id == build_id);
        let (rating, flags) = match earlier {
            _ if self.launch_failures.contains_key(cave_id) => (Rating::WontRun, Vec::new()),
            Some(saved) => (
                saved.rating,
                saved
                    .flags
                    .iter()
                    .filter_map(|id| crate::report::flag(id))
                    .map(|f| f.id)
                    .collect(),
            ),
            None => (Rating::Perfect, Vec::new()),
        };
        let draft = Report {
            game_id: game.id,
            upload_id: upload.id,
            build_id,
            rating,
            flags,
            run,
        };
        self.report = Some(ui::ReportView::new(game.title.clone(), draft));
    }

    /// Sends the open report in the background and closes it.
    fn send_report(&mut self) {
        let Some(view) = self.report.take() else {
            return;
        };
        if view.sample {
            return;
        }
        let report = view.finish();
        let sent_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let saved = SavedReport {
            upload_id: report.upload_id,
            build_id: report.build_id,
            rating: report.rating,
            flags: report.flags.iter().map(|f| f.to_string()).collect(),
            sent_at,
            sent: false,
            run: Some(report.run.clone()),
        };
        self.reports.insert(report.game_id, saved);
        self.save_reports();
        self.refresh_marks();
        self.backend.send(Command::Report(Box::new(report)));
    }

    fn save_reports(&self) {
        if let Some(path) = &self.reports_path {
            crate::report::save(path, &self.reports);
        }
    }

    /// Sends the reports that have not reached itch.io yet.
    fn resend_reports(&mut self) {
        let unsent: Vec<Report> = self
            .reports
            .iter()
            .filter_map(|(game_id, saved)| saved.unsent(*game_id))
            .collect();
        for report in unsent {
            self.backend.send(Command::Report(Box::new(report)));
        }
    }

    /// Something failed that has no page to show it on.
    fn notify(&mut self, message: String) {
        log::warn!("{message}");
        self.notice = Some((message, Instant::now()));
    }

    /// The game page's More button: what else there is to do with the
    /// installed game, as a list.
    fn open_game_options(&mut self, cave_id: &str) {
        let Some(cave) = self.cave(cave_id) else {
            return;
        };
        let mut options = Vec::new();
        if self.reportable(cave) {
            let reported = cave.game_id().is_some_and(|id| {
                reported(&self.reports, &self.caves, id).is_some_and(|(_, current)| current)
            });
            options.push((
                if reported {
                    "Update compatibility report"
                } else {
                    "Report compatibility"
                },
                Action::Report {
                    cave_id: cave_id.to_string(),
                },
            ));
        }
        options.push((
            "Uninstall",
            Action::Uninstall {
                cave_id: cave_id.to_string(),
            },
        ));
        let title = cave
            .game
            .as_ref()
            .map_or("Game", |g| g.title.as_str())
            .to_string();
        self.prompt = Some(Prompt {
            id: 0,
            origin: PromptOrigin::Options,
            title,
            body: String::new(),
            choices: options.iter().map(|(label, _)| label.to_string()).collect(),
            focus: 0,
            primary: None,
            stacked: true,
            details: Vec::new(),
            progress: None,
        });
        self.prompt_options = options.into_iter().map(|(_, action)| action).collect();
    }

    /// The drawer's row: open the dialog, checking unless there is
    /// already something to show.
    fn open_self_update(&mut self) {
        let Some(update) = self.self_update.as_mut() else {
            return;
        };
        update.check();
        self.menu = None;
        if let Some(prompt) = self.self_update_prompt(0)
            && let Some(open) = self.prompt.replace(prompt)
        {
            self.prompt_queue.push_front(open);
        }
    }

    fn self_update_prompt(&self, focus: usize) -> Option<Prompt> {
        use self_update::State;
        let running = env!("ZITCH_VERSION");
        let progress = match self.self_update.as_ref()?.state() {
            State::Downloading { release, done } => {
                let fraction = *done as f32 / release.size.max(1) as f32;
                let line = format!(
                    "{:.0}%, {} of {}",
                    fraction * 100.0,
                    human_size(*done as i64),
                    human_size(release.size as i64)
                );
                Some((line, fraction))
            }
            _ => None,
        };
        let (title, body, choices): (String, String, Vec<&str>) =
            match self.self_update.as_ref()?.state() {
                State::Idle => return None,
                State::Checking => (
                    "Checking for a zitch update…".into(),
                    format!("You have zitch {running}."),
                    vec!["Close"],
                ),
                State::UpToDate => (
                    "zitch is up to date".into(),
                    format!("You have zitch {running}, the latest release."),
                    vec!["Close"],
                ),
                State::Failed(message) => ("zitch update".into(), message.clone(), vec!["Close"]),
                State::Available(release) => (
                    format!("zitch {} is available", release.version),
                    format!(
                        "You have zitch {running}. The download ({}) goes into Archive \
                         Manager for you to manually install after quitting zitch.",
                        human_size(release.size as i64)
                    ),
                    vec!["Download", "Not now"],
                ),
                State::Downloading { release, .. } => (
                    format!("Downloading zitch {}", release.version),
                    "It carries on if you close this.".into(),
                    vec!["Close"],
                ),
                State::Ready(release) => (
                    format!("zitch {} is ready to install", release.version),
                    format!(
                        "Quit zitch, open Applications > Archive Manager, and pick {}.",
                        release.file_name
                    ),
                    vec!["Quit now", "Later"],
                ),
            };
        Some(Prompt {
            id: 0,
            origin: PromptOrigin::SelfUpdate,
            title,
            body,
            focus: focus.min(choices.len() - 1),
            primary: (choices.len() > 1).then_some(0),
            choices: choices.into_iter().map(String::from).collect(),
            stacked: false,
            details: Vec::new(),
            progress,
        })
    }

    fn answer_self_update(&mut self, choice: Option<usize>) {
        use self_update::State;
        let Some(update) = self.self_update.as_mut() else {
            return;
        };
        match (update.state(), choice) {
            // The dialog stays up to show the download.
            (State::Available(_), Some(0)) => {
                update.download();
                return;
            }
            (State::Ready(_), Some(0)) => self.quitting = Some(Self::QUIT_FRAMES),
            _ => update.dismiss(),
        }
        self.close_prompt();
    }

    fn poll_self_update(&mut self) {
        let Some(update) = self.self_update.as_mut() else {
            return;
        };
        let finished = update.poll();
        let open = self
            .prompt
            .as_ref()
            .filter(|p| p.origin == PromptOrigin::SelfUpdate)
            .map(|p| (p.focus, p.choices.len()));
        match open {
            Some((focus, choices)) => {
                let mut prompt = self.self_update_prompt(focus);
                // Different buttons: start from the first again.
                if let Some(prompt) = prompt.as_mut().filter(|p| p.choices.len() != choices) {
                    prompt.focus = 0;
                }
                self.prompt = prompt.or_else(|| self.prompt_queue.pop_front());
            }
            // The download finished behind a closed dialog.
            None if finished => self.open_self_update(),
            None => {}
        }
    }

    fn refresh_library(&mut self) {
        if self.online && !self.refreshing {
            self.refreshing = true;
            self.backend.send(Command::RefreshLibrary);
        }
    }

    /// Drops what belonged to the profile. Installs and downloads are the
    /// database's, not the profile's, and the next session resends them.
    fn sign_out(&mut self) {
        self.profile = None;
        self.report = None;
        self.viewer = None;
        self.owned = Loadable::Loading;
        self.collections = Loadable::default();
        self.collection_filtered = None;
        self.collections_wanted.clear();
        self.collection_loading.clear();
        self.updates.clear();
        self.refreshing = false;
        self.checking_updates = false;
        self.up_to_date_at = None;
        self.page = Page::Library;
        self.tab = Tab::default();
        self.toolbar_focus = [None; Tab::ALL.len()];
        self.query.clear();
        self.rows = ui::Rows::default();
        self.collection_rows = ui::Rows::default();
        self.catalog.clear();
        self.catalog_sources.clear();
        self.rebuild_rows();
    }

    /// What the menu drawer offers, top to bottom.
    fn menu_items(&self) -> Vec<ui::MenuItem> {
        let mut items = vec![ui::MenuItem {
            label: if self.refreshing {
                "Refreshing…".into()
            } else {
                "Refresh library".into()
            },
            action: Action::RefreshLibrary,
            busy: self.refreshing,
        }];
        if self.profile.is_some() {
            items.push(ui::MenuItem {
                label: "Change user".into(),
                action: Action::ChangeUser,
                busy: false,
            });
        }
        if let Some(update) = &self.self_update {
            items.push(ui::MenuItem {
                label: "Check for zitch update".into(),
                action: Action::SelfUpdate,
                busy: matches!(update.state(), self_update::State::Downloading { .. }),
            });
        }
        items.push(ui::MenuItem {
            label: "Settings".into(),
            action: Action::Open(Page::Settings { row: 0 }),
            busy: false,
        });
        items.push(ui::MenuItem {
            label: "Quit".into(),
            action: Action::Quit,
            busy: false,
        });
        items
    }

    /// The Settings page's rows, top to bottom.
    fn settings_rows(&self) -> Vec<ui::SettingRow> {
        let mut rows = vec![ui::SettingRow {
            group: "Library",
            label: "Show compatibility marks on covers",
            kind: ui::SettingKind::Toggle {
                on: self.cover_marks,
                action: Action::SetCoverMarks(!self.cover_marks),
            },
        }];
        if crate::muos::available() {
            rows.push(ui::SettingRow {
                group: "Library",
                label: "Playable types",
                kind: ui::SettingKind::Link(Page::PlayableTypes { row: 0 }),
            });
        }
        rows
    }

    /// The version lines at the bottom of the Settings page.
    fn about_rows(&self) -> Vec<(&'static str, String)> {
        let mut about = vec![("zitch", env!("ZITCH_VERSION").to_string())];
        if let Some(version) = &self.butler_version {
            about.push(("butler", version.clone()));
        }
        about
    }

    /// What the footer offers on the current page, in reading order.
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
        if let Some(view) = &self.report {
            let mut hints = vec![(vec![Glyph::Navigate], "Choose".to_string())];
            let confirm = match view.focused() {
                Some(ui::ReportRow::Rating(_)) => "Select",
                Some(ui::ReportRow::Flag(flag)) if view.draft.flags.contains(&flag) => "Uncheck",
                Some(ui::ReportRow::Flag(_)) => "Check",
                Some(ui::ReportRow::Send) => "Send",
                Some(ui::ReportRow::Cancel) | None => "Cancel",
            };
            hints.push((vec![Glyph::Confirm], confirm.to_string()));
            hints.push((vec![Glyph::Menu], "Send".to_string()));
            let back = if view.picked { "Back" } else { "Cancel" };
            hints.push((vec![Glyph::Back], back.to_string()));
            return hints;
        }
        if self.handed_off().is_some() {
            return vec![(vec![Glyph::Back], "Cancel".to_string())];
        }
        if let Some(view) = &self.viewer {
            let mut hints = Vec::new();
            if view.urls.len() > 1 {
                hints.push((
                    vec![Glyph::NavigateHorizontal],
                    "Previous / Next".to_string(),
                ));
            }
            hints.push((vec![Glyph::Back], "Close".to_string()));
            return hints;
        }
        if self.qr_shown() {
            return vec![(vec![Glyph::Back], "Close".to_string())];
        }
        if let Some(login) = &self.login {
            let mut hints = Vec::new();
            if login.has_checkbox() {
                hints.push((vec![Glyph::NavigateVertical], "Choose".to_string()));
            }
            let confirm = if login.focused && login.has_checkbox() {
                if login.share_device_info {
                    "Don't send device info"
                } else {
                    "Send device info"
                }
            } else if login.failure.is_some() {
                "Try again"
            } else {
                "New code"
            };
            hints.push((vec![Glyph::Confirm], confirm.to_string()));
            hints.push((vec![Glyph::Back], "Quit".to_string()));
            return hints;
        }
        // The tab strip already shows the bumpers, so no hint repeats them.
        match self.page {
            Page::Library => {
                let (_, stops) = self.toolbar();
                let rows_empty = self.rows_empty();
                let on_toolbar = self.toolbar_focus_in(stops.len(), rows_empty).is_some();
                let mut hints = Vec::new();
                // Confirm names what the focused control does.
                let confirm = self.confirm_target().map(|(label, _)| label);
                if !rows_empty || stops.len() > 1 {
                    hints.push((vec![Glyph::Navigate], "Browse".to_string()));
                }
                if let Some(label) = confirm {
                    hints.push((vec![Glyph::Confirm], label.to_string()));
                }
                if self.input_mode == InputMode::Gamepad && self.tab != Tab::Downloads {
                    let label = if !on_toolbar {
                        Some("Filters")
                    } else {
                        (!rows_empty).then_some("Games")
                    };
                    if let Some(label) = label {
                        hints.push((vec![Glyph::Secondary], label.to_string()));
                    }
                }
                match self.tab {
                    Tab::Library => {
                        if self.input_mode != InputMode::Gamepad && !self.handheld {
                            hints.push((vec![Glyph::Secondary], "Search".to_string()));
                        }
                        hints.push((vec![Glyph::Menu], "Menu".to_string()));
                    }
                    Tab::Collections | Tab::Downloads => {
                        hints.push((vec![Glyph::Back], "Back".to_string()));
                    }
                }
                hints
            }
            Page::Game { id, button, shot } => {
                let mut hints: Vec<(Vec<Glyph>, String)> = Vec::new();
                if let Some(game) = self.game(id) {
                    let buttons = self.game_buttons(game);
                    let shots = self.screenshot_count(id);
                    if shots > 0 {
                        hints.push((vec![Glyph::Navigate], "Choose".to_string()));
                    } else if buttons.len() > 1 {
                        hints.push((vec![Glyph::NavigateHorizontal], "Choose".to_string()));
                    }
                    match shot.filter(|&i| i < shots) {
                        Some(_) => hints.push((vec![Glyph::Confirm], "View".to_string())),
                        None => {
                            if let Some((label, _)) = buttons.get(button) {
                                hints.push((vec![Glyph::Confirm], label.to_string()));
                            }
                        }
                    }
                    if !game.url.is_empty() {
                        hints.push((vec![Glyph::Secondary], "QR code".to_string()));
                    }
                }
                if self.game_beside(id, -1).is_some() || self.game_beside(id, 1).is_some() {
                    hints.push((vec![Glyph::TabLeft, Glyph::TabRight], "Games".to_string()));
                }
                hints.push((vec![Glyph::Back], "Back".to_string()));
                hints
            }
            Page::PlayableTypes { row } => {
                let mut hints = vec![(vec![Glyph::NavigateVertical], "Choose".to_string())];
                match crate::model::type_at_row(row) {
                    // Everything is already on: A has nothing to do.
                    None if self.shown_type_count() == crate::model::device_platforms().len() => {}
                    None => hints.push((vec![Glyph::Confirm], "Turn all on".to_string())),
                    Some(t) => {
                        let label = if self.playable_hidden.contains(&t.id) {
                            "Turn on"
                        } else {
                            "Turn off"
                        };
                        hints.push((vec![Glyph::Confirm], label.to_string()));
                        hints.push((vec![Glyph::Secondary], "Only this".to_string()));
                    }
                }
                hints.push((vec![Glyph::Back], "Back".to_string()));
                hints
            }
            Page::Settings { row } => {
                let mut hints = vec![(vec![Glyph::NavigateVertical], "Choose".to_string())];
                if let Some(setting) = self.settings_rows().get(row) {
                    let label = match &setting.kind {
                        ui::SettingKind::Toggle { on: true, .. } => "Turn off",
                        ui::SettingKind::Toggle { on: false, .. } => "Turn on",
                        ui::SettingKind::Link(_) => "Open",
                    };
                    hints.push((vec![Glyph::Confirm], label.to_string()));
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
        self.poll_self_update();
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
            self.intro.skip();
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
        self.intro.tick(ui.ctx());
        // Pixels: an emulated screen is laid out at 1x, a real one may not be.
        let pixels = if self.emulate.is_some() {
            screen.size()
        } else {
            screen.size() * ui.ctx().pixels_per_point()
        };
        crate::device_info::set_resolution(pixels.x, pixels.y);
        let policy = crate::images::Policy::for_screen(screen.height(), self.low_spec);
        self.handheld = policy.low_spec;
        let m = ui::Metrics::for_screen(screen, self.handheld);
        self.covers.set_policy(policy);
        if self.input_mode != InputMode::Touch
            && (self.owned.get().is_some() || self.login.is_some())
        {
            let hints = self.hints();
            ui::footer(
                ui,
                &m,
                &self.glyphs,
                self.input_mode,
                &hints,
                self.prompt.is_some()
                    || self.menu.is_some()
                    || self.qr_shown()
                    || self.report.is_some(),
                self.menu.is_some(),
            );
        }
        let page = egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui::BG).inner_margin(egui::Margin {
                left: m.margin as i8,
                right: m.margin as i8,
                top: m.top as i8,
                bottom: m.frame(6.0) as i8,
            }))
            .show(ui, |ui| {
                // A row of the strip's height from the start, so the logo
                // and the right-hand text center on the tabs' line.
                let row = egui::vec2(ui.available_width(), ui::tab_strip_height(ui, &m));
                let centered = egui::Layout::left_to_right(egui::Align::Center);
                ui.allocate_ui_with_layout(row, centered, |ui| {
                    let home = ui::logo(ui, &m, &self.glyphs, !self.intro.running());
                    self.intro.set_home(home);
                    if self.login.is_some() {
                        ui.allocate_exact_size(
                            egui::vec2(0.0, ui::tab_strip_height(ui, &m)),
                            egui::Sense::hover(),
                        );
                    } else if self.page.is_library() {
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
                        let has_url = match self.page {
                            Page::Game { id, .. } => {
                                self.game(id).is_some_and(|g| !g.url.is_empty())
                            }
                            Page::Library | Page::PlayableTypes { .. } | Page::Settings { .. } => {
                                false
                            }
                        };
                        if has_url && ui::qr_button(ui, &m).clicked() {
                            self.actions.push(Action::ShowQr);
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
                        if let Some(reading) = self.battery.reading() {
                            ui::battery(ui, &m, reading);
                        }
                        if let Some(user) = self.profile.as_ref().and_then(|p| p.user.as_ref()) {
                            // Elide rather than wrap: on a 640-wide screen a
                            // long display name meets the tab strip.
                            ui::subtle_truncated(ui, &m, user.name());
                        }
                        if !self.online {
                            ui::offline(ui, &m);
                        } else if self.syncing {
                            ui::syncing(ui, &m);
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
                if let Some(login) = &self.login {
                    ui::login(ui, &m, login, &mut self.actions);
                    return;
                }
                match (&self.owned, self.page) {
                    // These pages stand on their own, whatever the library's state.
                    (_, Page::PlayableTypes { row }) => ui::playable_types(
                        ui,
                        &m,
                        crate::model::playable_types(),
                        &self.playable_hidden,
                        row,
                        &mut self.actions,
                    ),
                    (_, Page::Settings { row }) => {
                        let rows = self.settings_rows();
                        let about = self.about_rows();
                        ui::settings(ui, &m, &rows, &about, row, &mut self.actions)
                    }
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
                                marks: self.cover_marks.then_some(&self.marks),
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
                                            marks: self.cover_marks.then_some(&self.marks),
                                            covers: &self.covers,
                                            scrollbar: self.input_mode == InputMode::Keyboard,
                                            focused: self.toolbar_focus[tab_slot(Tab::Collections)]
                                                .is_none(),
                                        },
                                        &mut self.collection_rows,
                                        &mut self.actions,
                                    );
                                });
                                self.want_visible_collections();
                            }
                            _ => ui::centered_spinner(ui, &m),
                        },
                        Tab::Downloads => {
                            let rows = self.download_rows();
                            let (row, button) = self.downloads_row_in(&rows);
                            let (controls, stops) = self.toolbar();
                            let focus = match self.toolbar_focus_in(stops.len(), rows.is_empty()) {
                                Some(index) => ui::DownloadFocus::Toolbar(index),
                                None => ui::DownloadFocus::Row { row, button },
                            };
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
                    (Loadable::Loaded(_), Page::Game { id, button, shot }) => {
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
                                let (info, info_loading) = match self.page_info.get(
                                    ui.ctx(),
                                    game.id,
                                    &game.url,
                                    self.online,
                                ) {
                                    Lookup::Ready(info) => (Some(info), false),
                                    Lookup::Loading => (None, true),
                                    Lookup::Missing => (None, false),
                                };
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
                                        reported: reported(&self.reports, &self.caves, game.id),
                                        info: info.as_deref(),
                                        info_loading,
                                        focused_shot: shot,
                                        scroll: self.detail_scroll.take(),
                                        visit: self.detail_visit,
                                    },
                                    &mut self.page_scroll,
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
        if self.qr_shown()
            && let Some(qr) = &self.qr
        {
            ui::qr(ui.ctx(), &m, ui.max_rect(), page, qr, &mut self.actions);
        }
        if let Some(view) = &self.report {
            ui::report(ui.ctx(), &m, ui.max_rect(), page, view, &mut self.actions);
        }
        if let Some(view) = &self.viewer
            && matches!(self.page, Page::Game { .. })
        {
            ui::screenshot_viewer(
                ui.ctx(),
                &m,
                ui.max_rect(),
                page,
                view,
                &self.covers,
                &mut self.actions,
            );
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
            self.butler_version.as_deref(),
            &mut self.actions,
        );
        if let Some((_, title)) = self.handed_off() {
            ui::curtain(
                ui.ctx(),
                &m,
                ui.max_rect(),
                &format!("Launching {title}\u{2026}"),
            );
        }
        self.intro.draw(ui.ctx(), ui.max_rect(), self.glyphs.logo());
        if let Some(frames) = self.quitting {
            let black = self.power_off.draw(ui.ctx(), ui.max_rect());
            if black && frames == 0 {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                if black {
                    self.quitting = Some(frames - 1);
                }
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
/// Where an install the user asked for stands before butler lists it in
/// the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// Asked; butler has said nothing yet.
    Starting,
    /// butler is asking which upload.
    Choosing,
    /// An upload is picked; the queue listing follows.
    Picked,
}

struct ToolbarStop {
    /// What the footer says Confirm does.
    hint: &'static str,
    /// Its work is under way; pressing it does nothing.
    busy: bool,
    action: Action,
}

impl ToolbarStop {
    fn new(hint: &'static str, action: Action) -> Self {
        Self {
            hint,
            busy: false,
            action,
        }
    }
}

/// The player's report on a game, and whether it was of a build
/// installed now. A game not installed has nothing newer to compare.
fn reported<'a>(
    reports: &'a Reports,
    caves: &[Cave],
    game_id: i64,
) -> Option<(&'a SavedReport, bool)> {
    let saved = reports.get(&game_id)?;
    let mut installed = caves
        .iter()
        .filter(|c| c.game_id() == Some(game_id))
        .peekable();
    let current = installed.peek().is_none()
        || installed.any(|cave| {
            cave.upload.as_ref().map(|u| u.id) == Some(saved.upload_id)
                && cave.build.as_ref().map(|b| b.id) == saved.build_id
        });
    Some((saved, current))
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
        (Some(index), Direction::Up | Direction::PageUp | Direction::Top) => {
            Landing::Toolbar(index)
        }
        (Some(index), Direction::Down | Direction::PageDown | Direction::Bottom) if rows_empty => {
            Landing::Toolbar(index)
        }
        (Some(_), Direction::Down | Direction::PageDown | Direction::Bottom) => Landing::FirstRow,
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
        Direction::Up | Direction::Down => wrap_step(row, rows.len(), direction),
        // The list is short; a page reaches its end.
        Direction::Home | Direction::PageUp | Direction::Top => 0,
        Direction::End | Direction::PageDown | Direction::Bottom => last,
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
        assert_eq!(step((1, 1), Direction::Down), (0, 1));
        assert_eq!(step((1, 0), Direction::Up), (0, 0));
        assert_eq!(step((0, 0), Direction::Up), (1, 0));
        assert_eq!(step((1, 0), Direction::Home), (0, 0));
    }
}

/// Frees a replaced list on another thread; thousands of games take a few
/// frames to free on a handheld.
fn drop_later<T: Send + 'static>(value: T) {
    let _ = std::thread::Builder::new()
        .name("drop".into())
        .spawn(move || drop(value));
}

/// Which rows need rebuilding once the current batch of events is done.
#[derive(Default)]
struct Rebuilds {
    sections: bool,
    collections: bool,
}

/// butler's answers for one setting of the page-wide filters.
struct Filtered {
    filter: CollectionFilter,
    /// Every game in each collection that passes, by collection id;
    /// missing while unknown.
    games: std::collections::HashMap<i64, Vec<Arc<Game>>>,
    /// The latest question asked about each collection.
    asked: std::collections::HashMap<i64, u64>,
    /// Collections whose query failed, asked again after the next sync.
    failed: std::collections::HashSet<i64>,
}

/// Where a catalog entry came from. A later variant's copy of a game
/// replaces an earlier one's, never the reverse: the owned list's is the
/// profile's own view of the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Collection,
    Install,
    Owned,
}

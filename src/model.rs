//! Types shared between the backend and the interface. Wire types come from
//! the generated butlerd bindings; these are the app's own.

pub use crate::butlerd::types::{
    Cave, Collection, Download, DownloadProgress, Game, GameClassification, GameUpdate, Profile,
    Upload, User,
};

pub trait UserExt {
    /// The display name, or the username when none is set.
    fn name(&self) -> &str;
}

impl UserExt for User {
    fn name(&self) -> &str {
        if self.display_name.is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}

pub trait UploadExt {
    fn name(&self) -> &str;
}

impl UploadExt for Upload {
    fn name(&self) -> &str {
        if !self.display_name.is_empty() {
            &self.display_name
        } else if !self.filename.is_empty() {
            &self.filename
        } else {
            "upload"
        }
    }
}

pub trait CaveExt {
    fn game_id(&self) -> Option<i64>;
}

impl CaveExt for Cave {
    fn game_id(&self) -> Option<i64> {
        self.game.as_ref().map(|game| game.id)
    }
}

/// A queued or running download for a game, as the interface sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstallState {
    /// butlerd's download id; empty until the queue has answered.
    pub download_id: String,
    /// 0 to 1.
    pub progress: f64,
    pub bps: f64,
    pub eta_seconds: f64,
    /// What butler is doing right now: downloading, installing, and so on.
    pub stage: String,
    pub cancelling: bool,
    /// Set when the download stopped with an error; Retry or Dismiss apply.
    pub error: Option<String>,
}

/// A question the backend needs answered before a call can go on, shown as
/// a modal. The backend maps the chosen index back to the typed reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub choices: Vec<String>,
    pub focus: usize,
}

/// Which part of the library the main row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    /// Has an upload for this operating system.
    Playable,
    Games,
    /// Tools and asset packs.
    Tools,
    /// Soundtracks, books, comics, mods, physical games, and the rest.
    Other,
}

impl Filter {
    pub const ALL: [Filter; 5] = [
        Filter::All,
        Filter::Playable,
        Filter::Games,
        Filter::Tools,
        Filter::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Playable => "Playable here",
            Filter::Games => "Games",
            Filter::Tools => "Tools & assets",
            Filter::Other => "Other",
        }
    }

    pub fn matches(self, game: &Game) -> bool {
        match self {
            Filter::All => true,
            Filter::Playable => {
                let p = &game.platforms;
                if cfg!(target_os = "linux") {
                    p.linux.is_some()
                } else if cfg!(target_os = "macos") {
                    p.osx.is_some()
                } else {
                    p.windows.is_some()
                }
            }
            Filter::Games => game.classification == GameClassification::Game,
            Filter::Tools => matches!(
                game.classification,
                GameClassification::Tool | GameClassification::Assets
            ),
            Filter::Other => !matches!(
                game.classification,
                GameClassification::Game | GameClassification::Tool | GameClassification::Assets
            ),
        }
    }

    pub fn next(self, step: i32) -> Filter {
        let len = Self::ALL.len() as i32;
        let index = Self::ALL.iter().position(|f| *f == self).unwrap_or(0) as i32;
        Self::ALL[((index + step).rem_euclid(len)) as usize]
    }
}

/// What the window is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Page {
    Library,
    /// One game, with one of its buttons focused.
    Game {
        id: i64,
        button: usize,
    },
}

/// A collection with the games butler has for it.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionGames {
    pub collection: Collection,
    pub games: Vec<Game>,
}

/// The top-level screens, switched with the bumpers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Tab {
    #[default]
    Library,
    Collections,
    Downloads,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Library, Tab::Collections, Tab::Downloads];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Library => "Library",
            Tab::Collections => "Collections",
            Tab::Downloads => "Downloads",
        }
    }

    pub fn next(self, step: i32) -> Tab {
        let len = Self::ALL.len() as i32;
        let index = Self::ALL.iter().position(|t| *t == self).unwrap_or(0) as i32;
        Self::ALL[((index + step).rem_euclid(len)) as usize]
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum Loadable<T> {
    #[default]
    NotLoaded,
    Loading,
    Loaded(T),
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn get(&self) -> Option<&T> {
        match self {
            Loadable::Loaded(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// What the interface asked for while drawing. Applied after the frame so
/// views never mutate state they are reading.
#[derive(Debug, Clone)]
pub enum Action {
    MoveFocus(Direction),
    /// Focus the nth owned game, scrolling to it; for scripted screenshots.
    FocusIndex(usize),
    /// Focus a tile without scrolling to it; the pointer is already there.
    FocusTile {
        row: usize,
        col: usize,
    },
    /// Focus a detail-page button; the pointer is already there.
    FocusButton(usize),
    Activate,
    Back,
    Open(Page),
    Play {
        cave_id: String,
    },
    /// Answer the open prompt with a choice, or dismiss it with `None`.
    Answer {
        prompt: u64,
        choice: Option<usize>,
    },
    /// Focus a prompt button; the pointer is already there.
    PromptFocus(usize),
    SetFilter(Filter),
    /// Step through the filters, wrapping.
    CycleFilter(i32),
    SetTab(Tab),
    /// Step through the tabs, wrapping.
    CycleTab(i32),
    /// Put the cursor in the search box.
    FocusSearch,
    /// Leave the search box, keeping its text; focus goes to the results.
    SearchDone,
    ClearSearch,
    Install {
        game_id: i64,
    },
    /// Discard the game's download, whether running or failed.
    CancelInstall {
        game_id: i64,
    },
    RetryInstall {
        game_id: i64,
    },
    /// Queue the update butler found for this cave.
    Update {
        cave_id: String,
    },
    Uninstall {
        cave_id: String,
    },
}

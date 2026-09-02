//! Types shared between the backend and the interface. Wire types come from
//! the generated butlerd bindings; these are the app's own.

pub use crate::butlerd::types::{
    Cave, Download, DownloadProgress, Game, GameUpdate, Profile, Upload, User,
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

/// What the window is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Page {
    Library,
    /// One game from the library, with one of its buttons focused.
    Game {
        index: usize,
        button: usize,
    },
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
    /// Focus a game by library index, scrolling to it.
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

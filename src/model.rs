//! Types shared between the backend and the interface.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: i64,
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
}

impl User {
    pub fn name(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.username)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: i64,
    pub user: User,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub short_text: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub still_cover_url: Option<String>,
    #[serde(default)]
    pub classification: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadKey {
    pub id: i64,
    pub game: Game,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Upload {
    pub id: i64,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub size: i64,
}

impl Upload {
    pub fn name(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or(self.filename.as_deref())
            .unwrap_or("upload")
    }
}

/// One installed copy of a game, in butlerd's terms.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cave {
    pub id: String,
    pub game: Game,
    #[serde(default)]
    pub upload: Option<Upload>,
    #[serde(default)]
    pub stats: Option<CaveStats>,
    #[serde(default)]
    pub install_info: Option<CaveInstallInfo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaveStats {
    #[serde(default)]
    pub installed_at: Option<String>,
    #[serde(default)]
    pub last_touched_at: Option<String>,
    #[serde(default)]
    pub seconds_run: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaveInstallInfo {
    #[serde(default)]
    pub installed_size: i64,
    #[serde(default)]
    pub install_folder: String,
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
    /// Focus a tile without scrolling to it; the pointer is already there.
    FocusIndex(usize),
    /// Focus a detail-page button; the pointer is already there.
    FocusButton(usize),
    Activate,
    Back,
    Open(Page),
    Play {
        cave_id: String,
    },
    Install {
        game_id: i64,
    },
    Uninstall {
        cave_id: String,
    },
}

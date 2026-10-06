//! What the interface can be asked to do, and where it can be: the
//! actions every input source produces, the pages, the tabs, and
//! directions of movement.

use crate::model::RatingFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Library,
    /// One game, with one of its buttons focused, or one of its
    /// screenshots while `shot` is set. The button stays for coming back
    /// up from the screenshots.
    Game {
        id: i64,
        button: usize,
        shot: Option<usize>,
    },
    /// The types "Playable here" includes, with a row focused: 0 is
    /// Everything, then the types in [`playable_types`] order.
    PlayableTypes {
        row: usize,
    },
    /// The Settings page, with a row focused.
    Settings {
        row: usize,
    },
}

impl Page {
    pub fn is_library(&self) -> bool {
        matches!(self, Page::Library)
    }

    /// A game's page with its first button focused.
    pub fn game(id: i64) -> Self {
        Page::Game {
            id,
            button: 0,
            shot: None,
        }
    }
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

/// One step through a list of `len` rows that wraps at both ends: up or
/// left from the first row lands on the last, down or right from the
/// last on the first. Other directions leave `index` as it is.
pub fn wrap_step(index: usize, len: usize, direction: Direction) -> usize {
    if len == 0 {
        return 0;
    }
    match direction {
        Direction::Up | Direction::Left => (index + len - 1) % len,
        Direction::Down | Direction::Right => (index + 1) % len,
        _ => index,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
    /// To the first item in the row.
    Home,
    /// To the last item in the row.
    End,
    /// A screenful of rows up.
    PageUp,
    /// A screenful of rows down.
    PageDown,
    /// To the first row.
    Top,
    /// To the last row.
    Bottom,
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
    /// The row is near its end and has more games to fetch.
    MoreGames {
        row: usize,
    },
    /// Guide, Start or Escape: bring the window up over a running game
    /// and open or close the menu drawer.
    Menu,
    /// Focus a menu drawer item; the pointer is already there.
    MenuFocus(usize),
    /// Close the window; the hosts shut the backend down on the way out.
    Quit,
    /// Focus a detail-page button; the pointer is already there.
    FocusButton(usize),
    /// Focus a download row's button; the pointer is already there.
    FocusDownload {
        row: usize,
        button: usize,
    },
    /// Focus a button on the tab's toolbar; the pointer is already there.
    FocusToolbar(usize),
    Activate,
    Back,
    Open(Page),
    Play {
        cave_id: String,
    },
    /// Kill the running game.
    QuitGame {
        cave_id: String,
    },
    /// Get the window out of the running game's way.
    BackToGame,
    /// Answer the open prompt with a choice, or dismiss it with `None`.
    Answer {
        prompt: u64,
        choice: Option<usize>,
    },
    /// Focus a prompt button; the pointer is already there.
    PromptFocus(usize),
    /// Focus a row of the compatibility report; the pointer is already
    /// there.
    ReportFocus(usize),
    /// Ask how the game in this cave runs.
    Report {
        cave_id: String,
    },
    /// List what else there is to do with the game in this cave.
    GameOptions {
        cave_id: String,
    },
    /// Hide games with no upload for this device, on every tab.
    SetPlayableOnly(bool),
    /// Include or leave out one type in "Playable here".
    TogglePlayableType(String),
    /// Leave out every type but this one.
    OnlyPlayableType(String),
    /// Include every type again.
    AllPlayableTypes,
    /// Focus a row on the Playable types page; the pointer is already there.
    FocusTypeRow(usize),
    /// Show or hide the rating and tried marks on covers.
    SetCoverMarks(bool),
    /// The toolbar's rating pill: ask which of the player's ratings to show.
    RatingFilterMenu,
    SetRatingFilter(RatingFilter),
    /// Focus a row on the Settings page; the pointer is already there.
    FocusSettingsRow(usize),
    SetTab(Tab),
    /// Narrow the Collections tab to installed games, or show everything.
    SetCollectionsInstalledOnly(bool),
    /// The sign-in page's checkbox: report what device this is.
    SetShareDeviceInfo(bool),
    /// Step through the tabs, wrapping; on a game's page, step to the game
    /// beside it in the row it was opened from.
    CycleTab(i32),
    /// Y on a pad: in and out of the filters on the library, the QR code
    /// on a game's page.
    Secondary,
    /// Slash on a keyboard: the search box on the library, the QR code
    /// on a game's page.
    Search,
    /// Show the open game's page as a QR code.
    ShowQr,
    /// Close the QR code.
    HideQr,
    /// Open the game page's screenshot at this index full screen.
    ViewScreenshot(usize),
    /// Close the full screen screenshot.
    CloseScreenshot,
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
    /// Drop finished downloads from the Downloads tab, as butler keeps
    /// them listed until asked.
    ClearFinished,
    /// Queue the update butler found for this cave.
    Update {
        cave_id: String,
    },
    /// Queue every direct update butler found.
    UpdateAll,
    /// Ask butler for updates now.
    CheckUpdates,
    /// Refetch the owned list and collections from itch.io.
    RefreshLibrary,
    /// Forget the signed-in profile and show the sign-in page.
    ChangeUser,
    /// Open the dialog for updating zitch itself, checking for a release.
    SelfUpdate,
    Uninstall {
        cave_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_wrap_at_both_ends() {
        assert_eq!(wrap_step(0, 3, Direction::Up), 2);
        assert_eq!(wrap_step(2, 3, Direction::Down), 0);
        assert_eq!(wrap_step(1, 3, Direction::Down), 2);
        assert_eq!(wrap_step(0, 1, Direction::Left), 0);
        assert_eq!(wrap_step(1, 3, Direction::Home), 1);
        assert_eq!(wrap_step(4, 0, Direction::Down), 0);
    }
}

//! Types shared between the backend and the interface. Wire types come from
//! the generated butlerd bindings; these are the app's own.

pub use crate::butlerd::types::{
    Cave, Collection, Download, DownloadProgress, DownloadReason, Game, GameClassification,
    GameUpdate, Profile, Upload, User,
};
use crate::playable::UploadDetail;
use crate::report::Rating;

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

/// "1.2 GB" style sizes.
pub fn human_size(bytes: i64) -> String {
    let bytes = bytes.max(0) as f64;
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
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

/// Why a launch did not run, with the tail of what the game printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchFailure {
    pub message: String,
    pub log: Vec<String>,
}

impl Game {
    /// The animated cover, when the game has one distinct from its still.
    pub fn animated_cover(&self) -> Option<&str> {
        let cover = self.cover_url.as_deref()?;
        let is_gif = cover
            .rsplit('.')
            .next()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gif"));
        (is_gif && self.still_cover_url.as_deref() != Some(cover)).then_some(cover)
    }
}

/// Short remaining-time text for progress lines.
pub fn human_duration_seconds(seconds: i64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

/// Seconds since the Unix epoch for an RFC 3339 timestamp as butler
/// writes them: `2026-09-03T19:06:45.123Z` or with a `+hh:mm` offset.
pub fn rfc3339_to_unix(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, rest) = text.split_at_checked(10)?;
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    let rest = rest.strip_prefix(['T', 't', ' '])?;
    let (time, zone) = match rest.find(['Z', 'z', '+', '-']) {
        Some(at) => rest.split_at(at),
        None => (rest, "Z"),
    };
    let time = time.split('.').next()?;
    let mut parts = time.split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next().unwrap_or("0").parse().ok()?;
    let offset = match zone {
        "Z" | "z" => 0,
        _ => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let mut parts = zone[1..].split(':');
            let hours: i64 = parts.next()?.parse().ok()?;
            let minutes: i64 = parts.next().unwrap_or("0").parse().ok()?;
            sign * (hours * 3600 + minutes * 60)
        }
    };
    // Days from civil, Howard Hinnant's algorithm.
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hour * 3600 + minute * 60 + second - offset)
}

/// "just now", "5 min ago", "3h ago", "2 days ago".
pub fn human_time_ago(unix: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let seconds = (now - unix).max(0);
    if seconds < 60 {
        "just now".to_string()
    } else if seconds < 3600 {
        format!("{} min ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h ago", seconds / 3600)
    } else if seconds < 2 * 86400 {
        "yesterday".to_string()
    } else {
        format!("{} days ago", seconds / 86400)
    }
}

/// "5 min", "1h 20m": play time.
pub fn human_duration(seconds: i64) -> String {
    let minutes = seconds / 60;
    if minutes < 60 {
        format!("{minutes} min")
    } else {
        format!("{}h {:02}m", minutes / 60, minutes % 60)
    }
}

/// How a launch ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launched {
    /// The game ran and has exited.
    Ran,
    /// The user backed out before it started.
    Cancelled,
    Failed(LaunchFailure),
}

/// A question the backend needs answered before a call can go on, shown as
/// a modal. The backend maps the chosen index back to the typed reply.
/// Who asked a question, which decides who gets its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptOrigin {
    /// The backend, which gets the answer back by the prompt's id.
    Backend,
    /// The backend asking which of a game's uploads to install. The first
    /// `picks` choices are uploads; the rest show more or cancel.
    UploadPicker {
        game_id: i64,
        picks: usize,
    },
    /// The app's own list of actions, such as a game's More menu.
    Options,
    SelfUpdate,
    /// The screenshot script's stand-in, answered by closing it.
    Sample,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    /// The backend counts its ids up from 1; the app's own prompts are 0.
    pub id: u64,
    pub origin: PromptOrigin,
    pub title: String,
    pub body: String,
    pub choices: Vec<String>,
    pub focus: usize,
    /// The choice drawn as the primary button: the expected or safe answer
    /// to a question. A pick from a list of equals has none.
    pub primary: Option<usize>,
    /// The choices stand in a column stepped with Up and Down, rather
    /// than a row stepped with Left and Right.
    pub stacked: bool,
    /// Detail drawn under the choice at the same index; choices past its
    /// end have none.
    pub details: Vec<UploadDetail>,
    /// Work under way: a status line and how far along it is, 0 to 1.
    pub progress: Option<(String, f32)>,
}

/// What kind of thing a library entry is, one row each on the Library tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Games,
    /// Tools and asset packs.
    Tools,
    /// Soundtracks, books, comics, mods, physical games, and the rest.
    Other,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Games, Kind::Tools, Kind::Other];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Games => "Games",
            Kind::Tools => "Tools & assets",
            Kind::Other => "Other",
        }
    }

    pub fn matches(self, game: &Game) -> bool {
        match self {
            Kind::Games => game.classification == GameClassification::Game,
            Kind::Tools => matches!(
                game.classification,
                GameClassification::Tool | GameClassification::Assets
            ),
            Kind::Other => !matches!(
                game.classification,
                GameClassification::Game | GameClassification::Tool | GameClassification::Assets
            ),
        }
    }
}

/// A collection with the games butler has for it.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionGames {
    pub collection: Collection,
    /// The games fetched so far, in collection order.
    pub games: Vec<std::sync::Arc<Game>>,
    /// Where the next page starts, while there is one.
    pub next_cursor: Option<String>,
    /// A fresh copy of the games is on its way.
    pub refreshing: bool,
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

    pub fn get_mut(&mut self) -> Option<&mut T> {
        match self {
            Loadable::Loaded(value) => Some(value),
            _ => None,
        }
    }
}

/// A cover's corner mark: how the player rated the game, or that they
/// tried it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// The rating, and whether it was of the build installed now.
    Rated(Rating, bool),
    Tried,
}

/// The toolbar's filter on the player's own ratings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RatingFilter {
    #[default]
    Any,
    Runs,
    HasIssues,
    WontRun,
    /// Tried, with no report yet.
    Unrated,
    Untried,
}

impl RatingFilter {
    pub const ALL: [RatingFilter; 6] = [
        RatingFilter::Any,
        RatingFilter::Runs,
        RatingFilter::HasIssues,
        RatingFilter::WontRun,
        RatingFilter::Unrated,
        RatingFilter::Untried,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RatingFilter::Any => "Any rating",
            RatingFilter::Runs => "Runs",
            RatingFilter::HasIssues => "Has issues",
            RatingFilter::WontRun => "Doesn't run",
            RatingFilter::Unrated => "Not rated yet",
            RatingFilter::Untried => "Not tried",
        }
    }

    /// Whether a game with this mark passes. A rating counts whatever
    /// build it was for.
    pub fn matches(self, mark: Option<Mark>) -> bool {
        matches!(
            (self, mark),
            (RatingFilter::Any, _)
                | (
                    RatingFilter::Runs,
                    Some(Mark::Rated(Rating::Perfect | Rating::Playable, _))
                )
                | (
                    RatingFilter::HasIssues,
                    Some(Mark::Rated(Rating::MajorIssues, _))
                )
                | (RatingFilter::WontRun, Some(Mark::Rated(Rating::WontRun, _)))
                | (RatingFilter::Unrated, Some(Mark::Tried))
                | (RatingFilter::Untried, None)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_butler_timestamps() {
        assert_eq!(rfc3339_to_unix("2026-09-03T19:06:45Z"), Some(1788462405));
        assert_eq!(
            rfc3339_to_unix("2026-09-03T19:06:45.123456789Z"),
            Some(1788462405)
        );
        assert_eq!(
            rfc3339_to_unix("2026-09-03T12:06:45-07:00"),
            Some(1788462405)
        );
        assert_eq!(rfc3339_to_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_to_unix("nope"), None);
    }

    #[test]
    fn rating_filters_match_marks() {
        let rated = |r| Some(Mark::Rated(r, false));
        assert!(RatingFilter::Any.matches(None));
        assert!(RatingFilter::Runs.matches(rated(Rating::Playable)));
        assert!(!RatingFilter::Runs.matches(rated(Rating::MajorIssues)));
        assert!(RatingFilter::HasIssues.matches(rated(Rating::MajorIssues)));
        assert!(RatingFilter::WontRun.matches(rated(Rating::WontRun)));
        assert!(RatingFilter::Unrated.matches(Some(Mark::Tried)));
        assert!(!RatingFilter::Unrated.matches(None));
        assert!(RatingFilter::Untried.matches(None));
        assert!(!RatingFilter::Untried.matches(Some(Mark::Tried)));
    }
}

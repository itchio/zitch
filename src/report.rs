//! Compatibility reports: from a game's page, the player says how the
//! game runs here, and zitch sends that to itch.io with what the device is
//! and what it saw of the game's last run.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// How the game ran, in the player's words. Saved as its [`Rating::id`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rating {
    Perfect,
    Playable,
    MajorIssues,
    WontRun,
}

impl Rating {
    pub const ALL: [Rating; 4] = [
        Rating::Perfect,
        Rating::Playable,
        Rating::MajorIssues,
        Rating::WontRun,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Rating::Perfect => "perfect",
            Rating::Playable => "playable",
            Rating::MajorIssues => "major_issues",
            Rating::WontRun => "wont_run",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Rating::Perfect => "Runs great",
            Rating::Playable => "Playable with issues",
            Rating::MajorIssues => "Barely playable",
            Rating::WontRun => "Doesn't run",
        }
    }

    /// Whether the player picks what went wrong. A game that doesn't run
    /// is explained by the run itself.
    pub fn asks_flags(self) -> bool {
        matches!(self, Rating::Playable | Rating::MajorIssues)
    }
}

pub struct Flag {
    pub id: &'static str,
    pub label: &'static str,
}

pub struct FlagGroup {
    pub label: &'static str,
    pub flags: &'static [Flag],
}

/// Must match the site's `GameCompatibilityReports.flag_names`.
pub const FLAG_GROUPS: &[FlagGroup] = &[
    FlagGroup {
        label: "Display",
        flags: &[
            Flag {
                id: "cut_off",
                label: "Doesn't fit the screen",
            },
            Flag {
                id: "stretched",
                label: "Stretched or blurry",
            },
            Flag {
                id: "text_too_small",
                label: "Text too small to read",
            },
        ],
    },
    FlagGroup {
        label: "Controls",
        flags: &[
            Flag {
                id: "no_gamepad",
                label: "Buttons do nothing",
            },
            Flag {
                id: "wrong_mapping",
                label: "Buttons mapped wrong",
            },
            Flag {
                id: "needs_keyboard_mouse",
                label: "Needs keyboard or mouse",
            },
            Flag {
                id: "needs_text_input",
                label: "Asks for typed text",
            },
            Flag {
                id: "cant_exit",
                label: "No way to quit",
            },
        ],
    },
    FlagGroup {
        label: "Stability",
        flags: &[
            Flag {
                id: "slow",
                label: "Slow or long loading",
            },
            Flag {
                id: "crashes",
                label: "Crashes while playing",
            },
            Flag {
                id: "saves",
                label: "Progress not saved",
            },
        ],
    },
    FlagGroup {
        label: "Other",
        flags: &[
            Flag {
                id: "audio",
                label: "Sound problems",
            },
            Flag {
                id: "needs_network",
                label: "Needs an internet connection",
            },
        ],
    },
];

/// Every flag in screen order, with the index of its group.
pub fn flags() -> impl Iterator<Item = (usize, &'static Flag)> {
    FLAG_GROUPS
        .iter()
        .enumerate()
        .flat_map(|(group, g)| g.flags.iter().map(move |flag| (group, flag)))
}

/// How much of the game's error output a report carries.
pub const STDERR_TAIL: usize = 4096;
const LAUNCH_TARGET_MAX: usize = 512;

/// What ran the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    /// butler ran the game's own build.
    Native,
    /// Our LÖVE runtime.
    Love,
    /// The firmware's RetroArch.
    Retroarch,
}

/// What zitch saw of one run, without asking the player. Unknown fields
/// stay out of the report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Run {
    pub strategy: Option<Strategy>,
    /// The RetroArch core.
    pub core: Option<String>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// From the game starting to its exit.
    pub seconds: Option<u64>,
    pub stderr_tail: String,
    /// What was launched, e.g. `love:11.5 game.love`.
    pub launch_target: Option<String>,
}

impl Run {
    fn to_json(&self) -> Value {
        let mut run = Map::new();
        if let Some(strategy) = &self.strategy {
            run.insert("strategy".into(), json!(strategy));
        }
        if let Some(core) = &self.core {
            run.insert("core".into(), json!(core));
        }
        if let Some(code) = self.exit_code {
            run.insert("exit_code".into(), json!(code));
        }
        if let Some(signal) = self.signal {
            run.insert("signal".into(), json!(signal));
        }
        if let Some(seconds) = self.seconds {
            run.insert("seconds_to_exit".into(), json!(seconds));
        }
        if !self.stderr_tail.is_empty() {
            run.insert(
                "stderr_tail".into(),
                json!(tail(&self.stderr_tail, STDERR_TAIL)),
            );
        }
        Value::Object(run)
    }
}

/// A cave's last run, with what was installed when it ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRun {
    pub upload_id: i64,
    pub build_id: Option<i64>,
    pub run: Run,
}

pub type Runs = HashMap<String, SavedRun>;

/// What the player said about a game on this device, kept after sending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedReport {
    pub upload_id: i64,
    pub build_id: Option<i64>,
    pub rating: Rating,
    pub flags: Vec<String>,
    /// Unix seconds.
    pub sent_at: i64,
    /// Reached itch.io. Files from before this field was kept hold reports
    /// that were sent.
    #[serde(default = "yes")]
    pub sent: bool,
    /// The run to send with it, kept until it is sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<Run>,
}

fn yes() -> bool {
    true
}

impl SavedReport {
    /// The report to send again for `game_id`, while it is unsent.
    pub fn unsent(&self, game_id: i64) -> Option<Report> {
        if self.sent {
            return None;
        }
        Some(Report {
            game_id,
            upload_id: self.upload_id,
            build_id: self.build_id,
            rating: self.rating,
            flags: self
                .flags
                .iter()
                .filter_map(|id| flag(id))
                .map(|f| f.id)
                .collect(),
            run: self.run.clone().unwrap_or_default(),
        })
    }
}

/// Saved reports by game id.
pub type Reports = HashMap<i64, SavedReport>;

/// A flag's id as the static one in [`FLAG_GROUPS`], if it is one.
pub fn flag(id: &str) -> Option<&'static Flag> {
    flags().map(|(_, f)| f).find(|f| f.id == id)
}

/// A JSON file's contents, or the default when it is missing or
/// unreadable.
pub fn load<T: DeserializeOwned + Default>(path: &Path) -> T {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
            log::warn!("reading {}: {error}", path.display());
            T::default()
        }),
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("reading {}: {error}", path.display());
            }
            T::default()
        }
    }
}

/// Writes beside, then renames, like the settings.
pub fn save<T: Serialize>(path: &Path, value: &T) {
    let result = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string(value).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.part");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(error) = result {
        log::warn!("saving {}: {error}", path.display());
    }
}

/// The last `max` bytes of `text`, cut at a character boundary.
pub fn tail(text: &str, max: usize) -> &str {
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn head(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A report ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub game_id: i64,
    pub upload_id: i64,
    pub build_id: Option<i64>,
    pub rating: Rating,
    pub flags: Vec<&'static str>,
    pub run: Run,
}

impl Report {
    /// The form fields, in the order the API documents them.
    fn form(&self, device_info: String) -> Vec<(&'static str, String)> {
        let mut form = vec![("upload_id", self.upload_id.to_string())];
        if let Some(build_id) = self.build_id {
            form.push(("build_id", build_id.to_string()));
        }
        form.push(("rating", self.rating.id().to_string()));
        if !self.flags.is_empty() {
            form.push(("flags", json!(self.flags).to_string()));
        }
        form.push(("device_info", device_info));
        form.push(("run", self.run.to_json().to_string()));
        if let Some(target) = &self.run.launch_target {
            form.push(("launch_target", head(target, LAUNCH_TARGET_MAX).to_string()));
        }
        if let Some(seconds) = self.run.seconds {
            form.push(("seconds_played", seconds.to_string()));
        }
        form
    }
}

/// The signed-in profile's API key, as butler saved it. Read at send time
/// since signing in again replaces it.
pub fn saved_api_key(dbpath: &Path, profile_id: i64) -> Result<String> {
    let db = rusqlite::Connection::open_with_flags(
        dbpath,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", dbpath.display()))?;
    db.query_row(
        "select api_key from profiles where id = ?",
        [profile_id],
        |row| row.get(0),
    )
    .with_context(|| format!("no saved key for profile {profile_id}"))
}

/// Posts the report. Any failure is the caller's to log; nothing retries.
pub fn send(api_url: &str, api_key: &str, report: &Report) -> Result<()> {
    let url = format!("{api_url}/games/{}/compatibility-reports", report.game_id);
    let form = report.form(crate::device_info::for_report());
    let mut response = ureq::post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("User-Agent", concat!("zitch/", env!("ZITCH_VERSION")))
        .config()
        .http_status_as_error(false)
        .build()
        .send_form(form)
        .with_context(|| format!("posting to {url}"))?;
    let status = response.status();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    if !status.is_success() {
        bail!("{status}: {}", head(body.trim(), 500));
    }
    log::info!("compatibility report sent: {}", body.trim());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsent_reports_come_back_as_reports() {
        let saved = SavedReport {
            upload_id: 7,
            build_id: Some(9),
            rating: Rating::Playable,
            flags: vec!["cut_off".into(), "made_up".into()],
            sent_at: 0,
            sent: false,
            run: None,
        };
        let again = saved.unsent(3).unwrap();
        assert_eq!(
            (again.game_id, again.upload_id, again.build_id),
            (3, 7, Some(9))
        );
        assert_eq!(again.flags, vec!["cut_off"]);
        let sent = SavedReport {
            sent: true,
            ..saved
        };
        assert!(sent.unsent(3).is_none());
    }

    #[test]
    fn reports_saved_before_the_sent_flag_count_as_sent() {
        let parsed: SavedReport = serde_json::from_str(
            r#"{"upload_id":1,"build_id":null,"rating":"perfect","flags":[],"sent_at":5}"#,
        )
        .unwrap();
        assert!(parsed.sent);
        assert!(parsed.run.is_none());
    }

    #[test]
    fn tails_cut_on_characters() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("ab", 3), "ab");
        // "ö" is two bytes; a cut through it moves past it.
        assert_eq!(tail("aöb", 2), "b");
        assert_eq!(head("aöb", 2), "a");
    }

    #[test]
    fn unknown_run_fields_stay_out() {
        let report = Report {
            game_id: 1,
            upload_id: 2,
            build_id: None,
            rating: Rating::Perfect,
            flags: Vec::new(),
            run: Run {
                strategy: Some(Strategy::Retroarch),
                core: Some("mgba_libretro.so".into()),
                seconds: Some(95),
                launch_target: Some("rom:gba Game.gba".into()),
                ..Default::default()
            },
        };
        let form = report.form("{}".into());
        let keys: Vec<&str> = form.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            [
                "upload_id",
                "rating",
                "device_info",
                "run",
                "launch_target",
                "seconds_played"
            ]
        );
        let run: Value = serde_json::from_str(&form[3].1).unwrap();
        assert_eq!(
            run,
            json!({"strategy": "retroarch", "core": "mgba_libretro.so", "seconds_to_exit": 95})
        );
    }

    #[test]
    fn flags_are_a_json_array() {
        let report = Report {
            game_id: 1,
            upload_id: 2,
            build_id: Some(3),
            rating: Rating::Playable,
            flags: vec!["cut_off", "wrong_mapping"],
            run: Run::default(),
        };
        let form = report.form("{}".into());
        assert!(form.contains(&("build_id", "3".to_string())));
        assert!(form.contains(&("flags", r#"["cut_off","wrong_mapping"]"#.to_string())));
    }

    #[test]
    fn ratings_save_as_their_ids() {
        for rating in Rating::ALL {
            assert_eq!(json!(rating), json!(rating.id()));
        }
    }

    #[test]
    fn reads_the_key_from_butlers_database() {
        let path = std::env::temp_dir().join(format!("zitch-report-{}.db", std::process::id()));
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "create table profiles (id integer primary key, api_key text);
             insert into profiles values (7, 'secret');",
        )
        .unwrap();
        drop(db);
        assert_eq!(saved_api_key(&path, 7).unwrap(), "secret");
        assert!(saved_api_key(&path, 8).is_err());
        std::fs::remove_file(&path).unwrap();
    }
}

//! Extra details for the game page from the public `data.json` beside each
//! game's itch.io page: screenshots, tags, authors, price. Fetched only
//! while online and kept in memory for the session; the page is complete
//! without it.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

const TIMEOUT: Duration = Duration::from_secs(20);

/// Games whose details stay in memory; the least recently shown go first.
const KEEP: usize = 20;
/// How long a failed fetch waits before it is tried again.
const RETRY_AFTER: Duration = Duration::from_secs(10 * 60);
/// The wait after itch.io answers 429 Too Many Requests.
const RETRY_AFTER_LIMITED: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct PageInfo {
    pub screenshots: Vec<String>,
    pub tags: Vec<String>,
    pub authors: Vec<Author>,
    pub price: Option<String>,
    pub suggested_price: Option<String>,
    pub original_price: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub sale: Option<Sale>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Author {
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Sale {
    /// Percent off.
    pub rate: f64,
    pub title: Option<String>,
}

/// A field of an unexpected shape reads as missing instead of failing the
/// whole payload.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

impl PageInfo {
    pub fn parse(json: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if let Some(errors) = value.get("errors") {
            return Err(format!("itch.io says {errors}"));
        }
        serde_json::from_value(value).map_err(|e| e.to_string())
    }

    /// "Free", or the price with whatever else the page says about it:
    /// "$1.00, $2.00 suggested", "£17.50, 30% off £25.00".
    pub fn price_line(&self) -> Option<String> {
        let price = self.price.as_deref()?;
        let mut line = if is_zero(price) {
            "Free".to_string()
        } else {
            price.to_string()
        };
        if let Some(sale) = &self.sale {
            line.push_str(&format!(", {}% off", sale.rate.round()));
            if let Some(original) = &self.original_price {
                line.push_str(&format!(" {original}"));
            }
        }
        if let Some(suggested) = &self.suggested_price {
            line.push_str(&format!(", {suggested} suggested"));
        }
        Some(line)
    }

    /// "By Finji, Scott Benson, InfiniteAmmo".
    pub fn authors_line(&self) -> Option<String> {
        let names: Vec<&str> = self
            .authors
            .iter()
            .map(|a| a.name.as_str())
            .filter(|n| !n.is_empty())
            .collect();
        (!names.is_empty()).then(|| format!("By {}", names.join(", ")))
    }
}

/// True for "$0.00", "0,00 €" and the like.
fn is_zero(price: &str) -> bool {
    price.chars().any(|c| c.is_ascii_digit())
        && price.chars().filter(char::is_ascii_digit).all(|c| c == '0')
}

/// The data.json address for a game page.
pub fn data_url(page_url: &str) -> Option<String> {
    let page = page_url.trim().trim_end_matches('/');
    (!page.is_empty()).then(|| format!("{page}/data.json"))
}

/// Where a game's details stand, as the page draws them.
pub enum Lookup {
    Ready(Arc<PageInfo>),
    /// Being fetched.
    Loading,
    /// Offline, failed, or nowhere to fetch from; the page goes without.
    Missing,
}

enum Entry {
    Pending,
    Ready { info: Arc<PageInfo>, used: u64 },
    Failed { retry_at: Instant },
}

struct Job {
    game_id: i64,
    url: String,
    ctx: egui::Context,
}

#[derive(Default)]
struct State {
    entries: HashMap<i64, Entry>,
    /// The page asked for most recently and not yet started. A newer page
    /// replaces it, so flipping through games only fetches where it stops.
    next: Option<Job>,
    clock: u64,
}

impl State {
    /// Drops the least recently shown details beyond [`KEEP`].
    fn trim(&mut self) {
        let mut ready: Vec<(u64, i64)> = self
            .entries
            .iter()
            .filter_map(|(id, entry)| match entry {
                Entry::Ready { used, .. } => Some((*used, *id)),
                _ => None,
            })
            .collect();
        if ready.len() <= KEEP {
            return;
        }
        ready.sort_unstable();
        for (_, id) in &ready[..ready.len() - KEEP] {
            self.entries.remove(id);
        }
    }
}

struct Inner {
    state: Mutex<State>,
    queued: Condvar,
}

#[derive(Clone)]
pub struct PageInfoLoader {
    inner: Arc<Inner>,
}

impl PageInfoLoader {
    pub fn new() -> Self {
        let inner = Arc::new(Inner {
            state: Mutex::default(),
            queued: Condvar::new(),
        });
        let worker = Arc::clone(&inner);
        std::thread::Builder::new()
            .name("page-info".into())
            .spawn(move || worker.work())
            .expect("spawning page info worker");
        Self { inner }
    }

    /// The game's details if they are already in memory; never fetches.
    pub fn peek(&self, game_id: i64) -> Option<Arc<PageInfo>> {
        let state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        match state.entries.get(&game_id) {
            Some(Entry::Ready { info, .. }) => Some(Arc::clone(info)),
            _ => None,
        }
    }

    /// The game's details, once fetched. Asking while online starts the
    /// fetch; offline, only details already in memory come back.
    pub fn get(&self, ctx: &egui::Context, game_id: i64, page_url: &str, online: bool) -> Lookup {
        let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        state.clock += 1;
        let clock = state.clock;
        match state.entries.get_mut(&game_id) {
            Some(Entry::Ready { info, used }) => {
                *used = clock;
                return Lookup::Ready(Arc::clone(info));
            }
            Some(Entry::Pending) => return Lookup::Loading,
            Some(Entry::Failed { retry_at }) if Instant::now() < *retry_at => {
                return Lookup::Missing;
            }
            Some(Entry::Failed { .. }) | None => {}
        }
        if !online {
            return Lookup::Missing;
        }
        let Some(url) = data_url(page_url) else {
            return Lookup::Missing;
        };
        state.entries.insert(game_id, Entry::Pending);
        if let Some(replaced) = state.next.replace(Job {
            game_id,
            url,
            ctx: ctx.clone(),
        }) {
            state.entries.remove(&replaced.game_id);
        }
        self.inner.queued.notify_one();
        Lookup::Loading
    }
}

impl Inner {
    fn work(&self) {
        loop {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let job = loop {
                if let Some(job) = state.next.take() {
                    break job;
                }
                state = self.queued.wait(state).unwrap_or_else(|p| p.into_inner());
            };
            drop(state);
            let outcome = fetch(&job.url);
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let entry = match outcome {
                Ok(info) => {
                    log::debug!(
                        "page info for {}: {} screenshots",
                        job.game_id,
                        info.screenshots.len()
                    );
                    let used = state.clock;
                    Entry::Ready {
                        info: Arc::new(info),
                        used,
                    }
                }
                Err((error, wait)) => {
                    log::info!("page info {}: {error}", job.url);
                    Entry::Failed {
                        retry_at: Instant::now() + wait,
                    }
                }
            };
            state.entries.insert(job.game_id, entry);
            state.trim();
            drop(state);
            job.ctx.request_repaint();
        }
    }
}

/// The parsed payload, or why not with how long to wait before trying
/// again.
fn fetch(url: &str) -> Result<PageInfo, (String, Duration)> {
    // A small file: the whole exchange gets one deadline.
    let response = crate::http::agent()
        .get(url)
        .config()
        .timeout_global(Some(TIMEOUT))
        .build()
        .call();
    let mut response = match response {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(429)) => {
            return Err(("429 Too Many Requests".into(), RETRY_AFTER_LIMITED));
        }
        Err(error) => return Err((error.to_string(), RETRY_AFTER)),
    };
    let body = response
        .body_mut()
        .with_config()
        .limit(1 << 20)
        .read_to_string()
        .map_err(|e| (e.to_string(), RETRY_AFTER))?;
    PageInfo::parse(&body).map_err(|e| (e, RETRY_AFTER))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROCK_DECLUTTERER: &str = r#"{"price":"$0.00","title":"Rock Declutterer","screenshots":["https:\/\/img.itch.zone\/aW1hZ2UvNTAxNzc1Ny8zMDU3ODMxNC5wbmc=\/347x500\/qyFkl0.png","https:\/\/img.itch.zone\/aW1hZ2UvNTAxNzc1Ny8zMDU3ODMxNy5wbmc=\/347x500\/UFVlqe.png"],"authors":[{"name":"leafo","url":"https:\/\/leafo.itch.io"}],"links":{"comments":"https:\/\/leafo.itch.io\/rock-declutterer\/comments","self":"https:\/\/leafo.itch.io\/rock-declutterer"},"suggested_price":"$2.00","cover_image":"https:\/\/img.itch.zone\/aW1nLzMwMDYzNDU0LnBuZw==\/315x250%23c\/%2BhGAN6.png","tags":["arcade","love2d","physics","puzzle","touch-friendly","versus"],"id":5017757}"#;

    const NIGHT_IN_THE_WOODS: &str = r#"{"price":"$19.99","cover_image":"https:\/\/img.itch.zone\/aW1hZ2UvOTM2NjQvNDM4OTk3LnBuZw==\/315x250%23c\/bY3lZb.png","id":93664,"screenshots":["https:\/\/img.itch.zone\/aW1hZ2UvOTM2NjQvNDM4OTk4LmdpZg==\/original\/GQn%2BvX.gif"],"authors":[{"name":"Finji","url":"https:\/\/finji.itch.io"},{"name":"Scott Benson","url":"https:\/\/bombsfall.itch.io"},{"name":"InfiniteAmmo","url":"https:\/\/infiniteammo.itch.io"}],"title":"Night in the Woods","tags":["adventure","female-protagonist","narrative","story-rich"],"links":{"self":"https:\/\/finji.itch.io\/night-in-the-woods"}}"#;

    const ON_SALE: &str = r#"{"price":"£17.50","original_price":"£25.00","sale":{"rate":30,"end_date":"2026-12-31 04:59:00","title":"D.D. Discount 2026","id":177589},"title":"On Sale","id":1}"#;

    #[test]
    fn parses_a_free_game_with_a_suggested_price() {
        let info = PageInfo::parse(ROCK_DECLUTTERER).unwrap();
        assert_eq!(info.screenshots.len(), 2);
        assert_eq!(info.tags[0], "arcade");
        assert_eq!(info.authors_line().as_deref(), Some("By leafo"));
        assert_eq!(info.price_line().as_deref(), Some("Free, $2.00 suggested"));
    }

    #[test]
    fn lists_every_author() {
        let info = PageInfo::parse(NIGHT_IN_THE_WOODS).unwrap();
        assert_eq!(
            info.authors_line().as_deref(),
            Some("By Finji, Scott Benson, InfiniteAmmo")
        );
        assert_eq!(info.price_line().as_deref(), Some("$19.99"));
    }

    #[test]
    fn describes_a_sale() {
        let info = PageInfo::parse(ON_SALE).unwrap();
        assert_eq!(info.price_line().as_deref(), Some("£17.50, 30% off £25.00"));
    }

    #[test]
    fn missing_and_odd_fields_read_as_absent() {
        let info = PageInfo::parse(r#"{"sale":"none","links":5,"rewards":[{"id":1}]}"#).unwrap();
        assert_eq!(info, PageInfo::default());
        assert_eq!(info.price_line(), None);
        assert_eq!(info.authors_line(), None);
    }

    #[test]
    fn errors_and_garbage_fail() {
        assert!(PageInfo::parse(r#"{"errors":["invalid game"]}"#).is_err());
        assert!(PageInfo::parse("<html>429 Too Many Requests</html>").is_err());
    }

    #[test]
    fn free_means_all_zero_digits() {
        assert!(is_zero("$0.00"));
        assert!(is_zero("0,00 €"));
        assert!(!is_zero("$0.50"));
        assert!(!is_zero("Free"));
    }

    #[test]
    fn data_url_ignores_a_trailing_slash() {
        assert_eq!(
            data_url("https://leafo.itch.io/rock-declutterer/").as_deref(),
            Some("https://leafo.itch.io/rock-declutterer/data.json")
        );
        assert_eq!(
            data_url("https://leafo.itch.io/rock-declutterer").as_deref(),
            Some("https://leafo.itch.io/rock-declutterer/data.json")
        );
        assert_eq!(data_url(""), None);
    }

    #[test]
    fn keeps_the_most_recently_shown() {
        let mut state = State::default();
        for id in 0..(KEEP as i64 + 5) {
            state.entries.insert(
                id,
                Entry::Ready {
                    info: Arc::default(),
                    used: id as u64,
                },
            );
        }
        state.entries.insert(-1, Entry::Pending);
        state.trim();
        assert_eq!(state.entries.len(), KEEP + 1);
        assert!(state.entries.contains_key(&-1));
        assert!(!state.entries.contains_key(&4));
        assert!(state.entries.contains_key(&5));
    }
}

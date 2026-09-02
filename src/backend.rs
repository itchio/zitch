//! The butlerd conversation, on its own thread. The interface sends
//! [`Command`]s and polls [`Event`]s; nothing here blocks a frame.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::butlerd::types::{
    AnyNotification, AnyServerRequest, FetchCavesParams, FetchProfileOwnedKeysParams,
    ProfileListParams, ProfileLoginWithAPIKeyParams, ProfileUseSavedLoginParams,
};
use crate::butlerd::{Client, Daemon, Incoming};
use crate::model::{Cave, Game, Profile, UserExt};

pub struct Config {
    pub butler: PathBuf,
    pub dbpath: PathBuf,
    /// Used for `Profile.LoginWithAPIKey` when no saved profile exists.
    pub api_key: Option<String>,
}

pub enum Command {
    Shutdown,
}

pub enum Event {
    /// A one-line description of what the backend is doing.
    Status(String),
    SignedIn(Profile),
    OwnedGames(Vec<Game>),
    /// Every installed game known to this database.
    Caves(Vec<Cave>),
    Error(String),
}

/// Repaints the window when the backend has news. egui only redraws on
/// input, so without this a reply would sit unseen until the mouse moved.
#[derive(Clone, Default)]
pub struct Waker(Arc<Mutex<Option<egui::Context>>>);

impl Waker {
    pub fn attach(&self, ctx: &egui::Context) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(ctx.clone());
    }

    pub fn wake(&self) {
        if let Some(ctx) = self.0.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            ctx.request_repaint();
        }
    }
}

pub struct Backend {
    commands: mpsc::Sender<Command>,
    events: mpsc::Receiver<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Backend {
    pub fn spawn(config: Config, waker: Waker) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("zitch-backend".into())
            .spawn(move || {
                let emit = Emitter {
                    events: event_tx,
                    waker,
                };
                if let Err(error) = run(config, &emit, command_rx) {
                    log::error!("{error:#}");
                    emit.send(Event::Error(format!("{error:#}")));
                }
            })
            .expect("spawning backend thread");
        Self {
            commands: command_tx,
            events: event_rx,
            thread: Some(thread),
        }
    }

    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    pub fn shutdown(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Emitter {
    events: mpsc::Sender<Event>,
    waker: Waker,
}

impl Emitter {
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
        self.waker.wake();
    }

    fn status(&self, text: impl Into<String>) {
        let text = text.into();
        log::info!("{text}");
        self.send(Event::Status(text));
    }
}

fn run(config: Config, emit: &Emitter, commands: mpsc::Receiver<Command>) -> Result<()> {
    emit.status("Starting butler");
    let daemon = Daemon::spawn(&config.butler, &config.dbpath)?;
    let client = Client::connect(&daemon)?;
    emit.status(format!("Connected to butlerd at {}", daemon.address));

    let profile = sign_in(&client, &config, emit)?;
    let name = profile.user.as_ref().map_or("?", UserExt::name);
    emit.status(format!("Signed in as {name}"));
    emit.send(Event::SignedIn(profile.clone()));

    emit.status("Loading library");
    let games = owned_games(&client, profile.id, false)?;
    emit.send(Event::OwnedGames(games));
    emit.status("Library loaded");
    let caves = all_caves(&client)?;
    log::info!("{} installed games", caves.len());
    emit.send(Event::Caves(caves));

    loop {
        match commands.try_recv() {
            Ok(Command::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        for incoming in client.poll_timeout(Duration::from_millis(100)) {
            match incoming {
                Incoming::Notification { method, params } => {
                    match AnyNotification::decode(&method, params) {
                        Ok(notification) => log::debug!("{notification:?}"),
                        Err(error) => log::warn!("bad {method} notification: {error}"),
                    }
                }
                Incoming::Request { id, method, params } => {
                    match AnyServerRequest::decode(&method, params) {
                        Ok(request) => log::warn!("unhandled server request {request:?}"),
                        Err(error) => log::warn!("bad {method} request: {error}"),
                    }
                    let _ = client.reply_error(&id, -32601, "not supported by this client");
                }
            }
        }
    }
    Ok(())
}

fn sign_in(client: &Client, config: &Config, emit: &Emitter) -> Result<Profile> {
    let mut saved = client.call(ProfileListParams {})?.profiles;
    saved.sort_by(|a, b| b.last_connected.cmp(&a.last_connected));
    if let Some(entry) = saved.first() {
        emit.status("Using saved login");
        let result = client.call(ProfileUseSavedLoginParams {
            profile_id: entry.id,
        })?;
        return result
            .profile
            .ok_or_else(|| anyhow::anyhow!("saved login returned no profile"));
    }
    let Some(api_key) = &config.api_key else {
        bail!(
            "no saved profile in {}; pass --api-key-file or set ZITCH_API_KEY to sign in once",
            config.dbpath.display()
        );
    };
    emit.status("Signing in with API key");
    let result = client
        .call(ProfileLoginWithAPIKeyParams {
            api_key: api_key.clone(),
        })
        .context("API key login")?;
    result
        .profile
        .ok_or_else(|| anyhow::anyhow!("login returned no profile"))
}

fn owned_games(client: &Client, profile_id: i64, fresh: bool) -> Result<Vec<Game>> {
    let mut games = Vec::new();
    let mut cursor = None;
    loop {
        let page = client.call(FetchProfileOwnedKeysParams {
            profile_id,
            limit: Some(100),
            cursor: cursor.take(),
            fresh: Some(fresh),
            ..Default::default()
        })?;
        // The cache is empty on first run and answers stale with no items;
        // a fresh fetch fills it.
        if page.stale == Some(true) && !fresh && games.is_empty() && page.items.is_empty() {
            return owned_games(client, profile_id, true);
        }
        games.extend(page.items.into_iter().filter_map(|key| key.game));
        match page.next_cursor {
            Some(next) if !next.is_empty() => cursor = Some(next),
            _ => break,
        }
    }
    Ok(games)
}

fn all_caves(client: &Client) -> Result<Vec<Cave>> {
    let mut caves = Vec::new();
    let mut cursor = None;
    loop {
        let page = client.call(FetchCavesParams {
            limit: Some(100),
            cursor: cursor.take(),
            ..Default::default()
        })?;
        caves.extend(page.items);
        match page.next_cursor {
            Some(next) if !next.is_empty() => cursor = Some(next),
            _ => break,
        }
    }
    Ok(caves)
}

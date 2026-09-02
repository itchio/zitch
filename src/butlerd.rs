//! A butlerd client: spawns `butler daemon` and speaks newline-delimited
//! JSON-RPC 2.0 to it over TCP.
//!
//! See https://itch.io/docs/butler/launcher-integration.html and the full
//! protocol reference at https://itchio.github.io/butler/butlerd/.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A running `butler daemon` process. Killed when dropped; `--destiny-pid`
/// also reaps it if this process dies first.
pub struct Daemon {
    child: Child,
    pub address: String,
    pub secret: String,
}

#[derive(Deserialize)]
struct ListenNotification {
    #[serde(rename = "type")]
    kind: String,
    secret: Option<String>,
    tcp: Option<ListenTcp>,
}

#[derive(Deserialize)]
struct ListenTcp {
    address: String,
}

impl Daemon {
    pub fn spawn(butler: &Path, dbpath: &Path) -> Result<Self> {
        if let Some(parent) = dbpath.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut child = Command::new(butler)
            .arg("daemon")
            .arg("--json")
            .arg("--transport")
            .arg("tcp")
            .arg("--keep-alive")
            .arg("--dbpath")
            .arg(dbpath)
            .arg("--destiny-pid")
            .arg(std::process::id().to_string())
            .arg("--user-agent")
            .arg(concat!("zitch/", env!("CARGO_PKG_VERSION")))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning {}", butler.display()))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let mut lines = BufReader::new(stdout).lines();
        loop {
            let Some(line) = lines.next() else {
                let _ = child.kill();
                bail!("butler daemon exited before announcing its address");
            };
            let line = line.context("reading butler daemon stdout")?;
            let Ok(note) = serde_json::from_str::<ListenNotification>(&line) else {
                log::debug!("butler: {line}");
                continue;
            };
            if note.kind != "butlerd/listen-notification" {
                continue;
            }
            let (Some(secret), Some(tcp)) = (note.secret, note.tcp) else {
                let _ = child.kill();
                bail!("listen-notification without a tcp address and secret");
            };
            // Nothing else useful arrives on stdout; let a thread drain it so
            // the daemon never blocks on a full pipe.
            std::thread::Builder::new()
                .name("butler-stdout".into())
                .spawn(move || {
                    for line in lines.map_while(Result::ok) {
                        log::debug!("butler: {line}");
                    }
                })
                .expect("spawning stdout drain");
            return Ok(Self {
                child,
                address: tcp.address,
                secret,
            });
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An error returned by butlerd for a request.
#[derive(Debug, Clone, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // itch.io API failures carry the user-facing messages in
        // data.apiError.messages.
        let api_messages = self
            .data
            .as_ref()
            .and_then(|data| data.get("apiError"))
            .and_then(|error| error.get("messages"))
            .and_then(Value::as_array)
            .map(|messages| {
                messages
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|joined| !joined.is_empty());
        match api_messages {
            Some(messages) => write!(f, "{messages}"),
            None => write!(f, "{} (code {})", self.message, self.code),
        }
    }
}

impl std::error::Error for RpcError {}

/// A message from the daemon that is not a reply to one of our requests.
#[derive(Debug, Clone)]
pub enum Incoming {
    /// Progress and status, with no reply expected.
    Notification { method: String, params: Value },
    /// The daemon needs an answer before it can continue the call it is in.
    /// Answer with [`Client::reply`] or [`Client::reply_error`].
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

type Pending = Arc<Mutex<HashMap<u64, mpsc::Sender<Result<Value, RpcError>>>>>;

/// One authenticated JSON-RPC connection to the daemon.
pub struct Client {
    writer: Mutex<TcpStream>,
    pending: Pending,
    next_id: AtomicU64,
    incoming: Mutex<mpsc::Receiver<Incoming>>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<Value>,
    method: Option<String>,
    params: Option<Value>,
    result: Option<Value>,
    error: Option<RpcError>,
}

impl Client {
    pub fn connect(daemon: &Daemon) -> Result<Self> {
        let stream = TcpStream::connect(&daemon.address)
            .with_context(|| format!("connecting to butlerd at {}", daemon.address))?;
        stream.set_nodelay(true)?;
        let reader = stream.try_clone()?;
        let pending: Pending = Arc::default();
        let (incoming_tx, incoming_rx) = mpsc::channel();
        let reader_pending = Arc::clone(&pending);
        std::thread::Builder::new()
            .name("butlerd-reader".into())
            .spawn(move || read_loop(reader, reader_pending, incoming_tx))
            .expect("spawning butlerd reader");
        let client = Self {
            writer: Mutex::new(stream),
            pending,
            next_id: AtomicU64::new(1),
            incoming: Mutex::new(incoming_rx),
        };
        let ok: Value = client.call("Meta.Authenticate", json!({ "secret": daemon.secret }))?;
        if ok.get("ok") != Some(&Value::Bool(true)) {
            bail!("butlerd refused the secret: {ok}");
        }
        Ok(client)
    }

    /// Sends a request and blocks until its reply arrives. Notifications and
    /// server requests that arrive meanwhile queue up for [`Self::poll`].
    pub fn call<P: Serialize, R: DeserializeOwned>(&self, method: &str, params: P) -> Result<R> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, tx);
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if method.starts_with("Profile.Login") {
            log::trace!("-> {method} <credentials redacted>");
        } else {
            log::trace!("-> {method} {}", message["params"]);
        }
        self.send(&message)?;
        let result = rx
            .recv()
            .map_err(|_| anyhow!("butlerd connection closed while waiting for {method}"))?
            .with_context(|| format!("{method} failed"))?;
        log::trace!("<- {method} {result}");
        serde_json::from_value(result).with_context(|| format!("decoding {method} result"))
    }

    pub fn reply<R: Serialize>(&self, id: &Value, result: R) -> Result<()> {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    pub fn reply_error(&self, id: &Value, code: i64, message: &str) -> Result<()> {
        self.send(&json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": code, "message": message }
        }))
    }

    /// Everything the daemon sent that was not a reply, in order.
    pub fn poll(&self) -> Vec<Incoming> {
        self.incoming
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .try_iter()
            .collect()
    }

    /// Like [`Self::poll`] but waits up to `timeout` for the first message.
    pub fn poll_timeout(&self, timeout: Duration) -> Vec<Incoming> {
        let rx = self.incoming.lock().unwrap_or_else(|p| p.into_inner());
        let mut out = Vec::new();
        if let Ok(first) = rx.recv_timeout(timeout) {
            out.push(first);
            out.extend(rx.try_iter());
        }
        out
    }

    fn send(&self, message: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        let mut writer = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        writer.write_all(&line).context("writing to butlerd")?;
        writer.flush().context("flushing to butlerd")
    }
}

fn read_loop(stream: TcpStream, pending: Pending, incoming: mpsc::Sender<Incoming>) {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                log::warn!("butlerd connection lost: {error}");
                break;
            }
        };
        let message: Message = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(error) => {
                log::warn!("unparseable line from butlerd ({error}): {line}");
                continue;
            }
        };
        match (message.id, message.method) {
            (Some(id), Some(method)) => {
                let _ = incoming.send(Incoming::Request {
                    id,
                    method,
                    params: message.params.unwrap_or(Value::Null),
                });
            }
            (None, Some(method)) => {
                let _ = incoming.send(Incoming::Notification {
                    method,
                    params: message.params.unwrap_or(Value::Null),
                });
            }
            (Some(id), None) => {
                let Some(id) = id.as_u64() else {
                    log::warn!("reply with a non-numeric id: {id}");
                    continue;
                };
                let waiter = pending
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&id);
                let Some(waiter) = waiter else {
                    log::warn!("reply to unknown request {id}");
                    continue;
                };
                let outcome = match (message.result, message.error) {
                    (_, Some(error)) => Err(error),
                    (Some(result), None) => Ok(result),
                    (None, None) => Ok(Value::Null),
                };
                let _ = waiter.send(outcome);
            }
            (None, None) => log::warn!("message with neither id nor method: {line}"),
        }
    }
    // Wake every caller still waiting so they fail instead of hanging.
    pending.lock().unwrap_or_else(|p| p.into_inner()).clear();
}

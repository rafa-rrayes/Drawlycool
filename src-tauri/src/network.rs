//! Remote canvases: the servers this app is connected to, their canvas lists,
//! and live editing sessions.
//!
//! All networking happens here rather than in the webview, so there are no
//! CORS or mixed-content limits and `http://` LAN servers work like `https://`
//! ones. Live sessions stream server messages to the page over a Tauri
//! channel (which keeps them in order).

use crate::host;
use drawlycool_server::storage;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use tauri::ipc::{Channel, InvokeResponseBody, Request};
use tauri::{AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

type Result<T> = std::result::Result<T, String>;

/// Id of the pseudo-server for canvases this Mac hosts.
pub const HOST_ID: &str = "this-mac";

const MAX_MESSAGE: usize = 256 << 20;
const PING_EVERY: Duration = Duration::from_secs(20);
const SILENCE_LIMIT: Duration = Duration::from_secs(45);

// ------------------------------------------------------------------ config

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedServer {
    pub id: String,
    pub url: String,
    pub token: String,
    pub name: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HostConfig {
    pub enabled: bool,
    pub port: u16,
    pub token: String,
    pub name: String,
    /// Address put in invite links (an IP or a `.local` name).
    pub invite_address: Option<String>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self { enabled: false, port: 4180, token: String::new(), name: String::new(), invite_address: None }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NetConfig {
    pub servers: Vec<SavedServer>,
    pub host: HostConfig,
}

pub struct Net {
    path: PathBuf,
    config: Mutex<NetConfig>,
    http: reqwest::Client,
    sessions: Mutex<HashMap<u64, UnboundedSender<Cmd>>>,
    session_tasks: Mutex<Vec<tauri::async_runtime::JoinHandle<()>>>,
    /// `drawlycool://` links opened before the page was ready to take them.
    pending_links: Mutex<Vec<String>>,
    pub data_dir: PathBuf,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Net {
    pub fn open(data_dir: PathBuf) -> Self {
        let path = data_dir.join("servers.json");
        let config: NetConfig =
            std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(6))
            .timeout(Duration::from_secs(120))
            .build()
            .expect("http client");
        Self {
            path,
            config: Mutex::new(config),
            http,
            sessions: Mutex::new(HashMap::new()),
            session_tasks: Mutex::new(Vec::new()),
            pending_links: Mutex::new(Vec::new()),
            data_dir,
        }
    }

    pub fn config(&self) -> NetConfig {
        lock(&self.config).clone()
    }

    pub fn update_config(&self, change: impl FnOnce(&mut NetConfig)) -> Result<NetConfig> {
        let mut config = lock(&self.config);
        change(&mut config);
        let json = serde_json::to_vec_pretty(&*config).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &self.path)).map_err(|e| e.to_string())?;
        Ok(config.clone())
    }

    /// Base URL, token and display name for a server id.
    fn endpoint(&self, server_id: &str) -> Result<(String, String, String)> {
        let config = lock(&self.config);
        if server_id == HOST_ID {
            if !host::is_running() {
                return Err("Hosting is turned off.".into());
            }
            let h = &config.host;
            return Ok((format!("http://127.0.0.1:{}", h.port), h.token.clone(), h.name.clone()));
        }
        config
            .servers
            .iter()
            .find(|s| s.id == server_id)
            .map(|s| (s.url.clone(), s.token.clone(), s.name.clone()))
            .ok_or_else(|| "This server isn’t in your list anymore.".into())
    }

    pub fn push_link(&self, url: String) {
        lock(&self.pending_links).push(url);
    }
}

type Net_<'a> = State<'a, Net>;

/// Accepts "host:port", "http://host:port/" and friends.
fn normalize_url(input: &str) -> Result<String> {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Enter the server address.".into());
    }
    let with_scheme = if trimmed.contains("://") { trimmed.to_owned() } else { format!("http://{trimmed}") };
    let url = tauri::Url::parse(&with_scheme).map_err(|_| format!("“{input}” isn’t a valid address."))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(format!("“{input}” isn’t a valid address."));
    }
    let mut s = url.to_string();
    while s.ends_with('/') {
        s.pop();
    }
    Ok(s)
}

// ------------------------------------------------------------------ HTTP helpers

async fn call(
    net: &Net,
    server_id: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<reqwest::Response> {
    let (base, token, name) = net.endpoint(server_id)?;
    request(&net.http, &base, &token, &name, method, path, body).await
}

async fn request(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    name: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<reqwest::Response> {
    let mut req = http.request(method, format!("{base}{path}")).bearer_auth(token);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await.map_err(|e| {
        if e.is_connect() || e.is_timeout() {
            format!("Can’t reach {name}.")
        } else {
            format!("Couldn’t talk to {name}: {e}")
        }
    })?;
    match resp.status().as_u16() {
        200..=299 => Ok(resp),
        401 => Err(format!("{name} rejected the access token.")),
        404 => Err("That canvas doesn’t exist anymore.".into()),
        _ => {
            let status = resp.status();
            let msg = resp.json::<Value>().await.ok().and_then(|v| v["error"].as_str().map(str::to_owned));
            Err(msg.unwrap_or_else(|| format!("{name} answered {status}.")))
        }
    }
}

async fn json_of(resp: reqwest::Response) -> Result<Value> {
    resp.json().await.map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ servers

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerView {
    id: String,
    name: String,
    url: String,
    is_host: bool,
}

#[tauri::command]
pub fn list_servers(net: Net_) -> Vec<ServerView> {
    let config = net.config();
    let mut list = Vec::new();
    if host::is_running() {
        list.push(ServerView {
            id: HOST_ID.into(),
            name: config.host.name.clone(),
            url: host::invite_url(&config.host),
            is_host: true,
        });
    }
    list.extend(config.servers.into_iter().map(|s| ServerView { id: s.id, name: s.name, url: s.url, is_host: false }));
    list
}

/// Checks the address and token, then remembers the server.
#[tauri::command]
pub async fn add_server(net: Net_<'_>, url: String, token: String) -> Result<ServerView> {
    let url = normalize_url(&url)?;
    let token = token.trim().to_owned();
    if token.is_empty() {
        return Err("Enter the access token.".into());
    }
    let host_label = tauri::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_default();
    let info = json_of(request(&net.http, &url, &token, &host_label, reqwest::Method::GET, "/api/info", None).await?).await?;
    if info["protocol"].as_u64() != Some(drawlycool_server::PROTOCOL as u64) {
        return Err("That server runs an incompatible version.".into());
    }
    let name = info["name"].as_str().filter(|n| !n.trim().is_empty()).unwrap_or(&host_label).to_owned();

    let mut saved = None;
    net.update_config(|c| {
        // Same address again (e.g. a fresh invite): refresh it in place.
        if let Some(existing) = c.servers.iter_mut().find(|s| s.url == url) {
            existing.token = token.clone();
            existing.name = name.clone();
            saved = Some(existing.clone());
        } else {
            let s = SavedServer { id: uuid::Uuid::new_v4().to_string(), url: url.clone(), token: token.clone(), name: name.clone() };
            c.servers.push(s.clone());
            saved = Some(s);
        }
    })?;
    let s = saved.expect("saved");
    Ok(ServerView { id: s.id, name: s.name, url: s.url, is_host: false })
}

#[tauri::command]
pub fn remove_server(net: Net_, id: String) -> Result<()> {
    net.update_config(|c| c.servers.retain(|s| s.id != id)).map(|_| ())
}

/// `drawlycool://connect?server=…&token=…[&canvas=…]` for a server or canvas.
#[tauri::command]
pub fn invite_link(net: Net_, server_id: String, canvas_id: Option<String>) -> Result<String> {
    let config = net.config();
    let (url, token) = if server_id == HOST_ID {
        (host::invite_url(&config.host), config.host.token.clone())
    } else {
        let s = config.servers.iter().find(|s| s.id == server_id).ok_or("Unknown server")?;
        (s.url.clone(), s.token.clone())
    };
    let mut link = format!("drawlycool://connect?server={}&token={}", encode(&url), encode(&token));
    if let Some(id) = canvas_id {
        link.push_str(&format!("&canvas={}", encode(&id)));
    }
    Ok(link)
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tauri::command]
pub fn take_pending_links(net: Net_) -> Vec<String> {
    std::mem::take(&mut *lock(&net.pending_links))
}

// ------------------------------------------------------------------ remote canvases

#[tauri::command]
pub async fn remote_list(net: Net_<'_>, server_id: String) -> Result<Value> {
    json_of(call(&net, &server_id, reqwest::Method::GET, "/api/canvases", None).await?).await
}

#[tauri::command]
pub async fn remote_create(net: Net_<'_>, server_id: String, name: String) -> Result<Value> {
    let body = json!({ "name": name });
    json_of(call(&net, &server_id, reqwest::Method::POST, "/api/canvases", Some(body)).await?).await
}

#[tauri::command]
pub async fn remote_rename(net: Net_<'_>, server_id: String, canvas_id: String, name: String) -> Result<Value> {
    let path = format!("/api/canvases/{canvas_id}");
    json_of(call(&net, &server_id, reqwest::Method::PATCH, &path, Some(json!({ "name": name }))).await?).await
}

#[tauri::command]
pub async fn remote_duplicate(net: Net_<'_>, server_id: String, canvas_id: String) -> Result<Value> {
    let path = format!("/api/canvases/{canvas_id}/duplicate");
    json_of(call(&net, &server_id, reqwest::Method::POST, &path, None).await?).await
}

#[tauri::command]
pub async fn remote_delete(
    app: AppHandle,
    window: WebviewWindow,
    net: Net_<'_>,
    server_id: String,
    canvas_id: String,
    name: String,
) -> Result<bool> {
    let (_, _, server_name) = net.endpoint(&server_id)?;
    let confirmed = app
        .dialog()
        .message(format!("It will be deleted from {server_name} for everyone. This can’t be undone."))
        .title(format!("Delete “{name}”?"))
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom("Delete".into(), "Cancel".into()))
        .parent(&window)
        .blocking_show();
    if confirmed {
        call(&net, &server_id, reqwest::Method::DELETE, &format!("/api/canvases/{canvas_id}"), None).await?;
    }
    Ok(confirmed)
}

#[tauri::command]
pub async fn remote_export(
    app: AppHandle,
    window: WebviewWindow,
    net: Net_<'_>,
    server_id: String,
    canvas_id: String,
    name: String,
) -> Result<bool> {
    let bytes = call(&net, &server_id, reqwest::Method::GET, &format!("/api/canvases/{canvas_id}/scene"), None)
        .await?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    let Some(target) = crate::save_dialog(&app, &window, "Export canvas", &format!("{name}.excalidraw"), ("Excalidraw", &["excalidraw"]))
    else {
        return Ok(false);
    };
    std::fs::write(target, bytes).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Uploads a local canvas to a server and, once it's there, removes the local copy.
#[tauri::command]
pub async fn move_to_server(
    app: AppHandle,
    window: WebviewWindow,
    net: Net_<'_>,
    store: crate::Store<'_>,
    canvas_id: String,
    server_id: String,
) -> Result<Option<Value>> {
    let (meta, _) = crate::lock(&store).meta(&canvas_id)?;
    let (_, _, server_name) = net.endpoint(&server_id)?;
    let confirmed = app
        .dialog()
        .message(format!(
            "Everyone with access to {server_name} will be able to see and edit it. The copy on this Mac is removed."
        ))
        .title(format!("Move “{}” to {server_name}?", meta.name))
        .buttons(MessageDialogButtons::OkCancelCustom("Move".into(), "Cancel".into()))
        .parent(&window)
        .blocking_show();
    if !confirmed {
        return Ok(None);
    }
    let scene = String::from_utf8(crate::lock(&store).load_scene(&canvas_id)?).map_err(|e| e.to_string())?;
    let body = json!({ "name": meta.name, "scene": scene });
    let remote = json_of(call(&net, &server_id, reqwest::Method::POST, "/api/canvases", Some(body)).await?).await?;
    crate::lock(&store).remove(&canvas_id)?;
    Ok(Some(remote))
}

#[tauri::command]
pub fn copy_text(text: String) -> Result<()> {
    use std::io::Write;
    let mut child = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child.stdin.take().ok_or("no stdin")?.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}

/// The macOS account's full name, used as the default display name.
#[tauri::command]
pub fn user_full_name() -> String {
    std::process::Command::new("id")
        .arg("-F")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "Someone".into())
}

// ------------------------------------------------------------------ live sessions

enum Cmd {
    Send(String),
    Close,
}

enum Outcome {
    Reconnect,
    Stop,
}

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

struct Feed {
    channel: Channel<InvokeResponseBody>,
}

impl Feed {
    /// Sends `{ <field>: <raw> }`; `raw` is already JSON, so it isn't re-encoded.
    /// False once the page is gone.
    fn send(&mut self, field: &str, raw: &str) -> bool {
        let body = format!("{{\"{field}\":{raw}}}");
        self.channel.send(InvokeResponseBody::Json(body)).is_ok()
    }

    fn status(&mut self, state: &str, error: Option<&str>, fatal: bool) -> bool {
        let status = json!({ "state": state, "error": error, "fatal": fatal });
        self.send("status", &status.to_string())
    }
}

/// Opens a live session on a remote canvas. Server messages arrive on
/// `on_message`; send with `remote_send`, end with `remote_disconnect`.
#[tauri::command]
pub fn remote_connect(
    net: Net_,
    server_id: String,
    canvas_id: String,
    name: String,
    color: String,
    user_id: String,
    on_message: Channel<InvokeResponseBody>,
) -> Result<u64> {
    let (base, token, server_name) = net.endpoint(&server_id)?;
    if !storage::is_valid_id(&canvas_id) {
        return Err("Invalid canvas id".into());
    }
    let ws_url = format!("{}/api/canvases/{canvas_id}/ws", base.replacen("http", "ws", 1));
    let hello = json!({ "type": "hello", "name": name, "color": color, "userId": user_id }).to_string();
    let id = NEXT_SESSION.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = unbounded_channel();
    lock(&net.sessions).insert(id, tx);
    let feed = Feed { channel: on_message };
    let task = tauri::async_runtime::spawn(run_session(ws_url, token, server_name, hello, feed, rx));
    let mut tasks = lock(&net.session_tasks);
    tasks.retain(|t| !t.inner().is_finished());
    tasks.push(task);
    Ok(id)
}

#[tauri::command]
pub fn remote_send(net: Net_, request: Request<'_>) -> Result<()> {
    let id: u64 = crate::header(&request, "x-session")?.parse().map_err(|_| "bad session id")?;
    let text = String::from_utf8(crate::raw_body(&request)?.to_vec()).map_err(|e| e.to_string())?;
    if let Some(tx) = lock(&net.sessions).get(&id) {
        let _ = tx.send(Cmd::Send(text));
    }
    Ok(())
}

#[tauri::command]
pub fn remote_disconnect(net: Net_, session: u64) {
    if let Some(tx) = lock(&net.sessions).remove(&session) {
        let _ = tx.send(Cmd::Close);
    }
}

/// Closes every live session (after sending what's queued), waiting up to `limit`.
pub async fn close_all(app: &AppHandle, limit: Duration) {
    let net = app.state::<Net>();
    for (_, tx) in lock(&net.sessions).drain() {
        let _ = tx.send(Cmd::Close);
    }
    let tasks: Vec<_> = lock(&net.session_tasks).drain(..).collect();
    let _ = tokio::time::timeout(limit, futures_util::future::join_all(tasks)).await;
}

async fn run_session(
    url: String,
    token: String,
    server_name: String,
    hello: String,
    mut feed: Feed,
    mut rx: UnboundedReceiver<Cmd>,
) {
    let mut backoff = Duration::from_millis(500);
    loop {
        if !feed.status("connecting", None, false) {
            return;
        }
        let outcome = match connect(&url, &token).await {
            Ok(ws) => {
                backoff = Duration::from_millis(500);
                live(ws, &hello, &mut feed, &mut rx).await
            }
            Err(ConnectError::Fatal(msg)) => {
                feed.status("closed", Some(&msg), true);
                return;
            }
            Err(ConnectError::Retry) => Outcome::Reconnect,
        };
        if matches!(outcome, Outcome::Stop) {
            return;
        }
        if !feed.status("offline", Some(&format!("Can’t reach {server_name}. Retrying…")), false) {
            return;
        }
        // Wait before retrying, but still honour a disconnect. Edits sent in
        // the meantime are dropped; the page resends everything on reconnect.
        let deadline = tokio::time::sleep(backoff);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                cmd = rx.recv() => match cmd {
                    Some(Cmd::Send(_)) => continue,
                    Some(Cmd::Close) | None => return,
                },
            }
        }
        backoff = (backoff * 2).min(Duration::from_secs(8));
    }
}

enum ConnectError {
    Retry,
    Fatal(String),
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(url: &str, token: &str) -> std::result::Result<Socket, ConnectError> {
    let mut request = url.into_client_request().map_err(|e| ConnectError::Fatal(e.to_string()))?;
    let auth = format!("Bearer {token}").parse().map_err(|_| ConnectError::Fatal("Invalid token".into()))?;
    request.headers_mut().insert("Authorization", auth);
    let config = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE));
    let attempt = tokio_tungstenite::connect_async_with_config(request, Some(config), true);
    match tokio::time::timeout(Duration::from_secs(8), attempt).await {
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(resp))) => match resp.status().as_u16() {
            401 => Err(ConnectError::Fatal("The server rejected the access token.".into())),
            404 => Err(ConnectError::Fatal("This canvas doesn’t exist anymore.".into())),
            _ => Err(ConnectError::Retry),
        },
        Ok(Err(_)) => Err(ConnectError::Retry),
        Err(_) => Err(ConnectError::Retry),
    }
}

async fn live(ws: Socket, hello: &str, feed: &mut Feed, rx: &mut UnboundedReceiver<Cmd>) -> Outcome {
    let (mut sink, mut stream) = ws.split();
    if sink.send(Message::Text(hello.into())).await.is_err() {
        return Outcome::Reconnect;
    }
    if !feed.status("online", None, false) {
        return Outcome::Stop;
    }
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(Cmd::Send(text)) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        return Outcome::Reconnect;
                    }
                }
                Some(Cmd::Close) | None => {
                    let _ = sink.send(Message::Close(None)).await;
                    let _ = sink.flush().await;
                    return Outcome::Stop;
                }
            },
            incoming = tokio::time::timeout(SILENCE_LIMIT, stream.next()) => match incoming {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if !feed.send("msg", &text) {
                        let _ = sink.send(Message::Close(None)).await;
                        return Outcome::Stop;
                    }
                }
                Ok(Some(Ok(Message::Close(frame)))) => {
                    let code = frame.as_ref().map(|f| u16::from(f.code)).unwrap_or(1000);
                    // 44xx: the canvas is gone or access was revoked; don't retry.
                    if (4400..4500).contains(&code) {
                        let reason = frame.map(|f| f.reason.to_string()).unwrap_or_default();
                        feed.status("closed", Some(&reason), true);
                        return Outcome::Stop;
                    }
                    return Outcome::Reconnect;
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(_))) | Ok(None) | Err(_) => return Outcome::Reconnect,
            },
            _ = ping.tick() => {
                if sink.send(Message::Ping(Default::default())).await.is_err() {
                    return Outcome::Reconnect;
                }
            }
        }
    }
}

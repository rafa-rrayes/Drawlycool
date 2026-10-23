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

/// A server you've connected to. Kept after you remove it, so the Connect
/// panel can offer it again without a fresh invite.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentServer {
    pub url: String,
    pub token: String,
    pub name: String,
    pub last_connected: f64,
}

/// How many recent connections to remember.
const MAX_RECENT: usize = 8;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HostConfig {
    pub enabled: bool,
    pub port: u16,
    pub token: String,
    pub name: String,
    /// Address put in invite links (an IP or a `.local` name).
    pub invite_address: Option<String>,
    /// Invite over the internet through a Cloudflare tunnel instead.
    pub internet: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 4180,
            token: String::new(),
            name: String::new(),
            invite_address: None,
            internet: true,
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NetConfig {
    pub servers: Vec<SavedServer>,
    /// Most recent first.
    pub recent: Vec<RecentServer>,
    pub host: HostConfig,
}

impl NetConfig {
    /// Moves a server to the top of the recent list (one entry per address).
    fn remember(&mut self, server: RecentServer) {
        self.recent.retain(|r| r.url != server.url);
        self.recent.insert(0, server);
        self.recent.truncate(MAX_RECENT);
    }
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
        404 => {
            let msg = resp.json::<Value>().await.ok().and_then(|v| v["error"].as_str().map(str::to_owned));
            Err(if msg.as_deref() == Some("Folder not found") { MISSING_FOLDER } else { MISSING_CANVAS }.into())
        }
        _ => {
            let status = resp.status();
            let msg = resp.json::<Value>().await.ok().and_then(|v| v["error"].as_str().map(str::to_owned));
            Err(msg.unwrap_or_else(|| format!("{name} answered {status}.")))
        }
    }
}

const MISSING_CANVAS: &str = "That canvas doesn’t exist anymore.";
const MISSING_FOLDER: &str = "That folder doesn’t exist anymore.";

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
pub fn list_servers(net: Net_, host: State<host::HostState>) -> Vec<ServerView> {
    let config = net.config();
    let mut list = Vec::new();
    if host::is_running() {
        list.push(ServerView {
            id: HOST_ID.into(),
            name: config.host.name.clone(),
            // Empty while the internet link is on its way.
            url: host::invite_url(&config.host, &host).unwrap_or_default(),
            is_host: true,
        });
    }
    list.extend(config.servers.into_iter().map(|s| ServerView { id: s.id, name: s.name, url: s.url, is_host: false }));
    list
}

/// Checks the address and token, then remembers the server.
#[tauri::command]
pub async fn add_server(net: Net_<'_>, url: String, token: String) -> Result<ServerView> {
    connect_server(&net, &url, &token).await
}

async fn connect_server(net: &Net, url: &str, token: &str) -> Result<ServerView> {
    let url = normalize_url(url)?;
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
        c.remember(RecentServer { url: url.clone(), token: token.clone(), name: name.clone(), last_connected: storage::now_ms() });
    })?;
    let s = saved.expect("saved");
    Ok(ServerView { id: s.id, name: s.name, url: s.url, is_host: false })
}

#[tauri::command]
pub fn remove_server(net: Net_, id: String) -> Result<()> {
    net.update_config(|c| {
        let Some(i) = c.servers.iter().position(|s| s.id == id) else { return };
        let s = c.servers.remove(i);
        // Servers joined before recents existed: keep them rejoinable too.
        if !c.recent.iter().any(|r| r.url == s.url) {
            c.remember(RecentServer { url: s.url, token: s.token, name: s.name, last_connected: storage::now_ms() });
        }
    })
    .map(|_| ())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentView {
    url: String,
    name: String,
    last_connected: f64,
}

/// Recent connections that aren't in your server list right now.
#[tauri::command]
pub fn list_recent_servers(net: Net_) -> Vec<RecentView> {
    let config = net.config();
    config
        .recent
        .into_iter()
        .filter(|r| !config.servers.iter().any(|s| s.url == r.url))
        .map(|r| RecentView { url: r.url, name: r.name, last_connected: r.last_connected })
        .collect()
}

/// Joins a recent server again with the token it had last time.
#[tauri::command]
pub async fn reconnect_server(net: Net_<'_>, url: String) -> Result<ServerView> {
    let token = net
        .config()
        .recent
        .into_iter()
        .find(|r| r.url == url)
        .map(|r| r.token)
        .ok_or("This connection isn’t in your recent list anymore.")?;
    connect_server(&net, &url, &token).await
}

/// Drops a recent connection, token and all.
#[tauri::command]
pub fn forget_server(net: Net_, url: String) -> Result<()> {
    net.update_config(|c| c.recent.retain(|r| r.url != url)).map(|_| ())
}

/// The `drawlycool://` invite link for a server or one of its canvases.
#[tauri::command]
pub fn invite_link(
    net: Net_,
    host: State<host::HostState>,
    server_id: String,
    canvas_id: Option<String>,
) -> Result<String> {
    let config = net.config();
    let (url, token) = if server_id == HOST_ID {
        (host::invite_url(&config.host, &host)?, config.host.token.clone())
    } else {
        let s = config.servers.iter().find(|s| s.id == server_id).ok_or("Unknown server")?;
        (s.url.clone(), s.token.clone())
    };
    Ok(drawlycool_server::invite_link(&url, &token, canvas_id.as_deref()))
}

// ------------------------------------------------------------------ AI assistants

/// Ready-to-paste settings for one address of a server's `/mcp` endpoint.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSnippets {
    /// `<base>/mcp[?canvas=…]`, used with an `Authorization` header.
    url: String,
    /// `claude mcp add …` for Terminal.
    claude_code: String,
    /// `{"mcpServers": …}` for apps that speak HTTP MCP (Cursor, VS Code…).
    json_config: String,
    /// The URL with the token in it, for apps where you can only paste a URL
    /// (claude.ai / Claude Desktop custom connectors).
    connector_url: String,
    /// `{"mcpServers": …}` running `npx mcp-remote`, for apps that only start
    /// local (stdio) servers.
    bridge_config: String,
}

/// How an AI assistant joins a server (or one canvas on it) over MCP.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConnectInfo {
    server_name: String,
    canvas_id: Option<String>,
    canvas_name: Option<String>,
    token: String,
    /// Name the assistant's settings use for this server, e.g. `drawlycool-roadmap`.
    mcp_name: String,
    /// For assistants on this Mac (hosted servers only).
    local_url: Option<String>,
    /// For other machines and cloud apps.
    remote_url: Option<String>,
    local: Option<AiSnippets>,
    remote: Option<AiSnippets>,
    /// Why cloud apps such as claude.ai can't connect (yet).
    cloud_note: Option<String>,
    /// The internet link may show up soon, so it's worth asking again.
    waiting: bool,
}

/// Addresses and ready-made settings for connecting an AI assistant to a
/// server's MCP endpoint, starting in `canvas_id` if given.
#[tauri::command]
pub fn ai_connect_info(
    net: Net_,
    host: State<host::HostState>,
    server_id: String,
    canvas_id: Option<String>,
    canvas_name: Option<String>,
) -> Result<AiConnectInfo> {
    let (base, token, server_name) = net.endpoint(&server_id)?;
    if canvas_id.as_deref().is_some_and(|id| !storage::is_valid_id(id)) {
        return Err("Invalid canvas id".into());
    }
    let canvas_name = canvas_name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
    let (local_url, remote_url, cloud_note, waiting) = if server_id == HOST_ID {
        let reach = host::reach(&net, &host);
        (Some(base), reach.url, reach.cloud_note, reach.waiting)
    } else {
        let note = base.starts_with("http://").then(|| {
            format!("Cloud apps like claude.ai need a public https:// address, and {server_name} uses plain http.")
        });
        (None, Some(base), note, false)
    };

    let slug = slug(canvas_name.as_deref().unwrap_or(&server_name));
    let mcp_name = if slug.is_empty() { "drawlycool".to_owned() } else { format!("drawlycool-{slug}") };
    let snippets =
        |base: &String| ai_snippets(&mcp_name, &mcp_url(base, None, canvas_id.as_deref()), base, &token, canvas_id.as_deref());
    Ok(AiConnectInfo {
        local: local_url.as_ref().map(snippets),
        remote: remote_url.as_ref().map(snippets),
        local_url: local_url.as_ref().map(|b| mcp_url(b, None, canvas_id.as_deref())),
        remote_url: remote_url.as_ref().map(|b| mcp_url(b, None, canvas_id.as_deref())),
        server_name,
        canvas_id,
        canvas_name,
        token,
        mcp_name,
        cloud_note,
        waiting,
    })
}

/// `<base>/mcp`, with `token` and `canvas` in the query when given.
fn mcp_url(base: &str, token: Option<&str>, canvas: Option<&str>) -> String {
    let query: Vec<String> = [("token", token), ("canvas", canvas)]
        .into_iter()
        .filter_map(|(key, value)| value.map(|v| format!("{key}={}", encode(v))))
        .collect();
    let base = base.trim_end_matches('/');
    if query.is_empty() {
        format!("{base}/mcp")
    } else {
        format!("{base}/mcp?{}", query.join("&"))
    }
}

fn ai_snippets(name: &str, url: &str, base: &str, token: &str, canvas: Option<&str>) -> AiSnippets {
    let auth = format!("Bearer {token}");
    let claude_code = format!(
        "claude mcp add --transport http --scope user {} {} --header {}",
        sh_quote(name),
        sh_quote(url),
        sh_quote(&format!("Authorization: {auth}"))
    );
    let json_config = json!({
        "mcpServers": { name: { "type": "http", "url": url, "headers": { "Authorization": auth } } }
    });
    // mcp-remote refuses plain http unless told otherwise.
    let mut args = vec!["-y".to_owned(), "mcp-remote".into(), url.into(), "--header".into(), format!("Authorization: {auth}")];
    if url.starts_with("http://") {
        args.push("--allow-http".into());
    }
    let bridge_config = json!({ "mcpServers": { name: { "command": "npx", "args": args } } });
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default();
    AiSnippets {
        url: url.to_owned(),
        claude_code,
        json_config: pretty(&json_config),
        connector_url: mcp_url(base, Some(token), canvas),
        bridge_config: pretty(&bridge_config),
    }
}

/// Lowercase ASCII words joined by dashes, at most 32 characters.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(32);
    out.trim_end_matches('-').to_owned()
}

/// Quotes a word for zsh/bash, leaving plain ones alone.
fn sh_quote(s: &str) -> String {
    let plain = |b: u8| b.is_ascii_alphanumeric() || b"-_./:@%+=,".contains(&b);
    if !s.is_empty() && s.bytes().all(plain) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_snippets_quote_and_encode() {
        let base = "http://127.0.0.1:4180";
        let s = ai_snippets("drawlycool-plan", &mcp_url(base, None, Some("a b")), base, "t&k", Some("a b"));
        assert_eq!(s.url, "http://127.0.0.1:4180/mcp?canvas=a%20b");
        assert_eq!(s.connector_url, "http://127.0.0.1:4180/mcp?token=t%26k&canvas=a%20b");
        assert_eq!(
            s.claude_code,
            "claude mcp add --transport http --scope user drawlycool-plan 'http://127.0.0.1:4180/mcp?canvas=a%20b' --header 'Authorization: Bearer t&k'"
        );
        let bridge: Value = serde_json::from_str(&s.bridge_config).unwrap();
        assert_eq!(bridge["mcpServers"]["drawlycool-plan"]["args"][5], "--allow-http");
        let config: Value = serde_json::from_str(&s.json_config).unwrap();
        assert_eq!(config["mcpServers"]["drawlycool-plan"]["headers"]["Authorization"], "Bearer t&k");
        assert_eq!(mcp_url("https://x.trycloudflare.com", None, None), "https://x.trycloudflare.com/mcp");
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Rafa’s Mac"), "rafa-s-mac");
        assert_eq!(slug("  Q3 Roadmap!! "), "q3-roadmap");
        assert_eq!(slug("日本"), "");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }
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
pub async fn remote_create(net: Net_<'_>, server_id: String, name: String, folder: Option<String>) -> Result<Value> {
    let body = json!({ "name": name, "folder": folder });
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
pub async fn remote_delete(net: Net_<'_>, server_id: String, canvas_id: String) -> Result<bool> {
    call(&net, &server_id, reqwest::Method::DELETE, &format!("/api/canvases/{canvas_id}"), None).await?;
    Ok(true)
}

#[tauri::command]
pub async fn remote_set_folder(net: Net_<'_>, server_id: String, canvas_id: String, folder: Option<String>) -> Result<Value> {
    let path = format!("/api/canvases/{canvas_id}");
    json_of(call(&net, &server_id, reqwest::Method::PATCH, &path, Some(json!({ "folder": folder }))).await?).await
}

/// A server's folders, or null if it's from before folders existed.
#[tauri::command]
pub async fn remote_folders(net: Net_<'_>, server_id: String) -> Result<Option<Value>> {
    match call(&net, &server_id, reqwest::Method::GET, "/api/folders", None).await {
        Ok(resp) => json_of(resp).await.map(Some),
        // Older servers don't have the route at all.
        Err(e) if e == MISSING_CANVAS => Ok(None),
        Err(e) => Err(e),
    }
}

#[tauri::command]
pub async fn remote_create_folder(net: Net_<'_>, server_id: String, name: String) -> Result<Value> {
    json_of(call(&net, &server_id, reqwest::Method::POST, "/api/folders", Some(json!({ "name": name }))).await?).await
}

#[tauri::command]
pub async fn remote_rename_folder(net: Net_<'_>, server_id: String, folder_id: String, name: String) -> Result<Value> {
    let path = format!("/api/folders/{folder_id}");
    json_of(call(&net, &server_id, reqwest::Method::PATCH, &path, Some(json!({ "name": name }))).await?).await
}

#[tauri::command]
pub async fn remote_delete_folder(net: Net_<'_>, server_id: String, folder_id: String) -> Result<()> {
    call(&net, &server_id, reqwest::Method::DELETE, &format!("/api/folders/{folder_id}"), None).await?;
    Ok(())
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

/// Moves a canvas between this Mac (`None`) and servers, into `folder` there
/// if given. The original is removed only once the copy has arrived.
/// Resolves with the copy's metadata.
#[tauri::command]
pub async fn move_canvas(
    net: Net_<'_>,
    store: crate::Store<'_>,
    canvas_id: String,
    name: String,
    from: Option<String>,
    to: Option<String>,
    folder: Option<String>,
) -> Result<Value> {
    if from == to {
        return Err("It’s already there.".into());
    }
    let scene = match &from {
        None => String::from_utf8(crate::lock(&store).load_scene(&canvas_id)?).map_err(|e| e.to_string())?,
        Some(server) => {
            let path = format!("/api/canvases/{canvas_id}/scene");
            call(&net, server, reqwest::Method::GET, &path, None).await?.text().await.map_err(|e| e.to_string())?
        }
    };
    let copy = match &to {
        None => serde_json::to_value(crate::lock(&store).create(&name, Some(&scene), folder.as_deref())?)
            .map_err(|e| e.to_string())?,
        Some(server) => {
            let body = json!({ "name": name, "scene": scene, "folder": folder });
            json_of(call(&net, server, reqwest::Method::POST, "/api/canvases", Some(body)).await?).await?
        }
    };
    match &from {
        None => crate::lock(&store).remove(&canvas_id)?,
        Some(server) => {
            call(&net, server, reqwest::Method::DELETE, &format!("/api/canvases/{canvas_id}"), None).await?;
        }
    }
    Ok(copy)
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

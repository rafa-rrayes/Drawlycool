//! Drawlycool sync server.
//!
//! Hosts a folder of canvases that several Drawlycool apps edit at the
//! same time. Runs standalone (`drawlycool-server`) or inside the desktop app
//! when it "hosts" canvases.
//!
//! HTTP API (every `/api` route needs `Authorization: Bearer <token>`):
//!   GET    /api/info                     -> { name, version, protocol }
//!   GET    /api/canvases                 -> [{ id, name, createdAt, updatedAt, peers }]
//!   POST   /api/canvases                 { name, scene? } -> meta
//!   PATCH  /api/canvases/{id}            { name } -> meta
//!   POST   /api/canvases/{id}/duplicate  -> meta
//!   DELETE /api/canvases/{id}
//!   GET    /api/canvases/{id}/scene      -> .excalidraw JSON
//!   GET    /api/canvases/{id}/ws         -> live session (see `ClientMsg`)
//!
//! Live sessions exchange JSON text frames. The client opens with `hello`, the
//! server answers with `init` (the whole scene and who's there), then both
//! sides send changes. Elements are merged with Excalidraw's own rules (see
//! `scene.rs`), so every client converges on the same canvas.

pub mod fractional;
pub mod mcp;
pub mod scene;
pub mod storage;

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use scene::Scene;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use storage::{CanvasMeta, Storage};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::task::JoinHandle;

pub const PROTOCOL: u32 = 1;
const SAVE_DELAY: Duration = Duration::from_secs(1);
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_MESSAGE: usize = 256 << 20;

pub struct Config {
    /// Folder holding `canvases/` (same layout as the desktop app's data).
    pub data_dir: PathBuf,
    pub addr: SocketAddr,
    pub token: String,
    /// Shown to people who connect, e.g. "Rafa's Mac".
    pub name: String,
}

/// Generates a random access token: 122 random bits as 22 letters and digits.
pub fn new_token() -> String {
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut n = uuid::Uuid::new_v4().as_u128();
    (0..22)
        .map(|_| {
            let d = DIGITS[(n % 62) as usize] as char;
            n /= 62;
            d
        })
        .collect()
}

/// The `drawlycool://` invite link for a server, or for one canvas on it.
///
/// Short form: `drawlycool://host[:port]/<token>[/<canvas>]`, where a port
/// means `http://` and no port means `https://`. That covers LAN servers,
/// tunnels and TLS deployments. Anything else (an https port, a path, an IPv6
/// literal) uses `drawlycool://connect?server=…&token=…[&canvas=…]`.
pub fn invite_link(server_url: &str, token: &str, canvas: Option<&str>) -> String {
    let url = server_url.trim().trim_end_matches('/');
    let (https, authority) = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(rest), _) => (true, rest),
        (_, Some(rest)) => (false, rest),
        _ => (false, ""),
    };
    let has_port = authority.rsplit_once(':').is_some_and(|(_, p)| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    let plain_host = !authority.is_empty() && !authority.contains(['/', '[', '@', '?', '#']);
    let safe = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.~".contains(&b));
    if plain_host && https != has_port && safe(token) && canvas.is_none_or(safe) {
        let mut link = format!("drawlycool://{authority}/{token}");
        if let Some(c) = canvas {
            link.push('/');
            link.push_str(c);
        }
        return link;
    }
    let mut link = format!("drawlycool://connect?server={}&token={}", encode(url), encode(token));
    if let Some(c) = canvas {
        link.push_str(&format!("&canvas={}", encode(c)));
    }
    link
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ------------------------------------------------------------------ state

enum Outgoing {
    Text(Utf8Bytes),
    Close(u16, &'static str),
}

struct Peer {
    name: String,
    color: String,
    /// Stable per-person id (survives reconnects); Excalidraw derives the
    /// cursor colour from it.
    user_id: String,
    tx: UnboundedSender<Outgoing>,
}

impl Peer {
    fn to_json(&self, id: &str) -> Value {
        json!({ "id": id, "name": self.name, "color": self.color, "userId": self.user_id })
    }
}

struct RoomState {
    scene: Scene,
    peers: HashMap<String, Peer>,
    /// peer id -> ids of the peers following them.
    followers: HashMap<String, HashSet<String>>,
    dirty: bool,
    save_scheduled: bool,
    deleted: bool,
}

struct Room {
    id: String,
    state: Mutex<RoomState>,
    /// Serialises saves so an older snapshot can't overwrite a newer one.
    save_lock: tokio::sync::Mutex<()>,
}

struct Hub {
    name: String,
    token: String,
    storage: Mutex<Storage>,
    rooms: Mutex<HashMap<String, Arc<Room>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn text(value: Value) -> Utf8Bytes {
    Utf8Bytes::from(value.to_string())
}

impl RoomState {
    fn send_to(&self, peer: &str, msg: &Utf8Bytes) {
        if let Some(p) = self.peers.get(peer) {
            let _ = p.tx.send(Outgoing::Text(msg.clone()));
        }
    }

    fn broadcast(&self, except: Option<&str>, msg: Utf8Bytes) {
        for (id, peer) in &self.peers {
            if Some(id.as_str()) != except {
                let _ = peer.tx.send(Outgoing::Text(msg.clone()));
            }
        }
    }

    fn peer_list(&self) -> Vec<Value> {
        self.peers.iter().map(|(id, p)| p.to_json(id)).collect()
    }

    fn notify_followers_changed(&self, target: &str) {
        let ids: Vec<&String> = self.followers.get(target).map(|s| s.iter().collect()).unwrap_or_default();
        self.send_to(target, &text(json!({ "type": "followed-by", "ids": ids })));
    }
}

impl Hub {
    fn storage(&self) -> MutexGuard<'_, Storage> {
        lock(&self.storage)
    }

    /// The live room for a canvas, loading it from disk on first use.
    fn room(&self, id: &str) -> Result<Arc<Room>, ApiError> {
        let mut rooms = lock(&self.rooms);
        if let Some(room) = rooms.get(id) {
            return Ok(room.clone());
        }
        let bytes = self.storage().load_scene(id).map_err(|_| ApiError::not_found())?;
        let room = Arc::new(Room {
            id: id.to_owned(),
            state: Mutex::new(RoomState {
                scene: Scene::from_json(&bytes),
                peers: HashMap::new(),
                followers: HashMap::new(),
                dirty: false,
                save_scheduled: false,
                deleted: false,
            }),
            save_lock: tokio::sync::Mutex::new(()),
        });
        rooms.insert(id.to_owned(), room.clone());
        Ok(room)
    }

    fn loaded_room(&self, id: &str) -> Option<Arc<Room>> {
        lock(&self.rooms).get(id).cloned()
    }

    fn all_rooms(&self) -> Vec<Arc<Room>> {
        lock(&self.rooms).values().cloned().collect()
    }
}

async fn save_room(hub: &Arc<Hub>, room: &Arc<Room>) {
    let _guard = room.save_lock.lock().await;
    let bytes = {
        let mut st = lock(&room.state);
        if !st.dirty || st.deleted {
            return;
        }
        st.dirty = false;
        st.scene.prune(storage::now_ms());
        st.scene.to_json()
    };
    let (hub2, id) = (hub.clone(), room.id.clone());
    let result = tokio::task::spawn_blocking(move || hub2.storage().save_scene(&id, &bytes)).await;
    if !matches!(result, Ok(Ok(()))) {
        eprintln!("drawlycool-server: failed to save canvas {}", room.id);
        lock(&room.state).dirty = true;
    }
}

fn mark_dirty(hub: &Arc<Hub>, room: &Arc<Room>, st: &mut RoomState) {
    st.dirty = true;
    if st.save_scheduled {
        return;
    }
    st.save_scheduled = true;
    let (hub, room) = (hub.clone(), room.clone());
    tokio::spawn(async move {
        tokio::time::sleep(SAVE_DELAY).await;
        lock(&room.state).save_scheduled = false;
        save_room(&hub, &room).await;
    });
}

// ------------------------------------------------------------------ live sessions

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum ClientMsg {
    Hello {
        name: String,
        color: String,
        #[serde(default, rename = "userId")]
        user_id: String,
    },
    /// Changed elements (or the whole scene after a reconnect).
    Update {
        elements: Vec<Value>,
    },
    /// Image files (`BinaryFileData`) referenced by image elements.
    Files {
        files: Vec<Value>,
    },
    Background {
        color: String,
    },
    /// Cursor position, button and selection; relayed as-is.
    Pointer {
        #[serde(default)]
        pointer: Value,
        #[serde(default)]
        button: Value,
        #[serde(default, rename = "selectedElementIds")]
        selected_element_ids: Value,
    },
    /// Start/stop following another peer's viewport.
    Follow {
        target: String,
        follow: bool,
    },
    /// The sender's visible scene bounds, for whoever follows them.
    Bounds {
        bounds: Value,
    },
}

fn clip(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}

async fn ws_route(State(hub): State<Arc<Hub>>, Path(id): Path<String>, ws: WebSocketUpgrade) -> Response {
    if !hub.storage().exists(&id) {
        return ApiError::not_found().into_response();
    }
    ws.max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| session(hub, id, socket))
}

async fn session(hub: Arc<Hub>, canvas_id: String, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();

    let hello = match tokio::time::timeout(HELLO_TIMEOUT, stream.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<ClientMsg>(&t).ok(),
        _ => None,
    };
    let Some(ClientMsg::Hello { name, color, user_id }) = hello else {
        let _ = sink.send(close_frame(1008, "expected hello")).await;
        return;
    };
    let Ok(room) = hub.room(&canvas_id) else {
        let _ = sink.send(close_frame(4404, "This canvas no longer exists.")).await;
        return;
    };
    let meta = hub.storage().meta(&canvas_id).map(|(m, _)| m).ok();

    let peer_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_owned();
    let (tx, mut rx) = unbounded_channel::<Outgoing>();
    let joined = {
        let mut st = lock(&room.state);
        if st.deleted {
            false
        } else {
            let user_id = Some(clip(&user_id, 64)).filter(|u| !u.is_empty()).unwrap_or_else(|| peer_id.clone());
            let peer = Peer { name: clip(&name, 40), color: clip(&color, 32), user_id, tx: tx.clone() };
            let init = json!({
                "type": "init",
                "you": peer_id,
                "meta": meta,
                "elements": st.scene.elements(),
                "files": st.scene.files(),
                "background": st.scene.background(),
                "peers": st.peer_list(),
            });
            let _ = tx.send(Outgoing::Text(text(init)));
            st.broadcast(None, text(json!({ "type": "peer-joined", "peer": peer.to_json(&peer_id) })));
            st.peers.insert(peer_id.clone(), peer);
            true
        }
    };
    if !joined {
        let _ = sink.send(close_frame(4404, "This canvas no longer exists.")).await;
        return;
    }

    let writer = async {
        while let Some(out) = rx.recv().await {
            match out {
                Outgoing::Text(t) => {
                    if sink.send(Message::Text(t)).await.is_err() {
                        break;
                    }
                }
                Outgoing::Close(code, reason) => {
                    let _ = sink.send(close_frame(code, reason)).await;
                    break;
                }
            }
        }
    };
    let reader = async {
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                Message::Text(t) => handle_message(&hub, &room, &peer_id, &t),
                Message::Close(_) => break,
                _ => {}
            }
        }
    };
    tokio::select! {
        _ = writer => {}
        _ = reader => {}
    }

    leave(&room, &peer_id);
    let empty = lock(&room.state).peers.is_empty();
    if empty {
        save_room(&hub, &room).await;
    }
}

fn close_frame(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame { code, reason: Utf8Bytes::from(reason) }))
}

fn handle_message(hub: &Arc<Hub>, room: &Arc<Room>, me: &str, raw: &str) {
    let Ok(msg) = serde_json::from_str::<ClientMsg>(raw) else { return };
    let mut st = lock(&room.state);
    if st.deleted {
        return;
    }
    match msg {
        ClientMsg::Hello { .. } => {}
        ClientMsg::Update { elements } => {
            let accepted = st.scene.merge(elements);
            if !accepted.is_empty() {
                st.broadcast(Some(me), text(json!({ "type": "update", "from": me, "elements": accepted })));
                mark_dirty(hub, room, &mut st);
            }
        }
        ClientMsg::Files { files } => {
            let added = st.scene.add_files(files);
            if !added.is_empty() {
                st.broadcast(Some(me), text(json!({ "type": "files", "files": added })));
                mark_dirty(hub, room, &mut st);
            }
        }
        ClientMsg::Background { color } => {
            let color = clip(&color, 64);
            if st.scene.background() != Some(color.as_str()) {
                st.scene.set_background(&color);
                st.broadcast(Some(me), text(json!({ "type": "background", "color": color })));
                mark_dirty(hub, room, &mut st);
            }
        }
        ClientMsg::Pointer { pointer, button, selected_element_ids } => {
            let msg = json!({
                "type": "pointer",
                "from": me,
                "pointer": pointer,
                "button": button,
                "selectedElementIds": selected_element_ids,
            });
            st.broadcast(Some(me), text(msg));
        }
        ClientMsg::Follow { target, follow } => {
            if target == me || !st.peers.contains_key(&target) {
                return;
            }
            let set = st.followers.entry(target.clone()).or_default();
            let changed = if follow { set.insert(me.to_owned()) } else { set.remove(me) };
            if changed {
                st.notify_followers_changed(&target);
            }
        }
        ClientMsg::Bounds { bounds } => {
            if let Some(followers) = st.followers.get(me) {
                let msg = text(json!({ "type": "bounds", "from": me, "bounds": bounds }));
                for f in followers {
                    st.send_to(f, &msg);
                }
            }
        }
    }
}

fn leave(room: &Arc<Room>, me: &str) {
    let mut st = lock(&room.state);
    st.peers.remove(me);
    st.followers.remove(me);
    let mut lost_follower = Vec::new();
    for (target, set) in st.followers.iter_mut() {
        if set.remove(me) {
            lost_follower.push(target.clone());
        }
    }
    for target in lost_follower {
        st.notify_followers_changed(&target);
    }
    st.broadcast(None, text(json!({ "type": "peer-left", "id": me })));
}

// ------------------------------------------------------------------ HTTP API

struct ApiError(StatusCode, String);

impl ApiError {
    fn not_found() -> Self {
        Self(StatusCode::NOT_FOUND, "Canvas not found".into())
    }
    fn bad_request(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

/// Storage errors on a known canvas are "not found" (bad id) or I/O failures.
fn storage_err(e: String) -> ApiError {
    if e.starts_with("Canvas not found") || e.starts_with("Invalid canvas id") {
        ApiError::not_found()
    } else {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, e)
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn auth(State(hub): State<Arc<Hub>>, request: Request, next: Next) -> Response {
    let from_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);
    let from_query = request
        .uri()
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")))
        .map(str::to_owned);
    let ok = from_header.or(from_query).is_some_and(|t| constant_time_eq(t.as_bytes(), hub.token.as_bytes()));
    if !ok {
        return ApiError(StatusCode::UNAUTHORIZED, "Invalid access token".into()).into_response();
    }
    next.run(request).await
}

async fn info(State(hub): State<Arc<Hub>>) -> Json<Value> {
    Json(json!({ "name": hub.name, "version": env!("CARGO_PKG_VERSION"), "protocol": PROTOCOL }))
}

fn meta_json(meta: &CanvasMeta, peers: Vec<Value>) -> Value {
    let mut v = serde_json::to_value(meta).unwrap_or_default();
    v["peers"] = Value::Array(peers);
    v
}

async fn list(State(hub): State<Arc<Hub>>) -> Json<Vec<Value>> {
    let metas = hub.storage().list();
    let rooms = lock(&hub.rooms);
    Json(
        metas
            .iter()
            .map(|m| {
                let peers = rooms.get(&m.id).map(|r| lock(&r.state).peer_list()).unwrap_or_default();
                meta_json(m, peers)
            })
            .collect(),
    )
}

#[derive(Deserialize)]
struct CreateBody {
    name: String,
    /// An existing `.excalidraw` scene to start from (e.g. a local canvas).
    scene: Option<String>,
}

async fn create(State(hub): State<Arc<Hub>>, Json(body): Json<CreateBody>) -> Result<Json<Value>, ApiError> {
    if let Some(scene) = &body.scene {
        if !storage::is_excalidraw_scene(scene) {
            return Err(ApiError::bad_request("Not a valid Excalidraw scene"));
        }
    }
    let meta = hub.storage().create(&body.name, body.scene.as_deref()).map_err(storage_err)?;
    Ok(Json(meta_json(&meta, vec![])))
}

#[derive(Deserialize)]
struct RenameBody {
    name: String,
}

async fn rename(
    State(hub): State<Arc<Hub>>,
    Path(id): Path<String>,
    Json(body): Json<RenameBody>,
) -> Result<Json<Value>, ApiError> {
    let meta = hub.storage().rename(&id, &body.name).map_err(storage_err)?;
    if let Some(room) = hub.loaded_room(&id) {
        lock(&room.state).broadcast(None, text(json!({ "type": "meta", "meta": meta })));
    }
    Ok(Json(meta_json(&meta, vec![])))
}

async fn duplicate(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    if let Some(room) = hub.loaded_room(&id) {
        save_room(&hub, &room).await;
    }
    let meta = hub.storage().duplicate(&id).map_err(storage_err)?;
    Ok(Json(meta_json(&meta, vec![])))
}

async fn remove(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    if let Some(room) = lock(&hub.rooms).remove(&id) {
        let mut st = lock(&room.state);
        st.deleted = true;
        let msg = text(json!({ "type": "deleted" }));
        for peer in st.peers.values() {
            let _ = peer.tx.send(Outgoing::Text(msg.clone()));
            let _ = peer.tx.send(Outgoing::Close(4410, "This canvas was deleted."));
        }
        st.peers.clear();
    }
    hub.storage().remove(&id).map_err(storage_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn scene(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> Result<Response, ApiError> {
    let bytes = match hub.loaded_room(&id) {
        Some(room) => lock(&room.state).scene.to_json(),
        None => hub.storage().load_scene(&id).map_err(storage_err)?,
    };
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}

fn router(hub: Arc<Hub>) -> Router {
    let api = Router::new()
        .route("/info", get(info))
        .route("/canvases", get(list).post(create))
        .route("/canvases/{id}", axum::routing::patch(rename).delete(remove))
        .route("/canvases/{id}/duplicate", post(duplicate))
        .route("/canvases/{id}/scene", get(scene))
        .route("/canvases/{id}/ws", get(ws_route))
        .layer(middleware::from_fn_with_state(hub.clone(), auth));
    Router::new()
        .route("/", get(|| async { "Drawlycool sync server. Open an invite link in Drawlycool to connect.\n" }))
        .nest("/api", api)
        .layer(DefaultBodyLimit::max(MAX_MESSAGE))
        .with_state(hub)
}

// ------------------------------------------------------------------ running

pub struct Server {
    pub local_addr: SocketAddr,
    hub: Arc<Hub>,
    stop: tokio::sync::oneshot::Sender<()>,
    task: JoinHandle<()>,
    sweeper: JoinHandle<()>,
}

impl Server {
    pub async fn start(config: Config) -> std::io::Result<Server> {
        let storage = Storage::open(config.data_dir)?;
        let hub = Arc::new(Hub {
            name: config.name,
            token: config.token,
            storage: Mutex::new(storage),
            rooms: Mutex::new(HashMap::new()),
        });
        let listener = tokio::net::TcpListener::bind(config.addr).await?;
        let local_addr = listener.local_addr()?;
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let app = router(hub.clone());
        let task = tokio::spawn(async move {
            let shutdown = async {
                let _ = stopped.await;
            };
            if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown).await {
                eprintln!("drawlycool-server: {e}");
            }
        });

        // Unload rooms nobody has open (they're already saved).
        let sweep_hub = hub.clone();
        let sweeper = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(300)).await;
                lock(&sweep_hub.rooms).retain(|_, room| {
                    let st = lock(&room.state);
                    !st.peers.is_empty() || st.dirty || st.save_scheduled
                });
            }
        });

        Ok(Server { local_addr, hub, stop, task, sweeper })
    }

    /// Disconnects everyone, saves every canvas and stops listening.
    pub async fn stop(self) {
        self.sweeper.abort();
        for room in self.hub.all_rooms() {
            let st = lock(&room.state);
            for peer in st.peers.values() {
                let _ = peer.tx.send(Outgoing::Close(1001, "server stopping"));
            }
        }
        self.flush().await;
        let _ = self.stop.send(());
        let mut task = self.task;
        if tokio::time::timeout(Duration::from_secs(3), &mut task).await.is_err() {
            task.abort();
        }
    }

    /// Saves every canvas with unsaved changes, without stopping.
    pub async fn flush(&self) {
        for room in self.hub.all_rooms() {
            save_room(&self.hub, &room).await;
        }
    }
}

#[cfg(test)]
mod invite_tests {
    use super::*;

    #[test]
    fn tokens_are_short_and_url_safe() {
        let t = new_token();
        assert_eq!(t.len(), 22);
        assert!(t.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_ne!(t, new_token());
    }

    #[test]
    fn invite_links_are_short_when_they_can_be() {
        let link = |url, canvas| invite_link(url, "Tok3n", canvas);
        assert_eq!(link("https://tidy-otter.trycloudflare.com", None), "drawlycool://tidy-otter.trycloudflare.com/Tok3n");
        assert_eq!(link("https://draw.example.com/", Some("c-1")), "drawlycool://draw.example.com/Tok3n/c-1");
        assert_eq!(link("http://Rafas-Mac.local:4180", None), "drawlycool://Rafas-Mac.local:4180/Tok3n");
        assert_eq!(link("http://192.168.1.4:4180", Some("c-1")), "drawlycool://192.168.1.4:4180/Tok3n/c-1");
        // Port and scheme don't match the short form's rule: spelled out.
        assert_eq!(link("http://example.com", None), "drawlycool://connect?server=http%3A%2F%2Fexample.com&token=Tok3n");
        assert_eq!(
            link("https://example.com:8443", Some("c-1")),
            "drawlycool://connect?server=https%3A%2F%2Fexample.com%3A8443&token=Tok3n&canvas=c-1"
        );
        assert_eq!(
            link("https://example.com/draw", None),
            "drawlycool://connect?server=https%3A%2F%2Fexample.com%2Fdraw&token=Tok3n"
        );
        assert_eq!(
            invite_link("https://example.com", "a b", None),
            "drawlycool://connect?server=https%3A%2F%2Fexample.com&token=a%20b"
        );
    }
}

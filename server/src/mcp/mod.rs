//! MCP (Model Context Protocol) server: lets an AI assistant join shared
//! canvases as a live participant and draw on them.
//!
//! Transport: Streamable HTTP at `/mcp`, answering every request with plain
//! JSON (no server-initiated streams). Auth is the server's access token, as
//! `Authorization: Bearer <token>` or `?token=`. Optional query parameters:
//! `canvas=<id>` opens that canvas right away, `name=<name>` sets the name
//! people see next to the assistant's cursor.

mod agent;
pub mod args;
pub mod build;
pub mod colors;
pub mod describe;
mod diagram;
pub mod draft;
mod draw;
pub mod geometry;
mod guide;
pub mod layout;
pub mod model;
pub mod render;
pub mod text;
mod tools;

use crate::{lock, Hub};
use agent::Agent;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SUPPORTED_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const SESSION_HEADER: &str = "mcp-session-id";
/// The assistant leaves the canvas after this long without a tool call.
const IDLE_LEAVE: Duration = Duration::from_secs(15 * 60);
/// Sessions are forgotten after this long without any request.
const IDLE_FORGET: Duration = Duration::from_secs(6 * 3600);

pub(crate) struct Session {
    pub agent: tokio::sync::Mutex<Agent>,
    /// Canvas from the connection URL, opened on first use.
    pub start_canvas: Option<String>,
    last_seen: Mutex<Instant>,
}

impl Session {
    fn touch(&self) {
        *lock(&self.last_seen) = Instant::now();
    }
    fn idle(&self) -> Duration {
        lock(&self.last_seen).elapsed()
    }
}

#[derive(Default)]
pub(crate) struct Sessions {
    map: Mutex<HashMap<String, Arc<Session>>>,
    sweeping: AtomicBool,
}

impl Sessions {
    fn get(&self, id: &str) -> Option<Arc<Session>> {
        lock(&self.map).get(id).cloned()
    }

    fn insert(&self, id: String, session: Arc<Session>) {
        lock(&self.map).insert(id, session);
    }

    fn remove(&self, id: &str) -> Option<Arc<Session>> {
        lock(&self.map).remove(id)
    }
}

/// Leaves idle canvases and forgets abandoned sessions.
fn start_sweeper(hub: &Arc<Hub>) {
    if hub.mcp.sweeping.swap(true, Ordering::SeqCst) {
        return;
    }
    let hub = hub.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let sessions: Vec<(String, Arc<Session>)> = lock(&hub.mcp.map).iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            for (id, session) in sessions {
                let idle = session.idle();
                if idle > IDLE_LEAVE {
                    if let Ok(mut agent) = session.agent.try_lock() {
                        agent.leave(&hub);
                    }
                }
                if idle > IDLE_FORGET {
                    hub.mcp.remove(&id);
                }
            }
        }
    });
}

/// The name people see: `?name=`, else the client's own name.
fn display_name(query: &HashMap<String, String>, client: &Value) -> String {
    if let Some(n) = query.get("name").map(|n| n.trim()).filter(|n| !n.is_empty()) {
        return n.chars().take(40).collect();
    }
    let raw = client.get("title").or_else(|| client.get("name")).and_then(Value::as_str).unwrap_or("");
    let lower = raw.to_ascii_lowercase();
    let known = [("claude", "Claude"), ("cursor", "Cursor"), ("codex", "Codex"), ("openai", "ChatGPT"), ("chatgpt", "ChatGPT"),
        ("gemini", "Gemini"), ("copilot", "Copilot"), ("windsurf", "Windsurf"), ("zed", "Zed"), ("goose", "Goose")];
    if let Some((_, name)) = known.iter().find(|(k, _)| lower.contains(k)) {
        return (*name).to_owned();
    }
    let cleaned: String = raw.replace(['-', '_'], " ").split_whitespace().map(|w| {
        let mut c = w.chars();
        c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
    }).collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() { "AI assistant".into() } else { cleaned.chars().take(40).collect() }
}

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn json_response(status: StatusCode, body: Value, session: Option<&str>) -> Response {
    let mut resp = (status, axum::Json(body)).into_response();
    if let Some(id) = session.and_then(|s| HeaderValue::from_str(s).ok()) {
        resp.headers_mut().insert(SESSION_HEADER, id);
    }
    resp
}

pub(crate) async fn post(
    State(hub): State<Arc<Hub>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    start_sweeper(&hub);
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return json_response(StatusCode::BAD_REQUEST, rpc_error(&Value::Null, -32700, "Parse error"), None);
    };
    let (messages, batch) = match parsed {
        Value::Array(list) => (list, true),
        single => (vec![single], false),
    };
    let header_session = headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok()).map(str::to_owned);

    // A new session starts with `initialize`.
    if let Some(init) = messages.iter().find(|m| m["method"] == "initialize") {
        let id = init.get("id").cloned().unwrap_or(Value::Null);
        let params = &init["params"];
        let requested = params["protocolVersion"].as_str().unwrap_or("");
        let version = SUPPORTED_VERSIONS.iter().find(|v| **v == requested).copied().unwrap_or(SUPPORTED_VERSIONS[1]);
        let session_id = uuid::Uuid::new_v4().simple().to_string();
        let name = display_name(&query, &params["clientInfo"]);
        let session = Arc::new(Session {
            agent: tokio::sync::Mutex::new(Agent::new(name)),
            start_canvas: query.get("canvas").map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()),
            last_seen: Mutex::new(Instant::now()),
        });
        hub.mcp.insert(session_id.clone(), session.clone());
        let result = json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "drawlycool", "title": format!("Drawlycool — {}", hub.name), "version": env!("CARGO_PKG_VERSION") },
            "instructions": guide::instructions(session.start_canvas.is_some()),
        });
        return json_response(StatusCode::OK, rpc_result(&id, result), Some(&session_id));
    }

    let session = match &header_session {
        Some(id) => match hub.mcp.get(id) {
            Some(s) => s,
            // Unknown (e.g. the server restarted): the client should start over.
            None => return json_response(StatusCode::NOT_FOUND, rpc_error(&Value::Null, -32001, "Session not found"), None),
        },
        // Clients that don't keep the session header share one anonymous session.
        None => {
            let anon = format!("anon-{}", query.get("canvas").map(String::as_str).unwrap_or(""));
            hub.mcp.get(&anon).unwrap_or_else(|| {
                let s = Arc::new(Session {
                    agent: tokio::sync::Mutex::new(Agent::new(display_name(&query, &Value::Null))),
                    start_canvas: query.get("canvas").cloned().filter(|c| !c.is_empty()),
                    last_seen: Mutex::new(Instant::now()),
                });
                hub.mcp.insert(anon, s.clone());
                s
            })
        }
    };
    session.touch();

    let mut replies = Vec::new();
    for msg in messages {
        let Some(id) = msg.get("id").cloned().filter(|i| !i.is_null()) else { continue }; // notification or response
        let method = msg["method"].as_str().unwrap_or("");
        let reply = match method {
            "ping" => rpc_result(&id, json!({})),
            "tools/list" => rpc_result(&id, json!({ "tools": tools::definitions() })),
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("");
                let arguments = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
                let outcome = tools::call(&hub, &session, name, &arguments).await;
                rpc_result(&id, outcome)
            }
            "resources/list" => rpc_result(&id, json!({ "resources": [] })),
            "prompts/list" => rpc_result(&id, json!({ "prompts": [] })),
            "" => rpc_error(&id, -32600, "Invalid request"),
            _ => rpc_error(&id, -32601, &format!("Method not found: {method}")),
        };
        replies.push(reply);
    }
    session.touch();
    if replies.is_empty() {
        return StatusCode::ACCEPTED.into_response();
    }
    let body = if batch { Value::Array(replies) } else { replies.remove(0) };
    json_response(StatusCode::OK, body, header_session.as_deref())
}

/// No server-to-client stream: every reply comes with its request.
pub(crate) async fn get() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [("allow", "POST, DELETE")]).into_response()
}

/// The client is done: leave the canvas.
pub(crate) async fn delete(State(hub): State<Arc<Hub>>, headers: HeaderMap) -> Response {
    let Some(id) = headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match hub.mcp.remove(id) {
        Some(session) => {
            session.agent.lock().await.leave(&hub);
            StatusCode::NO_CONTENT.into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

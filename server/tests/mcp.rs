//! The MCP endpoint end to end: an assistant connects over HTTP, joins a
//! canvas and draws, while a person connected over WebSocket watches.

use drawlycool_server::{Config, Server};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "mcp-token";

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn start(name: &str) -> Server {
    let dir = std::env::temp_dir().join(format!("exc-mcp-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Server::start(Config { data_dir: dir, addr: "127.0.0.1:0".parse().unwrap(), token: TOKEN.into(), name: "Studio".into() })
        .await
        .unwrap()
}

struct Mcp {
    url: String,
    http: reqwest::Client,
    session: Option<String>,
    next: u64,
}

impl Mcp {
    async fn connect(server: &Server, query: &str) -> Self {
        let mut c = Mcp { url: format!("http://{}/mcp{query}", server.local_addr), http: reqwest::Client::new(), session: None, next: 1 };
        let resp = c
            .http
            .post(&c.url)
            .bearer_auth(TOKEN)
            .header("accept", "application/json, text/event-stream")
            .json(&json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "claude-code", "version": "2.0" } } }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        c.session = resp.headers().get("mcp-session-id").map(|v| v.to_str().unwrap().to_owned());
        let init: Value = resp.json().await.unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert!(init["result"]["instructions"].as_str().unwrap().contains("DESIGN RULES"));
        let note = c.post(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await;
        assert_eq!(note.status(), 202);
        c
    }

    async fn post(&self, body: Value) -> reqwest::Response {
        let mut r = self.http.post(&self.url).bearer_auth(TOKEN).json(&body);
        if let Some(s) = &self.session {
            r = r.header("mcp-session-id", s);
        }
        r.send().await.unwrap()
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let resp = self.post(json!({ "jsonrpc": "2.0", "id": self.next, "method": method, "params": params })).await;
        assert_eq!(resp.status(), 200);
        resp.json::<Value>().await.unwrap()
    }

    /// Calls a tool; returns (text, is_error).
    async fn tool(&mut self, name: &str, args: Value) -> (String, bool, Value) {
        let v = self.rpc("tools/call", json!({ "name": name, "arguments": args })).await;
        let result = v["result"].clone();
        let text = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        (text, result["isError"].as_bool().unwrap_or(false), result)
    }
}

async fn watch(server: &Server, canvas: &str) -> (Ws, Value) {
    let url = format!("ws://{}/api/canvases/{canvas}/ws", server.local_addr);
    let mut request = url.into_client_request().unwrap();
    request.headers_mut().insert("Authorization", format!("Bearer {TOKEN}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    ws.send(Message::Text(json!({ "type": "hello", "name": "Rafa", "color": "#f00", "userId": "rafa" }).to_string().into())).await.unwrap();
    let init = next(&mut ws).await.unwrap();
    (ws, init)
}

async fn next(ws: &mut Ws) -> Option<Value> {
    loop {
        match tokio::time::timeout(Duration::from_millis(1500), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => return Some(serde_json::from_str(&t).unwrap()),
            Ok(Some(Ok(_))) => continue,
            _ => return None,
        }
    }
}

/// Everything the person receives until things go quiet.
async fn drain(ws: &mut Ws) -> Vec<Value> {
    let mut all = Vec::new();
    while let Some(m) = next(ws).await {
        all.push(m);
    }
    all
}

async fn scene(server: &Server, canvas: &str) -> Vec<Value> {
    let url = format!("http://{}/api/canvases/{canvas}/scene", server.local_addr);
    let v: Value = reqwest::Client::new().get(url).bearer_auth(TOKEN).send().await.unwrap().json().await.unwrap();
    v["elements"].as_array().unwrap().iter().filter(|e| e["isDeleted"] != true).cloned().collect()
}

fn find<'a>(els: &'a [Value], id: &str) -> &'a Value {
    els.iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}"))
}

#[tokio::test]
async fn needs_the_token_and_a_known_session() {
    let server = start("auth").await;
    let http = reqwest::Client::new();
    let url = format!("http://{}/mcp", server.local_addr);
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    assert_eq!(http.post(&url).json(&ping).send().await.unwrap().status(), 401);
    let stale = http.post(&url).bearer_auth(TOKEN).header("mcp-session-id", "gone").json(&ping).send().await.unwrap();
    assert_eq!(stale.status(), 404);
    let by_query = http.post(format!("{url}?token={TOKEN}")).json(&ping).send().await.unwrap();
    assert_eq!(by_query.status(), 200);
    assert_eq!(http.get(&url).bearer_auth(TOKEN).send().await.unwrap().status(), 405);
    server.stop().await;
}

#[tokio::test]
async fn assistant_joins_and_draws_live() {
    let server = start("draw").await;
    let mut mcp = Mcp::connect(&server, "").await;
    let tools = mcp.rpc("tools/list", json!({})).await;
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for t in ["open_canvas", "draw", "draw_diagram", "describe_canvas", "view_canvas", "arrange", "delete_elements", "point_at"] {
        assert!(names.contains(&t), "missing tool {t}");
    }

    let (text, err, _) = mcp.tool("open_canvas", json!({ "create": "Architecture" })).await;
    assert!(!err, "{text}");
    assert!(text.contains("Joined “Architecture” as Claude"), "{text}");
    let list: Value = reqwest::Client::new()
        .get(format!("http://{}/api/canvases", server.local_addr))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let canvas = list[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(list[0]["peers"][0]["name"], "Claude");

    // A person opens the canvas and sees the assistant there.
    let (mut ws, init) = watch(&server, &canvas).await;
    assert_eq!(init["peers"][0]["name"], "Claude");

    let (text, err, _) = mcp
        .tool(
            "draw",
            json!({ "elements": [
                { "id": "api", "type": "rectangle", "label": "API Gateway", "color": "blue", "x": 0, "y": 0 },
                { "id": "db", "type": "ellipse", "label": "Postgres", "below": "api", "gap": 100 },
                { "type": "arrow", "from": "api", "to": "db", "label": "SQL" },
                { "type": "text", "text": "System overview", "font_size": "xl", "above": "api", "align": "start" }
            ] }),
        )
        .await;
    assert!(!err, "{text}");
    assert!(text.contains("api") && text.contains("api-to-db"), "{text}");

    let seen = drain(&mut ws).await;
    let pointers = seen.iter().filter(|m| m["type"] == "pointer").count();
    let updates: Vec<&Value> = seen.iter().filter(|m| m["type"] == "update").collect();
    assert!(pointers >= 4, "cursor should move while drawing ({pointers} pointer messages)");
    assert!(updates.len() >= 3, "elements should appear step by step ({} updates)", updates.len());

    let els = scene(&server, &canvas).await;
    let api = find(&els, "api");
    let db = find(&els, "db");
    let arrow = find(&els, "api-to-db");
    assert_eq!(api["strokeColor"], "#1971c2");
    assert_eq!(api["backgroundColor"], "#a5d8ff");
    assert_eq!(arrow["startBinding"]["elementId"], "api");
    assert_eq!(arrow["endBinding"]["elementId"], "db");
    // Shapes know about the arrow and their labels.
    assert!(api["boundElements"].as_array().unwrap().iter().any(|b| b["id"] == "api-to-db"));
    assert!(db["boundElements"].as_array().unwrap().iter().any(|b| b["id"] == "db-label"));
    assert_eq!(find(&els, "db-label")["containerId"], "db");
    // Placed below with the gap, centred under it.
    let api_bottom = api["y"].as_f64().unwrap() + api["height"].as_f64().unwrap();
    assert!((db["y"].as_f64().unwrap() - api_bottom - 100.0).abs() < 0.01);
    // Valid, increasing z-order keys.
    let mut idx: Vec<&str> = els.iter().map(|e| e["index"].as_str().unwrap()).collect();
    let sorted = { let mut s = idx.clone(); s.sort(); s };
    assert_eq!(idx, sorted);
    idx.dedup();
    assert_eq!(idx.len(), els.len());

    // The person selects something; the assistant sees it.
    ws.send(Message::Text(json!({ "type": "pointer", "pointer": { "x": 40, "y": 30, "tool": "pointer" }, "button": "up", "selectedElementIds": { "db": true } }).to_string().into())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (text, _, _) = mcp.tool("describe_canvas", json!({})).await;
    assert!(text.contains("Rafa"), "{text}");

    // Moving a shape drags its label and re-routes the arrow.
    let before = find(&els, "api-to-db")["points"].clone();
    let (text, err, _) = mcp.tool("arrange", json!({ "operations": [{ "op": "move", "ids": ["db"], "dx": 300 }], "animate": false })).await;
    assert!(!err, "{text}");
    let els = scene(&server, &canvas).await;
    assert_ne!(find(&els, "api-to-db")["points"], before);
    let label = find(&els, "db-label");
    let db = find(&els, "db");
    let (lc, dc) = (label["x"].as_f64().unwrap() + label["width"].as_f64().unwrap() / 2.0, db["x"].as_f64().unwrap() + db["width"].as_f64().unwrap() / 2.0);
    assert!((lc - dc).abs() < 1.0);

    // Update keeps what isn't mentioned.
    let (_, err, _) = mcp.tool("draw", json!({ "elements": [{ "id": "api", "label": "Gateway" }], "animate": false })).await;
    assert!(!err);
    let els = scene(&server, &canvas).await;
    assert_eq!(find(&els, "api-label")["originalText"], "Gateway");
    assert_eq!(find(&els, "api")["strokeColor"], "#1971c2");

    // A preview comes back as a PNG.
    let (_, err, result) = mcp.tool("view_canvas", json!({})).await;
    assert!(!err, "{result}");
    assert_eq!(result["content"][0]["type"], "image");
    assert!(result["content"][0]["data"].as_str().unwrap().starts_with("iVBOR"));

    // Deleting a shape takes its label and arrow along.
    let (_, err, _) = mcp.tool("delete_elements", json!({ "ids": ["db"] })).await;
    assert!(!err);
    let els = scene(&server, &canvas).await;
    assert!(!els.iter().any(|e| e["id"] == "db" || e["id"] == "db-label" || e["id"] == "api-to-db"));
    assert!(!find(&els, "api")["boundElements"].as_array().unwrap().iter().any(|b| b["id"] == "api-to-db"));

    // Ending the session makes the assistant leave.
    drain(&mut ws).await;
    let resp = mcp.http.delete(&mcp.url).bearer_auth(TOKEN).header("mcp-session-id", mcp.session.clone().unwrap()).send().await.unwrap();
    assert_eq!(resp.status(), 204);
    let left = drain(&mut ws).await;
    assert!(left.iter().any(|m| m["type"] == "peer-left"));
    server.stop().await;
}

#[tokio::test]
async fn diagrams_and_bad_input() {
    let server = start("diagram").await;
    let mut mcp = Mcp::connect(&server, "?name=Robo").await;
    // With a single canvas, tools open it on their own.
    let (_, err, _) = mcp.tool("open_canvas", json!({ "create": "Flow" })).await;
    assert!(!err);
    let (text, err, _) = mcp
        .tool(
            "draw_diagram",
            json!({
                "layout": "flow",
                "title": "Checkout",
                "nodes": [
                    { "id": "start", "label": "Start" },
                    { "id": "cart", "label": "Review cart" },
                    { "id": "paid", "label": "Paid?" },
                    { "id": "ship", "label": "Ship order", "cluster": "ops" },
                    { "id": "retry", "label": "Retry payment" }
                ],
                "edges": [
                    { "from": "start", "to": "cart" }, { "from": "cart", "to": "paid" },
                    { "from": "paid", "to": "ship", "label": "yes" }, { "from": "paid", "to": "retry", "label": "no" },
                    { "from": "retry", "to": "paid" }
                ],
                "animate": false
            }),
        )
        .await;
    assert!(!err, "{text}");
    let list: Value = reqwest::Client::new().get(format!("http://{}/api/canvases", server.local_addr)).bearer_auth(TOKEN).send().await.unwrap().json().await.unwrap();
    assert_eq!(list[0]["peers"][0]["name"], "Robo");
    let canvas = list[0]["id"].as_str().unwrap().to_owned();
    let els = scene(&server, &canvas).await;
    assert_eq!(find(&els, "paid")["type"], "diamond");
    assert_eq!(find(&els, "start")["type"], "ellipse");
    for e in ["start-to-cart", "paid-to-ship", "retry-to-paid"] {
        let a = find(&els, e);
        assert!(a["startBinding"]["elementId"].is_string() && a["endBinding"]["elementId"].is_string(), "{e} unbound");
    }

    // Mistakes come back as tool errors the model can act on.
    let (text, err, _) = mcp.tool("draw", json!({ "elements": [{ "type": "hexagon" }] })).await;
    assert!(err && text.contains("Unknown type"), "{text}");
    let (text, err, _) = mcp.tool("draw", json!({ "elements": [{ "type": "arrow", "from": "nope", "to": "cart" }] })).await;
    assert!(err && text.contains("nope"), "{text}");
    let (text, err, _) = mcp.tool("draw", json!({ "elements": [{ "type": "rectangle", "color": "sparkly" }] })).await;
    assert!(err && text.contains("Unknown colour"), "{text}");
    server.stop().await;
}

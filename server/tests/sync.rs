use drawlycool_server::{Config, Server};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "secret-token";

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("exc-server-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn start(dir: &std::path::Path) -> Server {
    Server::start(Config {
        data_dir: dir.to_owned(),
        addr: "127.0.0.1:0".parse().unwrap(),
        token: TOKEN.into(),
        name: "Test server".into(),
    })
    .await
    .unwrap()
}

struct Api {
    base: String,
    http: reqwest::Client,
}

impl Api {
    fn new(server: &Server) -> Self {
        Self { base: format!("http://{}", server.local_addr), http: reqwest::Client::new() }
    }
    async fn req(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut r = self.http.request(method, format!("{}{path}", self.base)).bearer_auth(TOKEN);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let status = resp.status().as_u16();
        (status, resp.json().await.unwrap_or(Value::Null))
    }
    async fn join(&self, canvas: &str, name: &str) -> (Ws, Value) {
        let url = format!("{}/api/canvases/{canvas}/ws", self.base.replace("http", "ws"));
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert("Authorization", format!("Bearer {TOKEN}").parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        send(&mut ws, json!({ "type": "hello", "name": name, "color": "#ff0000", "userId": format!("user-{name}") })).await;
        let init = recv(&mut ws).await.expect("init");
        assert_eq!(init["type"], "init");
        (ws, init)
    }
}

async fn send(ws: &mut Ws, value: Value) {
    ws.send(Message::Text(value.to_string().into())).await.unwrap();
}

/// Next JSON message, or None after 500ms of silence.
async fn recv(ws: &mut Ws) -> Option<Value> {
    loop {
        match tokio::time::timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => return Some(serde_json::from_str(&t).unwrap()),
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Err(_) => return None,
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) => return None,
        }
    }
}

fn rect(id: &str, version: i64, nonce: i64) -> Value {
    json!({ "id": id, "type": "rectangle", "version": version, "versionNonce": nonce, "index": "a0", "x": 1, "y": 2 })
}

#[tokio::test]
async fn rejects_requests_without_the_token() {
    let dir = temp_dir("auth");
    let server = start(&dir).await;
    let base = format!("http://{}", server.local_addr);
    let client = reqwest::Client::new();
    assert_eq!(client.get(format!("{base}/api/info")).send().await.unwrap().status(), 401);
    assert_eq!(client.get(format!("{base}/api/info")).bearer_auth("wrong").send().await.unwrap().status(), 401);
    let ok = client.get(format!("{base}/api/info")).bearer_auth(TOKEN).send().await.unwrap();
    assert_eq!(ok.status(), 200);
    assert_eq!(ok.json::<Value>().await.unwrap()["name"], "Test server");
    // WebSocket without a token is refused too.
    let url = format!("ws://{}/api/canvases/x/ws", server.local_addr);
    assert!(tokio_tungstenite::connect_async(url).await.is_err());
    server.stop().await;
}

#[tokio::test]
async fn two_clients_edit_the_same_canvas() {
    let dir = temp_dir("sync");
    let server = start(&dir).await;
    let api = Api::new(&server);

    let (status, meta) = api.req(reqwest::Method::POST, "/api/canvases", Some(json!({ "name": "Shared" }))).await;
    assert_eq!(status, 200);
    let id = meta["id"].as_str().unwrap().to_owned();

    let (mut a, init_a) = api.join(&id, "Alice").await;
    assert_eq!(init_a["peers"], json!([]));
    assert_eq!(init_a["meta"]["name"], "Shared");
    let (mut b, init_b) = api.join(&id, "Bob").await;
    assert_eq!(init_b["peers"][0]["name"], "Alice");
    let joined = recv(&mut a).await.unwrap();
    assert_eq!(joined["type"], "peer-joined");
    assert_eq!(joined["peer"]["name"], "Bob");
    assert_eq!(joined["peer"]["userId"], "user-Bob");

    // A draws; B gets it.
    send(&mut a, json!({ "type": "update", "elements": [rect("r1", 2, 100)] })).await;
    let update = recv(&mut b).await.unwrap();
    assert_eq!(update["type"], "update");
    assert_eq!(update["elements"][0]["id"], "r1");
    assert_eq!(update["from"], init_a["you"]);

    // A stale copy from B is ignored and not relayed.
    send(&mut b, json!({ "type": "update", "elements": [rect("r1", 1, 5)] })).await;
    assert!(recv(&mut a).await.is_none());

    // Pointers are relayed with the sender's id.
    send(&mut b, json!({ "type": "pointer", "pointer": { "x": 5, "y": 6, "tool": "pointer" }, "button": "up" })).await;
    let pointer = recv(&mut a).await.unwrap();
    assert_eq!(pointer["type"], "pointer");
    assert_eq!(pointer["from"], init_b["you"]);
    assert_eq!(pointer["pointer"]["x"], 5);

    // Background colour.
    send(&mut a, json!({ "type": "background", "color": "#ffc9c9" })).await;
    assert_eq!(recv(&mut b).await.unwrap()["color"], "#ffc9c9");

    // Following: B follows A, A learns about it; A's bounds reach B only.
    send(&mut b, json!({ "type": "follow", "target": init_a["you"], "follow": true })).await;
    let followed = recv(&mut a).await.unwrap();
    assert_eq!(followed["type"], "followed-by");
    assert_eq!(followed["ids"], json!([init_b["you"]]));
    send(&mut a, json!({ "type": "bounds", "bounds": [0, 0, 100, 100] })).await;
    assert_eq!(recv(&mut b).await.unwrap()["bounds"], json!([0, 0, 100, 100]));

    // The list shows who's in.
    let (_, list) = api.req(reqwest::Method::GET, "/api/canvases", None).await;
    assert_eq!(list[0]["peers"].as_array().unwrap().len(), 2);

    // Rename reaches everyone.
    api.req(reqwest::Method::PATCH, &format!("/api/canvases/{id}"), Some(json!({ "name": "Renamed" }))).await;
    assert_eq!(recv(&mut a).await.unwrap()["meta"]["name"], "Renamed");
    assert_eq!(recv(&mut b).await.unwrap()["meta"]["name"], "Renamed");

    // B leaves; A loses its follower, then is told B left.
    b.close(None).await.unwrap();
    let unfollowed = recv(&mut a).await.unwrap();
    assert_eq!(unfollowed["type"], "followed-by");
    assert_eq!(unfollowed["ids"], json!([]));
    let left = recv(&mut a).await.unwrap();
    assert_eq!(left["type"], "peer-left");
    assert_eq!(left["id"], init_b["you"]);

    // Saved to disk after the debounce.
    tokio::time::sleep(Duration::from_millis(1300)).await;
    let saved: Value = serde_json::from_slice(&std::fs::read(dir.join(format!("canvases/{id}.excalidraw"))).unwrap()).unwrap();
    assert_eq!(saved["elements"][0]["id"], "r1");
    assert_eq!(saved["appState"]["viewBackgroundColor"], "#ffc9c9");

    // Delete kicks A out.
    let (status, _) = api.req(reqwest::Method::DELETE, &format!("/api/canvases/{id}"), None).await;
    assert_eq!(status, 204);
    assert_eq!(recv(&mut a).await.unwrap()["type"], "deleted");
    assert!(recv(&mut a).await.is_none());
    server.stop().await;
}

#[tokio::test]
async fn edits_survive_a_restart_and_uploads_keep_their_scene() {
    let dir = temp_dir("restart");
    let server = start(&dir).await;
    let api = Api::new(&server);
    let scene = json!({ "type": "excalidraw", "version": 2, "elements": [rect("seed", 1, 1)], "appState": {}, "files": {} });
    let (_, meta) =
        api.req(reqwest::Method::POST, "/api/canvases", Some(json!({ "name": "Moved", "scene": scene.to_string() }))).await;
    let id = meta["id"].as_str().unwrap().to_owned();

    let (mut a, init) = api.join(&id, "Alice").await;
    assert_eq!(init["elements"][0]["id"], "seed");
    send(&mut a, json!({ "type": "update", "elements": [rect("late", 1, 1)] })).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Stop right away (before the save debounce): stop() must still save.
    server.stop().await;
    assert!(recv(&mut a).await.is_none(), "clients are disconnected");

    let server = start(&dir).await;
    let api = Api::new(&server);
    let (_, init) = api.join(&id, "Alice").await;
    let mut ids: Vec<_> = init["elements"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_owned()).collect();
    ids.sort();
    assert_eq!(ids, ["late", "seed"]);

    let (status, _) = api.req(reqwest::Method::POST, "/api/canvases", Some(json!({ "name": "Bad", "scene": "nope" }))).await;
    assert_eq!(status, 400);
    server.stop().await;
}

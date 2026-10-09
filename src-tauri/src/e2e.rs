//! Debug-only test hook (compiled out of release builds).
//!
//! When `DRAWLYCOOL_E2E_PORT` is set, listens on 127.0.0.1:<port>. Each
//! connection sends a JavaScript function body, which runs in the main
//! webview; whatever it returns (JSON-serialised) is written back.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Manager};

static PENDING: OnceLock<Mutex<HashMap<u64, Sender<String>>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn pending() -> &'static Mutex<HashMap<u64, Sender<String>>> {
    PENDING.get_or_init(Default::default)
}

#[tauri::command]
pub fn e2e_result(id: u64, value: String) {
    if let Some(tx) = pending().lock().unwrap().remove(&id) {
        let _ = tx.send(value);
    }
}

pub fn start(app: &AppHandle) {
    let Some(port) = std::env::var("DRAWLYCOOL_E2E_PORT").ok().and_then(|p| p.parse::<u16>().ok()) else {
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || {
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("e2e port in use");
        for stream in listener.incoming().flatten() {
            let app = app.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut js = String::new();
                if stream.read_to_string(&mut js).is_err() {
                    return;
                }
                let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
                let (tx, rx) = channel();
                pending().lock().unwrap().insert(id, tx);
                let script = format!(
                    r#"(async () => {{
                        let value;
                        try {{ value = JSON.stringify(await (async () => {{ {js} }})()) ?? "null"; }}
                        catch (e) {{ value = JSON.stringify({{ error: String(e && e.stack || e) }}); }}
                        window.__TAURI_INTERNALS__.invoke("e2e_result", {{ id: {id}, value }});
                    }})();"#
                );
                let reply = match app.get_webview_window("main").map(|w| w.eval(&script)) {
                    Some(Ok(())) => rx.recv_timeout(Duration::from_secs(30)).unwrap_or_else(|_| "\"<timeout>\"".into()),
                    _ => "\"<no window>\"".into(),
                };
                pending().lock().unwrap().remove(&id);
                let _ = stream.write_all(reply.as_bytes());
            });
        }
    });
}

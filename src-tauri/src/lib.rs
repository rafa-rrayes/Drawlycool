mod host;
mod network;

#[cfg(debug_assertions)]
mod e2e;

use drawlycool_server::storage;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use storage::{CanvasMeta, Storage, Viewport};
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::menu::{AboutMetadataBuilder, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::webview::NewWindowResponse;
use tauri::window::Color;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, Theme, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

const MAIN: &str = "main";

pub(crate) type Store<'a> = State<'a, Mutex<Storage>>;
type Result<T> = std::result::Result<T, String>;

pub(crate) fn lock<'a>(store: &'a Store<'_>) -> MutexGuard<'a, Storage> {
    // A panic mid-write can't leave the files inconsistent (writes are atomic),
    // so a poisoned lock is safe to keep using.
    store.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------ helpers

fn open_external(url: &str) {
    let allowed = Url::parse(url).map(|u| matches!(u.scheme(), "http" | "https" | "mailto")).unwrap_or(false);
    if allowed {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
}

fn is_app_url(url: &Url) -> bool {
    url.scheme() == "tauri" || (cfg!(debug_assertions) && url.host_str() == Some("localhost"))
}

fn strip_extension(file: &str) -> String {
    let base = Path::new(file).file_name().and_then(|f| f.to_str()).unwrap_or(file);
    for ext in [".excalidraw", ".json"] {
        if base.len() > ext.len() && base.to_lowercase().ends_with(ext) {
            return base[..base.len() - ext.len()].to_string();
        }
    }
    base.to_string()
}

/// Decodes the `encodeURIComponent` output used for header values.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn header<'a>(request: &'a Request<'_>, name: &str) -> Result<&'a str> {
    request
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| format!("missing header {name}"))
}

pub(crate) fn raw_body<'a>(request: &'a Request<'_>) -> Result<&'a [u8]> {
    match request.body() {
        InvokeBody::Raw(bytes) => Ok(bytes),
        InvokeBody::Json(_) => Err("expected a raw request body".into()),
    }
}

fn apply_theme(window: &WebviewWindow, theme: Option<&str>) {
    let theme = match theme {
        Some("light") => Some(Theme::Light),
        Some("dark") => Some(Theme::Dark),
        _ => None,
    };
    let _ = window.set_theme(theme);
    let dark = theme.or_else(|| window.theme().ok()) == Some(Theme::Dark);
    let bg = if dark { Color(0x12, 0x12, 0x12, 0xff) } else { Color(0xff, 0xff, 0xff, 0xff) };
    let _ = window.set_background_color(Some(bg));
}

// ------------------------------------------------------------------ canvas commands

#[tauri::command]
fn list_canvases(store: Store) -> Vec<CanvasMeta> {
    lock(&store).list()
}

#[tauri::command]
fn create_canvas(store: Store, name: String) -> Result<CanvasMeta> {
    lock(&store).create(&name, None)
}

#[derive(Serialize)]
struct LoadedMeta {
    meta: CanvasMeta,
    viewport: Option<Viewport>,
}

#[tauri::command]
fn canvas_meta(store: Store, id: String) -> Result<LoadedMeta> {
    let (meta, viewport) = lock(&store).meta(&id)?;
    Ok(LoadedMeta { meta, viewport })
}

/// Scenes travel as raw bytes in both directions: they can be several MB
/// (embedded images) and skipping JSON-in-JSON encoding keeps saves cheap.
#[tauri::command]
async fn load_scene(store: Store<'_>, id: String) -> Result<Response> {
    Ok(Response::new(lock(&store).load_scene(&id)?))
}

#[tauri::command]
async fn save_scene(store: Store<'_>, request: Request<'_>) -> Result<()> {
    let id = header(&request, "x-canvas-id")?;
    lock(&store).save_scene(id, raw_body(&request)?)
}

#[tauri::command]
fn save_viewport(store: Store, id: String, viewport: Viewport) -> Result<()> {
    lock(&store).save_viewport(&id, viewport)
}

#[tauri::command]
fn rename_canvas(store: Store, id: String, name: String) -> Result<CanvasMeta> {
    lock(&store).rename(&id, &name)
}

#[tauri::command]
fn duplicate_canvas(store: Store, id: String) -> Result<CanvasMeta> {
    lock(&store).duplicate(&id)
}

#[tauri::command]
async fn delete_canvas(app: AppHandle, window: WebviewWindow, store: Store<'_>, id: String) -> Result<bool> {
    let (meta, _) = lock(&store).meta(&id)?;
    let confirmed = app
        .dialog()
        .message("This canvas will be permanently deleted. This can’t be undone.")
        .title(format!("Delete “{}”?", meta.name))
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom("Delete".into(), "Cancel".into()))
        .parent(&window)
        .blocking_show();
    if confirmed {
        lock(&store).remove(&id)?;
    }
    Ok(confirmed)
}

#[tauri::command]
fn reveal_canvas(store: Store, id: String) -> Result<()> {
    let path = lock(&store).scene_path(&id)?;
    std::process::Command::new("open").arg("-R").arg(path).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn export_canvas(app: AppHandle, window: WebviewWindow, store: Store<'_>, id: String) -> Result<bool> {
    let (meta, _) = lock(&store).meta(&id)?;
    let Some(target) = save_dialog(&app, &window, "Export canvas", &format!("{}.excalidraw", meta.name), ("Excalidraw", &["excalidraw"])) else {
        return Ok(false);
    };
    let source = lock(&store).scene_path(&id)?;
    std::fs::copy(source, target).map_err(|e| e.to_string())?;
    Ok(true)
}

pub(crate) fn save_dialog(app: &AppHandle, window: &WebviewWindow, title: &str, file_name: &str, filter: (&str, &[&str])) -> Option<PathBuf> {
    let mut dialog = app.dialog().file().set_title(title).set_file_name(file_name).set_parent(window);
    if let Ok(dir) = app.path().document_dir() {
        dialog = dialog.set_directory(dir);
    }
    if !filter.1.is_empty() {
        dialog = dialog.add_filter(filter.0, filter.1);
    }
    dialog.blocking_save_file()?.into_path().ok()
}

#[derive(Deserialize)]
struct ImportFile {
    name: String,
    text: String,
}

#[derive(Serialize)]
struct ImportResult {
    imported: Vec<CanvasMeta>,
    errors: Vec<String>,
}

fn import_many(app: &AppHandle, window: &WebviewWindow, store: &Store, files: Vec<ImportFile>) -> ImportResult {
    let mut result = ImportResult { imported: vec![], errors: vec![] };
    for file in files {
        let name = strip_extension(&file.name);
        if !storage::is_excalidraw_scene(&file.text) {
            result.errors.push(format!("“{name}” is not a valid Excalidraw file."));
            continue;
        }
        match lock(store).create(&name, Some(&file.text)) {
            Ok(meta) => result.imported.push(meta),
            Err(e) => result.errors.push(e),
        }
    }
    if !result.errors.is_empty() {
        app.dialog()
            .message(result.errors.join("\n"))
            .title("Some files couldn’t be imported")
            .kind(MessageDialogKind::Error)
            .parent(window)
            .blocking_show();
    }
    result
}

#[tauri::command]
async fn import_dialog(app: AppHandle, window: WebviewWindow, store: Store<'_>) -> Result<ImportResult> {
    let picked = app
        .dialog()
        .file()
        .set_title("Import Excalidraw files")
        .add_filter("Excalidraw", &["excalidraw", "json"])
        .set_parent(&window)
        .blocking_pick_files()
        .unwrap_or_default();
    let mut files = Vec::new();
    for path in picked.into_iter().filter_map(|p| p.into_path().ok()) {
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        files.push(ImportFile { name: path.to_string_lossy().into_owned(), text });
    }
    Ok(import_many(&app, &window, &store, files))
}

#[tauri::command]
async fn import_data(app: AppHandle, window: WebviewWindow, store: Store<'_>, files: Vec<ImportFile>) -> Result<ImportResult> {
    Ok(import_many(&app, &window, &store, files))
}

/// Saves a file the page wanted to "download" (e.g. Export image → PNG/SVG).
#[tauri::command]
async fn save_download(app: AppHandle, window: WebviewWindow, request: Request<'_>) -> Result<bool> {
    let name = percent_decode(header(&request, "x-file-name")?);
    let ext = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
    let exts: Vec<&str> = if ext.is_empty() { vec![] } else { vec![ext.as_str()] };
    let Some(target) = save_dialog(&app, &window, "Save", &name, (&ext.to_uppercase(), &exts)) else {
        return Ok(false);
    };
    std::fs::write(target, raw_body(&request)?).map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
async fn show_error(app: AppHandle, window: WebviewWindow, title: String, message: String) {
    app.dialog().message(message).title(title).kind(MessageDialogKind::Error).parent(&window).blocking_show();
}

#[tauri::command]
fn open_url(url: String) {
    open_external(&url);
}

// ------------------------------------------------------------------ settings

#[tauri::command]
fn get_settings(store: Store) -> Map<String, Value> {
    lock(&store).read_settings()
}

#[tauri::command]
fn set_settings(window: WebviewWindow, store: Store, patch: Map<String, Value>) -> Result<Map<String, Value>> {
    let theme_changed = patch.contains_key("theme");
    let settings = lock(&store).write_settings(patch)?;
    if theme_changed {
        apply_theme(&window, settings.get("theme").and_then(Value::as_str));
    }
    Ok(settings)
}

// ------------------------------------------------------------------ quitting

static QUITTING: AtomicBool = AtomicBool::new(false);
static EXITING: AtomicBool = AtomicBool::new(false);

/// Asks the page to flush pending saves, then exits. Gives up waiting after 2s.
fn request_quit(app: &AppHandle) {
    if QUITTING.swap(true, Ordering::SeqCst) {
        return;
    }
    if app.emit_to(MAIN, "app:flush", ()).is_err() {
        exit_after_cleanup(app);
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        exit_after_cleanup(&app);
    });
}

/// Sends what live sessions still have queued, saves hosted canvases, exits.
fn exit_after_cleanup(app: &AppHandle) {
    if EXITING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        network::close_all(&app, Duration::from_millis(1500)).await;
        let _ = tokio::time::timeout(Duration::from_secs(4), host::stop(&app)).await;
        app.exit(0);
    });
}

/// Called by the page once everything is on disk.
#[tauri::command]
fn flushed(app: AppHandle) {
    if QUITTING.load(Ordering::SeqCst) {
        exit_after_cleanup(&app);
    }
}

// ------------------------------------------------------------------ menu

fn build_menu(app: &AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    let about = AboutMetadataBuilder::new()
        .name(Some("Drawlycool"))
        .version(Some(app.package_info().version.to_string()))
        .comments(Some("Built on the open-source Excalidraw editor (MIT)."))
        .build();
    let item = |id: &str, text: &str, accel: Option<&str>| {
        let mut b = MenuItemBuilder::with_id(id, text);
        if let Some(a) = accel {
            b = b.accelerator(a);
        }
        b.build(app)
    };

    let app_menu = SubmenuBuilder::new(app, "Drawlycool")
        .about(Some(about))
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        // Custom (not predefined) so pending saves are flushed first.
        .item(&item("quit", "Quit Drawlycool", Some("CmdOrCtrl+Q"))?)
        .build()?;

    let file = SubmenuBuilder::new(app, "File")
        .item(&item("new-canvas", "New Canvas", Some("CmdOrCtrl+N"))?)
        .item(&item("go-home", "All Canvases", Some("CmdOrCtrl+O"))?)
        .separator()
        .item(&item("connect", "Connect to Server…", None)?)
        .item(&item("host", "Host on This Mac…", None)?)
        .separator()
        .item(&item("import", "Import…", None)?)
        .separator()
        // Canvases auto-save; Cmd+S just flushes immediately.
        .item(&item("save", "Save", Some("CmdOrCtrl+S"))?)
        .separator()
        .close_window()
        .build()?;

    // Needed on macOS for clipboard shortcuts in text fields. WebKit lets the
    // page handle key equivalents first, so Excalidraw's own canvas shortcuts
    // (which call preventDefault) still win.
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    // No zoom items: Cmd +/-/0 belong to the canvas zoom.
    let view = SubmenuBuilder::new(app, "View").fullscreen().build()?;
    let window = SubmenuBuilder::new(app, "Window").minimize().maximize().separator().bring_all_to_front().build()?;

    MenuBuilder::new(app).items(&[&app_menu, &file, &edit, &view, &window]).build()
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "quit" => request_quit(app),
        "new-canvas" | "go-home" | "import" | "save" | "connect" | "host" => {
            if let Some(w) = app.get_webview_window(MAIN) {
                let _ = w.show();
                let _ = w.set_focus();
            }
            let _ = app.emit_to(MAIN, "menu", id);
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ window

fn create_main_window(app: &AppHandle, theme: Option<&str>) -> tauri::Result<WebviewWindow> {
    let window = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::App("index.html".into()))
        .title("Drawlycool")
        .inner_size(1400.0, 900.0)
        .min_inner_size(720.0, 500.0)
        // Shown by the page after its first render, to avoid a blank flash.
        .visible(false)
        // Let HTML5 drag & drop through (file import, dropping images on canvas).
        .disable_drag_drop_handler()
        .on_navigation(|url| {
            if is_app_url(url) {
                return true;
            }
            open_external(url.as_str());
            false
        })
        .on_new_window(|url, _features| {
            open_external(url.as_str());
            NewWindowResponse::Deny
        })
        .build()?;
    apply_theme(&window, theme);

    // Safety net in case the page never calls show().
    let fallback = window.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(4));
        if !fallback.is_visible().unwrap_or(true) {
            let _ = fallback.show();
        }
    });
    Ok(window)
}

fn data_dir(app: &AppHandle) -> PathBuf {
    // Lets tests run against a throwaway data folder.
    if let Some(dir) = std::env::var_os("DRAWLYCOOL_USER_DATA") {
        return PathBuf::from(dir);
    }
    let support = app.path().data_dir().expect("no Application Support directory");
    let dir = support.join("Drawlycool");
    // The app used to be called Excalidraw: bring its canvases, servers and
    // settings along the first time.
    let old = support.join("Excalidraw");
    if !dir.exists() && old.join("canvases").is_dir() {
        if let Err(e) = std::fs::rename(&old, &dir) {
            eprintln!("couldn't move {} to {}: {e}", old.display(), dir.display());
            return old;
        }
    }
    dir
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::all() & !tauri_plugin_window_state::StateFlags::VISIBLE,
                )
                .build(),
        )
        .setup(|app| {
            let handle = app.handle().clone();
            let storage = Storage::open(data_dir(&handle))?;
            let theme = storage.read_settings().get("theme").and_then(Value::as_str).map(str::to_owned);
            app.manage(Mutex::new(storage));
            app.manage(network::Net::open(data_dir(&handle)));
            app.manage(host::HostState::default());
            host::restore(&handle);

            app.set_menu(build_menu(&handle)?)?;
            app.on_menu_event(|app, event| on_menu(app, event.id().as_ref()));

            create_main_window(&handle, theme.as_deref())?;

            #[cfg(debug_assertions)]
            e2e::start(&handle);
            Ok(())
        })
        .on_window_event(|window, event| {
            // Mac convention: closing the window hides it; the Dock icon brings it back.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if !QUITTING.load(Ordering::SeqCst) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_canvases,
            create_canvas,
            canvas_meta,
            load_scene,
            save_scene,
            save_viewport,
            rename_canvas,
            duplicate_canvas,
            delete_canvas,
            reveal_canvas,
            export_canvas,
            import_dialog,
            import_data,
            save_download,
            open_url,
            show_error,
            get_settings,
            set_settings,
            flushed,
            network::list_servers,
            network::add_server,
            network::remove_server,
            network::invite_link,
            network::take_pending_links,
            network::remote_list,
            network::remote_create,
            network::remote_rename,
            network::remote_duplicate,
            network::remote_delete,
            network::remote_export,
            network::move_to_server,
            network::copy_text,
            network::user_full_name,
            network::remote_connect,
            network::remote_send,
            network::remote_disconnect,
            host::host_status,
            host::host_update,
            #[cfg(debug_assertions)]
            e2e::e2e_result,
        ])
        .build(tauri::generate_context!())
        .expect("error while building the app");

    app.run(|app, event| match event {
        RunEvent::Reopen { has_visible_windows: false, .. } => {
            if let Some(w) = app.get_webview_window(MAIN) {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
        // drawlycool://connect?… invite links.
        RunEvent::Opened { urls } => {
            let links: Vec<String> = urls.iter().filter(|u| u.scheme() == "drawlycool").map(|u| u.to_string()).collect();
            if links.is_empty() {
                return;
            }
            let net = app.state::<network::Net>();
            for link in links {
                net.push_link(link);
            }
            if let Some(w) = app.get_webview_window(MAIN) {
                let _ = w.show();
                let _ = w.set_focus();
            }
            let _ = app.emit_to(MAIN, "open-link", ());
        }
        // Quit from outside the menu (Dock, logout…): flush first.
        RunEvent::ExitRequested { code: None, api, .. } => {
            api.prevent_exit();
            request_quit(app);
        }
        _ => {}
    });
}

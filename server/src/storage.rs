//! Canvas storage, shared by the desktop app (local canvases) and the sync
//! server (hosted canvases).
//!
//! Layout (inside `~/Library/Application Support/Drawlycool/` for the app, or
//! the server's data folder):
//!   canvases/index.json        -> [{ id, name, createdAt, updatedAt, viewport?, folder? }]
//!   canvases/folders.json      -> [{ id, name, createdAt }]
//!   canvases/<id>.excalidraw   -> standard Excalidraw JSON scene
//!   settings.json              -> { theme }
//!
//! Every method runs under the `Mutex` the app keeps around `Storage`, so writes
//! are serialised and can never interleave.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const EMPTY_SCENE: &str = r##"{
  "type": "excalidraw",
  "version": 2,
  "source": "drawlycool",
  "elements": [],
  "appState": { "viewBackgroundColor": "#ffffff" },
  "files": {}
}"##;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub zoom: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    name: String,
    created_at: f64,
    updated_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    viewport: Option<Viewport>,
    /// The folder it's filed in; none for the top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
}

/// What the frontend sees for each canvas.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasMeta {
    pub id: String,
    pub name: String,
    pub created_at: f64,
    pub updated_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

impl From<&Entry> for CanvasMeta {
    fn from(e: &Entry) -> Self {
        Self {
            id: e.id.clone(),
            name: e.name.clone(),
            created_at: e.created_at,
            updated_at: e.updated_at,
            folder: e.folder.clone(),
        }
    }
}

/// A named group of canvases within one place (one level, no nesting).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub created_at: f64,
}

pub type Result<T> = std::result::Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

pub fn is_valid_id(id: &str) -> bool {
    id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn clean_name(name: &str, fallback: &str) -> String {
    let collapsed = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed: String = collapsed.chars().take(120).collect();
    if trimmed.is_empty() { fallback.to_string() } else { trimmed }
}

fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

fn file_time_ms(time: io::Result<SystemTime>) -> f64 {
    time.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as f64)
        .unwrap_or_else(now_ms)
}

/// True when `text` parses as an Excalidraw scene.
pub fn is_excalidraw_scene(text: &str) -> bool {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(o)) => {
            o.get("type").and_then(Value::as_str) == Some("excalidraw")
                && o.get("elements").map(Value::is_array).unwrap_or(false)
        }
        _ => false,
    }
}

pub struct Storage {
    dir: PathBuf,
    index_path: PathBuf,
    folders_path: PathBuf,
    settings_path: PathBuf,
    index: Vec<Entry>,
    folders: Vec<Folder>,
}

impl Storage {
    pub fn open(base_dir: PathBuf) -> io::Result<Self> {
        let dir = base_dir.join("canvases");
        fs::create_dir_all(&dir)?;
        let mut storage = Self {
            index_path: dir.join("index.json"),
            folders_path: dir.join("folders.json"),
            settings_path: base_dir.join("settings.json"),
            dir,
            index: Vec::new(),
            folders: Vec::new(),
        };
        storage.folders = storage.read_folders();
        storage.index = storage.read_index()?;
        Ok(storage)
    }

    // ---------------------------------------------------------- internals

    fn read_index(&self) -> io::Result<Vec<Entry>> {
        // A missing or corrupt index is rebuilt from the scene files below.
        let mut entries: Vec<Entry> = fs::read_to_string(&self.index_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        entries.retain(|e| is_valid_id(&e.id));

        // Self-heal: drop entries without a file, adopt files without an entry.
        let on_disk: HashSet<String> = fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".excalidraw").map(str::to_owned))
            .filter(|id| is_valid_id(id))
            .collect();
        entries.retain(|e| on_disk.contains(&e.id));
        let known: HashSet<String> = entries.iter().map(|e| e.id.clone()).collect();
        for id in on_disk.difference(&known) {
            let meta = fs::metadata(self.scene_path_unchecked(id))?;
            entries.push(Entry {
                id: id.clone(),
                name: "Recovered canvas".into(),
                created_at: file_time_ms(meta.created()),
                updated_at: file_time_ms(meta.modified()),
                viewport: None,
                folder: None,
            });
        }
        // A canvas filed in a folder that's gone is back at the top level.
        for e in &mut entries {
            if e.folder.as_ref().is_some_and(|f| !self.folders.iter().any(|x| &x.id == f)) {
                e.folder = None;
            }
        }
        Ok(entries)
    }

    fn read_folders(&self) -> Vec<Folder> {
        let mut folders: Vec<Folder> = fs::read_to_string(&self.folders_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        folders.retain(|f| is_valid_id(&f.id));
        folders
    }

    fn scene_path_unchecked(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.excalidraw"))
    }

    fn find_mut(&mut self, id: &str) -> Result<&mut Entry> {
        self.index.iter_mut().find(|e| e.id == id).ok_or_else(|| format!("Canvas not found: {id}"))
    }

    fn find(&self, id: &str) -> Result<&Entry> {
        self.index.iter().find(|e| e.id == id).ok_or_else(|| format!("Canvas not found: {id}"))
    }

    fn persist_index(&self) -> Result<()> {
        let json = serde_json::to_vec_pretty(&self.index).map_err(err)?;
        atomic_write(&self.index_path, &json).map_err(err)
    }

    fn persist_folders(&self) -> Result<()> {
        let json = serde_json::to_vec_pretty(&self.folders).map_err(err)?;
        atomic_write(&self.folders_path, &json).map_err(err)
    }

    fn find_folder_mut(&mut self, id: &str) -> Result<&mut Folder> {
        self.folders.iter_mut().find(|f| f.id == id).ok_or_else(|| format!("Folder not found: {id}"))
    }

    fn check_folder(&self, folder: Option<&str>) -> Result<()> {
        match folder {
            Some(id) if !self.folders.iter().any(|f| f.id == id) => Err(format!("Folder not found: {id}")),
            _ => Ok(()),
        }
    }

    // ---------------------------------------------------------- public API

    pub fn scene_path(&self, id: &str) -> Result<PathBuf> {
        if !is_valid_id(id) {
            return Err(format!("Invalid canvas id: {id}"));
        }
        self.find(id)?;
        Ok(self.scene_path_unchecked(id))
    }

    pub fn list(&self) -> Vec<CanvasMeta> {
        let mut entries: Vec<&Entry> = self.index.iter().collect();
        entries.sort_by(|a, b| b.updated_at.total_cmp(&a.updated_at));
        entries.into_iter().map(CanvasMeta::from).collect()
    }

    pub fn meta(&self, id: &str) -> Result<(CanvasMeta, Option<Viewport>)> {
        let e = self.find(id)?;
        Ok((e.into(), e.viewport))
    }

    /// A new canvas, filed in `folder` if given (at the top level otherwise).
    pub fn create(&mut self, name: &str, scene: Option<&str>, folder: Option<&str>) -> Result<CanvasMeta> {
        self.check_folder(folder)?;
        let now = now_ms();
        let entry = Entry {
            id: uuid::Uuid::new_v4().to_string(),
            name: clean_name(name, "Untitled"),
            created_at: now,
            updated_at: now,
            viewport: None,
            folder: folder.map(str::to_owned),
        };
        atomic_write(&self.scene_path_unchecked(&entry.id), scene.unwrap_or(EMPTY_SCENE).as_bytes())
            .map_err(err)?;
        let meta = CanvasMeta::from(&entry);
        self.index.push(entry);
        self.persist_index()?;
        Ok(meta)
    }

    pub fn exists(&self, id: &str) -> bool {
        self.find(id).is_ok()
    }

    pub fn load_scene(&self, id: &str) -> Result<Vec<u8>> {
        fs::read(self.scene_path(id)?).map_err(err)
    }

    pub fn save_scene(&mut self, id: &str, scene: &[u8]) -> Result<()> {
        let path = self.scene_path(id)?;
        atomic_write(&path, scene).map_err(err)?;
        self.find_mut(id)?.updated_at = now_ms();
        self.persist_index()
    }

    pub fn save_viewport(&mut self, id: &str, viewport: Viewport) -> Result<()> {
        if ![viewport.scroll_x, viewport.scroll_y, viewport.zoom].iter().all(|v| v.is_finite()) {
            return Ok(());
        }
        self.find_mut(id)?.viewport = Some(viewport);
        self.persist_index()
    }

    pub fn rename(&mut self, id: &str, name: &str) -> Result<CanvasMeta> {
        let entry = self.find_mut(id)?;
        entry.name = clean_name(name, &entry.name.clone());
        let meta = CanvasMeta::from(&*entry);
        self.persist_index()?;
        Ok(meta)
    }

    pub fn duplicate(&mut self, id: &str) -> Result<CanvasMeta> {
        let original = self.find(id)?;
        let name = format!("{} copy", original.name);
        let folder = original.folder.clone();
        let scene = fs::read_to_string(self.scene_path(id)?).map_err(err)?;
        self.create(&name, Some(&scene), folder.as_deref())
    }

    pub fn remove(&mut self, id: &str) -> Result<()> {
        let path = self.scene_path(id)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(err(e)),
        }
        self.index.retain(|e| e.id != id);
        self.persist_index()
    }

    /// Files a canvas in a folder, or takes it out (`None`).
    pub fn set_folder(&mut self, id: &str, folder: Option<&str>) -> Result<CanvasMeta> {
        self.check_folder(folder)?;
        let entry = self.find_mut(id)?;
        entry.folder = folder.map(str::to_owned);
        let meta = CanvasMeta::from(&*entry);
        self.persist_index()?;
        Ok(meta)
    }

    // ---------------------------------------------------------- folders

    pub fn folders(&self) -> Vec<Folder> {
        self.folders.clone()
    }

    pub fn create_folder(&mut self, name: &str) -> Result<Folder> {
        let folder = Folder { id: uuid::Uuid::new_v4().to_string(), name: clean_name(name, "Untitled folder"), created_at: now_ms() };
        self.folders.push(folder.clone());
        self.persist_folders()?;
        Ok(folder)
    }

    pub fn rename_folder(&mut self, id: &str, name: &str) -> Result<Folder> {
        let folder = self.find_folder_mut(id)?;
        folder.name = clean_name(name, &folder.name.clone());
        let folder = folder.clone();
        self.persist_folders()?;
        Ok(folder)
    }

    /// Deletes a folder; the canvases in it move to the top level.
    pub fn delete_folder(&mut self, id: &str) -> Result<()> {
        self.find_folder_mut(id)?;
        let mut moved = false;
        for e in self.index.iter_mut().filter(|e| e.folder.as_deref() == Some(id)) {
            e.folder = None;
            moved = true;
        }
        // Canvases first: if the second write fails, the folder is still there
        // (and empty) rather than its canvases pointing nowhere.
        if moved {
            self.persist_index()?;
        }
        self.folders.retain(|f| f.id != id);
        self.persist_folders()
    }

    pub fn read_settings(&self) -> Map<String, Value> {
        fs::read_to_string(&self.settings_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn write_settings(&self, patch: Map<String, Value>) -> Result<Map<String, Value>> {
        let mut settings = self.read_settings();
        settings.extend(patch);
        let json = serde_json::to_vec_pretty(&settings).map_err(err)?;
        atomic_write(&self.settings_path, &json).map_err(err)?;
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("drawlycool-storage-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn indexes_from_before_folders_still_load() {
        let base = temp("old-index");
        let dir = base.join("canvases");
        fs::create_dir_all(&dir).unwrap();
        let id = "0692fbd7-4930-4c1e-b6bf-e212aa52e820";
        fs::write(dir.join(format!("{id}.excalidraw")), EMPTY_SCENE).unwrap();
        let old = format!(r#"[{{"id":"{id}","name":"MySite","createdAt":1,"updatedAt":2}}]"#);
        fs::write(dir.join("index.json"), old).unwrap();

        let storage = Storage::open(base.clone()).unwrap();
        let list = storage.list();
        assert_eq!((list.len(), list[0].name.as_str(), list[0].folder.as_deref()), (1, "MySite", None));
        assert!(storage.folders().is_empty());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn canvases_in_a_missing_folder_return_to_the_top_level() {
        let base = temp("dangling");
        let mut storage = Storage::open(base.clone()).unwrap();
        let folder = storage.create_folder("Work").unwrap();
        let meta = storage.create("Plan", None, Some(&folder.id)).unwrap();
        assert_eq!(meta.folder.as_deref(), Some(folder.id.as_str()));
        // The folder list is lost (e.g. edited by hand, or written by an older version).
        fs::remove_file(base.join("canvases/folders.json")).unwrap();

        let storage = Storage::open(base.clone()).unwrap();
        assert_eq!(storage.list()[0].folder, None);
        let _ = fs::remove_dir_all(base);
    }
}

//! The authoritative copy of a shared canvas, merged with Excalidraw's own
//! rules so every client converges on the same scene.

use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// Deleted elements are kept this long so late edits and undo still merge
/// correctly, then dropped when the scene is saved.
const TOMBSTONE_TTL_MS: f64 = 24.0 * 3600.0 * 1000.0;

pub struct Scene {
    elements: HashMap<String, Value>,
    files: Map<String, Value>,
    app_state: Map<String, Value>,
}

fn version(element: &Value) -> (i64, i64) {
    let num = |key| element.get(key).and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))).unwrap_or(0);
    (num("version"), num("versionNonce"))
}

fn element_id(element: &Value) -> Option<&str> {
    element.get("id")?.as_str().filter(|id| !id.is_empty())
}

/// Same decision as Excalidraw's `reconcileElements`: the higher version wins,
/// ties go to the lower `versionNonce`.
fn incoming_wins(current: &Value, incoming: &Value) -> bool {
    let (cv, cn) = version(current);
    let (iv, inn) = version(incoming);
    iv > cv || (iv == cv && inn < cn)
}

impl Scene {
    pub fn empty() -> Self {
        Self { elements: HashMap::new(), files: Map::new(), app_state: Map::new() }
    }

    /// Parses a stored `.excalidraw` file. Unknown or broken parts are dropped.
    pub fn from_json(bytes: &[u8]) -> Self {
        let mut scene = Self::empty();
        let Ok(Value::Object(mut root)) = serde_json::from_slice::<Value>(bytes) else {
            return scene;
        };
        if let Some(Value::Array(elements)) = root.remove("elements") {
            for element in elements {
                if let Some(id) = element_id(&element) {
                    scene.elements.insert(id.to_owned(), element);
                }
            }
        }
        if let Some(Value::Object(files)) = root.remove("files") {
            scene.files = files;
        }
        if let Some(Value::Object(app_state)) = root.remove("appState") {
            scene.app_state = app_state;
        }
        scene
    }

    /// Merges elements sent by a client; returns the ones that were newer.
    pub fn merge(&mut self, incoming: Vec<Value>) -> Vec<Value> {
        let mut accepted = Vec::new();
        for element in incoming {
            let Some(id) = element_id(&element).map(str::to_owned) else { continue };
            if !element.is_object() {
                continue;
            }
            let newer = self.elements.get(&id).map_or(true, |current| incoming_wins(current, &element));
            if newer {
                accepted.push(element.clone());
                self.elements.insert(id, element);
            }
        }
        accepted
    }

    /// Adds image files; returns the ones that weren't known yet.
    pub fn add_files(&mut self, files: Vec<Value>) -> Vec<Value> {
        let mut added = Vec::new();
        for file in files {
            let Some(id) = file.get("id").and_then(Value::as_str).map(str::to_owned) else { continue };
            if file.get("dataURL").and_then(Value::as_str).is_none() || self.files.contains_key(&id) {
                continue;
            }
            added.push(file.clone());
            self.files.insert(id, file);
        }
        added
    }

    pub fn background(&self) -> Option<&str> {
        self.app_state.get("viewBackgroundColor").and_then(Value::as_str)
    }

    pub fn set_background(&mut self, color: &str) {
        self.app_state.insert("viewBackgroundColor".into(), color.into());
    }

    /// Elements in z-order (Excalidraw's fractional `index`).
    pub fn elements(&self) -> Vec<&Value> {
        let mut list: Vec<&Value> = self.elements.values().collect();
        list.sort_by(|a, b| {
            let key = |e: &Value| e.get("index").and_then(Value::as_str).map(str::to_owned);
            match (key(a), key(b)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| element_id(a).cmp(&element_id(b)))
        });
        list
    }

    pub fn get(&self, id: &str) -> Option<&Value> {
        self.elements.get(id)
    }

    pub fn files(&self) -> &Map<String, Value> {
        &self.files
    }

    /// Drops long-deleted elements and images nothing points at anymore.
    pub fn prune(&mut self, now_ms: f64) {
        self.elements.retain(|_, e| {
            let deleted = e.get("isDeleted").and_then(Value::as_bool).unwrap_or(false);
            let updated = e.get("updated").and_then(Value::as_f64).unwrap_or(now_ms);
            !deleted || now_ms - updated < TOMBSTONE_TTL_MS
        });
        let used: std::collections::HashSet<&str> =
            self.elements.values().filter_map(|e| e.get("fileId").and_then(Value::as_str)).collect();
        self.files.retain(|id, _| used.contains(id.as_str()));
    }

    /// A standard `.excalidraw` file.
    pub fn to_json(&self) -> Vec<u8> {
        let scene = json!({
            "type": "excalidraw",
            "version": 2,
            "source": "drawlycool",
            "elements": self.elements(),
            "appState": self.app_state,
            "files": self.files,
        });
        serde_json::to_vec(&scene).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(id: &str, version: i64, nonce: i64) -> Value {
        json!({ "id": id, "type": "rectangle", "version": version, "versionNonce": nonce })
    }

    #[test]
    fn newer_version_wins_and_ties_go_to_lower_nonce() {
        let mut scene = Scene::empty();
        assert_eq!(scene.merge(vec![el("a", 2, 50)]).len(), 1);
        assert!(scene.merge(vec![el("a", 1, 10)]).is_empty(), "older version is ignored");
        assert!(scene.merge(vec![el("a", 2, 60)]).is_empty(), "tie with higher nonce loses");
        assert_eq!(scene.merge(vec![el("a", 2, 40)]).len(), 1, "tie with lower nonce wins");
        assert_eq!(scene.merge(vec![el("a", 3, 99)]).len(), 1);
        assert_eq!(scene.elements()[0]["version"], 3);
    }

    #[test]
    fn prune_drops_old_tombstones_and_orphan_files() {
        let mut scene = Scene::empty();
        let now = 10.0 * TOMBSTONE_TTL_MS;
        scene.merge(vec![
            json!({ "id": "old", "version": 2, "versionNonce": 1, "isDeleted": true, "updated": now - TOMBSTONE_TTL_MS - 1.0 }),
            json!({ "id": "recent", "version": 2, "versionNonce": 1, "isDeleted": true, "updated": now - 1000.0 }),
            json!({ "id": "img", "version": 1, "versionNonce": 1, "fileId": "f1" }),
        ]);
        scene.add_files(vec![
            json!({ "id": "f1", "dataURL": "data:image/png;base64,AA" }),
            json!({ "id": "f2", "dataURL": "data:image/png;base64,AA" }),
        ]);
        scene.prune(now);
        let ids: Vec<_> = scene.elements().iter().map(|e| e["id"].as_str().unwrap().to_owned()).collect();
        assert_eq!(ids.len(), 2);
        assert!(!ids.contains(&"old".to_owned()));
        assert!(scene.files().contains_key("f1") && !scene.files().contains_key("f2"));
    }

    #[test]
    fn elements_come_out_in_fractional_index_order() {
        let mut scene = Scene::empty();
        scene.merge(vec![
            json!({ "id": "b", "version": 1, "versionNonce": 1, "index": "a1" }),
            json!({ "id": "a", "version": 1, "versionNonce": 1, "index": "a0" }),
            json!({ "id": "c", "version": 1, "versionNonce": 1, "index": "Zz" }),
        ]);
        let ids: Vec<_> = scene.elements().iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["c", "a", "b"]);
    }
}

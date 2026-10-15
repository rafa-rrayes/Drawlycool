//! A working copy of a canvas that one tool call edits. Changes are grouped
//! into steps (what appears together while the agent "draws"), and merged
//! into the live scene field by field, so a person editing the same element
//! meanwhile keeps their change to the fields the agent didn't touch.

use super::geometry::{Pt, Rect};
use super::model;
use crate::fractional;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Z {
    Front,
    Back,
}

pub struct Step {
    pub ids: Vec<String>,
    /// Where the cursor goes for this step.
    pub anchor: Pt,
    /// Shown as selected while it's drawn.
    pub focus: Option<String>,
}

pub struct Draft {
    /// The scene as it was read (deleted elements included).
    base: HashMap<String, Value>,
    els: HashMap<String, Value>,
    changed: Vec<String>,
    changed_set: HashSet<String>,
    pub steps: Vec<Step>,
    top: Option<String>,
    bottom: Option<String>,
    pub files: Map<String, Value>,
    pub background: Option<String>,
}

pub fn random_u32() -> u32 {
    (uuid::Uuid::new_v4().as_u128() & 0x7fff_ffff) as u32
}

impl Draft {
    pub fn new(elements: Vec<Value>, files: Map<String, Value>, background: Option<String>) -> Self {
        let mut top: Option<String> = None;
        let mut bottom: Option<String> = None;
        let mut base = HashMap::new();
        for el in elements {
            if let Some(i) = model::index(&el).filter(|i| fractional::is_valid(i)) {
                if top.as_deref().is_none_or(|t| i > t) {
                    top = Some(i.to_owned());
                }
                if bottom.as_deref().is_none_or(|b| i < b) {
                    bottom = Some(i.to_owned());
                }
            }
            base.insert(model::id(&el).to_owned(), el);
        }
        Self {
            els: base.clone(),
            base,
            changed: Vec::new(),
            changed_set: HashSet::new(),
            steps: Vec::new(),
            top,
            bottom,
            files,
            background,
        }
    }

    /// A live (not deleted) element.
    pub fn get(&self, id: &str) -> Option<&Value> {
        self.els.get(id).filter(|e| !model::is_deleted(e))
    }

    pub fn exists(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    /// Live elements in z-order.
    pub fn alive(&self) -> Vec<&Value> {
        let mut list: Vec<&Value> = self.els.values().filter(|e| !model::is_deleted(e)).collect();
        list.sort_by(|a, b| {
            let key = |e: &Value| model::index(e).map(str::to_owned);
            match (key(a), key(b)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| model::id(a).cmp(model::id(b)))
        });
        list
    }

    pub fn by_id(&self) -> HashMap<&str, &Value> {
        self.els.iter().filter(|(_, e)| !model::is_deleted(e)).map(|(k, v)| (k.as_str(), v)).collect()
    }

    /// Ids changed by this draft, in the order they were first changed.
    pub fn changed(&self) -> &[String] {
        &self.changed
    }

    pub fn is_new(&self, id: &str) -> bool {
        self.base.get(id).is_none_or(model::is_deleted)
    }

    /// Starts a new drawing step.
    pub fn step(&mut self, anchor: Pt, focus: Option<&str>) {
        self.steps.push(Step { ids: Vec::new(), anchor, focus: focus.map(str::to_owned) });
    }

    fn mark(&mut self, id: &str) {
        if self.steps.is_empty() {
            self.step((0.0, 0.0), None);
        }
        let step = self.steps.last_mut().expect("step");
        if !step.ids.iter().any(|i| i == id) {
            step.ids.push(id.to_owned());
        }
        if self.changed_set.insert(id.to_owned()) {
            self.changed.push(id.to_owned());
        }
    }

    /// Adds a new element (or brings back a deleted id) at the top or bottom
    /// of the z-order.
    pub fn insert(&mut self, mut el: Value, z: Z) {
        let id = model::id(&el).to_owned();
        let index = match z {
            Z::Front => fractional::key_between(self.top.as_deref(), None),
            Z::Back => fractional::key_between(None, self.bottom.as_deref()),
        };
        match z {
            Z::Front => self.top = Some(index.clone()),
            Z::Back => {
                if self.top.is_none() {
                    self.top = Some(index.clone());
                }
                self.bottom = Some(index.clone());
            }
        }
        el["index"] = Value::String(index);
        el["isDeleted"] = Value::Bool(false);
        self.els.insert(id.clone(), el);
        self.mark(&id);
    }

    /// Changes an existing element in place.
    pub fn update(&mut self, id: &str, change: impl FnOnce(&mut Value)) -> bool {
        let Some(el) = self.els.get_mut(id) else { return false };
        change(el);
        self.mark(id);
        true
    }

    /// Moves an element to the top or bottom of the z-order.
    pub fn restack(&mut self, id: &str, z: Z) {
        if !self.els.contains_key(id) {
            return;
        }
        let index = match z {
            Z::Front => fractional::key_between(self.top.as_deref(), None),
            Z::Back => fractional::key_between(None, self.bottom.as_deref()),
        };
        match z {
            Z::Front => self.top = Some(index.clone()),
            Z::Back => self.bottom = Some(index.clone()),
        }
        self.update(id, |el| el["index"] = Value::String(index));
    }

    /// Moves elements to sit just above `anchor` in the z-order, keeping their order.
    pub fn restack_above(&mut self, ids: &[String], anchor: &str) {
        let order = self.alive();
        let pos = order.iter().position(|e| model::id(e) == anchor);
        let Some(pos) = pos else { return };
        let lo = model::index(order[pos]).map(str::to_owned);
        let hi = order[pos + 1..].iter().find(|e| !ids.iter().any(|i| i == model::id(e))).and_then(|e| model::index(e)).map(str::to_owned);
        let keys = fractional::keys_between(lo.as_deref(), hi.as_deref(), ids.len());
        for (id, key) in ids.iter().zip(keys) {
            self.update(id, |el| el["index"] = Value::String(key));
        }
    }

    pub fn delete(&mut self, id: &str) {
        self.update(id, |el| el["isDeleted"] = Value::Bool(true));
    }

    /// Bounds of changed, live elements.
    pub fn changed_bounds(&self) -> Option<Rect> {
        super::geometry::union_all(self.changed.iter().filter_map(|id| self.get(id)).map(model::bounds))
    }

    /// The element to write for `id`, given what the live scene has now.
    /// Fields this draft changed are applied on top of the live version.
    pub fn finalize(&self, id: &str, live: Option<&Value>) -> Option<Value> {
        let draft = self.els.get(id)?;
        let base = self.base.get(id);
        let mut out = match (base, live) {
            (Some(b), Some(l)) if model::version(b) != model::version(l) => merge_fields(b, draft, l),
            _ => draft.clone(),
        };
        let version = live.map(model::version).max(base.map(model::version)).unwrap_or(0) + 1;
        out["version"] = version.into();
        out["versionNonce"] = random_u32().into();
        out["updated"] = crate::storage::now_ms().into();
        Some(out)
    }
}

/// `live` with the fields that changed between `base` and `draft` applied.
/// `boundElements` lists are merged entry by entry.
fn merge_fields(base: &Value, draft: &Value, live: &Value) -> Value {
    let (Some(b), Some(d), Some(l)) = (base.as_object(), draft.as_object(), live.as_object()) else {
        return draft.clone();
    };
    let mut out = l.clone();
    for (key, value) in d {
        if b.get(key) == Some(value) {
            continue;
        }
        if key == "boundElements" {
            let entries = |v: Option<&Value>| -> Vec<Value> { v.and_then(Value::as_array).cloned().unwrap_or_default() };
            let ids = |list: &[Value]| -> HashSet<String> {
                list.iter().filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_owned)).collect()
            };
            let (before, after, now) = (entries(b.get(key)), entries(Some(value)), entries(l.get(key)));
            let (before_ids, after_ids) = (ids(&before), ids(&after));
            let mut merged: Vec<Value> = now
                .into_iter()
                .filter(|e| e.get("id").and_then(Value::as_str).is_none_or(|i| !before_ids.contains(i) || after_ids.contains(i)))
                .collect();
            let have = ids(&merged);
            merged.extend(after.into_iter().filter(|e| e.get("id").and_then(Value::as_str).is_some_and(|i| !have.contains(i))));
            out.insert(key.clone(), if merged.is_empty() { Value::Null } else { Value::Array(merged) });
        } else {
            out.insert(key.clone(), value.clone());
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn concurrent_edits_keep_untouched_fields() {
        let base = json!({ "id": "a", "version": 3, "x": 0, "strokeColor": "#000", "boundElements": [{ "id": "t", "type": "text" }] });
        let mut d = Draft::new(vec![base], Map::new(), None);
        d.update("a", |el| {
            el["x"] = 50.into();
            el["boundElements"] = json!([{ "id": "t", "type": "text" }, { "id": "arrow1", "type": "arrow" }]);
        });
        // Someone recoloured it and bound their own arrow meanwhile.
        let live = json!({ "id": "a", "version": 5, "x": 0, "strokeColor": "#f00",
            "boundElements": [{ "id": "t", "type": "text" }, { "id": "theirs", "type": "arrow" }] });
        let out = d.finalize("a", Some(&live)).unwrap();
        assert_eq!(out["x"], 50);
        assert_eq!(out["strokeColor"], "#f00");
        assert_eq!(out["version"], 6);
        let ids: Vec<&str> = out["boundElements"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["t", "theirs", "arrow1"]);
    }

    #[test]
    fn new_elements_stack_on_top() {
        let mut d = Draft::new(vec![json!({ "id": "a", "version": 1, "index": "a5" })], Map::new(), None);
        d.insert(json!({ "id": "b" }), Z::Front);
        d.insert(json!({ "id": "c" }), Z::Back);
        let order: Vec<&str> = d.alive().iter().map(|e| model::id(e)).collect();
        assert_eq!(order, ["c", "a", "b"]);
    }
}

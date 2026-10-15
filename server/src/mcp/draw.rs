//! The `draw` tool: creates or updates elements from a list of loose,
//! model-friendly specs, and the editing helpers other tools share (moving
//! things with their labels and arrows, deleting cleanly).

use super::args::{as_point, as_points, slug, Args};
use super::build::{self, End, Route, Style};
use super::colors::{self, Slot};
use super::draft::{Draft, Z};
use super::geometry::{self as geo, Pt, Rect, Side};
use super::model;
use serde_json::{json, Value};
use std::collections::HashSet;

pub const DEFAULT_GAP: f64 = 60.0;

/// Normalised element type, accepting common synonyms.
pub fn element_type(t: &str) -> Option<&'static str> {
    Some(match t.trim().to_ascii_lowercase().replace(['_', ' '], "-").as_str() {
        "rectangle" | "rect" | "box" | "square" | "card" | "node" | "process" | "button" | "container" | "zone" => "rectangle",
        "ellipse" | "circle" | "oval" | "round" | "terminal" | "start" | "end" => "ellipse",
        "diamond" | "rhombus" | "decision" => "diamond",
        "text" | "label" | "title" | "heading" | "caption" | "paragraph" => "text",
        "arrow" | "connector" | "edge" | "link" | "connection" => "arrow",
        "line" | "polyline" | "polygon" | "divider" => "line",
        "frame" | "section" | "artboard" => "frame",
        "freedraw" | "freehand" | "pen" | "scribble" | "sketch" => "freedraw",
        "sticky" | "note" | "sticky-note" | "post-it" => "note",
        _ => return None,
    })
}

/// What a tool call did, for the reply.
#[derive(Default)]
pub struct Outcome {
    /// Main elements created or changed (not labels), in order.
    pub touched: Vec<String>,
    pub created: usize,
    pub updated: usize,
    pub notes: Vec<String>,
}

impl Outcome {
    pub fn touch(&mut self, id: &str) {
        if !self.touched.iter().any(|t| t == id) {
            self.touched.push(id.to_owned());
        }
    }
}

/// A fresh, readable id: the label's slug, or the type plus a number.
pub fn fresh_id(draft: &Draft, hint: &str, kind: &str) -> String {
    let base = match slug(hint) {
        s if s.is_empty() => kind.to_owned(),
        s => s,
    };
    if !draft.id_taken(&base) && !base.chars().all(|c| c.is_ascii_digit()) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|id| !draft.id_taken(id)).expect("free id")
}

fn label_id(draft: &Draft, owner: &str) -> String {
    let id = format!("{owner}-label");
    if !draft.id_taken(&id) || draft.get(&id).is_some_and(|t| model::container_id(t) == Some(owner)) {
        return id;
    }
    fresh_id(draft, &id, "label")
}

fn ref_rect(draft: &Draft, id: &str) -> Result<Rect, String> {
    draft.get(id).map(model::bounds).ok_or_else(|| format!("No element with id “{id}” (used for relative placement)."))
}

/// Top-left for a `w`×`h` element: explicit `x`/`y`, `center`, relative to
/// another element (`below`, `right_of`, …), or `fallback`.
pub fn position(a: &Args, draft: &Draft, w: f64, h: f64, fallback: impl FnOnce() -> Pt) -> Result<Pt, String> {
    let gap = a.num("gap").unwrap_or(DEFAULT_GAP);
    let align = a.str("align").unwrap_or("center").to_ascii_lowercase();
    let cross = |start: f64, len: f64, own: f64| match align.as_str() {
        "start" | "left" | "top" => start,
        "end" | "right" | "bottom" => start + len - own,
        _ => start + (len - own) / 2.0,
    };
    let rels: [(&[&str], u8); 4] = [
        (&["below", "under", "beneath"], 0),
        (&["above", "over"], 1),
        (&["right_of", "right", "after", "next_to"], 2),
        (&["left_of", "left", "before"], 3),
    ];
    for (keys, dir) in rels {
        if let Some(target) = a.any(keys).and_then(Value::as_str) {
            let r = ref_rect(draft, target)?;
            let (mut x, mut y) = match dir {
                0 => (cross(r.x, r.w, w), r.bottom() + gap),
                1 => (cross(r.x, r.w, w), r.y - gap - h),
                2 => (r.right() + gap, cross(r.y, r.h, h)),
                _ => (r.x - gap - w, cross(r.y, r.h, h)),
            };
            if let Some(dx) = a.num("dx") {
                x += dx;
            }
            if let Some(dy) = a.num("dy") {
                y += dy;
            }
            return Ok((x.round(), y.round()));
        }
    }
    if let Some((cx, cy)) = a.point("center").or_else(|| Some((a.num("cx")?, a.num("cy")?))) {
        return Ok(((cx - w / 2.0).round(), (cy - h / 2.0).round()));
    }
    if let Some(p) = a.point("at").or_else(|| a.point("position")) {
        return Ok(p);
    }
    match (a.num("x"), a.num("y")) {
        (Some(x), Some(y)) => Ok((x, y)),
        (x, y) => {
            let (fx, fy) = fallback();
            Ok((x.unwrap_or(fx), y.unwrap_or(fy)))
        }
    }
}

fn end_of(a: &Args, keys: &[&str]) -> Option<End> {
    let v = a.any(keys)?;
    if let Some(id) = v.as_str().filter(|s| as_point(v).is_none() || !s.contains(',')) {
        return Some(End::Element(id.trim().to_owned()));
    }
    as_point(v).map(End::At)
}

fn side(a: &Args, keys: &[&str]) -> Option<Side> {
    a.any(keys).and_then(Value::as_str).and_then(Side::parse)
}

fn arrowheads(a: &Args, el: &mut Value, default_end: bool) -> Result<(), String> {
    let mut start = el.get("startArrowhead").cloned().unwrap_or(Value::Null);
    let mut end = el.get("endArrowhead").cloned().unwrap_or(if default_end { "arrow".into() } else { Value::Null });
    if let Some(s) = a.str("arrowheads") {
        let head = |v: &Value| if v.is_null() { Value::from("arrow") } else { v.clone() };
        match s.to_ascii_lowercase().as_str() {
            "both" | "two-way" | "bidirectional" => (start, end) = (head(&end), head(&end)),
            "none" | "no" => (start, end) = (Value::Null, Value::Null),
            "start" | "reverse" | "backward" => (start, end) = (head(&end), Value::Null),
            _ => (start, end) = (Value::Null, head(&end)),
        }
    }
    if a.bool("bidirectional") == Some(true) || a.bool("both_ends") == Some(true) {
        start = if end.is_null() { "arrow".into() } else { end.clone() };
    }
    let parse = |name: &str| build::arrowhead(name).ok_or_else(|| format!("Unknown arrowhead “{name}”. Use arrow, triangle, triangle_outline, circle, circle_outline, diamond, diamond_outline, bar, crowfoot_one, crowfoot_many, crowfoot_one_or_many or none."));
    if let Some(v) = a.any(&["start_arrowhead", "start_head", "tail"]) {
        start = if v.is_null() || v == &Value::Bool(false) { Value::Null } else { parse(v.as_str().unwrap_or("arrow"))? };
    }
    if let Some(v) = a.any(&["end_arrowhead", "end_head", "head", "arrowhead"]) {
        end = if v.is_null() || v == &Value::Bool(false) { Value::Null } else { parse(v.as_str().unwrap_or("arrow"))? };
    }
    el["startArrowhead"] = start;
    el["endArrowhead"] = end;
    Ok(())
}

/// Common properties every element takes.
fn common(a: &Args, draft: &Draft, el: &mut Value) -> Result<(), String> {
    if let Some(g) = a.strings("group").or_else(|| a.strings("groups")) {
        el["groupIds"] = json!(g.iter().map(|g| slug(g)).filter(|g| !g.is_empty()).collect::<Vec<_>>());
    }
    if let Some(f) = a.get("frame").or_else(|| a.get("in_frame")) {
        match f.as_str().map(str::trim).filter(|s| !s.is_empty()) {
            Some(fid) if draft.get(fid).is_some_and(model::is_frame) => el["frameId"] = fid.into(),
            Some(fid) => return Err(format!("“{fid}” isn’t a frame.")),
            None => el["frameId"] = Value::Null,
        }
    }
    if let Some(link) = a.get("link") {
        el["link"] = link.as_str().map(Value::from).unwrap_or(Value::Null);
    }
    if let Some(l) = a.bool("locked") {
        el["locked"] = l.into();
    }
    if let Some(deg) = a.num("angle").or_else(|| a.num("rotation")) {
        el["angle"] = (deg.to_radians() % std::f64::consts::TAU).into();
    }
    Ok(())
}

/// Adds or replaces a container's label.
pub fn set_label(draft: &mut Draft, owner: &str, content: &str, style: &Style, a: &Args) {
    let existing = draft.get(owner).and_then(model::bound_text_id).filter(|t| draft.exists(t));
    if content.trim().is_empty() {
        if let Some(t) = existing {
            draft.delete(&t);
            draft.update(owner, |o| build::remove_bound(o, &t));
        }
        return;
    }
    let Some(container) = draft.get(owner).cloned() else { return };
    let mut label = match existing.as_deref().and_then(|t| draft.get(t)).cloned() {
        Some(mut t) => {
            let mut st = build::style_of(&t);
            st.text_color = None;
            let _ = st.apply(a, false);
            if let Some(c) = st.text_color.clone() {
                t["strokeColor"] = c.into();
            }
            t["fontSize"] = st.font_size.into();
            t["fontFamily"] = st.font_family.into();
            t["lineHeight"] = st.font().line_height.into();
            if let Some(al) = &st.text_align {
                t["textAlign"] = al.clone().into();
            }
            if let Some(v) = &st.vertical_align {
                t["verticalAlign"] = v.clone().into();
            }
            t["originalText"] = content.into();
            t["text"] = content.into();
            t
        }
        None => {
            let id = label_id(draft, owner);
            let mut st = style.clone();
            if model::kind(&container) == "arrow" && !a.has("font_size") {
                st.font_size = (style.font_size - 4.0).max(14.0);
            }
            let mut t = build::text_element(&id, 0.0, 0.0, content, &st, Some(owner));
            t["groupIds"] = container.get("groupIds").cloned().unwrap_or(json!([]));
            t["frameId"] = container.get("frameId").cloned().unwrap_or(Value::Null);
            t
        }
    };
    let mut container = container;
    let grew = build::fit_label(&mut container, &mut label);
    let label_id = model::id(&label).to_owned();
    if grew.is_some() {
        let (w, h) = (container["width"].clone(), container["height"].clone());
        draft.update(owner, |o| {
            o["width"] = w;
            o["height"] = h;
        });
    }
    if draft.exists(&label_id) {
        draft.update(&label_id, |t| *t = label);
    } else {
        draft.insert(label, Z::Front);
    }
    draft.update(owner, |o| build::add_bound(o, &label_id, "text"));
}

/// Re-centres a container's label after it moved or changed size.
pub fn refit_label(draft: &mut Draft, owner: &str) {
    let Some(container) = draft.get(owner).cloned() else { return };
    let Some(tid) = model::bound_text_id(&container).filter(|t| draft.exists(t)) else { return };
    let Some(mut label) = draft.get(&tid).cloned() else { return };
    let mut c = container;
    let grew = build::fit_label(&mut c, &mut label);
    if grew.is_some() {
        let (w, h) = (c["width"].clone(), c["height"].clone());
        draft.update(owner, |o| {
            o["width"] = w;
            o["height"] = h;
        });
    }
    draft.update(&tid, |t| *t = label);
}

/// Arrows bound to any of `ids`.
pub fn arrows_bound_to(draft: &Draft, ids: &HashSet<String>) -> Vec<String> {
    draft
        .alive()
        .into_iter()
        .filter(|e| model::kind(e) == "arrow")
        .filter(|e| {
            [model::binding_target(e, "startBinding"), model::binding_target(e, "endBinding")]
                .into_iter()
                .flatten()
                .any(|t| ids.contains(t))
        })
        .map(|e| model::id(e).to_owned())
        .collect()
}

/// Moves elements by (dx, dy), with their labels and (for frames) their
/// contents, then re-routes arrows attached to them.
pub fn move_by(draft: &mut Draft, ids: &[String], dx: f64, dy: f64) {
    if dx.abs() < 1e-9 && dy.abs() < 1e-9 {
        return;
    }
    let mut moving: Vec<String> = Vec::new();
    let add = |id: &str, list: &mut Vec<String>| {
        if !list.iter().any(|i| i == id) {
            list.push(id.to_owned());
        }
    };
    for id in ids {
        let Some(el) = draft.get(id) else { continue };
        add(id, &mut moving);
        if model::is_frame(el) {
            let kids: Vec<String> = draft.alive().iter().filter(|e| model::frame_id(e) == Some(id)).map(|e| model::id(e).to_owned()).collect();
            for k in kids {
                add(&k, &mut moving);
            }
        }
    }
    let labels: Vec<String> = moving.iter().filter_map(|id| draft.get(id).and_then(model::bound_text_id)).collect();
    for l in labels {
        add(&l, &mut moving);
    }
    let set: HashSet<String> = moving.iter().cloned().collect();
    for id in &moving {
        let Some(el) = draft.get(id) else { continue };
        // Arrows bound at both ends follow their shapes instead.
        let both_bound = model::kind(el) == "arrow"
            && model::binding_target(el, "startBinding").is_some()
            && model::binding_target(el, "endBinding").is_some();
        if both_bound {
            continue;
        }
        draft.update(id, |e| {
            e["x"] = build::round2(model::x(e) + dx).into();
            e["y"] = build::round2(model::y(e) + dy).into();
        });
    }
    for arrow in arrows_bound_to(draft, &set) {
        build::reroute(draft, &arrow);
    }
}

/// Deletes elements and what hangs off them: labels, frame contents, and
/// (unless `keep_arrows`) arrows connected to them. Returns all deleted ids.
pub fn delete_ids(draft: &mut Draft, ids: &[String], keep_arrows: bool) -> Vec<String> {
    let mut gone: Vec<String> = Vec::new();
    let mut queue: Vec<String> = ids.iter().filter(|id| draft.exists(id)).cloned().collect();
    while let Some(id) = queue.pop() {
        if gone.contains(&id) {
            continue;
        }
        let Some(el) = draft.get(&id).cloned() else { continue };
        gone.push(id.clone());
        if let Some(t) = model::bound_text_id(&el) {
            queue.push(t);
        }
        if model::is_frame(&el) {
            queue.extend(draft.alive().iter().filter(|e| model::frame_id(e) == Some(id.as_str())).map(|e| model::id(e).to_owned()));
        }
        if !keep_arrows && !model::is_linear(&el) {
            queue.extend(arrows_bound_to(draft, &HashSet::from([id.clone()])));
        }
    }
    let gone_set: HashSet<String> = gone.iter().cloned().collect();
    for id in &gone {
        draft.delete(id);
    }
    // Unhook survivors from what was deleted.
    let survivors: Vec<Value> = draft.alive().into_iter().cloned().collect();
    for el in survivors {
        let id = model::id(&el).to_owned();
        let bound_gone: Vec<String> = model::bound_elements(&el).into_iter().map(|(b, _)| b).filter(|b| gone_set.contains(b)).collect();
        let start_gone = model::binding_target(&el, "startBinding").is_some_and(|t| gone_set.contains(t));
        let end_gone = model::binding_target(&el, "endBinding").is_some_and(|t| gone_set.contains(t));
        let container_gone = model::container_id(&el).is_some_and(|c| gone_set.contains(c));
        if bound_gone.is_empty() && !start_gone && !end_gone && !container_gone {
            continue;
        }
        if container_gone {
            draft.delete(&id);
            continue;
        }
        draft.update(&id, |e| {
            for b in &bound_gone {
                build::remove_bound(e, b);
            }
            if start_gone {
                e["startBinding"] = Value::Null;
            }
            if end_gone {
                e["endBinding"] = Value::Null;
            }
        });
    }
    gone
}

/// Where the next auto-placed element goes: right of the previous one in
/// this call, else in free space.
pub struct Cursor {
    pub last: Option<Rect>,
}

impl Cursor {
    pub fn place(&self, draft: &Draft, w: f64, h: f64, near: Option<Pt>) -> Pt {
        if let Some(r) = self.last {
            return (r.right() + DEFAULT_GAP, r.y);
        }
        let alive = draft.alive();
        let p = super::describe::free_spot(&alive, w, h, 40.0, near);
        (p.0.round(), p.1.round())
    }
}

/// Applies one `draw` spec.
pub fn draw_one(draft: &mut Draft, item: &Value, cursor: &mut Cursor, out: &mut Outcome, near: Option<Pt>) -> Result<String, String> {
    let a = Args::of(item);
    if !item.is_object() {
        return Err("Each element must be an object like {\"type\": \"rectangle\", \"label\": \"…\"}.".into());
    }
    let given_id = a.text("id").map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    if let Some(id) = &given_id {
        if draft.exists(id) {
            return update_one(draft, id, &a, out).map(|_| id.clone());
        }
    }
    let raw_type = a.str("type").or_else(|| a.str("kind")).or_else(|| a.str("shape"));
    let kind = match raw_type {
        Some(t) => element_type(t).ok_or_else(|| format!("Unknown type “{t}”. Use rectangle, ellipse, diamond, text, arrow, line, frame, freedraw or note."))?,
        None if a.has("from") || a.has("to") || a.has("points") => "arrow",
        None if a.has("text") && !a.has("label") => "text",
        None => "rectangle",
    };
    let circle = raw_type.is_some_and(|t| t.eq_ignore_ascii_case("circle"));
    let label = a.text("label").or_else(|| if kind != "text" { a.text("text") } else { None });
    let hint = label.clone().or_else(|| a.text("text")).or_else(|| a.text("name")).unwrap_or_default();
    let id = given_id.unwrap_or_else(|| {
        if kind == "arrow" || kind == "line" {
            let name = |k: &str| a.str(k).filter(|s| !s.contains(',')).map(str::to_owned);
            match (name("from"), name("to")) {
                (Some(f), Some(t)) => fresh_id(draft, &format!("{f}-to-{t}"), kind),
                _ => fresh_id(draft, &hint, kind),
            }
        } else {
            fresh_id(draft, &hint, kind)
        }
    });

    let mut style = Style::default();
    if kind == "note" {
        style.stroke = colors::resolve("yellow", Slot::Stroke).unwrap_or_default();
        style.fill = colors::resolve("yellow", Slot::Fill).unwrap_or_default();
        style.text_color = Some(colors::BLACK.into());
        style.text_align = Some("left".into());
        style.vertical_align = Some("top".into());
        style.font_size = 16.0;
    }
    style.apply(&a, matches!(kind, "rectangle" | "ellipse" | "diamond" | "note" | "frame"))?;

    match kind {
        "rectangle" | "ellipse" | "diamond" | "note" => {
            let shape = if kind == "note" { "rectangle" } else { kind };
            let contains = a.strings("contains").or_else(|| a.strings("children"));
            let (r, z) = if let Some(ids) = &contains {
                let pad = a.num("padding").unwrap_or(30.0);
                let inner = geo::union_all(ids.iter().filter_map(|i| draft.get(i)).map(model::bounds))
                    .ok_or("“contains” lists no existing elements.")?;
                let top = if label.is_some() { pad + style.font_size * 1.25 + 8.0 } else { pad };
                if label.is_some() {
                    style.vertical_align.get_or_insert("top".into());
                    style.text_align.get_or_insert("left".into());
                }
                (Rect::new(inner.x - pad, inner.y - top, inner.w + 2.0 * pad, inner.h + pad + top), Z::Back)
            } else {
                let max_text = a.num("max_text_width").or_else(|| a.num("wrap"));
                let (aw, ah) = if kind == "note" {
                    let (w, h) = build::auto_size("rectangle", label.as_deref(), &style, Some(max_text.unwrap_or(200.0)));
                    (w.max(200.0), h.max(120.0))
                } else {
                    build::auto_size(shape, label.as_deref(), &style, max_text)
                };
                let (mut w, mut h) = (a.num("width").unwrap_or(aw), a.num("height").unwrap_or(ah));
                if circle {
                    let d = a.num("width").or_else(|| a.num("height")).or_else(|| a.num("diameter")).unwrap_or(aw.max(ah));
                    (w, h) = (d, d);
                }
                let at = position(&a, draft, w, h, || cursor.place(draft, w, h, near))?;
                let z = if a.str("z").is_some_and(|z| z.eq_ignore_ascii_case("back")) { Z::Back } else { Z::Front };
                (Rect::new(at.0, at.1, w.max(1.0), h.max(1.0)), z)
            };
            let mut el = build::base(shape, &id, r, &style);
            common(&a, draft, &mut el)?;
            draft.step(r.center(), Some(&id));
            draft.insert(el, z);
            if let Some(l) = &label {
                set_label(draft, &id, l, &style, &a);
            }
            if let Some(ids) = contains {
                if z == Z::Back {
                    let note = format!("“{id}” sits behind {} element(s) as a background zone.", ids.len());
                    out.notes.push(note);
                }
            }
        }
        "text" => {
            let content = a.text("text").or_else(|| label.clone()).ok_or("A text element needs \"text\".")?;
            let mut el = build::text_element(&id, 0.0, 0.0, &content, &style, None);
            if let Some(w) = a.num("width") {
                let font = style.font();
                let wrapped = font.wrap(&content, style.font_size, w);
                el["text"] = wrapped.clone().into();
                el["width"] = w.into();
                el["height"] = font.text_height(&wrapped, style.font_size).into();
                el["autoResize"] = false.into();
            }
            let (w, h) = (model::width(&el), model::height(&el));
            let (mut x, y) = position(&a, draft, w, h, || cursor.place(draft, w, h, near))?;
            // An explicit x with centred/right text means that edge, not the left one.
            if a.has("x") && !a.has("center") {
                match model::str_of(&el, "textAlign") {
                    "center" => x -= w / 2.0,
                    "right" => x -= w,
                    _ => {}
                }
            }
            el["x"] = build::round2(x).into();
            el["y"] = build::round2(y).into();
            common(&a, draft, &mut el)?;
            draft.step((x, y + h / 2.0), Some(&id));
            draft.insert(el, Z::Front);
        }
        "arrow" | "line" => draw_linear(draft, &id, kind, &a, &style, label.as_deref())?,
        "frame" => {
            let name = a.text("name").or(label.clone()).unwrap_or_else(|| "Frame".into());
            let contains = a.strings("contains").or_else(|| a.strings("children"));
            let r = match &contains {
                Some(ids) => {
                    let pad = a.num("padding").unwrap_or(40.0);
                    let inner = geo::union_all(ids.iter().filter_map(|i| draft.get(i)).map(model::bounds))
                        .ok_or("“contains” lists no existing elements.")?;
                    inner.inflate(pad)
                }
                None => {
                    let (w, h) = (a.num("width").unwrap_or(800.0), a.num("height").unwrap_or(500.0));
                    let at = position(&a, draft, w, h, || cursor.place(draft, w, h, near))?;
                    Rect::new(at.0, at.1, w, h)
                }
            };
            let mut fstyle = Style { stroke: "#bbb".into(), roughness: 0.0, rounded: false, ..Style::default() };
            fstyle.stroke_width = 2.0;
            let mut el = build::base("frame", &id, r, &fstyle);
            el["name"] = name.into();
            draft.step((r.x, r.y), Some(&id));
            draft.insert(el, Z::Back);
            if let Some(ids) = contains {
                adopt(draft, &id, &ids);
            }
        }
        "freedraw" => {
            let pts = a.get("points").and_then(as_points).filter(|p| p.len() >= 2).ok_or("freedraw needs \"points\": [[x,y], …] (absolute).")?;
            let (ox, oy) = pts[0];
            let rel: Vec<Pt> = pts.iter().map(|(x, y)| (x - ox, y - oy)).collect();
            let b = Rect::from_points(rel.iter().copied()).unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
            let mut el = build::base("freedraw", &id, Rect::new(ox, oy, b.w, b.h), &style);
            el["points"] = json!(rel.iter().map(|(x, y)| [build::round2(*x), build::round2(*y)]).collect::<Vec<_>>());
            el["pressures"] = json!([]);
            el["simulatePressure"] = true.into();
            el["lastCommittedPoint"] = Value::Null;
            el["roundness"] = Value::Null;
            common(&a, draft, &mut el)?;
            draft.step((ox, oy), Some(&id));
            draft.insert(el, Z::Front);
        }
        _ => unreachable!(),
    }
    if let Some(r) = draft.get(&id).map(model::bounds) {
        if !matches!(kind, "arrow" | "line") && !a.has("contains") {
            cursor.last = Some(r);
        }
    }
    out.created += 1;
    out.touch(&id);
    Ok(id)
}

/// Puts elements (and their labels, and arrows between them) into a frame.
pub fn adopt(draft: &mut Draft, frame: &str, ids: &[String]) {
    let mut all: Vec<String> = ids.iter().filter(|i| draft.exists(i) && i.as_str() != frame).cloned().collect();
    let labels: Vec<String> = all.iter().filter_map(|i| draft.get(i).and_then(model::bound_text_id)).collect();
    all.extend(labels);
    let inside: HashSet<String> = all.iter().cloned().collect();
    for arrow in arrows_bound_to(draft, &inside) {
        let both = draft.get(&arrow).is_some_and(|e| {
            [model::binding_target(e, "startBinding"), model::binding_target(e, "endBinding")].iter().all(|t| t.is_some_and(|t| inside.contains(t)))
        });
        if both {
            all.push(arrow.clone());
            if let Some(l) = draft.get(&arrow).and_then(model::bound_text_id) {
                all.push(l);
            }
        }
    }
    for id in all {
        draft.update(&id, |e| e["frameId"] = frame.into());
    }
}

fn draw_linear(draft: &mut Draft, id: &str, kind: &str, a: &Args, style: &Style, label: Option<&str>) -> Result<(), String> {
    let from = end_of(a, &["from", "start", "source"]);
    let to = end_of(a, &["to", "end", "target"]);
    let points = a.get("points").and_then(as_points);
    let route = match a.str("route").or_else(|| a.str("routing")).or_else(|| a.str("style_route")) {
        Some(r) => Route::parse(r).ok_or_else(|| format!("Unknown route “{r}”. Use straight, elbow or curved."))?,
        None if a.bool("elbow") == Some(true) || a.bool("elbowed") == Some(true) => Route::Elbow,
        None if a.bool("curved") == Some(true) => Route::Curved,
        None => Route::Straight,
    };
    let mut el;
    match (from, to) {
        (Some(f), Some(t)) => {
            if let (End::Element(x), End::Element(y)) = (&f, &t) {
                if x == y {
                    return self_loop(draft, id, x, style, a, label);
                }
            }
            let via = points.clone().unwrap_or_default();
            let r = build::route(draft, &f, &t, route, side(a, &["from_side", "start_side", "exit"]), side(a, &["to_side", "end_side", "entry"]), &via, a.num("bow").unwrap_or(0.18))?;
            el = build::linear(kind, id, &r.points, style, r.curved);
            build::apply_route(&mut el, &r);
            if kind == "line" {
                el.as_object_mut().map(|o| o.remove("elbowed"));
                el["startBinding"] = Value::Null;
                el["endBinding"] = Value::Null;
                el["roundness"] = if r.curved { json!({ "type": 2 }) } else { Value::Null };
            }
        }
        (None, None) => {
            let mut pts = points.filter(|p| p.len() >= 2).ok_or(format!("A {kind} needs \"from\" and \"to\" (element ids or [x, y]) or \"points\": [[x,y], …]."))?;
            if a.bool("closed") == Some(true) && pts.first() != pts.last() {
                pts.push(pts[0]);
            }
            el = build::linear(kind, id, &pts, style, route == Route::Curved);
        }
        _ => return Err(format!("A {kind} needs both \"from\" and \"to\".")),
    }
    arrowheads(a, &mut el, kind == "arrow")?;
    common(a, draft, &mut el)?;
    let first = model::abs_points(&el).first().copied().unwrap_or((0.0, 0.0));
    let targets = [model::binding_target(&el, "startBinding"), model::binding_target(&el, "endBinding")].map(|t| t.map(str::to_owned));
    // An arrow between two things in the same frame belongs to that frame.
    if el["frameId"].is_null() {
        let frames: Vec<Option<String>> = targets.iter().flatten().map(|t| draft.get(t).and_then(model::frame_id).map(str::to_owned)).collect();
        if frames.len() == 2 && frames[0].is_some() && frames[0] == frames[1] {
            el["frameId"] = frames[0].clone().into();
        }
    }
    draft.step(first, Some(id));
    draft.insert(el, Z::Front);
    for t in targets.into_iter().flatten() {
        draft.update(&t, |s| build::add_bound(s, id, "arrow"));
    }
    if let Some(l) = label {
        set_label(draft, id, l, style, a);
    }
    Ok(())
}

/// An arrow from a shape back to itself: a loop off its top-right corner.
fn self_loop(draft: &mut Draft, id: &str, target: &str, style: &Style, a: &Args, label: Option<&str>) -> Result<(), String> {
    let r = ref_rect(draft, target)?;
    let s = (r.right() - r.w * 0.25, r.y - 8.0);
    let e = (r.right() + 8.0, r.y + r.h * 0.25);
    let pts = [s, (s.0, r.y - 50.0), (r.right() + 50.0, r.y - 50.0), (r.right() + 50.0, e.1), e];
    let mut el = build::linear("arrow", id, &pts, style, true);
    let outline = geo::Outline::of(draft.get(target).map(model::kind).unwrap_or("rectangle"));
    el["startBinding"] = json!({ "elementId": target, "focus": build::round2(geo::focus(&r, outline, pts[1], s)), "gap": 8 });
    el["endBinding"] = json!({ "elementId": target, "focus": build::round2(geo::focus(&r, outline, pts[3], e)), "gap": 8 });
    arrowheads(a, &mut el, true)?;
    common(a, draft, &mut el)?;
    draft.step(s, Some(id));
    draft.insert(el, Z::Front);
    draft.update(target, |t| build::add_bound(t, id, "arrow"));
    if let Some(l) = label {
        set_label(draft, id, l, style, a);
    }
    Ok(())
}

/// Updates an existing element with the properties given (others are kept).
pub fn update_one(draft: &mut Draft, id: &str, a: &Args, out: &mut Outcome) -> Result<(), String> {
    let el = draft.get(id).cloned().ok_or_else(|| format!("No element “{id}”."))?;
    let kind = model::kind(&el).to_owned();
    if let Some(t) = a.str("type") {
        let want = element_type(t).unwrap_or("");
        let shapes = ["rectangle", "ellipse", "diamond"];
        if want != kind && shapes.contains(&want) && shapes.contains(&kind.as_str()) {
            draft.update(id, |e| e["type"] = want.into());
        } else if want != kind && !want.is_empty() && !(want == "note" && kind == "rectangle") {
            return Err(format!("“{id}” is a {kind}; delete it and draw a new {want} instead of changing its type."));
        }
    }
    let style_keys = ["color", "stroke", "fill", "background", "fill_style", "stroke_width", "stroke_style", "roughness", "opacity", "rounded", "sharp", "font", "font_family", "font_size", "text_color", "text_align", "vertical_align", "stroke_color", "background_color"];
    let styled = style_keys.iter().any(|k| a.has(k));
    let closed = matches!(kind.as_str(), "rectangle" | "ellipse" | "diamond" | "frame");
    let mut style = build::style_of(&el);
    if styled {
        style.apply(a, closed)?;
        if kind != "frame" {
            draft.update(id, |e| build::restyle(e, &style));
        }
        if kind == "text" {
            let st = style.clone();
            draft.update(id, |e| {
                e["fontSize"] = st.font_size.into();
                e["fontFamily"] = st.font_family.into();
                if let Some(al) = &st.text_align {
                    e["textAlign"] = al.clone().into();
                }
                build::remeasure_text(e);
            });
        }
    }
    let r0 = model::rect(&el);
    let mut geometry_changed = false;
    if matches!(kind.as_str(), "rectangle" | "ellipse" | "diamond" | "frame" | "image" | "embeddable") {
        let (w, h) = (a.num("width").unwrap_or(r0.w), a.num("height").unwrap_or(r0.h));
        let positional = ["x", "y", "center", "at", "below", "above", "right_of", "left_of", "cx"].iter().any(|k| a.has(k));
        let (x, y) = if positional { position(a, draft, w, h, || (r0.x, r0.y))? } else { (r0.x, r0.y) };
        if (w, h) != (r0.w, r0.h) {
            draft.update(id, |e| {
                e["width"] = w.into();
                e["height"] = h.into();
            });
            geometry_changed = true;
        }
        if (x, y) != (r0.x, r0.y) {
            move_by(draft, &[id.to_owned()], x - r0.x, y - r0.y);
        }
    } else if kind == "text" {
        if let Some(t) = a.text("text").or_else(|| a.text("label")) {
            draft.update(id, |e| {
                e["originalText"] = t.clone().into();
                e["text"] = t.into();
                build::remeasure_text(e);
            });
        }
        let cur = draft.get(id).map(model::rect).unwrap_or(r0);
        let positional = ["x", "y", "center", "at", "below", "above", "right_of", "left_of"].iter().any(|k| a.has(k));
        if positional && model::container_id(&el).is_none() {
            let (x, y) = position(a, draft, cur.w, cur.h, || (cur.x, cur.y))?;
            move_by(draft, &[id.to_owned()], x - cur.x, y - cur.y);
        }
    } else if model::is_linear(&el) {
        let (f0, t0) = build::arrow_ends(&el);
        let f = end_of(a, &["from", "start", "source"]);
        let t = end_of(a, &["to", "end", "target"]);
        let new_route = a.str("route").and_then(Route::parse);
        if f.is_some() || t.is_some() || new_route.is_some() || a.has("from_side") || a.has("to_side") {
            let (f, t) = (f.unwrap_or(f0.clone()), t.unwrap_or(t0.clone()));
            let r = build::route(draft, &f, &t, new_route.unwrap_or(build::route_of(&el)), side(a, &["from_side", "start_side"]), side(a, &["to_side", "end_side"]), &[], a.num("bow").unwrap_or(0.18))?;
            for old in [&f0, &t0] {
                if let End::Element(o) = old {
                    draft.update(o, |s| build::remove_bound(s, id));
                }
            }
            draft.update(id, |e| build::apply_route(e, &r));
            for new in [&f, &t] {
                if let End::Element(n) = new {
                    draft.update(n, |s| build::add_bound(s, id, "arrow"));
                }
            }
            geometry_changed = true;
        } else if let Some(pts) = a.get("points").and_then(as_points).filter(|p| p.len() >= 2) {
            draft.update(id, |e| build::set_points(e, &pts));
            geometry_changed = true;
        }
        let mut e2 = draft.get(id).cloned().unwrap_or(el.clone());
        arrowheads(a, &mut e2, kind == "arrow")?;
        draft.update(id, |e| {
            e["startArrowhead"] = e2["startArrowhead"].clone();
            e["endArrowhead"] = e2["endArrowhead"].clone();
        });
    }
    if kind == "frame" {
        if let Some(n) = a.text("name").or_else(|| a.text("label")) {
            draft.update(id, |e| e["name"] = n.into());
        }
        if let Some(ids) = a.strings("contains") {
            adopt(draft, id, &ids);
        }
    } else if kind != "text" {
        if let Some(l) = a.text("label").or_else(|| a.text("text")) {
            set_label(draft, id, &l, &style, a);
        } else if styled || geometry_changed {
            // Label styling and placement follow the container.
            if let Some(tid) = model::bound_text_id(&el).filter(|t| draft.exists(t)) {
                if styled {
                    let st = style.clone();
                    let mut ta = a.0.clone();
                    ta.remove("label");
                    let ta = Value::Object(ta);
                    let la = Args::of(&ta);
                    draft.update(&tid, |t| {
                        let mut ts = build::style_of(t);
                        let _ = ts.apply(&la, false);
                        t["strokeColor"] = st.text_color.clone().or_else(|| la.has("color").then(|| st.stroke.clone())).unwrap_or(ts.stroke.clone()).into();
                        t["fontSize"] = ts.font_size.into();
                        t["fontFamily"] = ts.font_family.into();
                        t["lineHeight"] = ts.font().line_height.into();
                        if let Some(al) = &ts.text_align {
                            t["textAlign"] = al.clone().into();
                        }
                        if let Some(v) = &ts.vertical_align {
                            t["verticalAlign"] = v.clone().into();
                        }
                    });
                }
                refit_label(draft, id);
            }
        }
    }
    let mut e = draft.get(id).cloned().unwrap_or(el);
    common(a, draft, &mut e)?;
    let fields = ["groupIds", "frameId", "link", "locked", "angle"];
    draft.update(id, |d| {
        for f in fields {
            if e.get(f) != d.get(f) {
                d[f] = e[f].clone();
            }
        }
    });
    if let Some(z) = a.str("z") {
        draft.restack(id, if z.eq_ignore_ascii_case("back") { Z::Back } else { Z::Front });
    }
    if geometry_changed && !model::is_linear(&e) {
        for arrow in arrows_bound_to(draft, &HashSet::from([id.to_owned()])) {
            build::reroute(draft, &arrow);
        }
    }
    if let Some(r) = draft.get(id).map(model::bounds) {
        draft.step(r.center(), Some(id));
    }
    out.updated += 1;
    out.touch(id);
    Ok(())
}

//! What the agent reads about a canvas: a compact text description of the
//! scene, layout checks ("lint") for what it just drew, and a finder for
//! free space to draw in.

use super::colors::{self, Slot};
use super::geometry::{dist, segment_hits_rect, segment_intersection, smooth, union_all, Pt, Rect};
use super::model::{self, bounds, container_id, frame_id, group_ids, id, is_frame, is_linear, kind, num, opt_num, opt_str, str_of};
use super::text::BOUND_TEXT_PADDING;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;

/// Someone on the canvas, as the description shows them.
pub struct PeerView {
    pub name: String,
    pub pointer: Option<Pt>,
    pub selected: Vec<String>,
    pub is_you: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Detail {
    Summary,
    Full,
}

pub struct DescribeOptions {
    /// Only elements touching this box.
    pub region: Option<Rect>,
    /// Only these elements (plus their labels and connected arrows).
    pub ids: Option<Vec<String>>,
    pub detail: Detail,
    /// Cap on listed elements; beyond it the scene is summarised by area.
    pub max_elements: usize,
}

impl Default for DescribeOptions {
    fn default() -> Self {
        Self { region: None, ids: None, detail: Detail::Summary, max_elements: 250 }
    }
}

/// Size of the area the free-space hint looks for.
const HINT_SIZE: (f64, f64) = (600.0, 400.0);
const HINT_MARGIN: f64 = 40.0;
const MAX_ISSUES: usize = 15;

// ---------------------------------------------------------------- lookups

/// Element lookups shared by the description and the checks.
struct Ctx<'m, 'a> {
    by_id: &'m HashMap<&'a str, &'a Value>,
    /// Container id → its bound text, from the texts' `containerId`.
    labels: HashMap<&'a str, &'a Value>,
}

impl<'m, 'a> Ctx<'m, 'a> {
    fn new(by_id: &'m HashMap<&'a str, &'a Value>, elements: &[&'a Value]) -> Self {
        let mut labels = HashMap::new();
        for &e in elements {
            if kind(e) == "text" {
                if let Some(c) = container_id(e).filter(|c| by_id.contains_key(c)) {
                    labels.insert(c, e);
                }
            }
        }
        Self { by_id, labels }
    }

    /// The label bound to a container.
    fn label(&self, el: &Value) -> Option<&'a Value> {
        if let Some(t) = self.labels.get(id(el)) {
            return Some(t);
        }
        let tid = model::bound_text_id(el)?;
        self.by_id.get(tid.as_str()).copied().filter(|t| kind(t) == "text")
    }

    /// Text bound to an existing container (shown as that container's label).
    fn is_label(&self, el: &Value) -> bool {
        kind(el) == "text" && container_id(el).is_some_and(|c| self.by_id.contains_key(c))
    }

    fn get(&self, id: &str) -> Option<&'a Value> {
        self.by_id.get(id).copied()
    }
}

// ------------------------------------------------------------- formatting

fn n(v: f64) -> i64 {
    v.round() as i64
}

/// A number as an integer when it is one, else with one decimal.
fn num_str(v: f64) -> String {
    if (v - v.round()).abs() < 0.05 {
        n(v).to_string()
    } else {
        format!("{v:.1}")
    }
}

fn fmt_pt(p: Pt) -> String {
    format!("({},{})", n(p.0), n(p.1))
}

fn fmt_box(r: &Rect) -> String {
    format!("({},{} {}×{})", n(r.x), n(r.y), n(r.w), n(r.h))
}

/// Text in quotes on one line: newlines as ⏎, cut at `max` characters.
fn quote(s: &str, max: usize) -> String {
    let flat = s.trim().replace("\r\n", "\n").replace(['\n', '\r'], "⏎");
    let mut out: String = flat.chars().take(max).collect();
    if flat.chars().count() > max {
        out = out.trim_end().to_owned();
        out.push('…');
    }
    format!("\"{}\"", out.replace('"', "\\\""))
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}

fn kind_word(t: &str) -> &str {
    match t {
        "freedraw" => "freehand stroke",
        "magicframe" => "frame",
        "embeddable" | "iframe" => "embed",
        "" => "element",
        other => other,
    }
}

fn family_name(f: f64) -> String {
    match f as i64 {
        5 => "hand".into(),
        6 => "sans".into(),
        7 => "heading".into(),
        8 => "code".into(),
        1 => "Virgil".into(),
        2 => "Helvetica".into(),
        3 => "Cascadia".into(),
        other => format!("font {other}"),
    }
}

fn is_white(c: &str) -> bool {
    matches!(c.trim().to_ascii_lowercase().as_str(), "" | "#ffffff" | "#fff" | "white" | "transparent")
}

fn is_black_stroke(c: &str) -> bool {
    c.is_empty() || colors::name(c, Slot::Stroke) == "black"
}

fn has_fill(el: &Value) -> bool {
    let c = str_of(el, "backgroundColor");
    !c.is_empty() && colors::rgba(c).is_none_or(|v| v[3] > 0)
}

fn is_elbow(el: &Value) -> bool {
    el.get("elbowed").and_then(Value::as_bool).unwrap_or(false)
}

fn is_rounded(el: &Value) -> bool {
    el.get("roundness").is_some_and(|r| !r.is_null())
}

fn frame_name(f: &Value) -> &str {
    opt_str(f, "name").unwrap_or("Frame")
}

/// How lint messages name an element: `"API Gateway" (api)`, or
/// `rectangle r1` when it has no text.
fn who(el: &Value, ctx: &Ctx) -> String {
    let i = id(el);
    let text = match kind(el) {
        "text" => Some(model::original_text(el)),
        "frame" | "magicframe" => return format!("frame {} ({i})", quote(frame_name(el), 30)),
        _ => ctx.label(el).map(model::original_text),
    };
    match text.filter(|t| !t.trim().is_empty()) {
        Some(t) => format!("{} ({i})", quote(t, 30)),
        None => format!("{} {i}", kind_word(kind(el))),
    }
}

/// Where an arrow end is: the bound element's id, or a point.
fn end_name(el: &Value, key: &str, p: Option<&Pt>, ctx: &Ctx) -> String {
    match model::binding_target(el, key) {
        Some(t) if ctx.by_id.contains_key(t) => t.to_owned(),
        Some(t) => format!("{t} (missing)"),
        None => p.map(|p| fmt_pt(*p)).unwrap_or_else(|| "?".into()),
    }
}

/// One-line summary of an element, as used in describe() lists and in tool results, e.g.
/// `api  rectangle (100,80 200×80) "API Gateway" · stroke blue, fill blue · in frame backend`
pub fn element_line(el: &Value, by_id: &HashMap<&str, &Value>) -> String {
    let ctx = Ctx { by_id, labels: HashMap::new() };
    summary(el, &ctx, true)
}

fn summary(el: &Value, ctx: &Ctx, show_frame: bool) -> String {
    let t = kind(el);
    let mut s = format!("{}  {}", id(el), if t.is_empty() { "element" } else { t });
    let mut attrs: Vec<String> = Vec::new();
    match t {
        "arrow" | "line" => {
            let pts = model::abs_points(el);
            let count = pts.len();
            if t == "arrow" {
                let _ = write!(s, " {} → {}", end_name(el, "startBinding", pts.first(), ctx), end_name(el, "endBinding", pts.last(), ctx));
            } else {
                let _ = write!(s, " {}", fmt_box(&bounds(el)));
            }
            if let Some(l) = ctx.label(el) {
                let _ = write!(s, " {}", quote(model::original_text(l), 60));
            }
            let shape = if is_elbow(el) {
                Some("elbow")
            } else if count <= 2 {
                (t == "arrow").then_some("straight")
            } else if is_rounded(el) {
                Some("curved")
            } else {
                Some(if t == "arrow" { "polyline" } else { "straight segments" })
            };
            let closed = el.get("polygon").and_then(Value::as_bool).unwrap_or(false);
            let mut style: Vec<String> = shape.into_iter().map(str::to_owned).collect();
            if closed {
                style.push("closed".into());
            }
            if count > 2 || t == "line" {
                style.push(format!("{count} pts"));
            }
            if !style.is_empty() {
                let _ = write!(s, " ({})", style.join(", "));
            }
            let start = opt_str(el, "startArrowhead");
            let end = opt_str(el, "endArrowhead");
            if let Some(h) = start {
                attrs.push(format!("start head {h}"));
            }
            match (t, end) {
                ("arrow", None) => attrs.push("no end head".into()),
                ("arrow", Some("arrow")) => {}
                (_, Some(h)) => attrs.push(format!("end head {h}")),
                _ => {}
            }
        }
        "text" => {
            let _ = write!(s, " {} {}", fmt_box(&model::rect(el)), quote(model::original_text(el), 60));
            attrs.push(format!("{}px {}", num_str(num(el, "fontSize")), family_name(num(el, "fontFamily"))));
            match str_of(el, "textAlign") {
                "center" if container_id(el).is_none() => attrs.push("centered".into()),
                "right" => attrs.push("right-aligned".into()),
                _ => {}
            }
            if let Some(c) = container_id(el) {
                attrs.push(format!("label of {c}"));
            }
        }
        "freedraw" => {
            let _ = write!(s, " {} {} pts", fmt_box(&bounds(el)), model::rel_points(el).len());
        }
        "frame" | "magicframe" => {
            let _ = write!(s, " {} {}", fmt_box(&model::rect(el)), quote(frame_name(el), 60));
        }
        _ => {
            let _ = write!(s, " {}", fmt_box(&model::rect(el)));
            if let Some(l) = ctx.label(el) {
                let _ = write!(s, " {}", quote(model::original_text(l), 60));
            }
        }
    }

    let stroke = str_of(el, "strokeColor");
    let fill = str_of(el, "backgroundColor");
    match t {
        "text" => {
            if !is_black_stroke(stroke) {
                attrs.push(format!("color {}", colors::name(stroke, Slot::Stroke)));
            }
        }
        "image" | "frame" | "magicframe" | "embeddable" | "iframe" => {}
        _ => {
            let mut c = Vec::new();
            if !is_black_stroke(stroke) {
                c.push(format!("stroke {}", colors::name(stroke, Slot::Stroke)));
            }
            if has_fill(el) && !fill.eq_ignore_ascii_case("transparent") {
                c.push(format!("fill {}", colors::name(fill, Slot::Fill)));
                if let Some(fs) = opt_str(el, "fillStyle").filter(|f| *f != "solid") {
                    c.push(fs.to_owned());
                }
            }
            if !c.is_empty() {
                attrs.push(c.join(", "));
            }
        }
    }
    if !matches!(t, "text" | "image" | "frame" | "magicframe" | "embeddable" | "iframe") {
        if let Some(st) = opt_str(el, "strokeStyle").filter(|s| *s != "solid") {
            attrs.push(st.to_owned());
        }
        if let Some(w) = opt_num(el, "strokeWidth").filter(|w| (*w - 2.0).abs() > 1e-6) {
            attrs.push(format!("stroke width {}", num_str(w)));
        }
    }
    if matches!(t, "rectangle" | "diamond") && !is_rounded(el) {
        attrs.push("sharp corners".into());
    }
    if let Some(o) = opt_num(el, "opacity").filter(|o| *o < 100.0) {
        attrs.push(format!("opacity {}%", n(o)));
    }
    let deg = n(model::angle(el).to_degrees().rem_euclid(360.0)) % 360;
    if deg != 0 {
        attrs.push(format!("rotated {deg}°"));
    }
    let groups = group_ids(el);
    if !groups.is_empty() {
        attrs.push(format!("{} {}", if groups.len() == 1 { "group" } else { "groups" }, groups.join(", ")));
    }
    if el.get("locked").and_then(Value::as_bool).unwrap_or(false) {
        attrs.push("locked".into());
    }
    if let Some(link) = opt_str(el, "link") {
        let short: String = link.chars().take(60).collect();
        attrs.push(format!("link {short}{}", if link.chars().count() > 60 { "…" } else { "" }));
    }
    if show_frame {
        if let Some(f) = frame_id(el) {
            attrs.push(format!("in frame {f}"));
        }
    }
    for a in attrs {
        s.push_str(" · ");
        s.push_str(&a);
    }
    s
}

// ------------------------------------------------------------ full detail

const NOISE: &[&str] = &[
    "id", "type", "seed", "version", "versionNonce", "updated", "index", "isDeleted", "boundElements", "lastCommittedPoint",
    "pressures", "simulatePressure", "baseline", "startIsSpecial", "endIsSpecial",
];
const TEXT_NOISE: &[&str] = &["strokeWidth", "strokeStyle", "roughness", "fillStyle", "roundness", "backgroundColor"];
const LABEL_NOISE: &[&str] = &["containerId", "x", "y", "width", "height", "angle", "groupIds", "frameId", "locked", "link"];

fn is_default_prop(el: &Value, t: &str, k: &str, v: &Value) -> bool {
    if v.is_null() {
        return !(k == "roundness" && matches!(t, "rectangle" | "diamond")) && !(k == "endArrowhead" && t == "arrow");
    }
    let f = v.as_f64();
    let s = v.as_str();
    let bound = container_id(el).is_some();
    match k {
        "angle" => f == Some(0.0),
        "opacity" => f == Some(100.0),
        "strokeColor" => s.is_some_and(is_black_stroke),
        "backgroundColor" => s == Some("transparent"),
        "fillStyle" | "strokeStyle" => s == Some("solid"),
        "strokeWidth" => f == Some(2.0),
        "roughness" => f == Some(1.0),
        "locked" | "elbowed" | "polygon" => v.as_bool() == Some(false),
        "autoResize" => v.as_bool() == Some(true),
        "groupIds" | "fixedSegments" => v.as_array().is_some_and(|a| a.is_empty()),
        "endArrowhead" => t == "arrow" && s == Some("arrow"),
        "roundness" => {
            let ty = v.get("type").and_then(Value::as_i64);
            let plain = v.get("value").is_none_or(Value::is_null);
            plain
                && match t {
                    "rectangle" | "diamond" => ty == Some(3),
                    "arrow" | "line" => ty == Some(2),
                    "image" | "embeddable" | "iframe" => false,
                    _ => true, // no effect on other types
                }
        }
        "textAlign" => s == Some(if bound { "center" } else { "left" }),
        "verticalAlign" => s == Some(if bound { "middle" } else { "top" }),
        "lineHeight" => f == Some(1.25),
        "originalText" => s == el.get("text").and_then(Value::as_str),
        "status" => s == Some("saved"),
        "scale" => v.as_array().is_some_and(|a| a.iter().all(|x| x.as_f64() == Some(1.0))),
        _ => false,
    }
}

fn round_json(v: &Value) -> Value {
    match v {
        Value::Number(num) if num.is_f64() => {
            let r = (num.as_f64().unwrap_or(0.0) * 10.0).round() / 10.0;
            if r.fract() == 0.0 && r.abs() < 9e15 {
                Value::from(r as i64)
            } else {
                Value::from(r)
            }
        }
        Value::Array(a) => Value::Array(a.iter().map(round_json).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), round_json(v))).collect()),
        _ => v.clone(),
    }
}

/// The element's properties as one JSON line, defaults and bookkeeping left out.
fn compact_json(el: &Value, as_label: bool) -> String {
    let Some(obj) = el.as_object() else { return el.to_string() };
    let t = kind(el);
    let mut out = Map::new();
    for (k, v) in obj {
        let k = k.as_str();
        if NOISE.contains(&k)
            || (t == "text" && TEXT_NOISE.contains(&k))
            || (as_label && LABEL_NOISE.contains(&k))
            || is_default_prop(el, t, k, v)
        {
            continue;
        }
        let v = match v.as_array() {
            Some(a) if k == "points" && t == "freedraw" && a.len() > 24 => Value::from(format!("{} points", a.len())),
            _ => round_json(v),
        };
        out.insert(k.to_owned(), v);
    }
    Value::Object(out).to_string()
}

// ---------------------------------------------------------------- describe

/// `elements`: all NON-deleted elements in z-order. `peers`: everyone on the
/// canvas, the agent included (`is_you`).
pub fn describe(canvas_name: &str, elements: &[&Value], background: Option<&str>, peers: &[PeerView], opts: &DescribeOptions) -> String {
    let by_id: HashMap<&str, &Value> = elements.iter().map(|&e| (id(e), e)).collect();
    let ctx = Ctx::new(&by_id, elements);
    let items: Vec<&Value> = elements.iter().copied().filter(|e| !ctx.is_label(e)).collect();
    let mut out = String::new();

    let _ = write!(out, "Canvas {}", quote(canvas_name, 80));
    match union_all(items.iter().map(|e| bounds(e))) {
        Some(b) => {
            let _ = write!(
                out,
                " — {}, content x {}…{}, y {}…{}",
                plural(items.len(), "element"),
                n(b.x),
                n(b.right()),
                n(b.y),
                n(b.bottom())
            );
        }
        None => out.push_str(" — empty"),
    }
    if let Some(bg) = background.filter(|b| !is_white(b)) {
        let _ = write!(out, ", background {}", colors::name(bg, Slot::Fill));
    }
    out.push('\n');

    // People and what they have selected (labels resolve to their shapes).
    let mut selected_by: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut selected_order: Vec<&str> = Vec::new();
    if !peers.is_empty() {
        out.push_str("People here:\n");
        for p in peers {
            let mut sel: Vec<&str> = Vec::new();
            for s in &p.selected {
                let Some(e) = ctx.get(s) else { continue };
                let r = if ctx.is_label(e) { container_id(e).unwrap_or(id(e)) } else { id(e) };
                if !sel.contains(&r) {
                    sel.push(r);
                }
            }
            let mut parts = Vec::new();
            if let Some(pt) = p.pointer {
                parts.push(format!("pointer at {}", fmt_pt(pt)));
            }
            if !sel.is_empty() {
                let mut t = format!("selected: {}", sel.iter().take(30).copied().collect::<Vec<_>>().join(", "));
                if sel.len() > 30 {
                    let _ = write!(t, " (+{} more)", sel.len() - 30);
                }
                parts.push(t);
            } else if !p.is_you {
                parts.push("nothing selected".into());
            }
            let _ = write!(out, "  {}{}", p.name, if p.is_you { " (you)" } else { "" });
            if !parts.is_empty() {
                let _ = write!(out, " — {}", parts.join(", "));
            }
            out.push('\n');
            if !p.is_you {
                for s in sel {
                    let names = selected_by.entry(s).or_default();
                    if names.is_empty() {
                        selected_order.push(s);
                    }
                    names.push(&p.name);
                }
            }
        }
    }

    // Filters.
    let total = items.len();
    let mut shown = items.clone();
    let mut filters = Vec::new();
    if let Some(ids) = &opts.ids {
        let (keep, missing) = expand_ids(ids, &items, &ctx);
        shown.retain(|e| keep.contains(id(e)));
        filters.push(format!("ids {}, with labels and connected arrows", ids.join(", ")));
        if !missing.is_empty() {
            let _ = writeln!(out, "Not found: {}", missing.join(", "));
        }
    }
    if let Some(r) = opts.region {
        shown.retain(|e| touches(&bounds(e), &r));
        filters.push(format!("region {}", fmt_box(&r)));
    }
    if !filters.is_empty() {
        let _ = writeln!(out, "Showing {} of {} ({}).", shown.len(), plural(total, "element"), filters.join("; "));
    }

    let marks = Marks { selected_by: &selected_by, detail: opts.detail };
    if shown.len() > opts.max_elements {
        summarize(&mut out, &shown, &ctx, &marks, &selected_order, opts.max_elements);
    } else if !shown.is_empty() {
        list_elements(&mut out, &shown, &ctx, &marks);
    }

    let ((x, y), place) = find_space(elements, HINT_SIZE.0, HINT_SIZE.1, HINT_MARGIN, None);
    let _ = match place {
        Place::Empty => writeln!(out, "Free space: the canvas is empty, start at ({},{}).", n(x), n(y)),
        _ => writeln!(out, "Free space for new content: {} at ({},{}).", place.words(), n(x), n(y)),
    };
    out
}

/// Bounds that touch, edges included (so zero-height lines count).
fn touches(a: &Rect, b: &Rect) -> bool {
    let (w, h) = a.overlap(b);
    w >= 0.0 && h >= 0.0
}

/// Ids asked for, plus frame children, group members and connected arrows.
fn expand_ids<'a>(ids: &[String], items: &[&'a Value], ctx: &Ctx<'_, 'a>) -> (HashSet<&'a str>, Vec<String>) {
    let mut keep: HashSet<&'a str> = HashSet::new();
    let mut missing = Vec::new();
    for want in ids {
        if let Some(e) = ctx.get(want) {
            keep.insert(if ctx.is_label(e) { container_id(e).unwrap_or(id(e)) } else { id(e) });
            continue;
        }
        let members: Vec<&'a str> = items.iter().filter(|e| group_ids(e).iter().any(|g| g == want)).map(|&e| id(e)).collect();
        if members.is_empty() {
            missing.push(want.clone());
        }
        keep.extend(members);
    }
    let base = keep.clone();
    for &e in items {
        let in_frame = frame_id(e).is_some_and(|f| base.contains(f));
        let connected = kind(e) == "arrow"
            && ["startBinding", "endBinding"].iter().any(|k| model::binding_target(e, k).is_some_and(|t| base.contains(t)));
        if in_frame || connected {
            keep.insert(id(e));
        }
    }
    (keep, missing)
}

/// Shapes first, then arrows and lines; each in reading order (rows by top
/// edge, then left to right).
fn reading_order<'a>(els: &[&'a Value]) -> Vec<&'a Value> {
    let (nodes, links): (Vec<&Value>, Vec<&Value>) = els.iter().partition(|e| !is_linear(e));
    let mut out = rows(&nodes);
    out.extend(rows(&links));
    out
}

fn rows<'a>(els: &[&'a Value]) -> Vec<&'a Value> {
    let mut keyed: Vec<(Rect, &Value)> = els.iter().map(|&e| (bounds(e), e)).collect();
    keyed.sort_by(|a, b| a.0.y.total_cmp(&b.0.y).then(a.0.x.total_cmp(&b.0.x)));
    let mut rows: Vec<Vec<(Rect, &Value)>> = Vec::new();
    let (mut top, mut tol) = (f64::NEG_INFINITY, 0.0);
    for (r, e) in keyed {
        if rows.is_empty() || r.y > top + tol {
            rows.push(Vec::new());
            top = r.y;
            tol = (r.h * 0.5).clamp(20.0, 100.0);
        }
        rows.last_mut().unwrap().push((r, e));
    }
    rows.into_iter()
        .flat_map(|mut row| {
            row.sort_by(|a, b| a.0.x.total_cmp(&b.0.x));
            row.into_iter().map(|(_, e)| e)
        })
        .collect()
}

/// For each shape that encloses others, the ids it most directly encloses.
fn containment<'a>(order: &[&'a Value]) -> HashMap<&'a str, Vec<&'a str>> {
    let rects: Vec<Rect> = order.iter().map(|e| bounds(e)).collect();
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for (i, &c) in order.iter().enumerate() {
        if is_linear(c) || is_frame(c) {
            continue;
        }
        let area = rects[i].w * rects[i].h;
        let mut best: Option<(f64, usize)> = None;
        for (j, &z) in order.iter().enumerate() {
            let za = rects[j].w * rects[j].h;
            if j == i || !matches!(kind(z), "rectangle" | "ellipse" | "diamond") || za <= area || !rects[j].contains_rect(&rects[i], 1.0) {
                continue;
            }
            if best.is_none_or(|(a, _)| za < a) {
                best = Some((za, j));
            }
        }
        if let Some((_, j)) = best {
            out.entry(id(order[j])).or_default().push(id(c));
        }
    }
    out
}

struct Marks<'s> {
    selected_by: &'s HashMap<&'s str, Vec<&'s str>>,
    detail: Detail,
}

fn write_element(out: &mut String, el: &Value, ctx: &Ctx, marks: &Marks, contains: Option<&Vec<&str>>) {
    let mut line = summary(el, ctx, false);
    if let Some(kids) = contains {
        let _ = write!(line, " · contains {}", kids.iter().take(8).copied().collect::<Vec<_>>().join(", "));
        if kids.len() > 8 {
            let _ = write!(line, " +{} more", kids.len() - 8);
        }
    }
    if let Some(names) = marks.selected_by.get(id(el)) {
        let _ = write!(line, " · selected by {}", names.join(", "));
    }
    let _ = writeln!(out, "  {line}");
    if marks.detail == Detail::Full {
        let _ = writeln!(out, "    {}", compact_json(el, false));
        if let Some(l) = ctx.label(el) {
            let _ = writeln!(out, "    label {}", compact_json(l, true));
        }
    }
}

fn frame_of<'a>(el: &Value, ctx: &Ctx<'_, 'a>) -> Option<&'a Value> {
    frame_id(el).and_then(|f| ctx.get(f)).filter(|f| is_frame(f))
}

fn list_elements(out: &mut String, shown: &[&Value], ctx: &Ctx, marks: &Marks) {
    let mut frames: Vec<&Value> = Vec::new();
    let mut children: HashMap<&str, Vec<&Value>> = HashMap::new();
    let mut top: Vec<&Value> = Vec::new();
    for &e in shown {
        if is_frame(e) {
            if !frames.iter().any(|f| id(f) == id(e)) {
                frames.push(e);
            }
            continue;
        }
        match frame_of(e, ctx) {
            Some(f) => {
                if !frames.iter().any(|g| id(g) == id(f)) {
                    frames.push(f);
                }
                children.entry(id(f)).or_default().push(e);
            }
            None => top.push(e),
        }
    }
    let frames = rows(&frames);
    let groups: Vec<(Option<&Value>, Vec<&Value>)> = frames
        .iter()
        .map(|&f| (Some(f), reading_order(children.get(id(f)).map(Vec::as_slice).unwrap_or(&[]))))
        .chain(std::iter::once((None, reading_order(&top))))
        .collect();
    let all: Vec<&Value> = groups.iter().flat_map(|(_, els)| els.iter().copied()).collect();
    let contains = containment(&all);

    for (frame, els) in &groups {
        match frame {
            Some(f) => {
                let _ = write!(out, "Frame {} {} {}", id(f), quote(frame_name(f), 60), fmt_box(&model::rect(f)));
                if let Some(names) = marks.selected_by.get(id(f)) {
                    let _ = write!(out, " · selected by {}", names.join(", "));
                }
                if els.is_empty() {
                    out.push_str(": empty\n");
                } else {
                    let _ = writeln!(out, ", {}:", plural(els.len(), "element"));
                }
                if marks.detail == Detail::Full {
                    let _ = writeln!(out, "    {}", compact_json(f, false));
                }
            }
            None if els.is_empty() => continue,
            None => out.push_str(if frames.is_empty() { "Elements:\n" } else { "Outside frames:\n" }),
        }
        for &e in els {
            write_element(out, e, ctx, marks, contains.get(id(e)));
        }
    }
}

/// Counts by type, most common first: `12 rectangles, 10 arrows`.
fn type_counts(els: &[&Value]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for &e in els {
        *counts.entry(kind_word(kind(e))).or_default() += 1;
    }
    let mut v: Vec<(&str, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    v.into_iter().map(|(k, c)| plural(c, k)).collect::<Vec<_>>().join(", ")
}

/// A few of the biggest texts in a set of elements, as a hint of what's there.
fn samples(els: &[&Value], ctx: &Ctx) -> String {
    let mut texts: Vec<(f64, &str)> = els
        .iter()
        .filter_map(|&e| {
            let t = if kind(e) == "text" { Some(e) } else { ctx.label(e) }?;
            let s = model::original_text(t).trim();
            (!s.is_empty()).then(|| (num(t, "fontSize"), s))
        })
        .collect();
    texts.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut picked: Vec<String> = Vec::new();
    for (_, s) in texts {
        let q = quote(s, 25);
        if !picked.contains(&q) {
            picked.push(q);
        }
        if picked.len() == 4 {
            break;
        }
    }
    if picked.is_empty() {
        String::new()
    } else {
        format!(" · e.g. {}", picked.join(", "))
    }
}

fn summarize(out: &mut String, shown: &[&Value], ctx: &Ctx, marks: &Marks, selected_order: &[&str], max: usize) {
    let _ = writeln!(out, "Too many elements to list them all ({} > {max}).", shown.len());
    let shown_ids: HashSet<&str> = shown.iter().map(|&e| id(e)).collect();
    let selected: Vec<&Value> =
        selected_order.iter().filter(|s| shown_ids.contains(*s)).filter_map(|s| ctx.get(s)).take(max).collect();
    if !selected.is_empty() {
        out.push_str("Selected:\n");
        for &e in &selected {
            write_element(out, e, ctx, marks, None);
        }
    }
    out.push_str("Summary by area (call describe again with `region` set to one of these boxes, or with `ids`, to list the elements):\n");

    let mut frames: Vec<&Value> = Vec::new();
    let mut children: HashMap<&str, Vec<&Value>> = HashMap::new();
    let mut top: Vec<&Value> = Vec::new();
    for &e in shown {
        if is_frame(e) {
            if !frames.iter().any(|f| id(f) == id(e)) {
                frames.push(e);
            }
        } else if let Some(f) = frame_of(e, ctx) {
            if !frames.iter().any(|g| id(g) == id(f)) {
                frames.push(f);
            }
            children.entry(id(f)).or_default().push(e);
        } else {
            top.push(e);
        }
    }
    for &f in &rows(&frames) {
        let kids = children.get(id(f)).map(Vec::as_slice).unwrap_or(&[]);
        let _ = write!(out, "  frame {} {} {}: {}", id(f), quote(frame_name(f), 40), fmt_box(&model::rect(f)), plural(kids.len(), "element"));
        if !kids.is_empty() {
            let _ = write!(out, " — {}{}", type_counts(kids), samples(kids, ctx));
        }
        out.push('\n');
    }

    let Some(b) = union_all(top.iter().map(|e| bounds(e))) else { return };
    let size = ((b.w.max(b.h) / 4.0 / 100.0).ceil() * 100.0).max(400.0);
    let (x0, y0) = ((b.x / 100.0).floor() * 100.0, (b.y / 100.0).floor() * 100.0);
    let mut tiles: BTreeMap<(i64, i64), Vec<&Value>> = BTreeMap::new();
    for &e in &top {
        let (cx, cy) = bounds(e).center();
        tiles.entry((((cy - y0) / size).floor() as i64, ((cx - x0) / size).floor() as i64)).or_default().push(e);
    }
    for ((row, col), els) in tiles {
        let area = Rect::new(x0 + col as f64 * size, y0 + row as f64 * size, size, size);
        let _ = writeln!(
            out,
            "  area {}: {} — {}{}",
            fmt_box(&area),
            plural(els.len(), "element"),
            type_counts(&els),
            samples(&els, ctx)
        );
    }
}

// --------------------------------------------------------------- free space

/// Buckets rects into square cells so neighbours can be found quickly.
struct Grid {
    cell: f64,
    cells: HashMap<(i64, i64), Vec<usize>>,
    /// Rects spanning too many cells; always candidates.
    big: Vec<usize>,
    rects: Vec<Rect>,
}

const GRID_MAX_CELLS: f64 = 400.0;

impl Grid {
    fn new(rects: Vec<Rect>, cell: f64) -> Self {
        let mut g = Grid { cell, cells: HashMap::new(), big: Vec::new(), rects };
        for i in 0..g.rects.len() {
            let (x0, y0, x1, y1) = g.span(&g.rects[i]);
            if (x1 as f64 - x0 as f64 + 1.0) * (y1 as f64 - y0 as f64 + 1.0) > GRID_MAX_CELLS {
                g.big.push(i);
                continue;
            }
            for cx in x0..=x1 {
                for cy in y0..=y1 {
                    g.cells.entry((cx, cy)).or_default().push(i);
                }
            }
        }
        g
    }

    fn span(&self, r: &Rect) -> (i64, i64, i64, i64) {
        let f = |v: f64| (v / self.cell).floor() as i64;
        (f(r.x), f(r.y), f(r.right()), f(r.bottom()))
    }

    /// Indices of rects that may touch `r`.
    fn near(&self, r: &Rect) -> Vec<usize> {
        let (x0, y0, x1, y1) = self.span(r);
        if (x1 as f64 - x0 as f64 + 1.0) * (y1 as f64 - y0 as f64 + 1.0) > 4.0 * GRID_MAX_CELLS {
            return (0..self.rects.len()).collect();
        }
        let mut out = self.big.clone();
        for cx in x0..=x1 {
            for cy in y0..=y1 {
                if let Some(v) = self.cells.get(&(cx, cy)) {
                    out.extend_from_slice(v);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn any_intersects(&self, r: &Rect) -> bool {
        self.near(r).into_iter().any(|i| self.rects[i].intersects(r))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Place {
    Empty,
    Near,
    Right,
    Below,
}

impl Place {
    fn words(self) -> &'static str {
        match self {
            Place::Empty => "empty canvas",
            Place::Near => "near the requested point",
            Place::Right => "right of everything",
            Place::Below => "below everything",
        }
    }
}

fn round10(v: f64) -> f64 {
    (v / 10.0).round() * 10.0
}

fn ceil10(v: f64) -> f64 {
    (v / 10.0).ceil() * 10.0
}

fn floor10(v: f64) -> f64 {
    (v / 10.0).floor() * 10.0
}

/// Top-left of a free w×h area with `margin` clearance from everything. With `near`, the free spot
/// whose top-left is closest to that point; otherwise right of the existing content (top-aligned),
/// then below it. Empty canvas → `near` or (0,0). Coordinates are multiples of 10.
pub fn free_spot(elements: &[&Value], w: f64, h: f64, margin: f64, near: Option<Pt>) -> Pt {
    find_space(elements, w, h, margin, near).0
}

fn find_space(elements: &[&Value], w: f64, h: f64, margin: f64, near: Option<Pt>) -> (Pt, Place) {
    let rects: Vec<Rect> = elements.iter().map(|e| bounds(e)).collect();
    let Some(content) = union_all(rects.iter().copied()) else {
        return (near.map(|p| (round10(p.0), round10(p.1))).unwrap_or((0.0, 0.0)), Place::Empty);
    };
    let margin = margin.max(0.0);
    let grid = Grid::new(rects.iter().map(|r| r.inflate(margin)).collect(), 200.0);
    let free = |p: Pt| !grid.any_intersects(&Rect::new(p.0, p.1, w, h));
    if let Some(p) = near {
        let base = (round10(p.0), round10(p.1));
        const STEP: f64 = 20.0;
        let reach = (((content.w.max(content.h) + w.max(h) + 2.0 * margin) / STEP).ceil() as i64 + 2).clamp(1, 250);
        for k in 0..=reach {
            let mut best: Option<(f64, Pt)> = None;
            let mut consider = |dx: i64, dy: i64| {
                let c = (base.0 + dx as f64 * STEP, base.1 + dy as f64 * STEP);
                if free(c) {
                    let d = dist(c, base);
                    if best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, c));
                    }
                }
            };
            if k == 0 {
                consider(0, 0);
            }
            for d in -k..=k {
                if k > 0 {
                    consider(d, -k);
                    consider(d, k);
                }
            }
            for d in (-k + 1)..k {
                consider(-k, d);
                consider(k, d);
            }
            if let Some((_, c)) = best {
                return (c, Place::Near);
            }
        }
    }
    let right = (ceil10(content.right() + margin), floor10(content.y));
    if free(right) {
        return (right, Place::Right);
    }
    ((floor10(content.x), ceil10(content.bottom() + margin)), Place::Below)
}

// -------------------------------------------------------------------- lint

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    /// Shapes, standalone text, images: things that shouldn't overlap.
    Solid,
    Frame,
    /// Text bound to a shape.
    Label,
    /// Text bound to an arrow.
    ArrowLabel,
    /// Arrows and lines.
    Link,
    Other,
}

fn role(el: &Value, ctx: &Ctx) -> Role {
    match kind(el) {
        "text" => match container_id(el).and_then(|c| ctx.get(c)) {
            Some(c) if kind(c) == "arrow" => Role::ArrowLabel,
            Some(_) => Role::Label,
            None => Role::Solid,
        },
        "rectangle" | "ellipse" | "diamond" | "image" | "embeddable" | "iframe" => Role::Solid,
        "frame" | "magicframe" => Role::Frame,
        "arrow" | "line" => Role::Link,
        _ => Role::Other,
    }
}

/// The drawn path of an arrow or line (curves approximated).
fn path_of(el: &Value) -> Vec<Pt> {
    let pts = model::abs_points(el);
    if is_rounded(el) && !is_elbow(el) && pts.len() >= 3 {
        smooth(&pts, 6)
    } else {
        pts
    }
}

fn share_group(a: &Value, b: &Value) -> bool {
    let ga = group_ids(a);
    !ga.is_empty() && group_ids(b).iter().any(|g| ga.contains(g))
}

/// Distance between two boxes (0 when they touch).
fn rect_gap(a: &Rect, b: &Rect) -> f64 {
    let dx = (b.x - a.right()).max(a.x - b.right()).max(0.0);
    let dy = (b.y - a.bottom()).max(a.y - b.bottom()).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

fn point_gap(r: &Rect, p: Pt) -> f64 {
    rect_gap(r, &Rect::new(p.0, p.1, 0.0, 0.0))
}

/// The shortest move of `a` that clears `b` by `gap`: distance and direction.
fn separation(a: &Rect, b: &Rect, gap: f64) -> (f64, &'static str) {
    let (ac, bc) = (a.center(), b.center());
    let (dx, xdir) = if ac.0 <= bc.0 { (a.right() - b.x + gap, "left") } else { (b.right() - a.x + gap, "right") };
    let (dy, ydir) = if ac.1 <= bc.1 { (a.bottom() - b.y + gap, "up") } else { (b.bottom() - a.y + gap, "down") };
    if dx <= dy {
        (ceil10(dx), xdir)
    } else {
        (ceil10(dy), ydir)
    }
}

/// Crossings between two paths, ignoring meetings near either path's ends.
fn crossings(a: &[Pt], b: &[Pt]) -> usize {
    let ends = [a[0], a[a.len() - 1], b[0], b[b.len() - 1]];
    let mut count = 0;
    for s in a.windows(2) {
        for t in b.windows(2) {
            if let Some(p) = segment_intersection(s[0], s[1], t[0], t[1]) {
                if ends.iter().all(|e| dist(*e, p) > 10.0) {
                    count += 1;
                }
            }
        }
    }
    count
}

fn contrast(a: f64, b: f64) -> f64 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[derive(Default)]
struct Issues(Vec<(u8, f64, String)>);

impl Issues {
    /// `severity`: 0 is worst. `size` orders issues of the same severity.
    fn push(&mut self, severity: u8, size: f64, msg: String) {
        self.0.push((severity, size, msg));
    }
}

/// Layout problems involving the `touched` elements (the ones just drawn/changed), each a short actionable sentence
/// naming ids and suggesting a fix. `elements` = all non-deleted elements. Assumes a white canvas.
pub fn lint(elements: &[&Value], touched: &HashSet<String>) -> Vec<String> {
    lint_on(elements, touched, None)
}

/// `lint` on a canvas with the given background colour.
pub fn lint_on(elements: &[&Value], touched: &HashSet<String>, background: Option<&str>) -> Vec<String> {
    let by_id: HashMap<&str, &Value> = elements.iter().map(|&e| (id(e), e)).collect();
    let ctx = Ctx::new(&by_id, elements);
    let index: HashMap<&str, usize> = elements.iter().enumerate().map(|(i, &e)| (id(e), i)).collect();
    let rects: Vec<Rect> = elements.iter().map(|e| bounds(e)).collect();
    let roles: Vec<Role> = elements.iter().map(|e| role(e, &ctx)).collect();
    let paths: Vec<Vec<Pt>> = elements.iter().map(|e| if is_linear(e) { path_of(e) } else { Vec::new() }).collect();
    let grid = Grid::new(rects.clone(), 200.0);

    // Touched elements, with labels and their shapes counted together.
    let mut hot: HashSet<usize> = HashSet::new();
    for (i, &e) in elements.iter().enumerate() {
        if !touched.contains(id(e)) {
            continue;
        }
        hot.insert(i);
        if let Some(&c) = container_id(e).and_then(|c| index.get(c)) {
            hot.insert(c);
        }
        if let Some(&t) = ctx.label(e).and_then(|t| index.get(id(t))) {
            hot.insert(t);
        }
    }
    if hot.is_empty() {
        return Vec::new();
    }
    let mut hot_list: Vec<usize> = hot.iter().copied().collect();
    hot_list.sort_unstable();
    let el = |i: usize| elements[i];
    let mut issues = Issues::default();

    // Partial overlaps between shapes, texts, images and frames.
    let mut pairs: HashSet<(usize, usize)> = HashSet::new();
    for &a in &hot_list {
        if !matches!(roles[a], Role::Solid | Role::Frame) {
            continue;
        }
        for b in grid.near(&rects[a]) {
            if b == a || !matches!(roles[b], Role::Solid | Role::Frame) || !pairs.insert((a.min(b), a.max(b))) {
                continue;
            }
            let (ea, eb, ra, rb) = (el(a), el(b), rects[a], rects[b]);
            let (ow, oh) = ra.overlap(&rb);
            if ow <= 4.0 || oh <= 4.0 || ra.contains_rect(&rb, 2.0) || rb.contains_rect(&ra, 2.0) || share_group(ea, eb) {
                continue;
            }
            if frame_id(eb) == Some(id(ea)) || frame_id(ea) == Some(id(eb)) {
                continue; // checked as frame membership below
            }
            let msg = match (roles[a], roles[b]) {
                (Role::Frame, Role::Frame) => format!(
                    "{} and {} overlap by {}×{} — move one so the frames sit side by side",
                    who(ea, &ctx),
                    who(eb, &ctx),
                    n(ow),
                    n(oh)
                ),
                (Role::Frame, _) | (_, Role::Frame) => {
                    let (f, x) = if roles[a] == Role::Frame { (ea, eb) } else { (eb, ea) };
                    format!(
                        "{} lies partly over {} — move it fully inside the frame (to make it part of it) or fully outside",
                        who(x, &ctx),
                        who(f, &ctx)
                    )
                }
                _ => {
                    let (d, dir) = separation(&ra, &rb, 20.0);
                    format!(
                        "{} overlaps {} by {}×{} — move {} {} px {}, or space them out with arrange",
                        who(ea, &ctx),
                        who(eb, &ctx),
                        n(ow),
                        n(oh),
                        id(ea),
                        n(d),
                        dir
                    )
                }
            };
            issues.push(1, ow * oh, msg);
        }
    }

    // Frame children outside their frame get clipped.
    let mut members: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, &e) in elements.iter().enumerate() {
        if let Some(f) = frame_id(e) {
            members.entry(f).or_default().push(i);
        }
    }
    let mut to_check: Vec<usize> = Vec::new();
    for &i in &hot_list {
        if frame_id(el(i)).is_some() {
            to_check.push(i);
        }
        if roles[i] == Role::Frame {
            to_check.extend(members.get(id(el(i))).into_iter().flatten());
        }
    }
    to_check.sort_unstable();
    to_check.dedup();
    for i in to_check {
        let e = el(i);
        if matches!(roles[i], Role::Label | Role::ArrowLabel) {
            continue;
        }
        let Some(&fi) = frame_id(e).and_then(|f| index.get(f)) else { continue };
        if roles[fi] != Role::Frame || rects[fi].contains_rect(&rects[i], 2.0) {
            continue;
        }
        let (ow, oh) = rects[fi].overlap(&rects[i]);
        if ow < 0.0 || oh < 0.0 {
            issues.push(
                0,
                0.0,
                format!(
                    "{} belongs to {} but lies outside it, so it is hidden — move it inside the frame or take it out of the frame",
                    who(e, &ctx),
                    who(el(fi), &ctx)
                ),
            );
        } else {
            issues.push(
                1,
                0.0,
                format!(
                    "{} sticks out of {} and the part outside is cut off — move it inside or enlarge the frame",
                    who(e, &ctx),
                    who(el(fi), &ctx)
                ),
            );
        }
    }

    // Labels that don't fit their shapes.
    for &c in &hot_list {
        let ec = el(c);
        let t = kind(ec);
        if !matches!(t, "rectangle" | "ellipse" | "diamond") {
            continue;
        }
        let Some(l) = ctx.label(ec) else { continue };
        let (w, h) = (model::width(ec), model::height(ec));
        let (tw, th) = (model::width(l), model::height(l));
        let pad = 2.0 * BOUND_TEXT_PADDING;
        let s2 = std::f64::consts::SQRT_2;
        let (iw, ih) = match t {
            "ellipse" => ((w / 2.0 * s2).round() - pad, (h / 2.0 * s2).round() - pad),
            "diamond" => ((w / 2.0).round() - pad, (h / 2.0).round() - pad),
            _ => (w - pad, h - pad),
        };
        if tw > iw + 1.0 || th > ih + 1.0 {
            let need = |text: f64, cur: f64| {
                let base = text + pad;
                let v = match t {
                    "ellipse" => base * s2,
                    "diamond" => base * 2.0,
                    _ => base,
                };
                n(ceil10(v.max(cur)))
            };
            issues.push(
                1,
                (tw - iw).max(0.0) + (th - ih).max(0.0),
                format!(
                    "label of {} doesn't fit: the text is {}×{} but {} only has room for {}×{} — resize {} to at least {}×{}, shorten the label, or use a smaller font",
                    who(ec, &ctx),
                    n(tw),
                    n(th),
                    id(ec),
                    n(iw.max(0.0)),
                    n(ih.max(0.0)),
                    id(ec),
                    need(tw, w),
                    need(th, h)
                ),
            );
        } else if !model::rect(ec).inflate(2.0).contains_rect(&model::rect(l), 0.0) {
            issues.push(
                1,
                0.0,
                format!("label of {} sits outside its shape — set the label again so it is re-centred", who(ec, &ctx)),
            );
        }
    }

    // Arrows and lines passing through shapes they don't connect.
    let passes = |a: usize, b: usize| -> bool {
        let (ea, eb, p) = (el(a), el(b), &paths[a]);
        if p.len() < 2 || share_group(ea, eb) {
            return false;
        }
        let bid = id(eb);
        if model::binding_target(ea, "startBinding") == Some(bid) || model::binding_target(ea, "endBinding") == Some(bid) {
            return false;
        }
        let around = rects[b].inflate(8.0);
        if around.contains(p[0]) || around.contains(p[p.len() - 1]) {
            return false;
        }
        p.windows(2).any(|s| segment_hits_rect(s[0], s[1], &rects[b], 6.0))
    };
    let along = |a: usize| -> Vec<usize> {
        let mut c: Vec<usize> = paths[a].windows(2).flat_map(|s| grid.near(&Rect::from_points([s[0], s[1]]).unwrap())).collect();
        c.sort_unstable();
        c.dedup();
        c
    };
    let mut through: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &a in &hot_list {
        if roles[a] != Role::Link {
            continue;
        }
        for b in along(a) {
            if b != a && roles[b] == Role::Solid && passes(a, b) {
                through.entry(a).or_default().push(b);
            }
        }
    }
    for &s in &hot_list {
        if roles[s] != Role::Solid {
            continue;
        }
        for a in grid.near(&rects[s]) {
            if roles[a] == Role::Link && !hot.contains(&a) && passes(a, s) {
                through.entry(a).or_default().push(s);
            }
        }
    }
    for (a, bs) in through {
        let ea = el(a);
        let names: Vec<String> = bs.iter().take(3).map(|&b| who(el(b), &ctx)).collect();
        let ids: Vec<&str> = bs.iter().take(3).map(|&b| id(el(b))).collect();
        let more = if bs.len() > 3 { format!(" and {} more", bs.len() - 3) } else { String::new() };
        let ids = ids.join(", ");
        let fix = match (kind(ea), is_elbow(ea)) {
            ("arrow", true) => format!("move {ids} out of its path or connect different sides"),
            ("arrow", false) => format!("use route: \"elbow\" or move {ids} out of its path"),
            _ => format!("move the line or {ids}"),
        };
        issues.push(2, bs.len() as f64, format!("{} {} passes through {}{more} — {fix}", kind(ea), id(ea), names.join(", ")));
    }

    // Arrow labels covering shapes or each other.
    let mut label_pairs: Vec<(usize, usize)> = Vec::new();
    for &l in &hot_list {
        if roles[l] == Role::ArrowLabel {
            label_pairs.extend(grid.near(&rects[l]).into_iter().map(|b| (l, b)));
        }
    }
    for &s in &hot_list {
        if matches!(roles[s], Role::Solid) {
            label_pairs.extend(grid.near(&rects[s]).into_iter().filter(|&l| roles[l] == Role::ArrowLabel).map(|l| (l, s)));
        }
    }
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    for (l, b) in label_pairs {
        if l == b || !matches!(roles[b], Role::Solid | Role::ArrowLabel) || !seen.insert((l.min(b), l.max(b))) {
            continue;
        }
        let (ow, oh) = rects[l].overlap(&rects[b]);
        if ow <= 4.0 || oh <= 4.0 {
            continue;
        }
        let (lab, eb) = (el(l), el(b));
        let arrow = container_id(lab).unwrap_or("?");
        let q = quote(model::original_text(lab), 30);
        let msg = if roles[b] == Role::ArrowLabel {
            format!(
                "labels of arrows {arrow} and {} overlap — move the arrows apart or shorten a label",
                container_id(eb).unwrap_or("?")
            )
        } else if rects[b].contains_rect(&rects[l], 1.0) {
            continue; // a label inside a zone
        } else if ctx.get(arrow).is_some_and(|a| {
            model::binding_target(a, "startBinding") == Some(id(eb)) || model::binding_target(a, "endBinding") == Some(id(eb))
        }) {
            format!(
                "label {q} of arrow {arrow} overlaps {} — the arrow is too short for its label; move {} further away or shorten the label",
                who(eb, &ctx),
                id(eb)
            )
        } else {
            format!("label {q} of arrow {arrow} covers {} — move {} or reroute the arrow", who(eb, &ctx), id(eb))
        };
        issues.push(3, ow * oh, msg);
    }

    // Arrows crossing each other.
    let mut crossed = 0;
    let mut examples: Vec<String> = Vec::new();
    let mut arrow_pairs: HashSet<(usize, usize)> = HashSet::new();
    for &a in &hot_list {
        if kind(el(a)) != "arrow" || paths[a].len() < 2 {
            continue;
        }
        for b in along(a) {
            if b == a || kind(el(b)) != "arrow" || paths[b].len() < 2 || !arrow_pairs.insert((a.min(b), a.max(b))) {
                continue;
            }
            let c = crossings(&paths[a], &paths[b]);
            if c > 0 {
                crossed += c;
                examples.push(if c > 1 { format!("{}×{} {c} times", id(el(a)), id(el(b))) } else { format!("{}×{}", id(el(a)), id(el(b))) });
            }
        }
    }
    if crossed >= 3 {
        let more = if examples.len() > 5 { format!(", +{} more", examples.len() - 5) } else { String::new() };
        examples.truncate(5);
        issues.push(
            5,
            crossed as f64,
            format!(
                "the arrows just drawn cross other arrows {crossed} times ({}{more}) — put connected shapes next to each other or use route: \"elbow\" to untangle",
                examples.join(", ")
            ),
        );
    }

    // Small or low-contrast text.
    let canvas = background.filter(|b| colors::rgba(b).is_some_and(|c| c[3] > 0)).unwrap_or(colors::WHITE);
    let solid_fill = |e: &Value| has_fill(e) && opt_str(e, "fillStyle").is_none_or(|f| f == "solid");
    // The filled shape drawn beneath z-index `below` at `p`, else the canvas.
    let backdrop = |below: usize, p: Pt| -> &str {
        grid.near(&Rect::new(p.0, p.1, 0.0, 0.0))
            .into_iter()
            .filter(|&j| j < below && roles[j] == Role::Solid && kind(el(j)) != "text" && solid_fill(el(j)) && rects[j].contains(p))
            .min_by(|&a, &b| (rects[a].w * rects[a].h).total_cmp(&(rects[b].w * rects[b].h)))
            .map(|j| str_of(el(j), "backgroundColor"))
            .unwrap_or(canvas)
    };
    for &t in &hot_list {
        let et = el(t);
        if kind(et) != "text" {
            continue;
        }
        let container = container_id(et).and_then(|c| index.get(c).copied());
        let desc = match (roles[t], container) {
            (Role::Label, Some(c)) => format!("label of {}", who(el(c), &ctx)),
            (Role::ArrowLabel, Some(c)) => format!("label {} of arrow {}", quote(model::original_text(et), 30), id(el(c))),
            _ => format!("text {}", who(et, &ctx)),
        };
        let size = num(et, "fontSize");
        if size > 0.0 && size < 12.0 {
            issues.push(4, 12.0 - size, format!("{desc} is only {}px — too small to read; use 16 or more", num_str(size)));
        }
        let center = rects[t].center();
        let bg = match (roles[t], container) {
            (Role::Label, Some(c)) if solid_fill(el(c)) => str_of(el(c), "backgroundColor"),
            (Role::Label, Some(c)) => backdrop(c, center),
            _ => backdrop(t, center),
        };
        let fg = str_of(et, "strokeColor");
        let fg = if fg.is_empty() { colors::BLACK } else { fg };
        if let (Some(lf), Some(lb)) = (colors::luminance(fg), colors::luminance(bg)) {
            let ratio = contrast(lf, lb);
            if ratio < 2.5 {
                issues.push(
                    4,
                    2.5 - ratio,
                    format!(
                        "{desc} is hard to read: {} text on {} (contrast {ratio:.1}) — use a {} text colour or change the background",
                        colors::name(fg, Slot::Stroke),
                        colors::name(bg, Slot::Fill),
                        if lb > 0.4 { "darker" } else { "lighter" }
                    ),
                );
            }
        }
    }

    // Broken bindings and degenerate arrows.
    for &a in &hot_list {
        let ea = el(a);
        if roles[a] == Role::Link && (paths[a].len() < 2 || rects[a].w.max(rects[a].h) < 1.0) {
            issues.push(1, 0.0, format!("{} {} has no length — delete it or redraw it between two points", kind(ea), id(ea)));
            continue;
        }
        if kind(ea) != "arrow" {
            continue;
        }
        let p = &paths[a];
        let raw = model::abs_points(ea);
        for (key, which, end) in [("startBinding", "start", raw.first()), ("endBinding", "end", raw.last())] {
            let Some(tid) = model::binding_target(ea, key) else { continue };
            match index.get(tid) {
                None => issues.push(
                    0,
                    0.0,
                    format!(
                        "the {which} of arrow {} is bound to \"{tid}\", which doesn't exist — connect it to an existing shape or leave that end free",
                        id(ea)
                    ),
                ),
                Some(&ti) => {
                    let gap = end.or(p.first()).map(|&q| point_gap(&rects[ti], q)).unwrap_or(0.0);
                    if gap > 30.0 {
                        issues.push(
                            1,
                            gap,
                            format!(
                                "the {which} of arrow {} is bound to {} but sits {} px away from it — reconnect the arrow so it touches {tid}",
                                id(ea),
                                who(el(ti), &ctx),
                                n(gap)
                            ),
                        );
                    }
                }
            }
        }
    }
    for &t in &hot_list {
        let et = el(t);
        if let Some(c) = container_id(et).filter(|c| kind(et) == "text" && !index.contains_key(c)) {
            issues.push(
                0,
                0.0,
                format!(
                    "text {} is a label of \"{c}\", which doesn't exist — recreate it as plain text or as the label of an existing shape",
                    id(et)
                ),
            );
        }
    }

    // Elements far from everything else.
    let body = |i: usize| !matches!(roles[i], Role::Label | Role::ArrowLabel);
    let rest = union_all((0..elements.len()).filter(|i| !hot.contains(i) && body(*i)).map(|i| rects[i]));
    let mut far: Vec<(usize, f64)> = Vec::new();
    match rest {
        Some(rest) => {
            for &i in &hot_list {
                let d = rect_gap(&rects[i], &rest);
                if body(i) && d > 3000.0 {
                    far.push((i, d));
                }
            }
        }
        None => {
            let bodies: Vec<usize> = hot_list.iter().copied().filter(|&i| body(i) && roles[i] != Role::Link).collect();
            for &i in &bodies {
                let d = bodies.iter().filter(|&&j| j != i).map(|&j| rect_gap(&rects[i], &rects[j])).fold(f64::INFINITY, f64::min);
                if d.is_finite() && d > 3000.0 {
                    far.push((i, d));
                }
            }
        }
    }
    if !far.is_empty() {
        let far_set: HashSet<usize> = far.iter().map(|f| f.0).collect();
        let others: Vec<&Value> = (0..elements.len()).filter(|i| !far_set.contains(i)).map(el).collect();
        let size = union_all(far.iter().map(|f| rects[f.0])).unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
        let spot = free_spot(&others, size.w.min(2000.0), size.h.min(2000.0), HINT_MARGIN, None);
        let ids: Vec<&str> = far.iter().take(6).map(|f| id(el(f.0))).collect();
        let more = if far.len() > 6 { format!(" and {} more", far.len() - 6) } else { String::new() };
        let d = far.iter().map(|f| f.1).fold(0.0, f64::max);
        let verb = if far.len() == 1 { "is" } else { "are" };
        issues.push(
            3,
            d,
            format!(
                "{}{more} {verb} about {} px away from everything else — probably misplaced; move near the rest of the drawing (free space at {})",
                ids.join(", "),
                n(d),
                fmt_pt(spot)
            ),
        );
    }

    let mut list = issues.0;
    list.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.total_cmp(&a.1)));
    let mut out: Vec<String> = Vec::new();
    let mut total = 0;
    for (_, _, msg) in list {
        if out.contains(&msg) {
            continue;
        }
        total += 1;
        if out.len() < MAX_ISSUES {
            out.push(msg);
        }
    }
    if total > MAX_ISSUES {
        out.push(format!("…and {} more layout issues; fix these first, then check again", total - MAX_ISSUES));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shape(t: &str, id: &str, x: f64, y: f64, w: f64, h: f64) -> Value {
        json!({
            "id": id, "type": t, "x": x, "y": y, "width": w, "height": h, "angle": 0,
            "strokeColor": "#1e1e1e", "backgroundColor": "transparent", "fillStyle": "solid",
            "strokeWidth": 2, "strokeStyle": "solid", "roughness": 1, "opacity": 100,
            "groupIds": [], "frameId": null, "roundness": { "type": 3 }, "boundElements": [],
            "locked": false, "link": null, "seed": 7, "version": 3, "versionNonce": 9,
            "isDeleted": false, "updated": 1, "index": "a0"
        })
    }

    fn rect(id: &str, x: f64, y: f64, w: f64, h: f64) -> Value {
        shape("rectangle", id, x, y, w, h)
    }

    fn text(id: &str, x: f64, y: f64, s: &str) -> Value {
        let w = s.chars().count() as f64 * 10.0;
        json!({
            "id": id, "type": "text", "x": x, "y": y, "width": w, "height": 25, "angle": 0,
            "strokeColor": "#1e1e1e", "backgroundColor": "transparent", "text": s, "originalText": s,
            "fontSize": 20, "fontFamily": 5, "textAlign": "left", "verticalAlign": "top",
            "containerId": null, "groupIds": [], "frameId": null, "lineHeight": 1.25, "opacity": 100
        })
    }

    /// A shape with a centred label.
    fn labeled(t: &str, id: &str, x: f64, y: f64, w: f64, h: f64, label: &str) -> [Value; 2] {
        let mut s = shape(t, id, x, y, w, h);
        let lid = format!("{id}-label");
        s["boundElements"] = json!([{ "id": lid, "type": "text" }]);
        let tw = label.chars().count() as f64 * 10.0;
        let mut l = text(&lid, x + (w - tw) / 2.0, y + (h - 25.0) / 2.0, label);
        l["containerId"] = json!(id);
        l["textAlign"] = json!("center");
        l["verticalAlign"] = json!("middle");
        [s, l]
    }

    fn arrow(id: &str, pts: &[Pt], from: Option<&str>, to: Option<&str>) -> Value {
        let (x0, y0) = pts[0];
        let rel: Vec<Value> = pts.iter().map(|(x, y)| json!([x - x0, y - y0])).collect();
        let r = Rect::from_points(pts.iter().copied()).unwrap();
        let bind = |t: Option<&str>| t.map(|t| json!({ "elementId": t, "focus": 0, "gap": 5 })).unwrap_or(Value::Null);
        json!({
            "id": id, "type": "arrow", "x": x0, "y": y0, "width": r.w, "height": r.h, "angle": 0,
            "strokeColor": "#1e1e1e", "backgroundColor": "transparent", "strokeWidth": 2, "strokeStyle": "solid",
            "points": rel, "startBinding": bind(from), "endBinding": bind(to),
            "startArrowhead": null, "endArrowhead": "arrow", "roundness": { "type": 2 },
            "elbowed": false, "groupIds": [], "frameId": null, "opacity": 100
        })
    }

    fn refs(v: &[Value]) -> Vec<&Value> {
        v.iter().collect()
    }

    fn touched(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    /// api → db with an "SQL" label, a title and a cache box.
    fn sample() -> Vec<Value> {
        let mut v = Vec::new();
        v.push(text("title", 100.0, 20.0, "Architecture"));
        v.extend(labeled("rectangle", "api", 100.0, 80.0, 200.0, 80.0, "API Gateway"));
        v.extend(labeled("rectangle", "db", 500.0, 80.0, 200.0, 80.0, "Database"));
        let mut a = arrow("api-to-db", &[(305.0, 120.0), (495.0, 120.0)], Some("api"), Some("db"));
        a["boundElements"] = json!([{ "id": "sql", "type": "text" }]);
        v.push(a);
        let mut l = text("sql", 385.0, 108.0, "SQL");
        l["containerId"] = json!("api-to-db");
        v.push(l);
        v[1]["backgroundColor"] = json!("#a5d8ff");
        v[1]["strokeColor"] = json!("#1971c2");
        v
    }

    #[test]
    fn describe_lists_shapes_with_labels_and_arrows() {
        let v = sample();
        let out = describe("Board", &refs(&v), None, &[], &DescribeOptions::default());
        assert!(out.starts_with("Canvas \"Board\" — 4 elements, content x 100…700, y 20…160"), "{out}");
        assert!(out.contains("  api  rectangle (100,80 200×80) \"API Gateway\" · stroke blue, fill blue\n"), "{out}");
        assert!(out.contains("  db  rectangle (500,80 200×80) \"Database\"\n"), "{out}");
        assert!(out.contains("  api-to-db  arrow api → db \"SQL\" (straight)\n"), "{out}");
        assert!(out.contains("  title  text (100,20 120×25) \"Architecture\" · 20px hand\n"), "{out}");
        assert!(!out.contains("api-label"), "bound text is not listed on its own: {out}");
        assert!(!out.contains("sql  text"), "{out}");
        // Reading order: title row first, then api, db, then arrows.
        let (t, a, d, ar) = (out.find("title  ").unwrap(), out.find("api  ").unwrap(), out.find("db  ").unwrap(), out.find("api-to-db  ").unwrap());
        assert!(t < a && a < d && d < ar);
        assert!(out.contains("Free space for new content: right of everything at (740,20)."), "{out}");
    }

    #[test]
    fn describe_shows_people_and_selection() {
        let v = sample();
        let peers = [
            PeerView { name: "Rafa".into(), pointer: Some((412.4, 233.0)), selected: vec!["api-label".into(), "db".into(), "gone".into()], is_you: false },
            PeerView { name: "Claude".into(), pointer: None, selected: vec![], is_you: true },
        ];
        let out = describe("Board", &refs(&v), Some("#f8f9fa"), &peers, &DescribeOptions::default());
        assert!(out.contains(", background gray-0\n"), "{out}");
        assert!(out.contains("  Rafa — pointer at (412,233), selected: api, db\n"), "{out}");
        assert!(out.contains("  Claude (you)\n"), "{out}");
        assert!(out.contains("\"API Gateway\" · stroke blue, fill blue · selected by Rafa"), "{out}");
    }

    #[test]
    fn describe_filters_by_ids_and_region() {
        let mut v = sample();
        v.push(rect("far", 3000.0, 3000.0, 100.0, 100.0));
        let opts = DescribeOptions { ids: Some(vec!["db".into(), "nope".into()]), ..Default::default() };
        let out = describe("Board", &refs(&v), None, &[], &opts);
        assert!(out.contains("Not found: nope"), "{out}");
        assert!(out.contains("Showing 2 of 5 elements"), "{out}");
        assert!(out.contains("  db  rectangle"), "{out}");
        assert!(out.contains("api-to-db  arrow"), "connected arrows come along: {out}");
        assert!(!out.contains("  api  rectangle"), "{out}");

        let opts = DescribeOptions { region: Some(Rect::new(2900.0, 2900.0, 500.0, 500.0)), ..Default::default() };
        let out = describe("Board", &refs(&v), None, &[], &opts);
        assert!(out.contains("Showing 1 of 5 elements (region (2900,2900 500×500))."), "{out}");
        assert!(out.contains("  far  rectangle (3000,3000 100×100)"), "{out}");
        assert!(!out.contains("title"), "{out}");
    }

    #[test]
    fn describe_groups_by_frame_and_marks_zones() {
        let mut f = shape("frame", "backend", 0.0, 0.0, 800.0, 600.0);
        f["name"] = json!("Backend");
        let mut zone = rect("zone", 20.0, 20.0, 500.0, 400.0);
        zone["frameId"] = json!("backend");
        zone["strokeStyle"] = json!("dashed");
        let mut inner = rect("svc", 60.0, 60.0, 100.0, 60.0);
        inner["frameId"] = json!("backend");
        inner["groupIds"] = json!(["g1"]);
        let outside = rect("ext", 1000.0, 0.0, 100.0, 60.0);
        let v = vec![f, zone, inner, outside];
        let out = describe("B", &refs(&v), None, &[], &DescribeOptions::default());
        assert!(out.contains("Frame backend \"Backend\" (0,0 800×600), 2 elements:\n"), "{out}");
        assert!(out.contains("  zone  rectangle (20,20 500×400) · dashed · contains svc\n"), "{out}");
        assert!(out.contains("  svc  rectangle (60,60 100×60) · group g1\n"), "{out}");
        assert!(out.contains("Outside frames:\n  ext  rectangle"), "{out}");
        assert!(!out.contains("in frame"), "{out}");
    }

    #[test]
    fn describe_summarises_large_scenes() {
        let mut v: Vec<Value> = (0..40).map(|i| rect(&format!("r{i}"), (i % 8) as f64 * 150.0, (i / 8) as f64 * 150.0, 100.0, 100.0)).collect();
        v.push(text("big", 0.0, -100.0, "Big title"));
        let peers = [PeerView { name: "Ana".into(), pointer: None, selected: vec!["r3".into()], is_you: false }];
        let opts = DescribeOptions { max_elements: 10, ..Default::default() };
        let out = describe("Big", &refs(&v), None, &peers, &opts);
        assert!(out.contains("Too many elements to list them all (41 > 10)."), "{out}");
        assert!(out.contains("Selected:\n  r3  rectangle (450,0 100×100) · selected by Ana\n"), "{out}");
        assert!(out.contains("  area (0,-100 400×400): "), "{out}");
        assert!(out.contains("rectangles"), "{out}");
        assert!(out.contains("e.g. \"Big title\""), "{out}");
        assert!(out.contains("`region`"), "{out}");
        assert!(!out.contains("r20  rectangle"), "{out}");
    }

    #[test]
    fn describe_full_detail_drops_noise() {
        let v = sample();
        let opts = DescribeOptions { detail: Detail::Full, ..Default::default() };
        let out = describe("Board", &refs(&v), None, &[], &opts);
        let api_json = out.lines().skip_while(|l| !l.starts_with("  api  ")).nth(1).unwrap();
        assert!(api_json.starts_with("    {"), "{out}");
        for noise in ["seed", "version", "updated", "isDeleted", "boundElements", "frameId", "\"index\"", "roughness", "opacity"] {
            assert!(!api_json.contains(noise), "{noise} in {api_json}");
        }
        assert!(api_json.contains("\"backgroundColor\":\"#a5d8ff\"") && api_json.contains("\"x\":100"), "{api_json}");
        assert!(out.contains("    label {"), "{out}");
        assert!(out.contains("\"text\":\"API Gateway\""), "{out}");
    }

    #[test]
    fn element_lines_show_unusual_styles() {
        let mut r = rect("box", 10.4, 20.0, 100.0, 50.0);
        r["roundness"] = Value::Null;
        r["opacity"] = json!(50);
        r["angle"] = json!(std::f64::consts::FRAC_PI_2);
        r["frameId"] = json!("f1");
        r["locked"] = json!(true);
        r["fillStyle"] = json!("hachure");
        r["backgroundColor"] = json!("#ffc9c9");
        let by_id: HashMap<&str, &Value> = HashMap::new();
        assert_eq!(
            element_line(&r, &by_id),
            "box  rectangle (10,20 100×50) · fill red, hachure · sharp corners · opacity 50% · rotated 90° · locked · in frame f1"
        );
        let mut a = arrow("a", &[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)], None, Some("x"));
        a["elbowed"] = json!(true);
        a["startArrowhead"] = json!("dot");
        a["endArrowhead"] = Value::Null;
        a["strokeStyle"] = json!("dotted");
        assert_eq!(element_line(&a, &by_id), "a  arrow (0,0) → x (missing) (elbow, 3 pts) · start head dot · no end head · dotted");
        let mut t = text("t", 0.0, 0.0, "line one\nline two");
        t["textAlign"] = json!("center");
        t["fontFamily"] = json!(8);
        t["strokeColor"] = json!("#e03131");
        assert_eq!(element_line(&t, &by_id), "t  text (0,0 170×25) \"line one⏎line two\" · 20px code · centered · color red");
        let long = "x".repeat(80);
        assert!(element_line(&text("l", 0.0, 0.0, &long), &by_id).contains(&format!("\"{}…\"", "x".repeat(60))));
    }

    #[test]
    fn lint_flags_partial_overlaps_only() {
        let mut v = sample();
        v.push(rect("cache", 250.0, 140.0, 100.0, 60.0)); // overlaps api's corner
        let issues = lint(&refs(&v), &touched(&["cache"]));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].starts_with("rectangle cache overlaps \"API Gateway\" (api) by 50×20 — move cache 40 px down"), "{issues:?}");
        // Nothing reported for untouched elements.
        assert!(lint(&refs(&v), &touched(&["title"])).is_empty());

        // Containment is intentional; so is a label in its own shape.
        let mut v = sample();
        v.insert(0, rect("zone", 50.0, 0.0, 800.0, 300.0));
        let issues = lint(&refs(&v), &touched(&["zone", "api", "db", "api-to-db", "api-label", "title"]));
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn lint_skips_grouped_overlaps_and_reports_frames() {
        let mut a = rect("a", 0.0, 0.0, 100.0, 100.0);
        let mut b = rect("b", 50.0, 50.0, 100.0, 100.0);
        a["groupIds"] = json!(["g"]);
        b["groupIds"] = json!(["g"]);
        let v = vec![a, b];
        assert!(lint(&refs(&v), &touched(&["a", "b"])).is_empty());

        let f = shape("frame", "f", 0.0, 0.0, 300.0, 300.0);
        let mut inside = rect("in", 250.0, 10.0, 100.0, 50.0);
        inside["frameId"] = json!("f");
        let mut hidden = rect("lost", 900.0, 10.0, 100.0, 50.0);
        hidden["frameId"] = json!("f");
        let v = vec![f, inside, hidden];
        let issues = lint(&refs(&v), &touched(&["in", "lost"]));
        assert_eq!(issues.len(), 2, "{issues:?}");
        assert!(issues[0].contains("rectangle lost belongs to frame \"Frame\" (f) but lies outside it"), "{issues:?}");
        assert!(issues[1].contains("rectangle in sticks out of frame"), "{issues:?}");
    }

    #[test]
    fn lint_flags_labels_that_dont_fit() {
        let [mut s, mut l] = labeled("rectangle", "tiny", 0.0, 0.0, 60.0, 30.0, "A very long label");
        l["x"] = json!(-55);
        l["width"] = json!(170);
        s["width"] = json!(60);
        let v = vec![s, l];
        let issues = lint(&refs(&v), &touched(&["tiny"]));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].starts_with("label of \"A very long label\" (tiny) doesn't fit: the text is 170×25 but tiny only has room for 50×20 — resize tiny to at least 180×40"), "{issues:?}");

        // Touching just the label checks its shape too; a diamond needs more room.
        let v: Vec<Value> = labeled("diamond", "d", 0.0, 0.0, 160.0, 80.0, "Approved?").into();
        let issues = lint(&refs(&v), &touched(&["d-label"]));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("d only has room for 70×30 — resize d to at least 200×80"), "{issues:?}");
    }

    #[test]
    fn lint_flags_arrows_through_shapes() {
        let mut v = Vec::new();
        v.extend(labeled("rectangle", "a", 0.0, 0.0, 100.0, 60.0, "A"));
        v.extend(labeled("rectangle", "b", 200.0, 0.0, 100.0, 60.0, "B"));
        v.extend(labeled("rectangle", "c", 400.0, 0.0, 100.0, 60.0, "C"));
        v.push(arrow("a-c", &[(105.0, 30.0), (395.0, 30.0)], Some("a"), Some("c")));
        v.push(arrow("a-b", &[(105.0, 20.0), (195.0, 20.0)], Some("a"), Some("b")));
        let issues = lint(&refs(&v), &touched(&["a-c", "a-b"]));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0], "arrow a-c passes through \"B\" (b) — use route: \"elbow\" or move b out of its path");
        // Moving the shape in the way is reported too.
        assert_eq!(lint(&refs(&v), &touched(&["b"])), issues);
        // An unbound arrow ending inside a shape is fine.
        let v2 = vec![rect("box", 0.0, 0.0, 100.0, 100.0), arrow("in", &[(-100.0, 50.0), (50.0, 50.0)], None, None)];
        assert!(lint(&refs(&v2), &touched(&["in"])).is_empty());
    }

    #[test]
    fn lint_flags_arrow_labels_and_crossings() {
        let mut v = Vec::new();
        v.extend(labeled("rectangle", "a", 0.0, 0.0, 100.0, 60.0, "A"));
        v.extend(labeled("rectangle", "b", 130.0, 0.0, 100.0, 60.0, "B"));
        let mut ar = arrow("a-b", &[(105.0, 30.0), (125.0, 30.0)], Some("a"), Some("b"));
        ar["boundElements"] = json!([{ "id": "lbl", "type": "text" }]);
        v.push(ar);
        let mut l = text("lbl", 75.0, 18.0, "calls");
        l["containerId"] = json!("a-b");
        v.push(l);
        let issues = lint(&refs(&v), &touched(&["a-b"]));
        assert!(issues.iter().any(|i| i.contains("label \"calls\" of arrow a-b overlaps \"A\" (a) — the arrow is too short")), "{issues:?}");

        // A grid of crossing arrows.
        let mut v = Vec::new();
        for i in 0..3 {
            let y = i as f64 * 100.0;
            v.push(arrow(&format!("h{i}"), &[(0.0, y), (300.0, y)], None, None));
            v.push(arrow(&format!("v{i}"), &[(50.0 + y, -50.0), (50.0 + y, 250.0)], None, None));
        }
        let issues = lint(&refs(&v), &touched(&["h0", "h1"]));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].starts_with("the arrows just drawn cross other arrows 6 times"), "{issues:?}");
        assert!(lint(&refs(&v), &touched(&["h0"])).iter().any(|i| i.contains("3 times")));
    }

    #[test]
    fn lint_flags_text_dangling_bindings_and_strays() {
        let mut faint = text("faint", 0.0, 0.0, "whisper");
        faint["strokeColor"] = json!("#e9ecef");
        let mut tiny = text("tiny", 0.0, 100.0, "small print");
        tiny["fontSize"] = json!(8);
        let [mut dark, label] = labeled("rectangle", "dark", 300.0, 0.0, 200.0, 80.0, "Ink");
        dark["backgroundColor"] = json!("#343a40");
        let lost = arrow("lost", &[(0.0, 300.0), (100.0, 300.0)], None, Some("ghost"));
        let v = vec![faint, tiny, dark, label, lost, rect("stray", 9000.0, 0.0, 50.0, 50.0)];
        let issues = lint(&refs(&v), &touched(&["faint", "tiny", "dark", "lost", "stray"]));
        assert_eq!(issues.len(), 5, "{issues:?}");
        assert!(issues[0].contains("the end of arrow lost is bound to \"ghost\", which doesn't exist"), "{issues:?}");
        assert!(issues.iter().any(|i| i.starts_with("stray is about 8") && i.contains("probably misplaced")), "{issues:?}");
        assert!(issues.iter().any(|i| i.starts_with("label of \"Ink\" (dark) is hard to read: black text on gray-4")), "{issues:?}");
        assert!(issues.iter().any(|i| i.starts_with("text \"whisper\" (faint) is hard to read: gray-1 text on white")), "{issues:?}");
        assert!(issues.iter().any(|i| i.starts_with("text \"small print\" (tiny) is only 8px")), "{issues:?}");
        // On a dark canvas light text is fine.
        assert!(lint_on(&refs(&v[..1]), &touched(&["faint"]), Some("#1e1e1e")).is_empty());
    }

    #[test]
    fn lint_is_capped() {
        let v: Vec<Value> = (0..40).map(|i| rect(&format!("r{i}"), i as f64 * 20.0, 0.0, 100.0, 100.0)).collect();
        let all: Vec<String> = (0..40).map(|i| format!("r{i}")).collect();
        let issues = lint(&refs(&v), &all.into_iter().collect());
        assert_eq!(issues.len(), MAX_ISSUES + 1);
        assert!(issues.last().unwrap().starts_with("…and "));
    }

    #[test]
    fn free_spot_avoids_content() {
        assert_eq!(free_spot(&[], 600.0, 400.0, 40.0, None), (0.0, 0.0));
        assert_eq!(free_spot(&[], 600.0, 400.0, 40.0, Some((123.0, 47.0))), (120.0, 50.0));
        let v = vec![rect("a", 0.0, 0.0, 400.0, 300.0), rect("b", 500.0, 33.0, 101.0, 100.0)];
        let els = refs(&v);
        assert_eq!(free_spot(&els, 600.0, 400.0, 40.0, None), (650.0, 0.0));
        // Near a point inside a shape: the closest clear spot, on the 10px grid.
        let p = free_spot(&els, 100.0, 100.0, 20.0, Some((200.0, 150.0)));
        assert!(p.0 % 10.0 == 0.0 && p.1 % 10.0 == 0.0);
        let r = Rect::new(p.0, p.1, 100.0, 100.0);
        assert!(v.iter().all(|e| !bounds(e).inflate(20.0).intersects(&r)), "{p:?}");
        assert!(dist(p, (200.0, 150.0)) < 300.0, "{p:?}");
        // A free point stays put.
        assert_eq!(free_spot(&els, 50.0, 50.0, 10.0, Some((1000.0, 1000.0))), (1000.0, 1000.0));
    }

    #[test]
    fn lint_handles_thousands_of_elements() {
        let v: Vec<Value> = (0..3000).map(|i| rect(&format!("r{i}"), (i % 60) as f64 * 150.0, (i / 60) as f64 * 150.0, 100.0, 100.0)).collect();
        let all: HashSet<String> = (0..3000).map(|i| format!("r{i}")).collect();
        let start = std::time::Instant::now();
        assert!(lint(&refs(&v), &all).is_empty());
        let _ = describe("big", &refs(&v), None, &[], &DescribeOptions::default());
        assert!(start.elapsed().as_secs_f64() < 5.0);
    }
}

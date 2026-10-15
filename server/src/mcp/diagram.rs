//! The `draw_diagram` tool: nodes and edges in, a laid-out, consistently
//! styled diagram out (see `layout` for the placement itself).

use super::args::{slug, Args};
use super::build::{self, Style};
use super::colors::{self, Slot, SERIES};
use super::draft::{Draft, Z};
use super::draw::{self, fresh_id, Outcome};
use super::geometry::{Outline, Pt, Rect};
use super::layout::{self, Dir, EdgeStyle, Kind, LEdge, LNode, LayoutInput, SeqInput, SeqMessage};
use super::model;
use super::text::{self, Font};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Graph(Kind),
    Sequence,
}

fn parse_layout(s: &str) -> Option<Layout> {
    Some(match s.trim().to_ascii_lowercase().replace(['_', ' '], "-").as_str() {
        "flow" | "flowchart" | "graph" | "dag" | "layered" | "architecture" | "pipeline" | "process" | "state" | "network" => Layout::Graph(Kind::Flow),
        "tree" | "hierarchy" | "org" | "org-chart" | "orgchart" | "taxonomy" => Layout::Graph(Kind::Tree),
        "mindmap" | "mind-map" | "brainstorm" => Layout::Graph(Kind::Mindmap),
        "radial" | "circle" | "rings" | "hub" => Layout::Graph(Kind::Radial),
        "grid" | "cards" | "matrix" | "gallery" => Layout::Graph(Kind::Grid),
        "sequence" | "sequence-diagram" | "timeline-sequence" => Layout::Sequence,
        _ => return None,
    })
}

fn parse_dir(s: &str) -> Option<Dir> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "down" | "tb" | "td" | "top-bottom" | "top-to-bottom" | "vertical" => Dir::Down,
        "right" | "lr" | "left-right" | "left-to-right" | "horizontal" => Dir::Right,
        "up" | "bt" | "bottom-top" => Dir::Up,
        "left" | "rl" | "right-left" => Dir::Left,
        _ => return None,
    })
}

struct Node<'a> {
    id: String,
    label: String,
    shape: &'static str,
    cluster: Option<String>,
    args: Args<'a>,
    style: Style,
    size: (f64, f64),
}

/// The edge list, from `edges` and nodes' `parent`.
fn edges_of<'a>(a: &Args<'a>, nodes: &[Node]) -> Vec<(String, String, Args<'a>)> {
    let mut edges: Vec<(String, String, Args)> = Vec::new();
    for n in nodes {
        if let Some(p) = n.args.str("parent") {
            edges.push((p.to_owned(), n.id.clone(), Args(&super::args::EMPTY)));
        }
    }
    for e in a.list("edges").map(Vec::as_slice).unwrap_or(&[]) {
        let ea = Args::of(e);
        if let (Some(f), Some(t)) = (ea.text("from").or_else(|| ea.text("source")), ea.text("to").or_else(|| ea.text("target"))) {
            edges.push((f, t, ea));
        }
    }
    edges
}

pub fn draw(draft: &mut Draft, a: &Args, near: Option<Pt>) -> Result<Outcome, String> {
    let raw_nodes = a.list("nodes").filter(|n| !n.is_empty()).ok_or("Pass `nodes`: [{\"id\": \"a\", \"label\": \"Start\"}, …].")?;
    let has_parents = raw_nodes.iter().any(|n| Args::of(n).has("parent"));
    let layout_kind = match a.str("layout").or_else(|| a.str("type")) {
        Some(l) => parse_layout(l).ok_or_else(|| format!("Unknown layout “{l}”. Use flow, tree, mindmap, radial, grid or sequence."))?,
        None if has_parents => Layout::Graph(Kind::Tree),
        None => Layout::Graph(Kind::Flow),
    };
    let dir = match a.str("direction").or_else(|| a.str("dir")) {
        Some(d) => parse_dir(d).ok_or_else(|| format!("Unknown direction “{d}”. Use down, right, up or left."))?,
        None => Dir::Down,
    };
    let colorful = !a.str("theme").is_some_and(|t| matches!(t.to_ascii_lowercase().as_str(), "mono" | "monochrome" | "plain" | "minimal"));
    let mut out = Outcome::default();

    // Clusters, with their colours.
    let mut cluster_info: Vec<(String, String, Option<String>)> = Vec::new(); // id, label, colour
    for c in a.list("clusters").or_else(|| a.list("groups")).map(Vec::as_slice).unwrap_or(&[]) {
        let ca = Args::of(c);
        if let Some(id) = ca.text("id").or_else(|| ca.text("name")) {
            let label = ca.text("label").or_else(|| ca.text("name")).unwrap_or_else(|| id.clone());
            cluster_info.push((id, label, ca.str("color").map(str::to_owned)));
        }
    }
    for n in raw_nodes {
        if let Some(c) = Args::of(n).text("cluster").or_else(|| Args::of(n).text("group")) {
            if !cluster_info.iter().any(|(id, ..)| *id == c) {
                cluster_info.push((c.clone(), c, None));
            }
        }
    }
    let cluster_hue = |id: &str| -> String {
        let i = cluster_info.iter().position(|(c, ..)| c == id).unwrap_or(0);
        cluster_info.get(i).and_then(|c| c.2.clone()).unwrap_or_else(|| SERIES[i % SERIES.len()].to_owned())
    };

    // Ids drawn by an earlier version of this diagram are replaced.
    let node_ids: Vec<String> = raw_nodes.iter().filter_map(|n| Args::of(n).text("id")).collect();
    let mut seen = HashSet::new();
    for id in &node_ids {
        if !seen.insert(id.clone()) {
            return Err(format!("Node id “{id}” is used twice."));
        }
    }
    let replaced: Vec<String> = node_ids.iter().filter(|id| draft.exists(id)).cloned().collect();
    if !replaced.is_empty() {
        draw::delete_ids(draft, &replaced, false);
        out.notes.push(format!("Replaced existing element(s) {} with the new diagram.", replaced.join(", ")));
    }

    // Depth (for tree/mind-map styling) from the parent/edge structure.
    let mut nodes: Vec<Node> = Vec::new();
    for (i, n) in raw_nodes.iter().enumerate() {
        let na = Args::of(n);
        let id = na.text("id").map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).unwrap_or_else(|| fresh_id(draft, &na.text("label").unwrap_or_default(), "node"));
        let label = na.text("label").or_else(|| na.text("text")).unwrap_or_else(|| id.clone());
        let shape = match na.str("shape").or_else(|| na.str("type")) {
            Some(s) => match draw::element_type(s) {
                Some(t @ ("rectangle" | "ellipse" | "diamond" | "note" | "text")) => t,
                _ => return Err(format!("Node “{id}”: unknown shape “{s}”. Use rectangle, ellipse, circle, diamond, note or text.")),
            },
            None if colorful && matches!(layout_kind, Layout::Graph(Kind::Flow)) => {
                let l = label.trim().to_ascii_lowercase();
                if l.ends_with('?') {
                    "diamond"
                } else if ["start", "end", "begin", "stop", "done", "finish"].contains(&l.as_str()) {
                    "ellipse"
                } else {
                    "rectangle"
                }
            }
            None if layout_kind == Layout::Graph(Kind::Mindmap) && i == 0 => "ellipse",
            None => "rectangle",
        };
        let cluster = na.text("cluster").or_else(|| na.text("group"));
        nodes.push(Node { id, label, shape, cluster, args: na, style: Style::default(), size: (0.0, 0.0) });
    }
    let edges = edges_of(a, &nodes);
    let index: HashMap<String, usize> = nodes.iter().enumerate().map(|(i, n)| (n.id.clone(), i)).collect();
    for (f, t, _) in &edges {
        for end in [f, t] {
            if !index.contains_key(end) {
                return Err(format!("Edge {f} → {t}: no node “{end}”. Edges connect node ids from this call."));
            }
        }
    }
    // Breadth-first depth and first-level branch from the roots.
    let mut depth: HashMap<usize, usize> = HashMap::new();
    let mut branch: HashMap<usize, usize> = HashMap::new();
    {
        let mut has_in = vec![false; nodes.len()];
        for (_, t, _) in &edges {
            has_in[index[t]] = true;
        }
        let mut queue: std::collections::VecDeque<usize> = (0..nodes.len()).filter(|i| !has_in[*i]).collect();
        if queue.is_empty() {
            queue.push_back(0);
        }
        for &r in &queue {
            depth.insert(r, 0);
        }
        let mut next_branch = 0;
        while let Some(i) = queue.pop_front() {
            for (f, t, _) in &edges {
                if index[f] == i && !depth.contains_key(&index[t]) {
                    let j = index[t];
                    depth.insert(j, depth[&i] + 1);
                    let b = match branch.get(&i) {
                        Some(b) => *b,
                        None => {
                            next_branch += 1;
                            next_branch - 1
                        }
                    };
                    branch.insert(j, b);
                    queue.push_back(j);
                }
            }
        }
    }

    // Styles and sizes.
    let mindmap = layout_kind == Layout::Graph(Kind::Mindmap);
    for (i, n) in nodes.iter_mut().enumerate() {
        let d = depth.get(&i).copied().unwrap_or(0);
        let hue: Option<String> = if !colorful {
            None
        } else if let Some(c) = &n.cluster {
            Some(cluster_hue(c))
        } else {
            Some(match layout_kind {
                Layout::Graph(Kind::Mindmap | Kind::Radial) if d == 0 => "violet".into(),
                Layout::Graph(Kind::Mindmap | Kind::Radial) => SERIES[branch.get(&i).copied().unwrap_or(0) % SERIES.len()].into(),
                Layout::Graph(Kind::Tree) => ["violet", "blue", "teal", "green", "yellow"][d.min(4)].into(),
                Layout::Graph(Kind::Flow) => match n.shape {
                    "diamond" => "yellow".into(),
                    "ellipse" => "green".into(),
                    _ => "blue".into(),
                },
                _ => "blue".into(),
            })
        };
        let mut st = Style::default();
        if let Some(h) = &hue {
            st.stroke = colors::resolve(h, Slot::Stroke).unwrap_or_else(|| colors::BLACK.into());
            st.fill = colors::resolve(h, Slot::Fill).unwrap_or_else(|| colors::TRANSPARENT.into());
            // Readable text on soft fills: near-black ink, coloured outline.
            st.text_color = Some(colors::BLACK.into());
        }
        if mindmap {
            st.font_size = match d {
                0 => 28.0,
                1 => 20.0,
                _ => 16.0,
            };
            if d == 0 && colorful {
                st.fill = colors::resolve("violet-1", Slot::Fill).unwrap_or_default();
            }
            if d >= 2 && n.args.str("shape").is_none() {
                n.shape = "text";
            }
        }
        if n.shape == "note" {
            st.fill = colors::resolve("yellow", Slot::Fill).unwrap_or_default();
            st.stroke = colors::resolve("yellow", Slot::Stroke).unwrap_or_default();
        }
        st.apply(&n.args, n.shape != "text").map_err(|e| format!("Node “{}”: {e}", n.id))?;
        let kind = if n.shape == "note" { "rectangle" } else { n.shape };
        n.size = if n.shape == "text" {
            let font = st.font();
            let wrapped = font.wrap(&n.label, st.font_size, 220.0);
            (font.text_width(&wrapped, st.font_size) + 8.0, font.text_height(&wrapped, st.font_size))
        } else {
            build::auto_size(kind, Some(&n.label), &st, n.args.num("max_text_width"))
        };
        if let Some(w) = n.args.num("width") {
            n.size.0 = w;
        }
        if let Some(h) = n.args.num("height") {
            n.size.1 = h;
        }
        if n.args.str("shape").is_some_and(|s| s.eq_ignore_ascii_case("circle")) {
            let m = n.size.0.max(n.size.1);
            n.size = (m, m);
        }
        n.style = st;
    }
    // Siblings look tidier the same size.
    let uniform = a.bool("uniform").unwrap_or(!mindmap && layout_kind != Layout::Graph(Kind::Radial));
    if uniform {
        let mut max: HashMap<&str, (f64, f64)> = HashMap::new();
        for n in &nodes {
            if n.args.has("width") || n.args.has("height") || n.shape == "text" {
                continue;
            }
            let e = max.entry(n.shape).or_insert((0.0, 0.0));
            *e = (e.0.max(n.size.0), e.1.max(n.size.1));
        }
        for n in nodes.iter_mut() {
            if let Some(m) = max.get(n.shape).filter(|_| !n.args.has("width") && !n.args.has("height")) {
                n.size = *m;
            }
        }
    }

    let edge_font = Font::new(text::EXCALIFONT);
    let label_size = |s: &str| {
        let wrapped = edge_font.wrap(s, 16.0, 180.0);
        (edge_font.text_width(&wrapped, 16.0) + 16.0, edge_font.text_height(&wrapped, 16.0) + 8.0)
    };
    let (node_gap, rank_gap) = match a.get("spacing") {
        Some(v) => match super::args::as_num(v) {
            Some(n) => (n.max(10.0), (n * 1.6).max(30.0)),
            None => match v.as_str().unwrap_or("").to_ascii_lowercase().as_str() {
                "compact" | "tight" | "dense" => (30.0, 56.0),
                "roomy" | "spacious" | "loose" | "airy" => (80.0, 130.0),
                _ => (50.0, 90.0),
            },
        },
        None if mindmap => (22.0, 70.0),
        None => (50.0, 90.0),
    };
    let edge_style = match a.str("edge_style").or_else(|| a.str("route")).or_else(|| a.str("edges_style")) {
        Some(s) => match build::Route::parse(s) {
            Some(build::Route::Elbow) => EdgeStyle::Elbow,
            Some(build::Route::Curved) => EdgeStyle::Curved,
            Some(build::Route::Straight) => EdgeStyle::Straight,
            None => return Err(format!("Unknown edge_style “{s}”. Use elbow, straight or curved.")),
        },
        None => match layout_kind {
            Layout::Graph(Kind::Mindmap) => EdgeStyle::Curved,
            Layout::Graph(Kind::Radial | Kind::Grid) => EdgeStyle::Straight,
            _ => EdgeStyle::Elbow,
        },
    };

    // Title above the diagram.
    let title = a.text("title");
    let title_style = Style { font_size: 36.0, ..Style::default() };
    let title_h = title.as_ref().map(|t| title_style.font().text_height(t, 36.0) + 36.0).unwrap_or(0.0);

    let lnode = |n: &Node| LNode {
        id: n.id.clone(),
        w: n.size.0,
        h: n.size.1,
        outline: Outline::of(if n.shape == "note" { "rectangle" } else { n.shape }),
        cluster: n.cluster.clone(),
    };

    match layout_kind {
        Layout::Sequence => {
            let messages: Vec<SeqMessage> = edges
                .iter()
                .map(|(f, t, ea)| SeqMessage { from: f.clone(), to: t.clone(), label: ea.text("label").map(|l| label_size(&l)) })
                .collect();
            let seq = layout::sequence(&SeqInput {
                participants: nodes.iter().map(lnode).collect(),
                messages,
                participant_gap: node_gap.max(40.0),
                message_gap: (rank_gap * 0.6).max(40.0),
                edge_gap: 0.0,
            });
            let origin = origin(draft, a, seq.bounds.w, seq.bounds.h + title_h, near)?;
            let off = (origin.0 - seq.bounds.x, origin.1 - seq.bounds.y + title_h);
            let shift = |p: Pt| (p.0 + off.0, p.1 + off.1);
            draw_title(draft, &title, &title_style, origin, &mut out);
            for n in &nodes {
                if let Some(r) = seq.participants.get(&n.id) {
                    place_node(draft, n, Rect::new(r.x + off.0, r.y + off.1, r.w, r.h), &mut out)?;
                }
            }
            let gray = colors::resolve("gray-3", Slot::Stroke).unwrap_or_default();
            for n in &nodes {
                let Some((top, bottom)) = seq.lifelines.get(&n.id) else { continue };
                let id = fresh_id(draft, &format!("{}-lifeline", n.id), "line");
                let st = Style { stroke: gray.clone(), stroke_style: "dashed".into(), stroke_width: 1.0, ..Style::default() };
                let el = build::linear("line", &id, &[shift(*top), shift(*bottom)], &st, false);
                draft.step(shift(*top), Some(&id));
                draft.insert(el, Z::Front);
                out.touch(&id);
                if a.bool("footers") == Some(true) {
                    if let Some(r) = seq.footers.get(&n.id) {
                        let fid = fresh_id(draft, &format!("{}-footer", n.id), "rectangle");
                        let mut copy = Node { id: fid, label: n.label.clone(), shape: n.shape, cluster: None, args: n.args, style: n.style.clone(), size: n.size };
                        copy.args = Args(&super::args::EMPTY);
                        place_node(draft, &copy, Rect::new(r.x + off.0, r.y + off.1, r.w, r.h), &mut out)?;
                    }
                }
            }
            for ((f, t, ea), route) in edges.iter().zip(&seq.messages) {
                if route.points.len() < 2 {
                    continue;
                }
                let id = fresh_id(draft, &format!("{f}-to-{t}"), "arrow");
                let mut st = Style::default();
                st.apply(ea, false)?;
                let pts: Vec<Pt> = route.points.iter().map(|p| shift(*p)).collect();
                let mut el = build::linear("arrow", &id, &pts, &st, false);
                if let Some(h) = ea.str("arrowheads") {
                    apply_heads(&mut el, h);
                }
                let group = format!("{id}-msg");
                el["groupIds"] = json!([group]);
                draft.step(pts[0], Some(&id));
                draft.insert(el, Z::Front);
                out.touch(&id);
                if let Some(l) = ea.text("label") {
                    let ls = Style { font_size: 16.0, ..Style::default() };
                    let lid = fresh_id(draft, &format!("{id}-label"), "text");
                    let mut t = build::text_element(&lid, 0.0, 0.0, &edge_font.wrap(&l, 16.0, 180.0), &ls, None);
                    t["textAlign"] = "center".into();
                    let (cx, cy) = shift(route.label_at);
                    t["x"] = build::round2(cx - model::width(&t) / 2.0).into();
                    t["y"] = build::round2(cy - model::height(&t) / 2.0).into();
                    t["groupIds"] = json!([group]);
                    draft.insert(t, Z::Front);
                }
            }
        }
        Layout::Graph(kind) => {
            let input = LayoutInput {
                kind,
                dir,
                edge_style,
                nodes: nodes.iter().map(lnode).collect(),
                edges: edges.iter().map(|(f, t, ea)| LEdge { from: f.clone(), to: t.clone(), label: ea.text("label").map(|l| label_size(&l)) }).collect(),
                node_gap,
                rank_gap,
                columns: a.num("columns").map(|c| c.max(1.0) as usize),
                cluster_padding: 28.0,
                cluster_title: 34.0,
                edge_gap: if edge_style == EdgeStyle::Elbow { 5.0 } else { 8.0 },
            };
            let lay = layout::layout(&input);
            let origin = origin(draft, a, lay.bounds.w, lay.bounds.h + title_h, near)?;
            let off = (origin.0 - lay.bounds.x, origin.1 - lay.bounds.y + title_h);
            let shift = |p: Pt| (p.0 + off.0, p.1 + off.1);
            draw_title(draft, &title, &title_style, origin, &mut out);
            for (cid, r) in &lay.clusters {
                let hue = cluster_hue(cid);
                let label = cluster_info.iter().find(|c| c.0 == *cid).map(|c| c.1.clone()).unwrap_or_else(|| cid.clone());
                let id = fresh_id(draft, &format!("{}-zone", slug(cid)), "zone");
                let mut st = Style {
                    stroke: colors::resolve(&hue, Slot::Stroke).unwrap_or_default(),
                    fill: colors::resolve(&format!("light-{hue}"), Slot::Fill).unwrap_or_else(|| colors::TRANSPARENT.into()),
                    stroke_style: "dashed".into(),
                    font_size: 16.0,
                    text_align: Some("left".into()),
                    vertical_align: Some("top".into()),
                    ..Style::default()
                };
                if !colorful {
                    st.stroke = colors::resolve("gray-3", Slot::Stroke).unwrap_or_default();
                    st.fill = colors::TRANSPARENT.into();
                }
                let rect = Rect::new(r.x + off.0, r.y + off.1, r.w, r.h);
                draft.step((rect.x, rect.y), Some(&id));
                draft.insert(build::base("rectangle", &id, rect, &st), Z::Front);
                let no_args = Value::Object(Default::default());
                draw::set_label(draft, &id, &label, &st, &Args::of(&no_args));
                out.touch(&id);
            }
            for n in &nodes {
                if let Some(r) = lay.nodes.get(&n.id) {
                    place_node(draft, n, Rect::new(r.x + off.0, r.y + off.1, r.w, r.h), &mut out)?;
                }
            }
            let mindmap_edges = kind == Kind::Mindmap || kind == Kind::Radial;
            for ((f, t, ea), route) in edges.iter().zip(&lay.edges) {
                if route.points.len() < 2 {
                    continue;
                }
                let id = fresh_id(draft, &format!("{f}-to-{t}"), "arrow");
                let mut st = Style::default();
                if mindmap_edges && colorful {
                    let to = &nodes[index[t]];
                    st.stroke = to.style.stroke.clone();
                }
                st.apply(ea, false)?;
                let pts: Vec<Pt> = route.points.iter().map(|p| shift(*p)).collect();
                let elbowed = route.style == EdgeStyle::Elbow;
                let curved = route.style == EdgeStyle::Curved && pts.len() > 2;
                let mut el = build::linear("arrow", &id, &pts, &st, curved);
                let n = pts.len();
                el["startBinding"] = build::binding(draft, f, pts[1], pts[0], elbowed);
                el["endBinding"] = build::binding(draft, t, pts[n - 2], pts[n - 1], elbowed);
                if elbowed {
                    el["elbowed"] = true.into();
                    el["fixedSegments"] = json!([]);
                    el["startIsSpecial"] = false.into();
                    el["endIsSpecial"] = false.into();
                    el["roundness"] = Value::Null;
                }
                if mindmap_edges && !ea.has("arrowheads") {
                    el["endArrowhead"] = Value::Null;
                }
                if let Some(h) = ea.str("arrowheads") {
                    apply_heads(&mut el, h);
                }
                draft.step(pts[0], Some(&id));
                draft.insert(el, Z::Front);
                draft.update(f, |s| build::add_bound(s, &id, "arrow"));
                draft.update(t, |s| build::add_bound(s, &id, "arrow"));
                if let Some(l) = ea.text("label") {
                    let ls = Style { font_size: 16.0, ..st.clone() };
                    draw::set_label(draft, &id, &l, &ls, ea);
                }
                out.touch(&id);
            }
        }
    }

    if let Some(name) = a.str("frame") {
        let inner = super::geometry::union_all(out.touched.iter().filter_map(|id| draft.get(id)).map(model::bounds)).unwrap_or(Rect::new(0.0, 0.0, 100.0, 100.0));
        let id = fresh_id(draft, name, "frame");
        let fs = Style { stroke: "#bbb".into(), roughness: 0.0, rounded: false, ..Style::default() };
        let mut el = build::base("frame", &id, inner.inflate(40.0), &fs);
        el["name"] = name.into();
        draft.step((inner.x, inner.y), Some(&id));
        draft.insert(el, Z::Back);
        let members = out.touched.clone();
        draw::adopt(draft, &id, &members);
        out.touch(&id);
    }
    out.created = out.touched.len();
    Ok(out)
}

fn apply_heads(el: &mut Value, h: &str) {
    let (s, e) = match h.to_ascii_lowercase().as_str() {
        "both" | "two-way" | "bidirectional" => ("arrow", "arrow"),
        "none" | "no" => ("", ""),
        "start" | "reverse" => ("arrow", ""),
        _ => ("", "arrow"),
    };
    el["startArrowhead"] = if s.is_empty() { Value::Null } else { s.into() };
    el["endArrowhead"] = if e.is_empty() { Value::Null } else { e.into() };
}

fn origin(draft: &Draft, a: &Args, w: f64, h: f64, near: Option<Pt>) -> Result<Pt, String> {
    let fallback = || {
        let alive = draft.alive();
        let p = super::describe::free_spot(&alive, w, h, 80.0, near);
        (p.0.round(), p.1.round())
    };
    let p = draw::position(&Args::of(&json!({
        "x": a.num("x"), "y": a.num("y"), "at": a.get("at"),
        "right_of": a.get("right_of"), "below": a.get("below"), "above": a.get("above"), "left_of": a.get("left_of"),
        "gap": a.num("gap").unwrap_or(120.0), "align": a.str("align").unwrap_or("start"),
    })), draft, w, h, fallback)?;
    Ok(p)
}

fn draw_title(draft: &mut Draft, title: &Option<String>, style: &Style, at: Pt, out: &mut Outcome) {
    let Some(t) = title else { return };
    let id = fresh_id(draft, t, "title");
    let el = build::text_element(&id, at.0, at.1, t, style, None);
    draft.step((at.0, at.1 + 18.0), Some(&id));
    draft.insert(el, Z::Front);
    out.touch(&id);
}

fn place_node(draft: &mut Draft, n: &Node, r: Rect, out: &mut Outcome) -> Result<(), String> {
    if n.shape == "text" {
        let mut el = build::text_element(&n.id, 0.0, 0.0, &n.style.font().wrap(&n.label, n.style.font_size, 220.0), &n.style, None);
        el["textAlign"] = "center".into();
        el["x"] = build::round2(r.center().0 - model::width(&el) / 2.0).into();
        el["y"] = build::round2(r.center().1 - model::height(&el) / 2.0).into();
        draft.step(r.center(), Some(&n.id));
        draft.insert(el, Z::Front);
    } else {
        let kind = if n.shape == "note" { "rectangle" } else { n.shape };
        let mut st = n.style.clone();
        if n.shape == "note" {
            st.text_align.get_or_insert("left".into());
            st.vertical_align.get_or_insert("top".into());
        }
        let mut el = build::base(kind, &n.id, r, &st);
        if let Some(link) = n.args.str("link") {
            el["link"] = link.into();
        }
        draft.step(r.center(), Some(&n.id));
        draft.insert(el, Z::Front);
        draw::set_label(draft, &n.id, &n.label, &st, &Args(&super::args::EMPTY));
        // Layout sized the box; a label that still grew it is fine.
    }
    out.touch(&n.id);
    Ok(())
}

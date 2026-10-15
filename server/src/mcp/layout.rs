//! Automatic diagram layout: turns nodes (sizes only) and edges into boxes
//! and edge routes. Layered flowcharts (Sugiyama style), tidy trees, mind
//! maps, radial trees, grids and sequence diagrams. Pure and deterministic:
//! ties always follow input order.

use super::geometry::{boundary_point, polyline_midpoint, segment_hits_rect, side_point, union_all, Outline, Pt, Rect, Side};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Flow,
    Tree,
    Mindmap,
    Grid,
    Radial,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Down,
    Right,
    Up,
    Left,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeStyle {
    Straight,
    Elbow,
    Curved,
}

#[derive(Clone, Debug)]
pub struct LNode {
    pub id: String,
    pub w: f64,
    pub h: f64,
    pub outline: Outline,
    pub cluster: Option<String>,
}

#[derive(Clone, Debug)]
pub struct LEdge {
    pub from: String,
    pub to: String,
    /// Label box size if the edge has a label.
    pub label: Option<(f64, f64)>,
}

#[derive(Clone, Debug)]
pub struct LayoutInput {
    pub kind: Kind,
    pub dir: Dir,
    pub edge_style: EdgeStyle,
    pub nodes: Vec<LNode>,
    pub edges: Vec<LEdge>,
    /// Gap between neighbouring nodes in the same rank/row, and between ranks/levels.
    pub node_gap: f64,
    pub rank_gap: f64,
    /// Grid only: number of columns (None = roughly square).
    pub columns: Option<usize>,
    /// Space between a cluster's box and its nodes, and extra room at the top
    /// of a cluster box for its title.
    pub cluster_padding: f64,
    pub cluster_title: f64,
    /// Distance between an edge end and the node outline (arrow binding gap).
    pub edge_gap: f64,
}

#[derive(Clone, Debug)]
pub struct EdgeRoute {
    pub points: Vec<Pt>,
    pub start_side: Option<Side>,
    pub end_side: Option<Side>,
    pub style: EdgeStyle,
    /// Where to centre the label.
    pub label_at: Pt,
    /// True if this edge was reversed to break a cycle (points still go from→to).
    pub reversed: bool,
}

#[derive(Clone, Debug)]
pub struct LayoutOutput {
    /// Node boxes; the layout's top-left is at (0, 0).
    pub nodes: HashMap<String, Rect>,
    /// One per input edge, in order (empty points for unknown node ids).
    pub edges: Vec<EdgeRoute>,
    /// Cluster boxes (padding and title room included), in first-appearance order.
    pub clusters: Vec<(String, Rect)>,
    /// Trees, mind maps and radial trees: depth from the root (root = 0).
    /// Flow: the node's rank. Grid: empty.
    pub depth: HashMap<String, usize>,
    /// Index of the first-level branch a node belongs to (roots: None).
    pub branch: HashMap<String, Option<usize>>,
    pub bounds: Rect,
}

#[derive(Clone, Debug)]
pub struct SeqMessage {
    pub from: String,
    pub to: String,
    pub label: Option<(f64, f64)>,
}

#[derive(Clone, Debug)]
pub struct SeqInput {
    pub participants: Vec<LNode>,
    pub messages: Vec<SeqMessage>,
    pub participant_gap: f64,
    pub message_gap: f64,
    pub edge_gap: f64,
}

#[derive(Clone, Debug)]
pub struct SeqOutput {
    /// Header boxes (top row).
    pub participants: HashMap<String, Rect>,
    /// The same boxes repeated at the bottom.
    pub footers: HashMap<String, Rect>,
    /// Dashed line from each header's bottom to its footer's top.
    pub lifelines: HashMap<String, (Pt, Pt)>,
    /// Arrows top to bottom in message order; self-messages loop to the right.
    pub messages: Vec<EdgeRoute>,
    pub bounds: Rect,
}

/// Spacing between edge ends sharing one side of a node.
const PORT_SEP: f64 = 28.0;
/// Minimum spacing between parallel elbow segments in one gap.
const TRACK: f64 = 12.0;
/// Clearance between rows of elbow segments and labels, and the nodes.
const ROW_GAP: f64 = 10.0;
/// Clearance around edge labels.
const LABEL_MARGIN: f64 = 8.0;
/// How far a self-loop reaches out of its node.
const LOOP: f64 = 26.0;
/// Offset between parallel edges joining the same two nodes.
const MULTI_SEP: f64 = 28.0;
/// How far a sequence diagram's self-message loops out.
const SEQ_LOOP: f64 = 40.0;

pub fn layout(input: &LayoutInput) -> LayoutOutput {
    let g = Graph::new(input);
    let p = Params::new(input);
    let (frame, placed) = match input.kind {
        Kind::Flow => (Frame(input.dir), flow(&g, &p, Frame(input.dir))),
        Kind::Tree => (Frame(input.dir), tree(&g, &p, Frame(input.dir))),
        Kind::Mindmap => (Frame(Dir::Down), mindmap(&g, &p)),
        Kind::Radial => (Frame(Dir::Down), radial(&g, &p)),
        Kind::Grid => (Frame(Dir::Down), grid(input, &g, &p)),
    };
    finalize(&g, frame, placed)
}

// ---------- shared plumbing ----------

fn fin(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

/// `v` clamped to [lo, hi], or the middle when the range is empty.
fn within(v: f64, lo: f64, hi: f64) -> f64 {
    if lo > hi {
        (lo + hi) / 2.0
    } else {
        v.clamp(lo, hi)
    }
}

/// The input with duplicate node ids dropped and edges resolved to indices.
struct Graph<'a> {
    nodes: Vec<&'a LNode>,
    /// Endpoints of each input edge (None when an id is unknown).
    edges: Vec<Option<(usize, usize)>>,
    labels: Vec<Option<(f64, f64)>>,
    /// Cluster of each node, numbered by first appearance.
    cluster: Vec<Option<usize>>,
    clusters: Vec<String>,
}

impl<'a> Graph<'a> {
    fn new(input: &'a LayoutInput) -> Self {
        let mut index: HashMap<&str, usize> = HashMap::new();
        let mut nodes = Vec::new();
        for n in &input.nodes {
            if !index.contains_key(n.id.as_str()) {
                index.insert(&n.id, nodes.len());
                nodes.push(n);
            }
        }
        let edges = input.edges.iter().map(|e| Some((*index.get(e.from.as_str())?, *index.get(e.to.as_str())?))).collect();
        let labels = input.edges.iter().map(|e| e.label.map(|(w, h)| (fin(w).max(0.0), fin(h).max(0.0)))).collect();
        let mut clusters: Vec<String> = Vec::new();
        let cluster = nodes
            .iter()
            .map(|n| {
                let name = n.cluster.as_ref()?;
                Some(clusters.iter().position(|c| c == name).unwrap_or_else(|| {
                    clusters.push(name.clone());
                    clusters.len() - 1
                }))
            })
            .collect();
        Graph { nodes, edges, labels, cluster, clusters }
    }

    fn size(&self, v: usize) -> (f64, f64) {
        (fin(self.nodes[v].w).max(1.0), fin(self.nodes[v].h).max(1.0))
    }

    fn outline(&self, v: usize) -> Outline {
        self.nodes[v].outline
    }
}

struct Params {
    node_gap: f64,
    rank_gap: f64,
    pad: f64,
    title: f64,
    gap: f64,
    style: EdgeStyle,
}

impl Params {
    fn new(i: &LayoutInput) -> Self {
        Params {
            node_gap: fin(i.node_gap).max(0.0),
            rank_gap: fin(i.rank_gap).max(0.0),
            pad: fin(i.cluster_padding).max(0.0),
            title: fin(i.cluster_title).max(0.0),
            gap: fin(i.edge_gap).max(0.0),
            style: i.edge_style,
        }
    }
}

/// The canonical frame layered layouts are computed in: ranks grow along +y,
/// nodes within a rank along +x. `Frame` maps it to the requested direction.
#[derive(Clone, Copy)]
struct Frame(Dir);

/// Cluster padding on each side of the canonical frame.
#[derive(Clone, Copy)]
struct Pads {
    l: f64,
    r: f64,
    t: f64,
    b: f64,
}

impl Frame {
    /// A real (w, h) as (across ranks, along ranks).
    fn size(self, w: f64, h: f64) -> (f64, f64) {
        match self.0 {
            Dir::Down | Dir::Up => (w, h),
            Dir::Right | Dir::Left => (h, w),
        }
    }
    fn point(self, (x, y): Pt) -> Pt {
        match self.0 {
            Dir::Down => (x, y),
            Dir::Up => (x, -y),
            Dir::Right => (y, x),
            Dir::Left => (-y, x),
        }
    }
    fn rect(self, r: Rect) -> Rect {
        match self.0 {
            Dir::Down => r,
            Dir::Up => Rect::new(r.x, -r.bottom(), r.w, r.h),
            Dir::Right => Rect::new(r.y, r.x, r.h, r.w),
            Dir::Left => Rect::new(-r.bottom(), r.x, r.h, r.w),
        }
    }
    fn side(self, s: Side) -> Side {
        let (x, y) = self.point(s.normal());
        if x.abs() > y.abs() {
            if x > 0.0 { Side::Right } else { Side::Left }
        } else if y > 0.0 {
            Side::Bottom
        } else {
            Side::Top
        }
    }
    fn pads(self, pad: f64, title: f64) -> Pads {
        let p = |s| if self.side(s) == Side::Top { pad + title } else { pad };
        Pads { l: p(Side::Left), r: p(Side::Right), t: p(Side::Top), b: p(Side::Bottom) }
    }
}

#[derive(Clone, Debug, Default)]
struct Route {
    points: Vec<Pt>,
    start: Option<Side>,
    end: Option<Side>,
    label_at: Pt,
    reversed: bool,
}

/// A finished layout in some frame, before mapping and normalising.
struct Placed {
    nodes: Vec<Option<Rect>>,
    routes: Vec<Option<Route>>,
    clusters: Vec<(usize, Rect)>,
    depth: Vec<Option<usize>>,
    branch: Vec<Option<usize>>,
    style: EdgeStyle,
}

impl Placed {
    fn new(g: &Graph, style: EdgeStyle) -> Self {
        let n = g.nodes.len();
        Placed { nodes: vec![None; n], routes: vec![None; g.edges.len()], clusters: Vec::new(), depth: vec![None; n], branch: vec![None; n], style }
    }
}

fn finalize(g: &Graph, frame: Frame, placed: Placed) -> LayoutOutput {
    let nodes: Vec<Option<Rect>> = placed.nodes.iter().map(|r| r.map(|r| frame.rect(r))).collect();
    let routes: Vec<Option<Route>> = placed
        .routes
        .into_iter()
        .map(|r| {
            r.map(|r| Route {
                points: r.points.iter().map(|&p| frame.point(p)).collect(),
                start: r.start.map(|s| frame.side(s)),
                end: r.end.map(|s| frame.side(s)),
                label_at: frame.point(r.label_at),
                reversed: r.reversed,
            })
        })
        .collect();
    let mut clusters: Vec<(usize, Rect)> = placed.clusters.iter().map(|&(c, r)| (c, frame.rect(r))).collect();
    clusters.sort_by_key(|c| c.0);

    let mut rects: Vec<Rect> = nodes.iter().flatten().copied().chain(clusters.iter().map(|c| c.1)).collect();
    for (i, r) in routes.iter().enumerate() {
        let Some(r) = r else { continue };
        rects.extend(r.points.iter().map(|p| Rect::new(p.0, p.1, 0.0, 0.0)));
        if let (Some((w, h)), false) = (g.labels[i], r.points.is_empty()) {
            rects.push(Rect::new(r.label_at.0 - w / 2.0, r.label_at.1 - h / 2.0, w, h));
        }
    }
    let b = union_all(rects).unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
    let shift = |p: Pt| (p.0 - b.x, p.1 - b.y);
    let shift_rect = |r: Rect| Rect::new(r.x - b.x, r.y - b.y, r.w, r.h);

    let mut out = LayoutOutput {
        nodes: HashMap::new(),
        edges: Vec::with_capacity(routes.len()),
        clusters: clusters.iter().map(|&(c, r)| (g.clusters[c].clone(), shift_rect(r))).collect(),
        depth: HashMap::new(),
        branch: HashMap::new(),
        bounds: Rect::new(0.0, 0.0, b.w, b.h),
    };
    for (v, r) in nodes.iter().enumerate() {
        let id = &g.nodes[v].id;
        if let Some(r) = r {
            out.nodes.insert(id.clone(), shift_rect(*r));
        }
        if let Some(d) = placed.depth[v] {
            out.depth.insert(id.clone(), d);
            out.branch.insert(id.clone(), placed.branch[v]);
        }
    }
    for r in routes {
        let r = r.unwrap_or_default();
        let empty = r.points.is_empty();
        out.edges.push(EdgeRoute {
            points: r.points.iter().map(|&p| shift(p)).collect(),
            start_side: r.start,
            end_side: r.end,
            style: placed.style,
            label_at: if empty { (0.0, 0.0) } else { shift(r.label_at) },
            reversed: r.reversed,
        });
    }
    out
}

/// Where the ray from `from` (inside the shape) along `dir` leaves the
/// outline, pushed `gap` further along the ray.
fn ray_exit(r: &Rect, outline: Outline, from: Pt, dir: Pt, gap: f64) -> Pt {
    let len = (dir.0 * dir.0 + dir.1 * dir.1).sqrt();
    if len < 1e-12 {
        return from;
    }
    let (ux, uy) = (dir.0 / len, dir.1 / len);
    let (cx, cy) = r.center();
    let (a, b) = ((r.w / 2.0).max(1e-9), (r.h / 2.0).max(1e-9));
    let inside = |t: f64| {
        let (dx, dy) = (((from.0 + ux * t) - cx) / a, ((from.1 + uy * t) - cy) / b);
        match outline {
            Outline::Rect => dx.abs() <= 1.0 && dy.abs() <= 1.0,
            Outline::Ellipse => dx * dx + dy * dy <= 1.0,
            Outline::Diamond => dx.abs() + dy.abs() <= 1.0,
        }
    };
    let mut t = 0.0;
    if inside(0.0) {
        let (mut lo, mut hi) = (0.0, 2.0 * (a + b) + (from.0 - cx).abs() + (from.1 - cy).abs());
        for _ in 0..60 {
            let mid = (lo + hi) / 2.0;
            if inside(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        t = lo;
    }
    (from.0 + ux * (t + gap), from.1 + uy * (t + gap))
}

/// The side of `r` a point on (or near) its outline belongs to.
fn side_at(r: &Rect, p: Pt) -> Side {
    let (cx, cy) = r.center();
    let dx = (p.0 - cx) / (r.w / 2.0).max(1e-9);
    let dy = (p.1 - cy) / (r.h / 2.0).max(1e-9);
    if dx.abs() >= dy.abs() {
        if dx >= 0.0 { Side::Right } else { Side::Left }
    } else if dy >= 0.0 {
        Side::Bottom
    } else {
        Side::Top
    }
}

/// A good spot for a label on a route: the middle segment of an elbow
/// route, else the middle of the line.
fn label_point(pts: &[Pt]) -> Pt {
    let segs = pts.len().saturating_sub(1);
    if segs < 2 {
        return polyline_midpoint(pts);
    }
    let len = |i: usize| super::geometry::dist(pts[i], pts[i + 1]);
    let i = if segs % 2 == 1 {
        segs / 2
    } else if len(segs / 2 - 1) >= len(segs / 2) {
        segs / 2 - 1
    } else {
        segs / 2
    };
    ((pts[i].0 + pts[i + 1].0) / 2.0, (pts[i].1 + pts[i + 1].1) / 2.0)
}

/// Drops repeated and collinear points of an axis-aligned polyline.
fn simplify(pts: Vec<Pt>) -> Vec<Pt> {
    let mut out: Vec<Pt> = Vec::with_capacity(pts.len());
    for p in pts {
        if let Some(&last) = out.last() {
            if (last.0 - p.0).abs() < 1e-6 && (last.1 - p.1).abs() < 1e-6 {
                continue;
            }
        }
        if out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let same_x = (a.0 - b.0).abs() < 1e-6 && (b.0 - p.0).abs() < 1e-6;
            let same_y = (a.1 - b.1).abs() < 1e-6 && (b.1 - p.1).abs() < 1e-6;
            if same_x || same_y {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

/// A smooth S-shaped curve from `s` to `e` that leaves and arrives along the
/// main axis, as points for Catmull-Rom smoothing.
fn s_curve(s: Pt, e: Pt, horizontal: bool) -> Vec<Pt> {
    let off = if horizontal { (e.1 - s.1).abs() } else { (e.0 - s.0).abs() };
    if off < 1.0 {
        return vec![s, e];
    }
    let (c1, c2) = if horizontal {
        let mx = (s.0 + e.0) / 2.0;
        ((mx, s.1), (mx, e.1))
    } else {
        let my = (s.1 + e.1) / 2.0;
        ((s.0, my), (e.0, my))
    };
    [0.0, 0.25, 0.5, 0.75, 1.0]
        .iter()
        .map(|&t: &f64| {
            let u = 1.0 - t;
            let f = |a: f64, b: f64, c: f64, d: f64| u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d;
            (f(s.0, c1.0, c2.0, e.0), f(s.1, c1.1, c2.1, e.1))
        })
        .collect()
}

/// How far right of its node a set of self-loops reaches.
fn loop_room(gap: f64, count: usize, label_w: f64) -> f64 {
    if count == 0 {
        return 0.0;
    }
    gap + LOOP + 12.0 * (count - 1) as f64 + if label_w > 0.0 { 10.0 + label_w } else { 0.0 } + 6.0
}

/// A small loop out of the right side of a node and back.
fn self_loop(r: &Rect, o: Outline, style: EdgeStyle, gap: f64, nth: usize, label: Option<(f64, f64)>) -> Route {
    let (cx, cy) = r.center();
    let (y1, y2) = (r.y + r.h * 0.3, r.y + r.h * 0.7);
    let p0 = ray_exit(r, o, (cx, y1), (1.0, 0.0), gap);
    let p3 = ray_exit(r, o, (cx, y2), (1.0, 0.0), gap);
    let xr = r.right() + gap + LOOP + 12.0 * nth as f64;
    let points = if style == EdgeStyle::Curved { vec![p0, (xr, cy), p3] } else { vec![p0, (xr, y1), (xr, y2), p3] };
    let lw = label.map_or(0.0, |l| l.0);
    Route { points, start: Some(Side::Right), end: Some(Side::Right), label_at: (xr + 6.0 + lw / 2.0, cy), reversed: false }
}

/// Offsets for edges joining the same pair of nodes, so they run side by side.
fn multi_offsets(g: &Graph, reserved: impl Fn(usize) -> bool) -> Vec<f64> {
    let mut groups: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for (e, ends) in g.edges.iter().enumerate() {
        if let Some((a, b)) = *ends {
            if a != b {
                groups.entry((a.min(b), a.max(b))).or_default().push(e);
            }
        }
    }
    let mut off = vec![0.0; g.edges.len()];
    for list in groups.values() {
        if list.iter().any(|&e| reserved(e)) {
            // A reserved edge keeps the centre line; the rest alternate around it.
            let mut j = 0;
            for &e in list.iter().filter(|&&e| !reserved(e)) {
                j += 1;
                let sign = if j % 2 == 1 { 1.0 } else { -1.0 };
                off[e] = sign * ((j + 1) / 2) as f64 * MULTI_SEP;
            }
        } else {
            let k = list.len() as f64;
            for (j, &e) in list.iter().enumerate() {
                off[e] = (j as f64 - (k - 1.0) / 2.0) * MULTI_SEP;
            }
        }
    }
    off
}

// ---------- separation constraints ----------

/// Constraints `x[r] - x[l] >= gap`, solved for positions as close as
/// possible (weighted least squares) to desired ones: the "satisfy" pass of
/// Dwyer, Marriott and Stuckey's VPSC, which is exact for chains (PAV).
struct Separation {
    cons: Vec<(usize, usize, f64)>,
    order: Vec<usize>,
    ins: Vec<Vec<usize>>,
}

impl Separation {
    fn new(n: usize, cons: Vec<(usize, usize, f64)>) -> Self {
        let mut ins = vec![Vec::new(); n];
        let mut outs = vec![Vec::new(); n];
        let mut indeg = vec![0usize; n];
        for (i, &(l, r, _)) in cons.iter().enumerate() {
            ins[r].push(i);
            outs[l].push(r);
            indeg[r] += 1;
        }
        let mut heap: BinaryHeap<Reverse<usize>> = (0..n).filter(|&v| indeg[v] == 0).map(Reverse).collect();
        let mut order = Vec::with_capacity(n);
        let mut done = vec![false; n];
        while let Some(Reverse(v)) = heap.pop() {
            order.push(v);
            done[v] = true;
            for &r in &outs[v] {
                indeg[r] -= 1;
                if indeg[r] == 0 {
                    heap.push(Reverse(r));
                }
            }
        }
        // Cycles cannot happen by construction; keep any leftovers anyway.
        order.extend((0..n).filter(|&v| !done[v]));
        Separation { cons, order, ins }
    }

    fn solve(&self, desired: &[f64], weight: &[f64]) -> Vec<f64> {
        let n = desired.len();
        let mut block: Vec<usize> = (0..n).collect();
        let mut offset = vec![0.0; n];
        // Block members as linked lists (first, next, last, count), and each
        // block's incoming constraints; ones that became internal are dropped
        // when seen.
        const END: usize = usize::MAX;
        let head: Vec<usize> = (0..n).collect();
        let mut next = vec![END; n];
        let mut tail: Vec<usize> = (0..n).collect();
        let mut count = vec![1usize; n];
        let mut ins: Vec<Vec<usize>> = self.ins.clone();
        let mut wsum: Vec<f64> = weight.iter().map(|w| w.max(1e-6)).collect();
        let mut wd: Vec<f64> = (0..n).map(|v| wsum[v] * desired[v]).collect();
        for &v in &self.order {
            loop {
                let b = block[v];
                let pos = |u: usize| wd[block[u]] / wsum[block[u]] + offset[u];
                let mut best = None;
                let mut worst = 1e-7;
                ins[b].retain(|&ci| block[self.cons[ci].0] != b);
                for &ci in &ins[b] {
                    let (l, r, g) = self.cons[ci];
                    let viol = pos(l) + g - pos(r);
                    if viol > worst {
                        worst = viol;
                        best = Some(ci);
                    }
                }
                let Some(ci) = best else { break };
                let (l, r, g) = self.cons[ci];
                let bl = block[l];
                let d = offset[l] + g - offset[r];
                let (keep, gone, shift) = if count[bl] >= count[b] { (bl, b, d) } else { (b, bl, -d) };
                let mut u = head[gone];
                while u != END {
                    offset[u] += shift;
                    block[u] = keep;
                    u = next[u];
                }
                next[tail[keep]] = head[gone];
                tail[keep] = tail[gone];
                count[keep] += count[gone];
                wd[keep] += wd[gone] - shift * wsum[gone];
                wsum[keep] += wsum[gone];
                let moved = std::mem::take(&mut ins[gone]);
                ins[keep].extend(moved);
            }
        }
        let mut x: Vec<f64> = (0..n).map(|u| wd[block[u]] / wsum[block[u]] + offset[u]).collect();
        // Make every constraint hold exactly, whatever rounding left behind.
        for &v in &self.order {
            for &ci in &self.ins[v] {
                let (l, _, g) = self.cons[ci];
                if x[l] + g > x[v] {
                    x[v] = x[l] + g;
                }
            }
        }
        x
    }
}

/// Least-squares positions near `want` (sorted order kept), at least `sep`
/// apart (less if they don't fit) and within [lo, hi].
fn spread(want: &[f64], lo: f64, hi: f64, sep: f64) -> Vec<f64> {
    let n = want.len();
    if n == 0 {
        return Vec::new();
    }
    let sep = if n > 1 { sep.min((hi - lo).max(0.0) / (n - 1) as f64) } else { sep };
    // Isotonic regression on want[i] - i*sep (pool adjacent violators).
    let mut blocks: Vec<(f64, usize)> = Vec::new();
    for (i, &w) in want.iter().enumerate() {
        blocks.push((w - i as f64 * sep, 1));
        while blocks.len() >= 2 {
            let (s2, c2) = blocks[blocks.len() - 1];
            let (s1, c1) = blocks[blocks.len() - 2];
            if s1 / c1 as f64 > s2 / c2 as f64 {
                blocks.pop();
                *blocks.last_mut().unwrap() = (s1 + s2, c1 + c2);
            } else {
                break;
            }
        }
    }
    let mut out = Vec::with_capacity(n);
    for (s, c) in blocks {
        out.extend(std::iter::repeat_n(s / c as f64, c));
    }
    for (i, x) in out.iter_mut().enumerate() {
        let i = i as f64;
        let (lo_i, hi_i) = (lo + i * sep, hi - (n as f64 - 1.0 - i) * sep);
        *x = (*x + i * sep).max(lo_i).min(hi_i.max(lo_i));
    }
    out
}

/// Groups of nodes that are laid out together: connected by edges or
/// sharing a cluster. Largest first, ties by first node.
fn components(n: usize, edges: &[Option<(usize, usize)>], cluster: &[Option<usize>]) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut v: usize) -> usize {
        while p[v] != v {
            p[v] = p[p[v]];
            v = p[v];
        }
        v
    }
    let join = |p: &mut Vec<usize>, a: usize, b: usize| {
        let (ra, rb) = (find(p, a), find(p, b));
        if ra != rb {
            p[ra.max(rb)] = ra.min(rb);
        }
    };
    for &(a, b) in edges.iter().flatten() {
        join(&mut parent, a, b);
    }
    let mut first: HashMap<usize, usize> = HashMap::new();
    for v in 0..n {
        if let Some(c) = cluster[v] {
            match first.get(&c) {
                Some(&u) => join(&mut parent, u, v),
                None => {
                    first.insert(c, v);
                }
            }
        }
    }
    let mut index: HashMap<usize, usize> = HashMap::new();
    let mut comps: Vec<Vec<usize>> = Vec::new();
    for v in 0..n {
        let r = find(&mut parent, v);
        let i = *index.entry(r).or_insert_with(|| {
            comps.push(Vec::new());
            comps.len() - 1
        });
        comps[i].push(v);
    }
    comps.sort_by_key(|c| Reverse(c.len()));
    comps
}

// ---------- layered flow ----------

/// A node of the layered graph: a real node, a dummy carrying a long edge
/// through a rank, or a placeholder holding a cluster's place in a rank.
struct Lnode {
    real: bool,
    w: f64,
    h: f64,
    /// Room needed left and right of the centre.
    lext: f64,
    rext: f64,
    cluster: Option<usize>,
    rank: usize,
}

struct FlowCtx<'a> {
    g: &'a Graph<'a>,
    p: &'a Params,
    frame: Frame,
    /// Canonical (across, along) sizes.
    size: Vec<(f64, f64)>,
    /// Extra room on the right for self-loops.
    extra: Vec<f64>,
    loops: Vec<Vec<usize>>,
    pads: Pads,
}

impl FlowCtx<'_> {
    fn label(&self, e: usize) -> Option<(f64, f64)> {
        self.g.labels[e].map(|(w, h)| self.frame.size(w, h))
    }
}

/// One laid-out component, in its own coordinates.
struct Part {
    nodes: Vec<(usize, Rect)>,
    routes: Vec<(usize, Route)>,
    clusters: Vec<(usize, Rect)>,
    depth: Vec<(usize, usize)>,
    bbox: Rect,
}

fn flow(g: &Graph, p: &Params, frame: Frame) -> Placed {
    let n = g.nodes.len();
    let mut placed = Placed::new(g, p.style);
    let mut loops = vec![Vec::new(); n];
    for (e, ends) in g.edges.iter().enumerate() {
        if let Some((a, b)) = *ends {
            if a == b {
                loops[a].push(e);
            }
        }
    }
    let ctx = FlowCtx {
        g,
        p,
        frame,
        size: (0..n).map(|v| { let (w, h) = g.size(v); frame.size(w, h) }).collect(),
        extra: loops
            .iter()
            .map(|ls| {
                let lw = ls.iter().filter_map(|&e| g.labels[e]).map(|(w, h)| frame.size(w, h).0).fold(0.0, f64::max);
                loop_room(p.gap, ls.len(), lw)
            })
            .collect(),
        loops,
        pads: frame.pads(p.pad, p.title),
    };
    let links: Vec<Option<(usize, usize)>> = g.edges.iter().map(|e| e.filter(|(a, b)| a != b)).collect();
    let comps = components(n, &links, &g.cluster);
    let mut comp_of = vec![0; n];
    for (i, c) in comps.iter().enumerate() {
        for &v in c {
            comp_of[v] = i;
        }
    }
    let mut comp_edges = vec![Vec::new(); comps.len()];
    for (e, l) in links.iter().enumerate() {
        if let Some((a, _)) = l {
            comp_edges[comp_of[*a]].push(e);
        }
    }
    let comp_gap = (p.node_gap * 1.5).max(30.0);
    let mut cursor = 0.0;
    for (c, nodes) in comps.iter().enumerate() {
        let part = flow_part(&ctx, nodes, &comp_edges[c]);
        let (dx, dy) = (cursor - part.bbox.x, -part.bbox.y);
        cursor += part.bbox.w + comp_gap;
        let mv = |r: Rect| Rect::new(r.x + dx, r.y + dy, r.w, r.h);
        for (v, r) in part.nodes {
            placed.nodes[v] = Some(mv(r));
        }
        for (e, mut r) in part.routes {
            r.points.iter_mut().for_each(|p| *p = (p.0 + dx, p.1 + dy));
            r.label_at = (r.label_at.0 + dx, r.label_at.1 + dy);
            placed.routes[e] = Some(r);
        }
        placed.clusters.extend(part.clusters.into_iter().map(|(c, r)| (c, mv(r))));
        for (v, d) in part.depth {
            placed.depth[v] = Some(d);
        }
    }
    placed
}

/// Reverses DFS back edges so the graph becomes acyclic. The search starts
/// from nodes in input order, so the first listed node of a cycle stays on top.
fn break_cycles(m: usize, es: &[(usize, usize)]) -> Vec<bool> {
    let mut out: Vec<Vec<(usize, usize)>> = vec![Vec::new(); m];
    for (k, &(a, b)) in es.iter().enumerate() {
        out[a].push((k, b));
    }
    let mut state = vec![0u8; m];
    let mut rev = vec![false; es.len()];
    for s in 0..m {
        if state[s] != 0 {
            continue;
        }
        state[s] = 1;
        let mut stack = vec![(s, 0usize)];
        while let Some(top) = stack.last_mut() {
            let (u, i) = *top;
            if i < out[u].len() {
                top.1 += 1;
                let (k, v) = out[u][i];
                match state[v] {
                    0 => {
                        state[v] = 1;
                        stack.push((v, 0));
                    }
                    1 => rev[k] = true,
                    _ => {}
                }
            } else {
                state[u] = 2;
                stack.pop();
            }
        }
    }
    rev
}

/// Longest-path ranks, then nodes with more outgoing than incoming edges are
/// pulled down next to their successors (shorter edges overall). Ranks no
/// node or edge uses are dropped.
fn assign_ranks(m: usize, dag: &[(usize, usize)]) -> Vec<usize> {
    let mut succ = vec![Vec::new(); m];
    let mut npred = vec![0usize; m];
    let mut indeg = vec![0usize; m];
    for &(a, b) in dag {
        succ[a].push(b);
        npred[b] += 1;
        indeg[b] += 1;
    }
    let mut heap: BinaryHeap<Reverse<usize>> = (0..m).filter(|&v| indeg[v] == 0).map(Reverse).collect();
    let mut order = Vec::with_capacity(m);
    while let Some(Reverse(v)) = heap.pop() {
        order.push(v);
        for &s in &succ[v] {
            indeg[s] -= 1;
            if indeg[s] == 0 {
                heap.push(Reverse(s));
            }
        }
    }
    let mut rank = vec![0usize; m];
    for &v in &order {
        for &s in &succ[v] {
            rank[s] = rank[s].max(rank[v] + 1);
        }
    }
    for _ in 0..m.min(16) {
        let mut changed = false;
        for &v in order.iter().rev() {
            if succ[v].len() > npred[v] {
                let hi = succ[v].iter().map(|&s| rank[s]).min().unwrap() - 1;
                if hi > rank[v] {
                    rank[v] = hi;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let nr = rank.iter().max().map_or(0, |r| r + 1);
    let mut used = vec![false; nr];
    for &r in &rank {
        used[r] = true;
    }
    for &(a, b) in dag {
        for u in used.iter_mut().take(rank[b]).skip(rank[a] + 1) {
            *u = true;
        }
    }
    let mut map = vec![0; nr];
    let mut next = 0;
    for r in 0..nr {
        map[r] = next;
        if used[r] {
            next += 1;
        }
    }
    rank.iter().map(|&r| map[r]).collect()
}

fn reindex(layers: &[Vec<usize>], pos: &mut [usize]) {
    for l in layers {
        for (i, &v) in l.iter().enumerate() {
            pos[v] = i;
        }
    }
}

/// Order of clusters across all ranks, by their mean relative position.
fn cluster_order(layers: &[Vec<usize>], pos: &[usize], cl: &[Option<usize>], ncl: usize) -> Vec<usize> {
    let mut sum = vec![(0.0, 0usize); ncl];
    for l in layers {
        let len = l.len() as f64;
        for &v in l {
            if let Some(c) = cl[v] {
                sum[c].0 += (pos[v] as f64 + 0.5) / len;
                sum[c].1 += 1;
            }
        }
    }
    let mean = |c: usize| sum[c].0 / sum[c].1.max(1) as f64;
    let mut order: Vec<usize> = (0..ncl).collect();
    order.sort_by(|&a, &b| mean(a).total_cmp(&mean(b)).then(a.cmp(&b)));
    let mut cpos = vec![0; ncl];
    for (i, &c) in order.iter().enumerate() {
        cpos[c] = i;
    }
    cpos
}

/// Sorts a rank by `key`, keeping each cluster's nodes together and
/// clusters in their global order.
fn arrange(layer: &[usize], key: &[f64], cl: &[Option<usize>], cpos: &[usize]) -> Vec<usize> {
    struct Group {
        key: f64,
        first: usize,
        cluster: Option<usize>,
        nodes: Vec<usize>,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut of_cluster: HashMap<usize, usize> = HashMap::new();
    for (i, &v) in layer.iter().enumerate() {
        match cl[v] {
            Some(c) => match of_cluster.get(&c) {
                Some(&gi) => groups[gi].nodes.push(v),
                None => {
                    of_cluster.insert(c, groups.len());
                    groups.push(Group { key: 0.0, first: i, cluster: Some(c), nodes: vec![v] });
                }
            },
            None => groups.push(Group { key: 0.0, first: i, cluster: None, nodes: vec![v] }),
        }
    }
    for gr in &mut groups {
        gr.key = gr.nodes.iter().map(|&v| key[v]).sum::<f64>() / gr.nodes.len() as f64;
        gr.nodes.sort_by(|&a, &b| key[a].total_cmp(&key[b]));
    }
    groups.sort_by(|a, b| a.key.total_cmp(&b.key).then(a.first.cmp(&b.first)));
    let slots: Vec<usize> = (0..groups.len()).filter(|&i| groups[i].cluster.is_some()).collect();
    let mut by_order: Vec<usize> = slots.clone();
    by_order.sort_by_key(|&i| cpos[groups[i].cluster.unwrap()]);
    let mut out = Vec::with_capacity(layer.len());
    let mut next_slot = 0;
    for i in 0..groups.len() {
        let gi = if groups[i].cluster.is_some() {
            next_slot += 1;
            by_order[next_slot - 1]
        } else {
            i
        };
        out.extend_from_slice(&groups[gi].nodes);
    }
    out
}

fn count_crossings(layers: &[Vec<usize>], down: &[Vec<(usize, f64)>], pos: &[usize]) -> usize {
    let mut total = 0;
    for r in 0..layers.len().saturating_sub(1) {
        let mut pairs: Vec<(usize, usize)> = layers[r].iter().flat_map(|&u| down[u].iter().map(move |&(v, _)| (u, v))).map(|(u, v)| (pos[u], pos[v])).collect();
        pairs.sort_unstable();
        let len = layers[r + 1].len();
        let mut tree = vec![0usize; len + 1];
        for (i, &(_, b)) in pairs.iter().enumerate() {
            let mut le = 0;
            let mut j = b + 1;
            while j > 0 {
                le += tree[j];
                j &= j - 1;
            }
            total += i - le;
            let mut j = b + 1;
            while j <= len {
                tree[j] += 1;
                j += j & j.wrapping_neg();
            }
        }
    }
    total
}

/// Crossings among the edges of `u` and `v` when `u` is left of `v`.
fn pair_crossings(u: usize, v: usize, up: &[Vec<(usize, f64)>], down: &[Vec<(usize, f64)>], pos: &[usize]) -> usize {
    let mut c = 0;
    for adj in [up, down] {
        for &(a, _) in &adj[u] {
            c += adj[v].iter().filter(|&&(b, _)| pos[a] > pos[b]).count();
        }
    }
    c
}

/// Orders every rank to reduce crossings: barycenter sweeps up and down,
/// adjacent transpositions, clusters kept contiguous and in one global
/// order (so cluster boxes can be laid out without overlap).
fn order_layers(ln: &[Lnode], up: &[Vec<(usize, f64)>], down: &[Vec<(usize, f64)>], cl: &[Option<usize>], ncl: usize, nr: usize, m: usize) -> Vec<Vec<usize>> {
    let nl = ln.len();
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); nr];
    let mut seen = vec![false; nl];
    for s in (0..m).filter(|&v| up[v].is_empty()).chain(0..nl) {
        if seen[s] {
            continue;
        }
        seen[s] = true;
        layers[ln[s].rank].push(s);
        let mut stack = vec![(s, 0usize)];
        while let Some(top) = stack.last_mut() {
            let (u, i) = *top;
            if i < down[u].len() {
                top.1 += 1;
                let v = down[u][i].0;
                if !seen[v] {
                    seen[v] = true;
                    layers[ln[v].rank].push(v);
                    stack.push((v, 0));
                }
            } else {
                stack.pop();
            }
        }
    }
    let mut pos = vec![0usize; nl];
    let mut key = vec![0.0; nl];
    let regroup = |layers: &mut Vec<Vec<usize>>, pos: &mut Vec<usize>, key: &mut Vec<f64>, cpos: &[usize]| {
        for l in layers.iter_mut() {
            for &v in l.iter() {
                key[v] = pos[v] as f64;
            }
            *l = arrange(l, key, cl, cpos);
        }
        reindex(layers, pos);
    };
    reindex(&layers, &mut pos);
    let mut cpos = cluster_order(&layers, &pos, cl, ncl);
    regroup(&mut layers, &mut pos, &mut key, &cpos);

    let mut best = layers.clone();
    let mut best_c = count_crossings(&layers, down, &pos);
    let mut stale = 0;
    for it in 0..24 {
        if best_c == 0 {
            break;
        }
        let downward = it % 2 == 0;
        let ranks: Vec<usize> = if downward { (1..nr).collect() } else { (0..nr.saturating_sub(1)).rev().collect() };
        for r in ranks {
            for &v in &layers[r] {
                let adj = if downward { &up[v] } else { &down[v] };
                let tw: f64 = adj.iter().map(|a| a.1).sum();
                key[v] = if adj.is_empty() { pos[v] as f64 } else { adj.iter().map(|&(u, w)| w * pos[u] as f64).sum::<f64>() / tw };
            }
            layers[r] = arrange(&layers[r], &key, cl, &cpos);
            for (i, &v) in layers[r].iter().enumerate() {
                pos[v] = i;
            }
        }
        for _ in 0..8 {
            let mut improved = false;
            for l in layers.iter_mut() {
                for i in 0..l.len().saturating_sub(1) {
                    let (u, v) = (l[i], l[i + 1]);
                    if cl[u] == cl[v] && pair_crossings(v, u, up, down, &pos) < pair_crossings(u, v, up, down, &pos) {
                        l.swap(i, i + 1);
                        pos[u] = i + 1;
                        pos[v] = i;
                        improved = true;
                    }
                }
            }
            if !improved {
                break;
            }
        }
        cpos = cluster_order(&layers, &pos, cl, ncl);
        regroup(&mut layers, &mut pos, &mut key, &cpos);
        let c = count_crossings(&layers, down, &pos);
        if c < best_c {
            best = layers.clone();
            best_c = c;
            stale = 0;
        } else {
            stale += 1;
            if stale >= 6 {
                break;
            }
        }
    }
    best
}

fn weighted_median(adj: &[(usize, f64)], x: &[f64]) -> Option<(f64, f64)> {
    if adj.is_empty() {
        return None;
    }
    let mut v: Vec<(f64, f64)> = adj.iter().map(|&(u, w)| (x[u], w)).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: f64 = v.iter().map(|a| a.1).sum();
    let mut acc = 0.0;
    for i in 0..v.len() {
        acc += v[i].1;
        if (acc - total / 2.0).abs() < 1e-9 && i + 1 < v.len() {
            return Some(((v[i].0 + v[i + 1].0) / 2.0, total));
        }
        if acc > total / 2.0 {
            return Some((v[i].0, total));
        }
    }
    v.last().map(|l| (l.0, total))
}

/// Positions across ranks: alternating sweeps pull each node to the
/// weighted median of its neighbours above or below, then the separation
/// solver keeps nodes, edge lanes and cluster boxes apart. Ends with an
/// upward sweep so parents sit centred over their children.
#[allow(clippy::too_many_arguments)]
fn assign_x(ln: &[Lnode], layers: &[Vec<usize>], up: &[Vec<(usize, f64)>], down: &[Vec<(usize, f64)>], cl: &[Option<usize>], ncl: usize, p: &Params, pads: &Pads) -> Vec<f64> {
    let nl = ln.len();
    let nv = nl + 2 * ncl;
    let (lvar, rvar) = (|c: usize| nl + 2 * c, |c: usize| nl + 2 * c + 1);
    let edge_sep = (p.node_gap * 0.4).clamp(8.0, 24.0);
    let solid = |v: usize| ln[v].real || ln[v].w > 0.0;
    let mut cons = Vec::new();
    for l in layers {
        for w in l.windows(2) {
            let (a, b) = (w[0], w[1]);
            if cl[a] == cl[b] {
                let sep = match (solid(a), solid(b)) {
                    (true, true) => p.node_gap,
                    (false, false) => edge_sep,
                    _ => (p.node_gap * 0.5).max(edge_sep),
                };
                cons.push((a, b, ln[a].rext + ln[b].lext + sep));
            } else {
                let (l, lo) = match cl[a] {
                    Some(c) => (rvar(c), 0.0),
                    None => (a, ln[a].rext),
                };
                let (r, ro) = match cl[b] {
                    Some(c) => (lvar(c), 0.0),
                    None => (b, ln[b].lext),
                };
                cons.push((l, r, lo + ro + p.node_gap));
            }
        }
    }
    for v in 0..nl {
        if let Some(c) = cl[v] {
            cons.push((lvar(c), v, pads.l + ln[v].lext));
            cons.push((v, rvar(c), ln[v].rext + pads.r));
        }
    }
    let solver = Separation::new(nv, cons);
    let mut x = vec![0.0; nv];
    for l in layers {
        let mut cur = 0.0;
        for (i, &v) in l.iter().enumerate() {
            if i > 0 {
                cur += ln[l[i - 1]].rext + ln[v].lext + p.node_gap;
            }
            x[v] = cur;
        }
        for &v in l {
            x[v] -= cur / 2.0;
        }
    }
    let mut base = vec![1.0; nv];
    for w in base.iter_mut().skip(nl) {
        *w = 1e-3;
    }
    x = solver.solve(&x, &base);
    let nr = layers.len();
    // Sweeps down (towards the neighbours above) and up (towards those
    // below), then two that balance both sides.
    let plan = (0..8).map(|p| if p % 2 == 0 { (true, Pull::Above) } else { (false, Pull::Below) }).chain([(true, Pull::Both), (false, Pull::Both)]);
    for (downward, pull) in plan {
        let ranks: Vec<usize> = if downward { (0..nr).collect() } else { (0..nr).rev().collect() };
        for r in ranks {
            let mut d = x.clone();
            let mut wt = base.clone();
            let mut moved = false;
            for &v in &layers[r] {
                let both: Vec<(usize, f64)>;
                let adj: &[(usize, f64)] = match pull {
                    Pull::Above => &up[v],
                    Pull::Below => &down[v],
                    Pull::Both => {
                        both = up[v].iter().chain(&down[v]).copied().collect();
                        &both
                    }
                };
                if let Some((m, tw)) = weighted_median(adj, &x) {
                    d[v] = m;
                    wt[v] = tw;
                    moved = true;
                }
            }
            if moved {
                x = solver.solve(&d, &wt);
            }
        }
    }
    // Line each long edge's dummies up on one x (one of theirs, or an end
    // node's) where that breaks no constraint, so the lane runs straight.
    let mut touching: Vec<Vec<usize>> = vec![Vec::new(); nv];
    for (i, &(l, r, _)) in solver.cons.iter().enumerate() {
        touching[l].push(i);
        touching[r].push(i);
    }
    let fits = |x: &[f64], v: usize, at: f64| {
        touching[v].iter().all(|&i| {
            let (l, r, g) = solver.cons[i];
            let pos = |u: usize| if u == v { at } else { x[u] };
            pos(r) - pos(l) >= g - 1e-6
        })
    };
    for v in 0..nl {
        if ln[v].real || up[v].len() != 1 || !ln[up[v][0].0].real {
            continue;
        }
        let mut run = vec![v];
        while let [(next, _)] = down[*run.last().unwrap()][..] {
            if ln[next].real {
                run.push(next);
                break;
            }
            run.push(next);
        }
        let ends = [up[v][0].0, *run.last().unwrap()];
        let dummies: Vec<usize> = run.iter().copied().filter(|&d| !ln[d].real).collect();
        let mut cands: Vec<f64> = dummies.iter().chain(ends.iter().filter(|&&e| ln[e].real)).map(|&d| x[d]).collect();
        let cost = |c: f64| dummies.iter().map(|&d| (x[d] - c).abs()).sum::<f64>();
        cands.sort_by(|a, b| cost(*a).total_cmp(&cost(*b)));
        if let Some(c) = cands.into_iter().find(|&c| dummies.iter().all(|&d| fits(&x, d, c))) {
            for &d in &dummies {
                x[d] = c;
            }
        }
    }
    x
}

#[derive(Clone, Copy)]
enum Pull {
    Above,
    Below,
    Both,
}

/// Something that needs a row in the gap between two ranks: an elbow
/// jog from `from` to `to`, or the label of a short edge (from == to).
#[derive(Clone)]
struct Track {
    k: usize,
    jog: Option<usize>,
    from: f64,
    to: f64,
    /// Horizontal extent, label included.
    lo: f64,
    hi: f64,
    h: f64,
    row: usize,
}

/// Gives every item a row so that overlapping items never share one, and
/// jogs heading the same way are stacked so they don't cross.
fn assign_tracks(items: &mut [Track]) {
    // 0: rightward, 1: leftward, 2: labels.
    let kind = |t: &Track| if t.to > t.from { 0 } else if t.to < t.from { 1 } else { 2 };
    let key = |t: &Track| if kind(t) == 0 { -t.from } else { t.from };
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| kind(&items[a]).cmp(&kind(&items[b])).then(key(&items[a]).total_cmp(&key(&items[b]))).then(a.cmp(&b)));
    let overlap = |a: &Track, b: &Track| a.lo < b.hi + 6.0 && b.lo < a.hi + 6.0;
    let mut done: Vec<usize> = Vec::new();
    for &s in &order {
        let it = &items[s];
        let mut t = done.iter().filter(|&&d| kind(it) < 2 && kind(&items[d]) == kind(it) && overlap(&items[d], it)).map(|&d| items[d].row + 1).max().unwrap_or(0);
        while done.iter().any(|&d| items[d].row == t && overlap(&items[d], it)) {
            t += 1;
        }
        items[s].row = t;
        done.push(s);
    }
}

fn flow_part(ctx: &FlowCtx, nodes: &[usize], edges: &[usize]) -> Part {
    let (g, p, pads) = (ctx.g, ctx.p, &ctx.pads);
    let elbow = p.style == EdgeStyle::Elbow;
    let m = nodes.len();
    let local: HashMap<usize, usize> = nodes.iter().enumerate().map(|(i, &v)| (v, i)).collect();
    let es: Vec<(usize, usize)> = edges
        .iter()
        .map(|&e| {
            let (a, b) = g.edges[e].unwrap();
            (local[&a], local[&b])
        })
        .collect();
    let reversed = break_cycles(m, &es);
    let dag: Vec<(usize, usize)> = es.iter().zip(&reversed).map(|(&(a, b), &r)| if r { (b, a) } else { (a, b) }).collect();
    let rank = assign_ranks(m, &dag);
    let nr = rank.iter().max().map_or(1, |r| r + 1);

    // The layered graph: real nodes first, then dummies, then placeholders.
    let mut ln: Vec<Lnode> = nodes
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let (w, h) = ctx.size[v];
            Lnode { real: true, w, h, lext: w / 2.0, rext: w / 2.0 + ctx.extra[v], cluster: g.cluster[v], rank: rank[i] }
        })
        .collect();
    let mut chains: Vec<Vec<usize>> = Vec::with_capacity(dag.len());
    let mut label_node = vec![None; dag.len()];
    for (k, &(a, b)) in dag.iter().enumerate() {
        let lab = ctx.label(edges[k]);
        let cl = if ln[a].cluster == ln[b].cluster { ln[a].cluster } else { None };
        let mid = rank[a] + (rank[b] - rank[a]) / 2;
        let mut chain = vec![a];
        for r in rank[a] + 1..rank[b] {
            let (w, h) = if r == mid { lab.unwrap_or((0.0, 0.0)) } else { (0.0, 0.0) };
            if r == mid && lab.is_some() {
                label_node[k] = Some(ln.len());
            }
            chain.push(ln.len());
            ln.push(Lnode { real: false, w, h, lext: w / 2.0, rext: w / 2.0, cluster: cl, rank: r });
        }
        chain.push(b);
        chains.push(chain);
    }
    let mut cl_ids: Vec<usize> = ln.iter().filter_map(|v| v.cluster).collect();
    cl_ids.sort_unstable();
    cl_ids.dedup();
    let ncl = cl_ids.len();
    let cl_local = |c: usize| cl_ids.binary_search(&c).unwrap();
    let mut span = vec![(usize::MAX, 0usize); ncl];
    let mut present = vec![vec![false; nr]; ncl];
    for v in &ln {
        if let Some(c) = v.cluster.map(cl_local) {
            present[c][v.rank] = true;
            span[c] = (span[c].0.min(v.rank), span[c].1.max(v.rank));
        }
    }
    let first_placeholder = ln.len();
    for c in 0..ncl {
        for r in span[c].0..=span[c].1 {
            if !present[c][r] {
                ln.push(Lnode { real: false, w: 0.0, h: 0.0, lext: 0.0, rext: 0.0, cluster: Some(cl_ids[c]), rank: r });
            }
        }
    }
    let nl = ln.len();
    let cl: Vec<Option<usize>> = ln.iter().map(|v| v.cluster.map(cl_local)).collect();
    let mut up = vec![Vec::new(); nl];
    let mut down = vec![Vec::new(); nl];
    for chain in &chains {
        for w in chain.windows(2) {
            let wt = match (ln[w[0]].real, ln[w[1]].real) {
                (true, true) => 1.0,
                (false, false) => 8.0,
                _ => 1.0,
            };
            down[w[0]].push((w[1], wt));
            up[w[1]].push((w[0], wt));
        }
    }

    let layers = order_layers(&ln, &up, &down, &cl, ncl, nr, m);
    let x = assign_x(&ln, &layers, &up, &down, &cl, ncl, p, pads);
    let thick: Vec<f64> = layers.iter().map(|l| l.iter().map(|&v| ln[v].h).fold(0.0, f64::max)).collect();
    let outline = |v: usize| g.outline(nodes[v]);
    let half = |v: usize| {
        let w = ln[v].w;
        match outline(v) {
            Outline::Rect => (w / 2.0 - (w * 0.15).clamp(4.0, 12.0)).max(0.0),
            Outline::Ellipse => w * 0.3,
            Outline::Diamond => w * 0.18,
        }
    };

    // Elbow edges out of a decision diamond may leave from its left and
    // right corners, the classic flowchart look, when the way is clear.
    let mut exit: Vec<Option<Side>> = vec![None; chains.len()];
    if elbow {
        let mut outs_of = vec![Vec::new(); m];
        for (k, c) in chains.iter().enumerate() {
            outs_of[c[0]].push(k);
        }
        for v in 0..m {
            if outline(v) != Outline::Diamond || outs_of[v].len() < 2 || ctx.extra[nodes[v]] > 0.0 {
                continue;
            }
            let mut ks = outs_of[v].clone();
            ks.sort_by(|&a, &b| x[chains[a][1]].total_cmp(&x[chains[b][1]]).then(a.cmp(&b)));
            let reach = |k: usize| {
                let t = chains[k][1];
                if ln[t].real { (x[t] - half(t), x[t] + half(t)) } else { (x[t], x[t]) }
            };
            let clear = |a: f64, b: f64| layers[ln[v].rank].iter().all(|&u| u == v || x[u] + ln[u].rext < a - 6.0 || x[u] - ln[u].lext > b + 6.0);
            let (left, right) = (x[v] - ln[v].w / 2.0, x[v] + ln[v].w / 2.0);
            let (k0, k1) = (ks[0], ks[ks.len() - 1]);
            let lo = reach(k0).0;
            if lo < left - 4.0 && clear(lo, left) {
                exit[k0] = Some(Side::Left);
            }
            let hi = reach(k1).1;
            if hi > right + 4.0 && clear(right, hi) {
                exit[k1] = Some(Side::Right);
            }
        }
    }

    // Ports: where each edge leaves and enters its nodes, spread along the
    // side in the order of the other ends, aligned with them when possible.
    struct End {
        k: usize,
        other: f64,
        want: f64,
    }
    let mut outs: Vec<Vec<End>> = (0..m).map(|_| Vec::new()).collect();
    let mut ins: Vec<Vec<End>> = (0..m).map(|_| Vec::new()).collect();
    for (k, c) in chains.iter().enumerate() {
        let (a, b) = (c[0], c[c.len() - 1]);
        let (n1, nb) = (c[1], c[c.len() - 2]);
        let (ha, hb, xa, xb) = (half(a), half(b), x[a], x[b]);
        if exit[k].is_some() {
            let want = if c.len() == 2 { xb } else { x[nb].clamp(xb - hb, xb + hb) };
            ins[b].push(End { k, other: x[nb], want });
            continue;
        }
        if c.len() == 2 {
            let (lo, hi) = ((xa - ha).max(xb - hb), (xa + ha).min(xb + hb));
            let (wa, wb) = if lo <= hi {
                let mid = ((xa + xb) / 2.0).clamp(lo, hi);
                (mid, mid)
            } else {
                (xb.clamp(xa - ha, xa + ha), xa.clamp(xb - hb, xb + hb))
            };
            outs[a].push(End { k, other: xb, want: wa });
            ins[b].push(End { k, other: xa, want: wb });
        } else {
            outs[a].push(End { k, other: x[n1], want: x[n1].clamp(xa - ha, xa + ha) });
            ins[b].push(End { k, other: x[nb], want: x[nb].clamp(xb - hb, xb + hb) });
        }
    }
    let mut port = vec![(0.0, 0.0); chains.len()];
    for v in 0..m {
        for (list, is_out) in [(&mut outs[v], true), (&mut ins[v], false)] {
            list.sort_by(|a, b| a.other.total_cmp(&b.other).then(a.k.cmp(&b.k)));
            let want: Vec<f64> = list.iter().map(|e| e.want).collect();
            for (e, px) in list.iter().zip(spread(&want, x[v] - half(v), x[v] + half(v), PORT_SEP)) {
                if is_out {
                    port[e.k].0 = px;
                } else {
                    port[e.k].1 = px;
                }
            }
        }
    }
    // Ends nearly aligned with the other end (or the lane next to them)
    // would make tiny jogs: line them up where the side has room.
    let move_port = |port: &mut Vec<(f64, f64)>, k: usize, out: bool, to: f64| {
        let c = &chains[k];
        let v = if out { c[0] } else { c[c.len() - 1] };
        let list = if out { &outs[v] } else { &ins[v] };
        let w = ln[v].w;
        let loose = match outline(v) {
            Outline::Rect => (w / 2.0 - 3.0).max(0.0),
            Outline::Ellipse => w * 0.38,
            Outline::Diamond => w * 0.25,
        };
        let fits = (to - x[v]).abs() <= loose.max(half(v)) + 1e-9;
        let roomy = list.iter().filter(|e| e.k != k).all(|e| {
            let px = if out { port[e.k].0 } else { port[e.k].1 };
            (px - to).abs() >= PORT_SEP * 0.6
        });
        if fits && roomy {
            if out {
                port[k].0 = to;
            } else {
                port[k].1 = to;
            }
        }
        fits && roomy
    };
    let near = |a: f64, b: f64| (a - b).abs() > 1e-9 && (a - b).abs() <= 24.0;
    for (k, c) in chains.iter().enumerate() {
        if c.len() == 2 {
            let (po, pi) = port[k];
            if exit[k].is_none() && near(po, pi) && !move_port(&mut port, k, true, pi) {
                move_port(&mut port, k, false, po);
            }
        } else {
            let (first, last) = (x[c[1]], x[c[c.len() - 2]]);
            if exit[k].is_none() && near(port[k].0, first) {
                move_port(&mut port, k, true, first);
            }
            if near(port[k].1, last) {
                move_port(&mut port, k, false, last);
            }
        }
    }

    // Lanes (x of each chain element) and the jogs elbow routes make in gaps.
    let mut lanes: Vec<Vec<f64>> = Vec::with_capacity(chains.len());
    let mut jogs: Vec<Vec<(usize, f64, f64)>> = Vec::with_capacity(chains.len());
    for (k, c) in chains.iter().enumerate() {
        let len = c.len();
        let mut xs: Vec<f64> = c.iter().map(|&v| x[v]).collect();
        xs[len - 1] = port[k].1;
        xs[0] = if exit[k].is_some() { xs[1] } else { port[k].0 };
        let mut cur = xs[0];
        let mut lane = vec![cur];
        let mut jg = Vec::new();
        for i in 0..len - 1 {
            if elbow && (xs[i + 1] - cur).abs() > 1.0 {
                jg.push((ln[c[i]].rank, cur, xs[i + 1]));
                cur = xs[i + 1];
            }
            lane.push(if elbow { cur } else { xs[i + 1] });
        }
        lanes.push(lane);
        jogs.push(jg);
    }

    // Rows in each gap: elbow jogs get their own horizontal track, and
    // labels of short edges a row where they don't collide.
    let ngaps = nr.saturating_sub(1);
    let mut items: Vec<Vec<Track>> = vec![Vec::new(); ngaps];
    for (k, c) in chains.iter().enumerate() {
        let lab = if c.len() == 2 { ctx.label(edges[k]) } else { None };
        for (j, &(r, from, to)) in jogs[k].iter().enumerate() {
            let (mut lo, mut hi) = (from.min(to), from.max(to));
            if let Some((lw, _)) = lab {
                lo = lo.min((from + to - lw) / 2.0);
                hi = hi.max((from + to + lw) / 2.0);
            }
            items[r].push(Track { k, jog: Some(j), from, to, lo, hi, h: lab.map_or(TRACK, |l| l.1 + 8.0), row: 0 });
        }
        if let (Some((lw, lh)), true) = (lab, jogs[k].is_empty()) {
            let at = (lanes[k][0] + lanes[k][1]) / 2.0;
            items[ln[c[0]].rank].push(Track { k, jog: None, from: at, to: at, lo: at - lw / 2.0, hi: at + lw / 2.0, h: lh + 8.0, row: 0 });
        }
    }
    let mut rows_h: Vec<Vec<f64>> = vec![Vec::new(); ngaps];
    let mut jog_row: Vec<Vec<usize>> = jogs.iter().map(|j| vec![0; j.len()]).collect();
    let mut label_row = vec![None; chains.len()];
    for r in 0..ngaps {
        assign_tracks(&mut items[r]);
        for t in &items[r] {
            if rows_h[r].len() <= t.row {
                rows_h[r].resize(t.row + 1, TRACK);
            }
            rows_h[r][t.row] = rows_h[r][t.row].max(t.h);
            match t.jog {
                Some(j) => jog_row[t.k][j] = t.row,
                None => label_row[t.k] = Some(t.row),
            }
        }
    }

    // Gaps between ranks: room for the rows and cluster padding.
    let mut end_pad = vec![0.0; nr];
    let mut start_pad = vec![0.0; nr];
    for &(lo, hi) in &span {
        start_pad[lo] = pads.t;
        end_pad[hi] = pads.b;
    }
    let gaps: Vec<f64> = (0..ngaps)
        .map(|r| {
            let rows = &rows_h[r];
            let content = if rows.is_empty() { 0.0 } else { rows.iter().sum::<f64>() + (rows.len() + 1) as f64 * ROW_GAP };
            let pads = end_pad[r] + start_pad[r + 1];
            let need = if pads > 0.0 { pads + content.max(24.0) } else { content };
            p.rank_gap.max(need)
        })
        .collect();
    let mut top = vec![0.0; nr];
    for r in 1..nr {
        top[r] = top[r - 1] + thick[r - 1] + gaps[r - 1];
    }
    let zone = |r: usize| (top[r] + thick[r] + end_pad[r], top[r + 1] - start_pad[r + 1]);
    let track_y = |r: usize, t: usize| {
        let (zt, zb) = zone(r);
        let hs = &rows_h[r];
        let spare = ((zb - zt) - hs.iter().sum::<f64>()).max(0.0) / (hs.len() + 1) as f64;
        zt + spare * (t + 1) as f64 + hs[..t].iter().sum::<f64>() + hs[t] / 2.0
    };
    let rect = |v: usize| {
        let r = ln[v].rank;
        Rect::new(x[v] - ln[v].w / 2.0, top[r] + (thick[r] - ln[v].h) / 2.0, ln[v].w, ln[v].h)
    };
    let mid_y = |v: usize| top[ln[v].rank] + thick[ln[v].rank] / 2.0;

    let mut part = Part { nodes: Vec::new(), routes: Vec::new(), clusters: Vec::new(), depth: Vec::new(), bbox: Rect::new(0.0, 0.0, 0.0, 0.0) };
    let real_rects: Vec<Rect> = (0..m).map(rect).collect();
    for v in 0..m {
        part.nodes.push((nodes[v], real_rects[v]));
        part.depth.push((nodes[v], rank[v]));
    }

    for (k, c) in chains.iter().enumerate() {
        let (a, b) = (c[0], c[c.len() - 1]);
        let (ra, rb) = (real_rects[a], real_rects[b]);
        let (cya, cyb) = (ra.center().1, rb.center().1);
        let lab_r = ln[a].rank;
        // Height for a short edge's label: its row, else the middle of the gap.
        let label_y = match label_row[k] {
            Some(row) => track_y(lab_r, row),
            None if lab_r + 1 < nr => {
                let (zt, zb) = zone(lab_r);
                (zt + zb) / 2.0
            }
            None => cya,
        };
        let mut start = Side::Bottom;
        let mut pts: Vec<Pt>;
        let label_at: Pt;
        if elbow {
            pts = Vec::new();
            if let Some(s) = exit[k] {
                pts.push(side_point(&ra, s, p.gap));
                pts.push((lanes[k][1], cya));
                start = s;
            } else {
                pts.push(ray_exit(&ra, outline(a), (lanes[k][0], cya), (0.0, 1.0), p.gap));
            }
            for (j, &(r, from, to)) in jogs[k].iter().enumerate() {
                let y = track_y(r, jog_row[k][j]);
                pts.push((from, y));
                pts.push((to, y));
            }
            let last = lanes[k][c.len() - 1];
            pts.push(ray_exit(&rb, outline(b), (last, cyb), (0.0, -1.0), p.gap));
            pts = simplify(pts);
            label_at = if let Some(d) = label_node[k] {
                let i = c.iter().position(|&u| u == d).unwrap();
                (lanes[k][i], mid_y(d))
            } else if c.len() == 2 {
                match jogs[k].first() {
                    Some(&(r, from, to)) => ((from + to) / 2.0, track_y(r, jog_row[k][0])),
                    None => (lanes[k][0], label_y),
                }
            } else {
                label_point(&pts)
            };
        } else {
            pts = vec![ray_exit(&ra, outline(a), (port[k].0, cya), (0.0, 1.0), p.gap)];
            for &d in &c[1..c.len() - 1] {
                let (t, th) = (top[ln[d].rank], thick[ln[d].rank]);
                if p.style == EdgeStyle::Straight && th > 1.0 {
                    pts.push((x[d], t));
                    pts.push((x[d], t + th));
                } else {
                    pts.push((x[d], t + th / 2.0));
                }
            }
            pts.push(ray_exit(&rb, outline(b), (port[k].1, cyb), (0.0, -1.0), p.gap));
            // A steep first or last segment could clip a neighbour of a short node.
            let others: Vec<&Rect> = real_rects.iter().enumerate().filter(|&(i, _)| i != a && i != b).map(|(_, r)| r).collect();
            let hits = |p0: Pt, p1: Pt| others.iter().any(|r| segment_hits_rect(p0, p1, r, 1.0));
            let band_bottom = top[ln[a].rank] + thick[ln[a].rank];
            if hits(pts[0], pts[1]) && band_bottom > pts[0].1 + 1.0 {
                pts.insert(1, (pts[0].0, band_bottom));
            }
            let n = pts.len();
            let band_top = top[ln[b].rank];
            if hits(pts[n - 2], pts[n - 1]) && band_top < pts[n - 1].1 - 1.0 {
                pts.insert(n - 1, (pts[n - 1].0, band_top));
            }
            label_at = if let Some(d) = label_node[k] {
                (x[d], mid_y(d))
            } else if c.len() == 2 {
                point_at_y(&pts, label_y)
            } else {
                label_point(&pts)
            };
        }
        let mut route = Route { points: pts, start: Some(start), end: Some(Side::Top), label_at, reversed: reversed[k] };
        if reversed[k] {
            route.points.reverse();
            std::mem::swap(&mut route.start, &mut route.end);
        }
        part.routes.push((edges[k], route));
    }
    for v in 0..m {
        for (j, &e) in ctx.loops[nodes[v]].iter().enumerate() {
            part.routes.push((e, self_loop(&real_rects[v], outline(v), p.style, p.gap, j, ctx.label(e))));
        }
    }

    for (c, &id) in cl_ids.iter().enumerate() {
        let members = (0..first_placeholder).filter(|&v| cl[v] == Some(c)).map(|v| {
            let r = rect(v);
            Rect::new(x[v] - ln[v].lext, r.y, ln[v].lext + ln[v].rext, r.h)
        });
        if let Some(u) = union_all(members) {
            part.clusters.push((id, Rect::new(u.x - pads.l, u.y - pads.t, u.w + pads.l + pads.r, u.h + pads.t + pads.b)));
        }
    }

    let mut all: Vec<Rect> = part.nodes.iter().map(|n| n.1).chain(part.clusters.iter().map(|c| c.1)).collect();
    for (e, r) in &part.routes {
        all.extend(r.points.iter().map(|p| Rect::new(p.0, p.1, 0.0, 0.0)));
        if let Some((w, h)) = ctx.label(*e) {
            all.push(Rect::new(r.label_at.0 - w / 2.0, r.label_at.1 - h / 2.0, w, h));
        }
    }
    part.bbox = union_all(all).unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
    part
}

/// The point where a polyline first reaches height `y` (its midpoint if never).
fn point_at_y(pts: &[Pt], y: f64) -> Pt {
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if (a.1 - y) * (b.1 - y) <= 0.0 && (b.1 - a.1).abs() > 1e-9 {
            let t = (y - a.1) / (b.1 - a.1);
            return (a.0 + (b.0 - a.0) * t, y);
        }
    }
    polyline_midpoint(pts)
}

// ---------- trees ----------

/// A spanning forest of the graph, found breadth first from the roots.
struct Forest {
    parent: Vec<Option<usize>>,
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,
    /// Every node, parents before children.
    order: Vec<usize>,
    depth: Vec<usize>,
    branch: Vec<Option<usize>>,
    tree_edge: Vec<bool>,
}

impl Forest {
    /// Roots are nodes without incoming edges (the first node if there are
    /// none). With `single_root`, everything left unreached hangs off the
    /// first root (without an edge) instead of starting its own tree.
    fn new(g: &Graph, single_root: bool) -> Forest {
        let n = g.nodes.len();
        let mut out: Vec<Vec<(usize, usize)>> = vec![Vec::new(); n];
        let mut indeg = vec![0; n];
        for (e, ends) in g.edges.iter().enumerate() {
            if let Some((a, b)) = *ends {
                if a != b {
                    out[a].push((e, b));
                    indeg[b] += 1;
                }
            }
        }
        let mut f = Forest {
            parent: vec![None; n],
            children: vec![Vec::new(); n],
            roots: Vec::new(),
            order: Vec::with_capacity(n),
            depth: vec![0; n],
            branch: vec![None; n],
            tree_edge: vec![false; g.edges.len()],
        };
        if n == 0 {
            return f;
        }
        let sources: Vec<usize> = (0..n).filter(|&v| indeg[v] == 0).collect();
        let candidates: Vec<usize> = sources.iter().copied().chain(0..n).collect();
        let mut seen = vec![false; n];
        let mut queue = VecDeque::new();
        let initial = if single_root || sources.is_empty() { vec![candidates[0]] } else { sources };
        for r in initial {
            seen[r] = true;
            f.roots.push(r);
            queue.push_back(r);
        }
        let mut next = 0;
        loop {
            while let Some(u) = queue.pop_front() {
                f.order.push(u);
                for &(e, v) in &out[u] {
                    if !seen[v] {
                        seen[v] = true;
                        f.parent[v] = Some(u);
                        f.children[u].push(v);
                        f.tree_edge[e] = true;
                        queue.push_back(v);
                    }
                }
            }
            while next < candidates.len() && seen[candidates[next]] {
                next += 1;
            }
            let Some(&v) = candidates.get(next) else { break };
            seen[v] = true;
            if single_root {
                let r = f.roots[0];
                f.parent[v] = Some(r);
                f.children[r].push(v);
            } else {
                f.roots.push(v);
            }
            queue.push_back(v);
        }
        for i in 0..f.order.len() {
            let u = f.order[i];
            for (j, &c) in f.children[u].iter().enumerate() {
                f.depth[c] = f.depth[u] + 1;
                f.branch[c] = if f.parent[u].is_none() { Some(j) } else { f.branch[u] };
            }
        }
        f
    }
}

/// One depth of a subtree's outline: its extent along the levels (`main`)
/// and across them.
#[derive(Clone, Copy)]
struct Level {
    lo_main: f64,
    hi_main: f64,
    lo: f64,
    hi: f64,
}

fn levels_overlap(a: &Level, b: &Level) -> bool {
    a.lo_main < b.hi_main - 1e-6 && b.lo_main < a.hi_main - 1e-6
}

fn merge_contour(acc: &mut Vec<Level>, other: &[Level], dx: f64, depth: usize) {
    for (i, l) in other.iter().enumerate() {
        let l = Level { lo: l.lo + dx, hi: l.hi + dx, ..*l };
        match acc.get_mut(depth + i) {
            Some(a) => {
                a.lo_main = a.lo_main.min(l.lo_main);
                a.hi_main = a.hi_main.max(l.hi_main);
                a.lo = a.lo.min(l.lo);
                a.hi = a.hi.max(l.hi);
            }
            None => acc.push(l),
        }
    }
}

/// Offsets for subtrees placed side by side in order, `gap` apart where
/// their outlines meet, centred so the first and last are symmetric about
/// zero. Packing from the left and from the right and averaging spreads
/// small subtrees evenly between big ones.
fn pack(subs: &[Vec<Level>], gap: f64) -> Vec<f64> {
    let k = subs.len();
    if k == 0 {
        return Vec::new();
    }
    let mut lr = vec![0.0f64; k];
    let mut acc = subs[0].clone();
    for i in 1..k {
        let mut s = lr[i - 1];
        for a in &acc {
            for b in &subs[i] {
                if levels_overlap(a, b) {
                    s = s.max(a.hi - b.lo + gap);
                }
            }
        }
        lr[i] = s;
        merge_contour(&mut acc, &subs[i], s, 0);
    }
    let mut rl = vec![0.0f64; k];
    let mut acc = subs[k - 1].clone();
    for i in (0..k - 1).rev() {
        let mut s = rl[i + 1];
        for a in &acc {
            for b in &subs[i] {
                if levels_overlap(a, b) {
                    s = s.min(a.lo - b.hi - gap);
                }
            }
        }
        rl[i] = s;
        merge_contour(&mut acc, &subs[i], s, 0);
    }
    let base = rl[0];
    let mut pos: Vec<f64> = lr.iter().zip(&rl).map(|(a, b)| (a + b - base) / 2.0).collect();
    let mid = (pos[0] + pos[k - 1]) / 2.0;
    pos.iter_mut().for_each(|p| *p -= mid);
    pos
}

/// Tidy tree (Reingold–Tilford style, bottom up): each node's centre offset
/// from its parent's across the levels, and each subtree's outline.
fn tidy(f: &Forest, width: &[f64], main: &[(f64, f64)], gap: f64, skip: Option<usize>) -> (Vec<f64>, Vec<Vec<Level>>) {
    let n = width.len();
    let mut rel = vec![0.0; n];
    let mut cont: Vec<Vec<Level>> = vec![Vec::new(); n];
    for &v in f.order.iter().rev() {
        if Some(v) == skip {
            continue;
        }
        let kids = &f.children[v];
        let subs: Vec<Vec<Level>> = kids.iter().map(|&c| std::mem::take(&mut cont[c])).collect();
        let pos = pack(&subs, gap);
        let mut c = vec![Level { lo_main: main[v].0, hi_main: main[v].1, lo: -width[v] / 2.0, hi: width[v] / 2.0 }];
        for ((&kid, sub), &px) in kids.iter().zip(&subs).zip(&pos) {
            rel[kid] = px;
            merge_contour(&mut c, sub, px, 1);
        }
        cont[v] = c;
    }
    (rel, cont)
}

/// Routes the edges no layout step placed: around other nodes, side by side
/// when several join the same pair.
#[allow(clippy::too_many_arguments)]
fn route_extra(g: &Graph, rects: &[Rect], e: usize, style: EdgeStyle, gap: f64, offset: f64, nth_loop: usize, gutters: (&[f64], &[f64]), index: Option<&Blockers>) -> Option<Route> {
    let (a, b) = g.edges[e]?;
    if a == b {
        return Some(self_loop(&rects[a], g.outline(a), style, gap, nth_loop, g.labels[e]));
    }
    let obstacles: Vec<Rect> = rects.iter().enumerate().filter(|&(i, _)| i != a && i != b).map(|(_, r)| *r).collect();
    Some(route_free(&rects[a], g.outline(a), &rects[b], g.outline(b), style, gap, offset, &obstacles, gutters, index))
}

fn tree(g: &Graph, p: &Params, frame: Frame) -> Placed {
    let n = g.nodes.len();
    let mut placed = Placed::new(g, p.style);
    if n == 0 {
        return placed;
    }
    let f = Forest::new(g, false);
    let size: Vec<(f64, f64)> = (0..n).map(|v| { let (w, h) = g.size(v); frame.size(w, h) }).collect();
    let maxd = f.depth.iter().copied().max().unwrap_or(0);
    let mut thick = vec![0.0f64; maxd + 1];
    for v in 0..n {
        thick[f.depth[v]] = thick[f.depth[v]].max(size[v].1);
    }
    let elbow = p.style == EdgeStyle::Elbow;
    let mut lab = vec![0.0f64; maxd + 1];
    for (e, &t) in f.tree_edge.iter().enumerate() {
        if let (true, Some((w, h))) = (t, g.labels[e]) {
            let d = f.depth[g.edges[e].unwrap().0];
            lab[d] = lab[d].max(frame.size(w, h).1);
        }
    }
    let gaps: Vec<f64> = (0..maxd).map(|d| if lab[d] > 0.0 { p.rank_gap.max(lab[d] + 2.0 * LABEL_MARGIN + if elbow { 16.0 } else { 0.0 }) } else { p.rank_gap }).collect();
    let mut top = vec![0.0; maxd + 1];
    for d in 1..=maxd {
        top[d] = top[d - 1] + thick[d - 1] + gaps[d - 1];
    }
    let main: Vec<(f64, f64)> = (0..n)
        .map(|v| {
            let d = f.depth[v];
            let y = top[d] + (thick[d] - size[v].1) / 2.0;
            (y, y + size[v].1)
        })
        .collect();
    let width: Vec<f64> = size.iter().map(|s| s.0).collect();
    let (rel, cont) = tidy(&f, &width, &main, p.node_gap, None);
    let subs: Vec<Vec<Level>> = f.roots.iter().map(|&r| cont[r].clone()).collect();
    let mut x = vec![0.0; n];
    for (&r, px) in f.roots.iter().zip(pack(&subs, p.node_gap * 2.0)) {
        x[r] = px;
    }
    for &v in &f.order {
        if let Some(par) = f.parent[v] {
            x[v] = x[par] + rel[v];
        }
    }
    let rects: Vec<Rect> = (0..n).map(|v| Rect::new(x[v] - size[v].0 / 2.0, main[v].0, size[v].0, size[v].1)).collect();
    for v in 0..n {
        placed.nodes[v] = Some(rects[v]);
        placed.depth[v] = Some(f.depth[v]);
        placed.branch[v] = f.branch[v];
    }

    let offsets = multi_offsets(g, |e| f.tree_edge[e]);
    let mut nloops = vec![0; n];
    for e in 0..g.edges.len() {
        let Some((a, b)) = g.edges[e] else { continue };
        if !f.tree_edge[e] {
            placed.routes[e] = route_extra(g, &rects, e, p.style, p.gap, offsets[e], nloops[a], (&[], &[]), None);
            if a == b {
                nloops[a] += 1;
            }
            continue;
        }
        let (ra, rb) = (rects[a], rects[b]);
        let d = f.depth[a];
        let s = ray_exit(&ra, g.outline(a), ra.center(), (0.0, 1.0), p.gap);
        let route = match p.style {
            EdgeStyle::Elbow => {
                let bus = top[d] + thick[d] + if lab[d] > 0.0 { 16.0 } else { gaps[d] / 2.0 };
                let cx = if (rb.center().0 - s.0).abs() <= 1.0 { s.0 } else { rb.center().0 };
                let e_pt = ray_exit(&rb, g.outline(b), (cx, rb.center().1), (0.0, -1.0), p.gap);
                let pts = simplify(vec![s, (s.0, bus), (cx, bus), e_pt]);
                Route { label_at: (cx, (bus + e_pt.1) / 2.0), points: pts, start: Some(Side::Bottom), end: Some(Side::Top), reversed: false }
            }
            EdgeStyle::Curved => {
                let e_pt = ray_exit(&rb, g.outline(b), rb.center(), (0.0, -1.0), p.gap);
                Route { label_at: ((s.0 + e_pt.0) / 2.0, (s.1 + e_pt.1) / 2.0), points: s_curve(s, e_pt, false), start: Some(Side::Bottom), end: Some(Side::Top), reversed: false }
            }
            EdgeStyle::Straight => {
                let s = boundary_point(&ra, g.outline(a), rb.center(), p.gap);
                let e_pt = boundary_point(&rb, g.outline(b), ra.center(), p.gap);
                let mid = top[d] + thick[d] + gaps[d] / 2.0;
                let pts = vec![s, e_pt];
                Route { label_at: point_at_y(&pts, mid), start: Some(side_at(&ra, s)), end: Some(side_at(&rb, e_pt)), points: pts, reversed: false }
            }
        };
        placed.routes[e] = Some(route);
    }
    placed
}

fn mindmap(g: &Graph, p: &Params) -> Placed {
    let n = g.nodes.len();
    let mut placed = Placed::new(g, p.style);
    if n == 0 {
        return placed;
    }
    let f = Forest::new(g, true);
    let root = f.roots[0];
    let size: Vec<(f64, f64)> = (0..n).map(|v| g.size(v)).collect();
    let mut into = vec![None; n];
    for (e, &t) in f.tree_edge.iter().enumerate() {
        if t {
            into[g.edges[e].unwrap().1] = Some(e);
        }
    }
    // Distance from the root's centre, outwards, of each node's near and far side.
    let mut main = vec![(0.0, 0.0); n];
    main[root] = (-size[root].0 / 2.0, size[root].0 / 2.0);
    for &v in &f.order {
        if let Some(par) = f.parent[v] {
            let lw = into[v].and_then(|e| g.labels[e]).map_or(0.0, |l| l.0);
            let gap = if lw > 0.0 { p.rank_gap.max(lw + 2.0 * LABEL_MARGIN + 16.0) } else { p.rank_gap };
            let near = main[par].1 + gap;
            main[v] = (near, near + size[v].0);
        }
    }
    let height: Vec<f64> = size.iter().map(|s| s.1).collect();
    let (rel, cont) = tidy(&f, &height, &main, p.node_gap, Some(root));
    let first = &f.children[root];
    let extent = |c: usize| {
        let l = &cont[c];
        l.iter().map(|x| x.hi).fold(f64::MIN, f64::max) - l.iter().map(|x| x.lo).fold(f64::MAX, f64::min)
    };
    let ext: Vec<f64> = first.iter().map(|&c| extent(c) + p.node_gap).collect();
    let total: f64 = ext.iter().sum();
    let (mut best, mut k, mut acc) = (f64::INFINITY, first.len(), 0.0);
    for i in 1..=first.len() {
        acc += ext[i - 1];
        let diff = (acc - (total - acc)).abs();
        if diff <= best + 1e-9 {
            best = diff;
            k = i;
        }
    }
    // Clockwise from the top right: the first part goes down the right
    // side, the rest comes back up the left.
    let right: Vec<usize> = first[..k].to_vec();
    let left: Vec<usize> = first[k..].iter().rev().copied().collect();
    let mut y = vec![0.0; n];
    let mut sign = vec![1.0; n];
    for (list, s) in [(&right, 1.0), (&left, -1.0)] {
        let subs: Vec<Vec<Level>> = list.iter().map(|&c| cont[c].clone()).collect();
        for (&c, py) in list.iter().zip(pack(&subs, p.node_gap * 1.5)) {
            y[c] = py;
            sign[c] = s;
        }
    }
    for &v in &f.order {
        if let Some(par) = f.parent[v] {
            if par != root {
                y[v] = y[par] + rel[v];
                sign[v] = sign[par];
            }
        }
    }
    let rects: Vec<Rect> = (0..n)
        .map(|v| {
            let x = if v == root || sign[v] > 0.0 { main[v].0 } else { -main[v].1 };
            Rect::new(x, y[v] - size[v].1 / 2.0, size[v].0, size[v].1)
        })
        .collect();
    for v in 0..n {
        placed.nodes[v] = Some(rects[v]);
        placed.depth[v] = Some(f.depth[v]);
        placed.branch[v] = f.branch[v];
    }
    let offsets = multi_offsets(g, |e| f.tree_edge[e]);
    let mut nloops = vec![0; n];
    for e in 0..g.edges.len() {
        let Some((a, b)) = g.edges[e] else { continue };
        if !f.tree_edge[e] {
            placed.routes[e] = route_extra(g, &rects, e, p.style, p.gap, offsets[e], nloops[a], (&[], &[]), None);
            if a == b {
                nloops[a] += 1;
            }
            continue;
        }
        let (out_side, in_side) = if sign[b] > 0.0 { (Side::Right, Side::Left) } else { (Side::Left, Side::Right) };
        let s = side_point(&rects[a], out_side, p.gap);
        let t = side_point(&rects[b], in_side, p.gap);
        let points = match p.style {
            EdgeStyle::Curved => s_curve(s, t, true),
            EdgeStyle::Straight => vec![s, t],
            EdgeStyle::Elbow => {
                let xm = (s.0 + t.0) / 2.0;
                simplify(vec![s, (xm, s.1), (xm, t.1), t])
            }
        };
        let label_at = ((s.0 + t.0) / 2.0, (s.1 + t.1) / 2.0);
        placed.routes[e] = Some(Route { points, start: Some(out_side), end: Some(in_side), label_at, reversed: false });
    }
    placed
}

fn radial(g: &Graph, p: &Params) -> Placed {
    let n = g.nodes.len();
    let mut placed = Placed::new(g, EdgeStyle::Straight);
    if n == 0 {
        return placed;
    }
    let f = Forest::new(g, true);
    let root = f.roots[0];
    let size: Vec<(f64, f64)> = (0..n).map(|v| g.size(v)).collect();
    let mut leaves = vec![0.0f64; n];
    for &v in f.order.iter().rev() {
        leaves[v] = if f.children[v].is_empty() { 1.0 } else { f.children[v].iter().map(|&c| leaves[c]).sum() };
    }
    // Each subtree gets a wedge proportional to its leaves; the first
    // branch sits straight up.
    let mut angle = vec![0.0; n];
    let mut wedge = vec![(0.0, 0.0); n];
    let first_share = f.children[root].first().map_or(0.0, |&c| 2.0 * PI * leaves[c] / leaves[root]);
    wedge[root] = (-PI / 2.0 - first_share / 2.0, 2.0 * PI);
    for &v in &f.order {
        let (mut a, span) = wedge[v];
        for &c in &f.children[v] {
            let s = span * leaves[c] / leaves[v];
            wedge[c] = (a, s);
            angle[c] = a + s / 2.0;
            a += s;
        }
    }
    let maxd = f.depth.iter().copied().max().unwrap_or(0);
    let mut rings: Vec<Vec<usize>> = vec![Vec::new(); maxd + 1];
    for &v in &f.order {
        rings[f.depth[v]].push(v);
    }
    for r in &mut rings {
        r.sort_by(|&a, &b| angle[a].total_cmp(&angle[b]));
    }
    let ext = |v: usize, t: f64| t.cos().abs() * size[v].0 / 2.0 + t.sin().abs() * size[v].1 / 2.0;
    let mut radius = vec![0.0f64; maxd + 1];
    for d in 1..=maxd {
        let inner = if d == 1 { rings[1].iter().map(|&v| ext(root, angle[v])).fold(0.0, f64::max) } else { rings[d - 1].iter().map(|&v| ext(v, angle[v])).fold(0.0, f64::max) };
        let outer = rings[d].iter().map(|&v| ext(v, angle[v])).fold(0.0, f64::max);
        let mut r = radius[d - 1] + inner + outer + p.rank_gap;
        let ring = &rings[d];
        if ring.len() >= 2 {
            for i in 0..ring.len() {
                let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                let mut delta = angle[b] - angle[a];
                if i + 1 == ring.len() {
                    delta += 2.0 * PI;
                }
                if delta < 1e-6 {
                    delta = 1e-6;
                }
                let phi = (angle[a] + angle[b]) / 2.0 + PI / 2.0;
                let need = ext(a, phi) + ext(b, phi) + p.node_gap;
                r = r.max(need / (2.0 * (delta / 2.0).sin().max(1e-6)));
            }
        }
        radius[d] = r;
    }
    let place = |radius: &[f64]| -> Vec<Rect> {
        (0..n)
            .map(|v| {
                let r = radius[f.depth[v]];
                let (cx, cy) = (r * angle[v].cos(), r * angle[v].sin());
                Rect::new(cx - size[v].0 / 2.0, cy - size[v].1 / 2.0, size[v].0, size[v].1)
            })
            .collect()
    };
    let mut rects = place(&radius);
    let step = (p.node_gap * 0.5).max(10.0);
    for _ in 0..100 {
        let mut worst = None;
        'scan: for a in 0..n {
            for b in a + 1..n {
                if rects[a].inflate(p.node_gap / 4.0).intersects(&rects[b].inflate(p.node_gap / 4.0)) {
                    worst = Some(f.depth[a].max(f.depth[b]).max(1));
                    break 'scan;
                }
            }
        }
        let Some(d) = worst else { break };
        for r in radius.iter_mut().skip(d) {
            *r += step;
        }
        rects = place(&radius);
    }
    for v in 0..n {
        placed.nodes[v] = Some(rects[v]);
        placed.depth[v] = Some(f.depth[v]);
        placed.branch[v] = f.branch[v];
    }
    let offsets = multi_offsets(g, |_| false);
    let mut nloops = vec![0; n];
    for e in 0..g.edges.len() {
        let Some((a, _)) = g.edges[e] else { continue };
        placed.routes[e] = route_extra(g, &rects, e, EdgeStyle::Straight, p.gap, offsets[e], nloops[a], (&[], &[]), None);
        if g.edges[e].is_some_and(|(a, b)| a == b) {
            nloops[a] += 1;
        }
    }
    placed
}

// ---------- grid ----------

fn grid(input: &LayoutInput, g: &Graph, p: &Params) -> Placed {
    let n = g.nodes.len();
    let mut placed = Placed::new(g, p.style);
    if n == 0 {
        return placed;
    }
    // One block per cluster (unclustered nodes share one), in first-appearance order.
    let mut blocks: Vec<(Option<usize>, Vec<usize>)> = Vec::new();
    for v in 0..n {
        match blocks.iter_mut().find(|b| b.0 == g.cluster[v]) {
            Some(b) => b.1.push(v),
            None => blocks.push((g.cluster[v], vec![v])),
        }
    }
    struct Block {
        cluster: Option<usize>,
        nodes: Vec<(usize, Rect)>,
        w: f64,
        h: f64,
        xg: Vec<f64>,
        yg: Vec<f64>,
    }
    let mut laid: Vec<Block> = Vec::new();
    for (cluster, members) in &blocks {
        let m = members.len();
        let cols = input.columns.filter(|&c| c > 0).unwrap_or_else(|| (m as f64).sqrt().ceil() as usize).clamp(1, m);
        let rows = m.div_ceil(cols);
        let mut colw = vec![0.0f64; cols];
        let mut rowh = vec![0.0f64; rows];
        for (i, &v) in members.iter().enumerate() {
            let (w, h) = g.size(v);
            colw[i % cols] = colw[i % cols].max(w);
            rowh[i / cols] = rowh[i / cols].max(h);
        }
        let (ox, oy) = if cluster.is_some() { (p.pad, p.pad + p.title) } else { (0.0, 0.0) };
        let mut cx = vec![ox; cols];
        for c in 1..cols {
            cx[c] = cx[c - 1] + colw[c - 1] + p.node_gap;
        }
        let mut ry = vec![oy; rows];
        for r in 1..rows {
            ry[r] = ry[r - 1] + rowh[r - 1] + p.rank_gap;
        }
        let nodes = members
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let (w, h) = g.size(v);
                let (c, r) = (i % cols, i / cols);
                (v, Rect::new(cx[c] + (colw[c] - w) / 2.0, ry[r] + (rowh[r] - h) / 2.0, w, h))
            })
            .collect();
        let inner_w = cx[cols - 1] + colw[cols - 1] - ox;
        let inner_h = ry[rows - 1] + rowh[rows - 1] - oy;
        let (w, h) = if cluster.is_some() { (inner_w + 2.0 * p.pad, inner_h + 2.0 * p.pad + p.title) } else { (inner_w, inner_h) };
        laid.push(Block {
            cluster: *cluster,
            nodes,
            w,
            h,
            xg: (1..cols).map(|c| cx[c] - p.node_gap / 2.0).collect(),
            yg: (1..rows).map(|r| ry[r] - p.rank_gap / 2.0).collect(),
        });
    }
    // Blocks flow left to right and wrap into rows of roughly square overall shape.
    let block_gap = (p.node_gap * 1.5).max(30.0);
    let area: f64 = laid.iter().map(|b| (b.w + block_gap) * (b.h + block_gap)).sum();
    let widest = laid.iter().map(|b| b.w).fold(0.0, f64::max);
    let target = widest.max(area.sqrt() * 1.3);
    let (mut cx, mut cy, mut row_h) = (0.0, 0.0, 0.0f64);
    let mut rects = vec![Rect::new(0.0, 0.0, 0.0, 0.0); n];
    let (mut xg, mut yg) = (Vec::new(), Vec::new());
    for b in &laid {
        if cx > 0.0 && cx + b.w > target {
            cx = 0.0;
            cy += row_h + block_gap;
            row_h = 0.0;
        }
        for &(v, r) in &b.nodes {
            rects[v] = Rect::new(r.x + cx, r.y + cy, r.w, r.h);
        }
        if let Some(c) = b.cluster {
            placed.clusters.push((c, Rect::new(cx, cy, b.w, b.h)));
        }
        xg.extend(b.xg.iter().map(|x| x + cx));
        xg.extend([cx - block_gap / 2.0, cx + b.w + block_gap / 2.0]);
        yg.extend(b.yg.iter().map(|y| y + cy));
        yg.extend([cy - block_gap / 2.0, cy + b.h + block_gap / 2.0]);
        cx += b.w + block_gap;
        row_h = row_h.max(b.h);
    }
    for v in 0..n {
        placed.nodes[v] = Some(rects[v]);
    }
    let offsets = multi_offsets(g, |_| false);
    let mut nloops = vec![0; n];
    let index = Blockers::new(rects.iter().copied());
    for e in 0..g.edges.len() {
        let Some((a, b)) = g.edges[e] else { continue };
        placed.routes[e] = route_extra(g, &rects, e, p.style, p.gap, offsets[e], nloops[a], (&xg, &yg), Some(&index));
        if a == b {
            nloops[a] += 1;
        }
    }
    placed
}

// ---------- free routing ----------

/// A route between two placed nodes that avoids `obstacles` where it can:
/// a straight line between the outlines, or the best of a handful of
/// orthogonal candidates through the gutters.
#[allow(clippy::too_many_arguments)]
fn route_free(a: &Rect, ao: Outline, b: &Rect, bo: Outline, style: EdgeStyle, gap: f64, offset: f64, obstacles: &[Rect], gutters: (&[f64], &[f64]), index: Option<&Blockers>) -> Route {
    if style != EdgeStyle::Elbow {
        let (ca, cb) = (a.center(), b.center());
        let d = (cb.0 - ca.0, cb.1 - ca.1);
        let len = (d.0 * d.0 + d.1 * d.1).sqrt();
        if len > 1e-9 {
            let lim = a.w.min(a.h).min(b.w).min(b.h) * 0.35;
            let o = offset.clamp(-lim, lim);
            let nrm = (-d.1 / len * o, d.0 / len * o);
            let s = ray_exit(a, ao, (ca.0 + nrm.0, ca.1 + nrm.1), d, gap);
            let e = ray_exit(b, bo, (cb.0 + nrm.0, cb.1 + nrm.1), (-d.0, -d.1), gap);
            let pts = vec![s, e];
            let hits = count_hits(&pts, a, b, obstacles);
            if hits > 0 {
                match best_ortho(a, ao, b, bo, gap, offset, obstacles, gutters) {
                    Some((best, 0)) => return best,
                    best => {
                        if let Some(r) = maze(a, ao, b, bo, gap, offset, obstacles, gutters, index) {
                            return r;
                        }
                        if let Some((best, h)) = best {
                            if h < hits {
                                return best;
                            }
                        }
                    }
                }
            }
            return Route { start: Some(side_at(a, s)), end: Some(side_at(b, e)), label_at: label_point(&pts), points: pts, reversed: false };
        }
    }
    match best_ortho(a, ao, b, bo, gap, offset, obstacles, gutters) {
        Some((best, 0)) => best,
        best => maze(a, ao, b, bo, gap, offset, obstacles, gutters, index).or(best.map(|r| r.0)).unwrap_or_else(|| {
            let pts = vec![a.center(), b.center()];
            Route { label_at: label_point(&pts), points: pts, ..Route::default() }
        }),
    }
}

/// Rects grouped by the bands between their edges along one axis, to find
/// those spanning a coordinate quickly.
struct Slabs {
    cuts: Vec<f64>,
    lists: Vec<Vec<usize>>,
}

impl Slabs {
    fn new(rects: &[Rect], span: impl Fn(&Rect) -> (f64, f64)) -> Slabs {
        let mut cuts: Vec<f64> = rects.iter().flat_map(|r| { let (lo, hi) = span(r); [lo, hi] }).filter(|v| v.is_finite()).collect();
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        let mut lists = vec![Vec::new(); cuts.len()];
        for (i, r) in rects.iter().enumerate() {
            let (lo, hi) = span(r);
            let (from, to) = (cuts.partition_point(|&c| c < lo), cuts.partition_point(|&c| c < hi));
            for list in &mut lists[from..to.min(cuts.len())] {
                list.push(i);
            }
        }
        Slabs { cuts, lists }
    }
    /// Rects whose span may contain `v` (a superset; callers check exactly).
    fn at(&self, v: f64) -> &[usize] {
        let k = self.cuts.partition_point(|&c| c <= v);
        if k == 0 { &[] } else { &self.lists[k - 1] }
    }
}

/// Every node of a layout, indexed for quick tests of axis-aligned segments.
struct Blockers {
    rects: Vec<Rect>,
    by_x: Slabs,
    by_y: Slabs,
    bounds: Option<Rect>,
}

impl Blockers {
    fn new(rects: impl IntoIterator<Item = Rect>) -> Blockers {
        let all: Vec<Rect> = rects.into_iter().collect();
        let rects: Vec<Rect> = all.iter().map(|r| r.inflate(-1.0)).filter(|r| r.w > 0.0 && r.h > 0.0).collect();
        let (by_x, by_y) = (Slabs::new(&rects, |r| (r.x, r.right())), Slabs::new(&rects, |r| (r.y, r.bottom())));
        Blockers { rects, by_x, by_y, bounds: union_all(all) }
    }
    /// Whether the segment runs through a rect (touching an edge is fine).
    fn blocked(&self, p: Pt, q: Pt) -> bool {
        let (x0, x1, y0, y1) = (p.0.min(q.0), p.0.max(q.0), p.1.min(q.1), p.1.max(q.1));
        let cross = |r: &Rect| r.x < x1 && r.right() > x0 && r.y < y1 && r.bottom() > y0;
        if x1 - x0 < 1e-9 {
            self.by_x.at(x0).iter().any(|&i| cross(&self.rects[i]))
        } else if y1 - y0 < 1e-9 {
            self.by_y.at(y0).iter().any(|&i| cross(&self.rects[i]))
        } else {
            self.rects.iter().any(|r| segment_hits_rect(p, q, r, 0.0))
        }
    }
}

/// Last resort when no simple route is clear: the shortest orthogonal path,
/// with few bends, over a grid made of the gutter lines and lines just
/// outside the two end nodes (A*). None if every path is blocked.
/// `index`, when given, holds every node (the ends included).
#[allow(clippy::too_many_arguments)]
fn maze(a: &Rect, ao: Outline, b: &Rect, bo: Outline, gap: f64, o: f64, obstacles: &[Rect], (xg, yg): (&[f64], &[f64]), index: Option<&Blockers>) -> Option<Route> {
    use Side::*;
    const BEND: f64 = 40.0;
    const SIDES: [Side; 4] = [Top, Right, Bottom, Left];
    let margin = (gap * 3.0).max(20.0);
    let local;
    let index = match index {
        Some(i) => i,
        None => {
            local = Blockers::new([a, b].into_iter().chain(obstacles).copied());
            &local
        }
    };
    let blocked = |p: Pt, q: Pt| index.blocked(p, q);
    // Each side's port and the point a short way out where routing starts:
    // the nearest gutter line beyond it, or a margin away.
    let stubs = |r: &Rect, ol: Outline| -> Vec<Option<(Pt, Pt)>> {
        let c = r.center();
        SIDES
            .iter()
            .map(|&sd| {
                let port = match sd {
                    Left | Right => ray_exit(r, ol, (c.0, within(c.1 + o, r.y + 2.0, r.bottom() - 2.0)), sd.normal(), gap),
                    Top | Bottom => ray_exit(r, ol, (within(c.0 + o, r.x + 2.0, r.right() - 2.0), c.1), sd.normal(), gap),
                };
                let (list, at, less) = match sd {
                    Left => (xg, port.0, true),
                    Right => (xg, port.0, false),
                    Top => (yg, port.1, true),
                    Bottom => (yg, port.1, false),
                };
                let v = list
                    .iter()
                    .copied()
                    .filter(|&g| if less { g < at - 1.0 } else { g > at + 1.0 })
                    .min_by(|p, q| (p - at).abs().total_cmp(&(q - at).abs()))
                    .unwrap_or(if less { at - margin } else { at + margin });
                let st = if sd.horizontal() { (v, port.1) } else { (port.0, v) };
                (fin(st.0) == st.0 && fin(st.1) == st.1 && !blocked(port, st)).then_some((port, st))
            })
            .collect()
    };
    let (sa, sb) = (stubs(a, ao), stubs(b, bo));
    // Grid lines: gutters, stubs, and a frame around everything.
    let all = union_all([*a, *b, index.bounds?])?.inflate(margin);
    let mut xs: Vec<f64> = xg.iter().copied().chain([all.x, all.right()]).collect();
    let mut ys: Vec<f64> = yg.iter().copied().chain([all.y, all.bottom()]).collect();
    for (_, st) in sa.iter().chain(&sb).flatten() {
        xs.push(st.0);
        ys.push(st.1);
    }
    let tidy = |v: &mut Vec<f64>| {
        v.retain(|x| x.is_finite());
        v.sort_by(f64::total_cmp);
        v.dedup_by(|p, q| (*p - *q).abs() < 0.5);
    };
    tidy(&mut xs);
    tidy(&mut ys);
    let (nx, ny) = (xs.len(), ys.len());
    if nx * ny > 40_000 {
        return None;
    }
    let find = |v: &[f64], t: f64| v.iter().position(|&x| (x - t).abs() < 0.5);
    let at = |st: Pt| Some(find(&xs, st.0)? * ny + find(&ys, st.1)?);
    let pt = |i: usize| (xs[i / ny], ys[i % ny]);
    // Neighbour of grid point `i` in direction `d` (Top/Right/Bottom/Left = up/right/down/left).
    let step = |i: usize, d: usize| -> Option<usize> {
        let (cx, cy) = (i / ny, i % ny);
        match d {
            0 if cy > 0 => Some(i - 1),
            1 if cx + 1 < nx => Some(i + ny),
            2 if cy + 1 < ny => Some(i + 1),
            3 if cx > 0 => Some(i - ny),
            _ => None,
        }
    };
    // A* over (grid point, heading); the heuristic is the Manhattan
    // distance to the nearest end stub, plus a bend when it isn't in line,
    // which never overestimates.
    let ends: Vec<(usize, usize)> = sb.iter().enumerate().filter_map(|(d, s)| Some((at(s.as_ref()?.1)?, d))).collect();
    let goals: Vec<Pt> = ends.iter().map(|&(j, _)| pt(j)).collect();
    let h = |i: usize| {
        let q = pt(i);
        let turn = |g: &Pt| if (g.0 - q.0).abs() > 0.5 && (g.1 - q.1).abs() > 0.5 { BEND } else { 0.0 };
        goals.iter().map(|g| (g.0 - q.0).abs() + (g.1 - q.1).abs() + turn(g)).fold(f64::INFINITY, f64::min)
    };
    // Per point: whether the segment to its right / below is open (0 = unknown, 1 = open, 2 = blocked).
    let mut open_seg = vec![[0u8; 2]; nx * ny];
    let mut cost = vec![f64::INFINITY; nx * ny * 4];
    let mut prev = vec![usize::MAX; nx * ny * 4];
    let mut heap = BinaryHeap::new();
    let key = |c: f64| Reverse(c.max(0.0).to_bits());
    for (d, s) in sa.iter().enumerate() {
        let Some((port, st)) = s else { continue };
        let Some(i) = at(*st) else { continue };
        let c = super::geometry::dist(*port, *st);
        if c < cost[i * 4 + d] {
            cost[i * 4 + d] = c;
            heap.push((key(c + h(i)), i * 4 + d));
        }
    }
    let mut best: Option<(f64, usize, usize)> = None;
    while let Some((Reverse(bits), state)) = heap.pop() {
        let f = f64::from_bits(bits);
        if best.is_some_and(|b| f >= b.0) {
            break;
        }
        let (i, d) = (state / 4, state % 4);
        let c = cost[state];
        if f > c + h(i) + 1e-6 {
            continue;
        }
        for &(j, side) in &ends {
            if j == i {
                // Going in means moving opposite to the side's normal.
                let turn = if d == (side + 2) % 4 { 0.0 } else if d == side { 2.0 * BEND } else { BEND };
                let (port, st) = sb[side].unwrap();
                let total = c + turn + super::geometry::dist(port, st);
                if best.is_none_or(|b| total < b.0) {
                    best = Some((total, state, side));
                }
            }
        }
        for d2 in 0..4 {
            if d2 == (d + 2) % 4 {
                continue;
            }
            let Some(j) = step(i, d2) else { continue };
            let (lo, axis) = (i.min(j), usize::from(d2 % 2 == 0));
            if open_seg[lo][axis] == 0 {
                open_seg[lo][axis] = if blocked(pt(i), pt(j)) { 2 } else { 1 };
            }
            if open_seg[lo][axis] == 2 {
                continue;
            }
            let nc = c + super::geometry::dist(pt(i), pt(j)) + if d2 == d { 0.0 } else { BEND };
            let ns = j * 4 + d2;
            if nc < cost[ns] {
                cost[ns] = nc;
                prev[ns] = state;
                heap.push((key(nc + h(j)), ns));
            }
        }
    }
    let (_, last, side_b) = best?;
    let mut chain = vec![last];
    while prev[*chain.last().unwrap()] != usize::MAX {
        chain.push(prev[*chain.last().unwrap()]);
    }
    chain.reverse();
    let side_a = chain[0] % 4;
    let mut pts = vec![sa[side_a].unwrap().0];
    pts.extend(chain.iter().map(|&s| pt(s / 4)));
    pts.push(sb[side_b].unwrap().0);
    let pts = simplify(pts);
    Some(Route { label_at: label_point(&pts), points: pts, start: Some(SIDES[side_a]), end: Some(SIDES[side_b]), reversed: false })
}

/// Segments of `pts` passing through obstacles, or back through the end nodes.
fn count_hits(pts: &[Pt], a: &Rect, b: &Rect, obstacles: &[Rect]) -> usize {
    let segs = pts.len().saturating_sub(1);
    let mut hits = 0;
    for i in 0..segs {
        let (p, q) = (pts[i], pts[i + 1]);
        let (x0, x1, y0, y1) = (p.0.min(q.0), p.0.max(q.0), p.1.min(q.1), p.1.max(q.1));
        hits += obstacles.iter().filter(|r| r.x < x1 && r.right() > x0 && r.y < y1 && r.bottom() > y0 && segment_hits_rect(p, q, r, 1.0)).count();
        if i > 0 && segment_hits_rect(p, q, a, 1.0) {
            hits += 1;
        }
        if i + 1 < segs && segment_hits_rect(p, q, b, 1.0) {
            hits += 1;
        }
    }
    hits
}

#[allow(clippy::too_many_arguments)]
fn best_ortho(a: &Rect, ao: Outline, b: &Rect, bo: Outline, gap: f64, o: f64, obstacles: &[Rect], (xg, yg): (&[f64], &[f64])) -> Option<(Route, usize)> {
    use Side::*;
    let (ca, cb) = (a.center(), b.center());
    let on = |r: &Rect, ol: Outline, s: Side, t: f64| match s {
        Left | Right => ray_exit(r, ol, (r.center().0, t), s.normal(), gap),
        Top | Bottom => ray_exit(r, ol, (t, r.center().1), s.normal(), gap),
    };
    let ya = within(ca.1 + o, a.y + 2.0, a.bottom() - 2.0);
    let yb = within(cb.1 + o, b.y + 2.0, b.bottom() - 2.0);
    let xa = within(ca.0 + o, a.x + 2.0, a.right() - 2.0);
    let xb = within(cb.0 + o, b.x + 2.0, b.right() - 2.0);
    let near = |lo: f64, hi: f64, list: &[f64]| {
        let mid = (lo + hi) / 2.0;
        let mut v: Vec<f64> = list.iter().copied().filter(|&g| g > lo && g < hi).collect();
        v.sort_by(|p, q| (p - mid).abs().total_cmp(&(q - mid).abs()));
        v.truncate(3);
        v.insert(0, mid);
        v
    };
    let mut cands: Vec<(Vec<Pt>, Side, Side)> = Vec::new();
    // Side by side: straight across, or a Z bending in a gutter.
    let (hl, hr) = if cb.0 >= ca.0 { (a.right(), b.x) } else { (b.right(), a.x) };
    if hr - hl > 2.0 * gap {
        let (sa, sb) = if cb.0 >= ca.0 { (Right, Left) } else { (Left, Right) };
        let (lo, hi) = (a.y.max(b.y) + 4.0, a.bottom().min(b.bottom()) - 4.0);
        if lo <= hi {
            let y = ((ca.1 + cb.1) / 2.0 + o).clamp(lo, hi);
            cands.push((vec![on(a, ao, sa, y), on(b, bo, sb, y)], sa, sb));
        }
        for xm in near(hl, hr, xg) {
            cands.push((vec![on(a, ao, sa, ya), (xm + o, ya), (xm + o, yb), on(b, bo, sb, yb)], sa, sb));
        }
    }
    let (vt, vb) = if cb.1 >= ca.1 { (a.bottom(), b.y) } else { (b.bottom(), a.y) };
    if vb - vt > 2.0 * gap {
        let (sa, sb) = if cb.1 >= ca.1 { (Bottom, Top) } else { (Top, Bottom) };
        let (lo, hi) = (a.x.max(b.x) + 4.0, a.right().min(b.right()) - 4.0);
        if lo <= hi {
            let x = ((ca.0 + cb.0) / 2.0 + o).clamp(lo, hi);
            cands.push((vec![on(a, ao, sa, x), on(b, bo, sb, x)], sa, sb));
        }
        for ym in near(vt, vb, yg) {
            cands.push((vec![on(a, ao, sa, xa), (xa, ym + o), (xb, ym + o), on(b, bo, sb, xb)], sa, sb));
        }
    }
    // L shapes.
    if (xb > a.right() || xb < a.x) && (ya < b.y || ya > b.bottom()) {
        let sa = if xb > a.right() { Right } else { Left };
        let sb = if ya < b.y { Top } else { Bottom };
        cands.push((vec![on(a, ao, sa, ya), (xb, ya), on(b, bo, sb, xb)], sa, sb));
    }
    if (xa > b.right() || xa < b.x) && (yb < a.y || yb > a.bottom()) {
        let sa = if yb < a.y { Top } else { Bottom };
        let sb = if xa > b.right() { Right } else { Left };
        cands.push((vec![on(a, ao, sa, xa), (xa, yb), on(b, bo, sb, yb)], sa, sb));
    }
    // Around both, through a gutter above, below, left or right.
    let margin = (gap * 3.0).max(20.0);
    // The two nearest gutters beyond `limit` (below it if `less`), plus a margin line.
    let beyond = |list: &[f64], limit: f64, less: bool| {
        let mut v: Vec<f64> = list.iter().copied().filter(|&g| if less { g < limit } else { g > limit }).collect();
        v.sort_by(|p, q| (p - limit).abs().total_cmp(&(q - limit).abs()));
        v.truncate(2);
        v.push(if less { limit - margin } else { limit + margin });
        v
    };
    for y in beyond(yg, a.y.min(b.y), true) {
        cands.push((vec![on(a, ao, Top, xa), (xa, y + o), (xb, y + o), on(b, bo, Top, xb)], Top, Top));
    }
    for y in beyond(yg, a.bottom().max(b.bottom()), false) {
        cands.push((vec![on(a, ao, Bottom, xa), (xa, y + o), (xb, y + o), on(b, bo, Bottom, xb)], Bottom, Bottom));
    }
    for x in beyond(xg, a.x.min(b.x), true) {
        cands.push((vec![on(a, ao, Left, ya), (x + o, ya), (x + o, yb), on(b, bo, Left, yb)], Left, Left));
    }
    for x in beyond(xg, a.right().max(b.right()), false) {
        cands.push((vec![on(a, ao, Right, ya), (x + o, ya), (x + o, yb), on(b, bo, Right, yb)], Right, Right));
    }
    cands
        .into_iter()
        .map(|(pts, sa, sb)| {
            let pts = simplify(pts);
            let hits = count_hits(&pts, a, b, obstacles);
            let len: f64 = pts.windows(2).map(|w| super::geometry::dist(w[0], w[1])).sum();
            let score = hits as f64 * 1e6 + len + 30.0 * pts.len() as f64;
            (score, hits, pts, sa, sb)
        })
        .min_by(|p, q| p.0.total_cmp(&q.0))
        .map(|(_, hits, pts, sa, sb)| (Route { label_at: label_point(&pts), points: pts, start: Some(sa), end: Some(sb), reversed: false }, hits))
}

// ---------- sequence diagrams ----------

pub fn sequence(input: &SeqInput) -> SeqOutput {
    let mut parts: Vec<&LNode> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for n in &input.participants {
        if !index.contains_key(n.id.as_str()) {
            index.insert(&n.id, parts.len());
            parts.push(n);
        }
    }
    let n = parts.len();
    let size: Vec<(f64, f64)> = parts.iter().map(|p| (fin(p.w).max(1.0), fin(p.h).max(1.0))).collect();
    let gap = fin(input.participant_gap).max(0.0);
    let mg = fin(input.message_gap).max(0.0);
    let label = |m: &SeqMessage| m.label.map(|(w, h)| (fin(w).max(0.0), fin(h).max(0.0)));
    let ends: Vec<Option<(usize, usize)>> = input.messages.iter().map(|m| Some((*index.get(m.from.as_str())?, *index.get(m.to.as_str())?))).collect();

    // Centre-to-centre spacing: wide enough for the boxes and for every
    // label between the lifelines it spans.
    let mut space: Vec<f64> = (1..n).map(|i| (size[i - 1].0 + size[i].0) / 2.0 + gap).collect();
    let mut reqs: Vec<(usize, usize, f64)> = Vec::new();
    for (m, e) in input.messages.iter().zip(&ends) {
        let (Some((a, b)), Some((lw, _))) = (*e, label(m)) else { continue };
        if a != b {
            reqs.push((a.min(b), a.max(b), lw + 24.0));
        } else if a + 1 < n {
            reqs.push((a, a + 1, SEQ_LOOP + 8.0 + lw + 16.0));
        }
    }
    reqs.sort_by_key(|r| r.1 - r.0);
    for (lo, hi, need) in reqs {
        let cur: f64 = space[lo..hi].iter().sum();
        if cur < need {
            let add = (need - cur) / (hi - lo) as f64;
            space[lo..hi].iter_mut().for_each(|s| *s += add);
        }
    }
    let mut cx = vec![0.0; n];
    if n > 0 {
        cx[0] = size[0].0 / 2.0;
    }
    for i in 1..n {
        cx[i] = cx[i - 1] + space[i - 1];
    }
    let header_h = size.iter().map(|s| s.1).fold(0.0, f64::max);

    let mut cursor = header_h;
    let mut messages = Vec::with_capacity(input.messages.len());
    for (m, e) in input.messages.iter().zip(&ends) {
        let Some((a, b)) = *e else {
            messages.push(EdgeRoute { points: Vec::new(), start_side: None, end_side: None, style: EdgeStyle::Straight, label_at: (0.0, 0.0), reversed: false });
            continue;
        };
        let lab = label(m);
        cursor += mg + lab.map_or(0.0, |l| l.1 + 4.0);
        let y = cursor;
        let (xa, xb) = (cx[a], cx[b]);
        if a == b {
            let lw = lab.map_or(0.0, |l| l.0);
            let y2 = y + mg / 2.0;
            messages.push(EdgeRoute {
                points: vec![(xa, y), (xa + SEQ_LOOP, y), (xa + SEQ_LOOP, y2), (xa, y2)],
                start_side: None,
                end_side: None,
                style: EdgeStyle::Elbow,
                label_at: (xa + SEQ_LOOP + 8.0 + lw / 2.0, (y + y2) / 2.0),
                reversed: false,
            });
            cursor = y2;
        } else {
            let lh = lab.map_or(0.0, |l| l.1);
            messages.push(EdgeRoute {
                points: vec![(xa, y), (xb, y)],
                start_side: None,
                end_side: None,
                style: EdgeStyle::Straight,
                label_at: ((xa + xb) / 2.0, y - lh / 2.0 - 4.0),
                reversed: false,
            });
        }
    }
    let footer_top = cursor + mg;

    let mut rects: Vec<Rect> = Vec::new();
    let headers: Vec<Rect> = (0..n).map(|i| Rect::new(cx[i] - size[i].0 / 2.0, 0.0, size[i].0, size[i].1)).collect();
    let footers: Vec<Rect> = (0..n).map(|i| Rect::new(cx[i] - size[i].0 / 2.0, footer_top, size[i].0, size[i].1)).collect();
    rects.extend(headers.iter().chain(&footers).copied());
    for (r, m) in messages.iter().zip(&input.messages) {
        rects.extend(r.points.iter().map(|p| Rect::new(p.0, p.1, 0.0, 0.0)));
        if let (Some((w, h)), false) = (label(m), r.points.is_empty()) {
            rects.push(Rect::new(r.label_at.0 - w / 2.0, r.label_at.1 - h / 2.0, w, h));
        }
    }
    let b = union_all(rects).unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
    let shift = |p: Pt| (p.0 - b.x, p.1 - b.y);
    let shift_rect = |r: &Rect| Rect::new(r.x - b.x, r.y - b.y, r.w, r.h);
    for m in &mut messages {
        if !m.points.is_empty() {
            m.points.iter_mut().for_each(|p| *p = shift(*p));
            m.label_at = shift(m.label_at);
        }
    }
    let id = |i: usize| parts[i].id.clone();
    SeqOutput {
        participants: (0..n).map(|i| (id(i), shift_rect(&headers[i]))).collect(),
        footers: (0..n).map(|i| (id(i), shift_rect(&footers[i]))).collect(),
        lifelines: (0..n).map(|i| (id(i), (shift((cx[i], headers[i].bottom())), shift((cx[i], footer_top))))).collect(),
        messages,
        bounds: Rect::new(0.0, 0.0, b.w, b.h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::geometry::{dist, smooth};

    fn node(id: &str, w: f64, h: f64) -> LNode {
        LNode { id: id.into(), w, h, outline: Outline::Rect, cluster: None }
    }
    fn shaped(id: &str, w: f64, h: f64, outline: Outline) -> LNode {
        LNode { outline, ..node(id, w, h) }
    }
    fn member(id: &str, cluster: &str) -> LNode {
        LNode { cluster: Some(cluster.into()), ..node(id, 120.0, 50.0) }
    }
    fn edge(a: &str, b: &str) -> LEdge {
        LEdge { from: a.into(), to: b.into(), label: None }
    }
    fn labelled(a: &str, b: &str, w: f64, h: f64) -> LEdge {
        LEdge { label: Some((w, h)), ..edge(a, b) }
    }
    fn input(kind: Kind, nodes: Vec<LNode>, edges: Vec<LEdge>) -> LayoutInput {
        LayoutInput {
            kind,
            dir: Dir::Down,
            edge_style: EdgeStyle::Elbow,
            nodes,
            edges,
            node_gap: 40.0,
            rank_gap: 70.0,
            columns: None,
            cluster_padding: 16.0,
            cluster_title: 28.0,
            edge_gap: 6.0,
        }
    }

    /// An architecture diagram: three clusters, a cycle, labels, a self-loop.
    fn architecture() -> LayoutInput {
        let nodes = vec![
            member("web", "Clients"),
            member("mobile", "Clients"),
            node("cdn", 110.0, 50.0),
            member("gateway", "Backend"),
            member("auth", "Backend"),
            member("users", "Backend"),
            member("orders", "Backend"),
            member("payments", "Backend"),
            member("postgres", "Data"),
            member("redis", "Data"),
            member("kafka", "Data"),
            node("worker", 120.0, 50.0),
            shaped("email", 120.0, 60.0, Outline::Ellipse),
            node("analytics", 130.0, 50.0),
            shaped("monitor", 130.0, 80.0, Outline::Diamond),
        ];
        let edges = vec![
            edge("web", "cdn"),
            edge("web", "gateway"),
            edge("mobile", "gateway"),
            edge("gateway", "auth"),
            edge("gateway", "users"),
            edge("gateway", "orders"),
            labelled("orders", "payments", 60.0, 20.0),
            labelled("payments", "orders", 70.0, 20.0),
            edge("users", "postgres"),
            edge("orders", "postgres"),
            edge("auth", "redis"),
            labelled("orders", "kafka", 50.0, 20.0),
            edge("payments", "kafka"),
            edge("kafka", "worker"),
            edge("worker", "email"),
            edge("worker", "analytics"),
            edge("monitor", "gateway"),
            edge("monitor", "worker"),
            edge("worker", "worker"),
            edge("ghost", "web"),
        ];
        input(Kind::Flow, nodes, edges)
    }

    fn org_chart() -> LayoutInput {
        let names = ["CEO", "CTO", "CFO", "COO", "Platform", "Product", "Data", "Accounting", "Legal", "Ops", "Support", "Infra", "SRE", "Web", "Mobile"];
        let nodes = names.iter().map(|n| node(n, 110.0, 48.0)).collect();
        let pairs = [
            ("CEO", "CTO"),
            ("CEO", "CFO"),
            ("CEO", "COO"),
            ("CTO", "Platform"),
            ("CTO", "Product"),
            ("CTO", "Data"),
            ("CFO", "Accounting"),
            ("CFO", "Legal"),
            ("COO", "Ops"),
            ("COO", "Support"),
            ("Platform", "Infra"),
            ("Platform", "SRE"),
            ("Product", "Web"),
            ("Product", "Mobile"),
        ];
        input(Kind::Tree, nodes, pairs.iter().map(|(a, b)| edge(a, b)).collect())
    }

    fn mind_map() -> LayoutInput {
        let mut nodes = vec![shaped("Launch plan", 160.0, 70.0, Outline::Ellipse)];
        let mut edges = Vec::new();
        let branches = [
            ("Research", vec!["Users", "Competitors", "Pricing"]),
            ("Design", vec!["Wireframes", "Brand"]),
            ("Build", vec!["Backend", "Frontend", "Mobile app", "QA"]),
            ("Marketing", vec!["Blog", "Social"]),
            ("Launch", vec!["Press", "Waitlist"]),
            ("Support", vec!["Docs", "Chat"]),
        ];
        for (b, kids) in &branches {
            nodes.push(node(b, 110.0, 44.0));
            edges.push(edge("Launch plan", b));
            for k in kids {
                nodes.push(node(k, 40.0 + 9.0 * k.len() as f64, 36.0));
                edges.push(edge(b, k));
            }
        }
        let mut i = input(Kind::Mindmap, nodes, edges);
        i.edge_style = EdgeStyle::Curved;
        i
    }

    fn rects(out: &LayoutOutput) -> Vec<(&String, Rect)> {
        let mut v: Vec<(&String, Rect)> = out.nodes.iter().map(|(k, r)| (k, *r)).collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        v
    }

    fn assert_no_overlap(out: &LayoutOutput) {
        let r = rects(out);
        for i in 0..r.len() {
            for j in i + 1..r.len() {
                assert!(!r[i].1.inflate(-0.5).intersects(&r[j].1), "{} overlaps {}: {:?} {:?}", r[i].0, r[j].0, r[i].1, r[j].1);
            }
        }
    }

    fn assert_finite(out: &LayoutOutput) {
        let ok = |v: f64| v.is_finite();
        for r in out.nodes.values().chain(out.clusters.iter().map(|c| &c.1)).chain([&out.bounds]) {
            assert!(ok(r.x) && ok(r.y) && ok(r.w) && ok(r.h));
        }
        for e in &out.edges {
            assert!(e.points.iter().all(|p| ok(p.0) && ok(p.1)) && ok(e.label_at.0) && ok(e.label_at.1));
        }
        assert!(out.bounds.x == 0.0 && out.bounds.y == 0.0);
        for r in out.nodes.values() {
            assert!(r.x >= -1e-6 && r.y >= -1e-6 && r.right() <= out.bounds.w + 1e-6 && r.bottom() <= out.bounds.h + 1e-6);
        }
    }

    /// How far outside a node's outline a point is (negative inside).
    fn outline_distance(r: &Rect, o: Outline, p: Pt) -> f64 {
        match o {
            Outline::Rect => {
                let ox = (r.x - p.0).max(p.0 - r.right()).max(0.0);
                let oy = (r.y - p.1).max(p.1 - r.bottom()).max(0.0);
                if ox == 0.0 && oy == 0.0 { -1.0 } else { (ox * ox + oy * oy).sqrt() }
            }
            _ => {
                let c = r.center();
                dist(p, c) - dist(boundary_point(r, o, p, 0.0), c)
            }
        }
    }

    fn assert_ends_on_outlines(inp: &LayoutInput, out: &LayoutOutput) {
        let by_id: HashMap<&str, &LNode> = inp.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        for (i, (e, r)) in inp.edges.iter().zip(&out.edges).enumerate() {
            let (Some(a), Some(b)) = (by_id.get(e.from.as_str()), by_id.get(e.to.as_str())) else {
                assert!(r.points.is_empty());
                continue;
            };
            assert!(r.points.len() >= 2, "edge {i} has no route");
            let (ra, rb) = (out.nodes[&a.id], out.nodes[&b.id]);
            let s = outline_distance(&ra, a.outline, r.points[0]);
            let t = outline_distance(&rb, b.outline, *r.points.last().unwrap());
            assert!(s > 0.0 && s <= inp.edge_gap + 1.0, "edge {i} starts {s} from {}", a.id);
            assert!(t > 0.0 && t <= inp.edge_gap + 1.0, "edge {i} ends {t} from {}", b.id);
        }
    }

    fn assert_axis_aligned(out: &LayoutOutput) {
        for (i, e) in out.edges.iter().enumerate() {
            for w in e.points.windows(2) {
                let (dx, dy) = ((w[0].0 - w[1].0).abs(), (w[0].1 - w[1].1).abs());
                assert!(dx < 1e-6 || dy < 1e-6, "edge {i} has a diagonal segment {:?}", w);
            }
        }
    }

    fn assert_clusters_exact(inp: &LayoutInput, out: &LayoutOutput) {
        for (name, b) in &out.clusters {
            for n in &inp.nodes {
                let Some(r) = out.nodes.get(&n.id) else { continue };
                if n.cluster.as_deref() == Some(name.as_str()) {
                    assert!(b.contains_rect(r, 0.01), "{} not inside {name}", n.id);
                } else {
                    assert!(!b.intersects(r), "{} intrudes into {name}", n.id);
                }
            }
        }
        for i in 0..out.clusters.len() {
            for j in i + 1..out.clusters.len() {
                assert!(!out.clusters[i].1.intersects(&out.clusters[j].1), "clusters {} and {} overlap", out.clusters[i].0, out.clusters[j].0);
            }
        }
    }

    /// Edge routes don't pass through nodes other than their own ends.
    fn assert_routes_clear(inp: &LayoutInput, out: &LayoutOutput) {
        for (i, (e, r)) in inp.edges.iter().zip(&out.edges).enumerate() {
            for (id, rect) in &out.nodes {
                if *id == e.from || *id == e.to {
                    continue;
                }
                for w in r.points.windows(2) {
                    assert!(!segment_hits_rect(w[0], w[1], rect, 1.0), "edge {i} ({}->{}) crosses {id}", e.from, e.to);
                }
            }
        }
    }

    fn assert_direction(inp: &LayoutInput, out: &LayoutOutput) {
        for (e, r) in inp.edges.iter().zip(&out.edges) {
            if r.reversed || e.from == e.to || r.points.is_empty() {
                continue;
            }
            let (a, b) = (out.nodes[&e.from].center(), out.nodes[&e.to].center());
            let ok = match inp.dir {
                Dir::Down => b.1 > a.1,
                Dir::Up => b.1 < a.1,
                Dir::Right => b.0 > a.0,
                Dir::Left => b.0 < a.0,
            };
            assert!(ok, "{:?}: {} -> {} goes the wrong way", inp.dir, e.from, e.to);
        }
    }

    #[test]
    fn flow_architecture_is_clean() {
        let inp = architecture();
        let out = layout(&inp);
        assert_eq!(out.nodes.len(), 15);
        assert_eq!(out.edges.len(), inp.edges.len());
        assert_finite(&out);
        assert_no_overlap(&out);
        assert_ends_on_outlines(&inp, &out);
        assert_axis_aligned(&out);
        assert_clusters_exact(&inp, &out);
        assert_routes_clear(&inp, &out);
        assert_eq!(out.clusters.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["Clients", "Backend", "Data"]);
        // The orders <-> payments cycle: exactly one of the pair is reversed.
        assert!(out.edges[6].reversed != out.edges[7].reversed);
        // Unknown ids give an empty route.
        assert!(out.edges[19].points.is_empty());
        // The self-loop leaves and returns on the right.
        let lp = &out.edges[18];
        assert!(lp.points.len() >= 3 && lp.start_side == Some(Side::Right) && lp.end_side == Some(Side::Right));
        // Labels don't sit on nodes.
        for (e, r) in inp.edges.iter().zip(&out.edges) {
            if let (Some((w, h)), false) = (e.label, r.points.is_empty()) {
                let lr = Rect::new(r.label_at.0 - w / 2.0, r.label_at.1 - h / 2.0, w, h);
                for (id, n) in &out.nodes {
                    assert!(!lr.intersects(n), "label of {}->{} covers {id}", e.from, e.to);
                }
            }
        }
    }

    #[test]
    fn flow_respects_direction() {
        for dir in [Dir::Down, Dir::Up, Dir::Right, Dir::Left] {
            for style in [EdgeStyle::Elbow, EdgeStyle::Straight, EdgeStyle::Curved] {
                let mut inp = architecture();
                inp.dir = dir;
                inp.edge_style = style;
                let out = layout(&inp);
                assert_finite(&out);
                assert_no_overlap(&out);
                assert_direction(&inp, &out);
                assert_ends_on_outlines(&inp, &out);
                assert_clusters_exact(&inp, &out);
                if style == EdgeStyle::Elbow {
                    assert_axis_aligned(&out);
                }
                if style == EdgeStyle::Straight {
                    assert_routes_clear(&inp, &out);
                }
            }
        }
    }

    #[test]
    fn flow_sides_follow_direction() {
        let mut inp = input(Kind::Flow, vec![node("a", 100.0, 40.0), node("b", 100.0, 40.0)], vec![edge("a", "b")]);
        for (dir, s, t) in [(Dir::Down, Side::Bottom, Side::Top), (Dir::Up, Side::Top, Side::Bottom), (Dir::Right, Side::Right, Side::Left), (Dir::Left, Side::Left, Side::Right)] {
            inp.dir = dir;
            let out = layout(&inp);
            assert_eq!((out.edges[0].start_side, out.edges[0].end_side), (Some(s), Some(t)), "{dir:?}");
            // Aligned nodes: one straight segment.
            assert_eq!(out.edges[0].points.len(), 2);
        }
    }

    #[test]
    fn flow_chain_is_straight_and_parents_centred() {
        let nodes = ["a", "b", "c", "d", "e"].iter().map(|n| node(n, 100.0, 40.0)).collect();
        let inp = input(Kind::Flow, nodes, vec![edge("a", "b"), edge("b", "c"), edge("b", "d"), edge("b", "e")]);
        let out = layout(&inp);
        let cx = |n: &str| out.nodes[n].center().0;
        assert!((cx("a") - cx("b")).abs() < 0.5);
        assert!((cx("b") - (cx("c") + cx("e")) / 2.0).abs() < 0.5);
        assert!((cx("d") - cx("b")).abs() < 0.5);
        assert_eq!(out.edges[0].points.len(), 2);
        // Three edges leave b's bottom at distinct points.
        let starts: Vec<f64> = out.edges[1..].iter().map(|e| e.points[0].0).collect();
        assert!(starts[0] < starts[1] && starts[1] < starts[2]);
    }

    #[test]
    fn flow_long_edges_and_cycles() {
        let nodes: Vec<LNode> = ["a", "b", "c", "d", "x"].iter().map(|n| node(n, 80.0, 40.0)).collect();
        let edges = vec![edge("a", "b"), edge("b", "c"), edge("c", "d"), edge("a", "d"), edge("d", "a"), edge("x", "c")];
        for style in [EdgeStyle::Elbow, EdgeStyle::Straight, EdgeStyle::Curved] {
            let mut inp = input(Kind::Flow, nodes.clone(), edges.clone());
            inp.edge_style = style;
            let out = layout(&inp);
            assert_no_overlap(&out);
            assert_ends_on_outlines(&inp, &out);
            assert!(out.edges[4].reversed && out.edges.iter().filter(|e| e.reversed).count() == 1);
            // The reversed edge still runs from d to a.
            let (d, a) = (out.nodes["d"], out.nodes["a"]);
            let e = &out.edges[4];
            assert!(d.inflate(8.0).contains(e.points[0]) && a.inflate(8.0).contains(*e.points.last().unwrap()));
            assert!(out.edges[3].points.len() >= 3, "{style:?}: long edges bend through their lanes");
            if style == EdgeStyle::Straight {
                assert_eq!(out.edges[0].points.len(), 2);
            }
            if style != EdgeStyle::Curved {
                assert_routes_clear(&inp, &out);
            }
            // x is pulled down next to c instead of sitting on top.
            assert_eq!(out.depth["x"] + 1, out.depth["c"]);
        }
    }

    #[test]
    fn flow_multi_edges_are_apart() {
        for style in [EdgeStyle::Elbow, EdgeStyle::Straight, EdgeStyle::Curved] {
            let mut inp = input(Kind::Flow, vec![node("a", 100.0, 40.0), node("b", 100.0, 40.0)], vec![edge("a", "b"), edge("a", "b"), edge("b", "a")]);
            inp.edge_style = style;
            let out = layout(&inp);
            let s: Vec<Pt> = out.edges.iter().map(|e| e.points[0]).collect();
            assert!(dist(s[0], s[1]) > 5.0 && dist(s[0], s[2]) > 5.0, "{style:?}");
        }
    }

    #[test]
    fn flow_components_side_by_side() {
        let nodes = ["a", "b", "c", "d", "e", "lonely"].iter().map(|n| node(n, 90.0, 40.0)).collect();
        let inp = input(Kind::Flow, nodes, vec![edge("a", "b"), edge("c", "d"), edge("d", "e"), edge("c", "e")]);
        let out = layout(&inp);
        assert_no_overlap(&out);
        // The largest component comes first.
        assert!(out.nodes["c"].x < out.nodes["a"].x && out.nodes["a"].x < out.nodes["lonely"].x);
        assert_eq!(out.nodes["lonely"].y, 0.0);
    }

    #[test]
    fn flow_cluster_members_without_edges_stay_together() {
        let nodes = vec![member("a", "K"), node("b", 100.0, 40.0), member("c", "K"), member("d", "J"), node("e", 60.0, 60.0)];
        let edges = vec![edge("b", "a"), edge("b", "e"), edge("e", "d"), edge("a", "d")];
        let mut inp = input(Kind::Flow, nodes, edges);
        for dir in [Dir::Down, Dir::Right] {
            inp.dir = dir;
            let out = layout(&inp);
            assert_no_overlap(&out);
            assert_clusters_exact(&inp, &out);
        }
    }

    #[test]
    fn diamond_corners_are_used_for_branches() {
        let nodes = vec![node("start", 100.0, 40.0), shaped("ok?", 120.0, 70.0, Outline::Diamond), node("yes", 100.0, 40.0), node("no", 100.0, 40.0)];
        let edges = vec![edge("start", "ok?"), labelled("ok?", "yes", 30.0, 20.0), labelled("ok?", "no", 30.0, 20.0)];
        let inp = input(Kind::Flow, nodes, edges);
        let out = layout(&inp);
        let sides: Vec<_> = out.edges[1..].iter().map(|e| e.start_side.unwrap()).collect();
        assert!(sides.contains(&Side::Left) || sides.contains(&Side::Right), "{sides:?}");
        assert_ends_on_outlines(&inp, &out);
        assert_axis_aligned(&out);
    }

    #[test]
    fn empty_and_single() {
        for kind in [Kind::Flow, Kind::Tree, Kind::Mindmap, Kind::Grid, Kind::Radial] {
            let out = layout(&input(kind, vec![], vec![edge("a", "b")]));
            assert!(out.nodes.is_empty() && out.edges.len() == 1 && out.edges[0].points.is_empty());
            assert_eq!(out.bounds, Rect::new(0.0, 0.0, 0.0, 0.0));
            let out = layout(&input(kind, vec![node("a", 100.0, 50.0), node("a", 10.0, 10.0)], vec![edge("a", "zzz")]));
            assert_eq!(out.nodes["a"], Rect::new(0.0, 0.0, 100.0, 50.0), "{kind:?}");
            assert_eq!(out.bounds, Rect::new(0.0, 0.0, 100.0, 50.0));
        }
    }

    #[test]
    fn bad_numbers_stay_finite() {
        for kind in [Kind::Flow, Kind::Tree, Kind::Mindmap, Kind::Grid, Kind::Radial] {
            let mut inp = input(kind, vec![node("a", f64::NAN, 50.0), node("b", -5.0, f64::INFINITY), node("c", 0.0, 0.0)], vec![edge("a", "b"), labelled("b", "c", f64::NAN, 10.0)]);
            inp.node_gap = f64::NAN;
            inp.rank_gap = -10.0;
            let out = layout(&inp);
            assert_finite(&out);
        }
    }

    #[test]
    fn tree_parents_centred_over_children() {
        for dir in [Dir::Down, Dir::Right, Dir::Up, Dir::Left] {
            for style in [EdgeStyle::Elbow, EdgeStyle::Straight, EdgeStyle::Curved] {
                let mut inp = org_chart();
                inp.dir = dir;
                inp.edge_style = style;
                let out = layout(&inp);
                assert_finite(&out);
                assert_no_overlap(&out);
                assert_ends_on_outlines(&inp, &out);
                assert_direction(&inp, &out);
                let across = |n: &str| {
                    let c = out.nodes[n].center();
                    if matches!(dir, Dir::Down | Dir::Up) { c.0 } else { c.1 }
                };
                for (p, a, b) in [("CEO", "CTO", "COO"), ("CTO", "Platform", "Data"), ("Product", "Web", "Mobile")] {
                    assert!((across(p) - (across(a) + across(b)) / 2.0).abs() < 0.5, "{dir:?} {p}");
                }
                if style == EdgeStyle::Elbow {
                    assert_axis_aligned(&out);
                }
                assert_eq!(out.depth["CEO"], 0);
                assert_eq!(out.depth["Infra"], 3);
                assert_eq!(out.branch["CEO"], None);
                assert_eq!(out.branch["Infra"], Some(0));
                assert_eq!(out.branch["Support"], Some(2));
            }
        }
    }

    #[test]
    fn tree_levels_and_extra_edges() {
        let mut inp = org_chart();
        inp.edges.push(edge("Data", "SRE"));
        inp.edges.push(edge("CEO", "CTO"));
        let out = layout(&inp);
        assert_no_overlap(&out);
        assert_ends_on_outlines(&inp, &out);
        assert_axis_aligned(&out);
        let y = |n: &str| out.nodes[n].y;
        assert!(y("CTO") == y("CFO") && y("CFO") == y("COO") && y("Infra") == y("Web"));
        assert!(y("CTO") >= out.nodes["CEO"].bottom() + 70.0);
        // The duplicate CEO->CTO runs beside the tree edge, not on it.
        assert_ne!(out.edges[15].points, out.edges[0].points);
    }

    #[test]
    fn tree_with_cycle_and_forest() {
        let nodes = ["a", "b", "c", "x", "y"].iter().map(|n| node(n, 80.0, 40.0)).collect();
        let inp = input(Kind::Tree, nodes, vec![edge("a", "b"), edge("b", "c"), edge("c", "a"), edge("x", "y")]);
        let out = layout(&inp);
        assert_no_overlap(&out);
        assert_eq!(out.depth["x"], 0);
        assert_eq!(out.depth["a"], 0);
        assert_eq!(out.depth["c"], 2);
    }

    #[test]
    fn mindmap_splits_left_and_right() {
        let inp = mind_map();
        let out = layout(&inp);
        assert_finite(&out);
        assert_no_overlap(&out);
        assert_ends_on_outlines(&inp, &out);
        let root = out.nodes["Launch plan"].center();
        let side = |n: &str| out.nodes[n].center().0 > root.0;
        // Clockwise from the top right: the first branches go right, top down.
        assert!(side("Research") && side("Design") && side("Build"));
        assert!(!side("Marketing") && !side("Launch") && !side("Support"));
        assert!(out.nodes["Research"].y < out.nodes["Design"].y && out.nodes["Design"].y < out.nodes["Build"].y);
        assert!(out.nodes["Support"].y < out.nodes["Launch"].y && out.nodes["Launch"].y < out.nodes["Marketing"].y);
        // Children grow outwards on their branch's side.
        assert!(side("QA") && !side("Chat"));
        assert!(out.nodes["QA"].x > out.nodes["Build"].right());
        assert!(out.nodes["Chat"].right() < out.nodes["Support"].x);
        // Parents are centred on their children.
        let cy = |n: &str| out.nodes[n].center().1;
        assert!((cy("Build") - (cy("Backend") + cy("QA")) / 2.0).abs() < 0.5);
        assert_eq!(out.depth["Launch plan"], 0);
        assert_eq!(out.depth["Brand"], 2);
        assert_eq!(out.branch["Launch plan"], None);
        assert_eq!(out.branch["Brand"], Some(1));
        assert_eq!(out.branch["Chat"], Some(5));
        // Curved edges leave the root's side and arrive at the child's facing side.
        let e = &out.edges[0];
        assert_eq!((e.start_side, e.end_side), (Some(Side::Right), Some(Side::Left)));
        let left = &out.edges[inp.edges.iter().position(|e| e.to == "Support").unwrap()];
        assert_eq!((left.start_side, left.end_side), (Some(Side::Left), Some(Side::Right)));
    }

    #[test]
    fn mindmap_unreached_nodes_become_branches() {
        let inp = input(Kind::Mindmap, vec![node("root", 100.0, 50.0), node("a", 60.0, 30.0), node("b", 60.0, 30.0)], vec![edge("root", "a")]);
        let out = layout(&inp);
        assert_no_overlap(&out);
        assert_eq!(out.depth["b"], 1);
        assert_eq!(out.branch["b"], Some(1));
    }

    #[test]
    fn radial_rings_dont_overlap() {
        let mut inp = mind_map();
        inp.kind = Kind::Radial;
        let out = layout(&inp);
        assert_finite(&out);
        assert_no_overlap(&out);
        assert_ends_on_outlines(&inp, &out);
        let c = out.nodes["Launch plan"].center();
        let r = |n: &str| dist(out.nodes[n].center(), c);
        assert!(r("Research") > 50.0 && (r("Research") - r("Support")).abs() < 1e-6);
        assert!(r("Brand") > r("Design"));
        assert!(out.edges.iter().all(|e| e.points.len() == 2 && e.style == EdgeStyle::Straight));
        // The first branch sits straight above the root.
        assert!((out.nodes["Research"].center().0 - c.0).abs() < 1e-6 && out.nodes["Research"].y < c.1);
    }

    #[test]
    fn grid_columns_and_clusters() {
        let nodes: Vec<LNode> = (0..7).map(|i| node(&format!("n{i}"), 80.0 + 10.0 * i as f64, 40.0)).collect();
        let mut inp = input(Kind::Grid, nodes, vec![edge("n0", "n5"), edge("n1", "n2"), edge("n0", "n2")]);
        inp.columns = Some(3);
        let out = layout(&inp);
        assert_no_overlap(&out);
        assert_ends_on_outlines(&inp, &out);
        assert_axis_aligned(&out);
        assert_routes_clear(&inp, &out);
        let cx = |i: usize| out.nodes[&format!("n{i}")].center().0;
        let cy = |i: usize| out.nodes[&format!("n{i}")].center().1;
        assert!((cx(0) - cx(3)).abs() < 1e-6 && (cx(3) - cx(6)).abs() < 1e-6 && (cx(1) - cx(4)).abs() < 1e-6);
        assert!(cy(0) == cy(2) && cy(3) > cy(0) && cy(6) > cy(3));
        let mut xs: Vec<i64> = out.nodes.values().map(|r| r.center().0.round() as i64).collect();
        xs.sort();
        xs.dedup();
        assert_eq!(xs.len(), 3);
        // Default: roughly square.
        inp.columns = None;
        let out = layout(&inp);
        let mut ys: Vec<i64> = out.nodes.values().map(|r| r.center().1.round() as i64).collect();
        ys.sort();
        ys.dedup();
        assert_eq!(ys.len(), 3);

        let nodes = vec![member("a", "X"), member("b", "X"), member("c", "Y"), node("d", 90.0, 40.0), member("e", "X"), member("f", "Y")];
        let mut inp = input(Kind::Grid, nodes, vec![edge("a", "c"), edge("d", "f")]);
        inp.edge_style = EdgeStyle::Straight;
        let out = layout(&inp);
        assert_no_overlap(&out);
        assert_clusters_exact(&inp, &out);
        assert_ends_on_outlines(&inp, &out);
        assert_eq!(out.clusters.len(), 2);

        // Edges between blocks find a way around the cards in between.
        let nodes: Vec<LNode> = (0..10).map(|i| LNode { cluster: Some(["A", "B", "C"][i % 3].into()), ..node(&format!("card{i}"), 120.0, 60.0) }).collect();
        for style in [EdgeStyle::Elbow, EdgeStyle::Straight] {
            let mut inp = input(Kind::Grid, nodes.clone(), vec![edge("card0", "card4"), edge("card1", "card2"), edge("card3", "card9"), edge("card7", "card8")]);
            inp.edge_style = style;
            let out = layout(&inp);
            assert_ends_on_outlines(&inp, &out);
            assert_routes_clear(&inp, &out);
        }
    }

    #[test]
    fn sequence_stacks_messages() {
        let parts = vec![node("user", 100.0, 40.0), node("app", 100.0, 40.0), node("db", 80.0, 40.0)];
        let msg = |a: &str, b: &str, l: Option<(f64, f64)>| SeqMessage { from: a.into(), to: b.into(), label: l };
        let inp = SeqInput {
            participants: parts,
            messages: vec![
                msg("user", "app", Some((60.0, 20.0))),
                msg("app", "db", Some((300.0, 20.0))),
                msg("db", "db", Some((50.0, 20.0))),
                msg("db", "app", None),
                msg("app", "nobody", None),
                msg("app", "user", Some((40.0, 20.0))),
            ],
            participant_gap: 60.0,
            message_gap: 40.0,
            edge_gap: 6.0,
        };
        let out = sequence(&inp);
        let x = |n: &str| out.lifelines[n].0 .0;
        assert!(x("user") < x("app") && x("app") < x("db"));
        assert!(x("db") - x("app") >= 300.0 + 20.0);
        let ys: Vec<f64> = out.messages.iter().filter(|m| !m.points.is_empty()).map(|m| m.points[0].1).collect();
        assert!(ys.windows(2).all(|w| w[1] > w[0]));
        assert!(out.messages[4].points.is_empty());
        let m = &out.messages[0];
        assert_eq!(m.points, vec![(x("user"), m.points[0].1), (x("app"), m.points[0].1)]);
        assert!((m.label_at.1 - (m.points[0].1 - 14.0)).abs() < 1e-9);
        let s = &out.messages[2];
        assert!(s.points.len() >= 3 && s.points.iter().all(|p| p.0 >= x("db")));
        assert_eq!(out.messages[5].points[1].0, x("user"));
        for (id, (top, bottom)) in &out.lifelines {
            assert_eq!(top.1, out.participants[id].bottom());
            assert_eq!(bottom.1, out.footers[id].y);
            assert!(bottom.1 > ys.last().copied().unwrap());
        }
        assert!(out.bounds.x == 0.0 && out.footers.values().all(|r| r.bottom() <= out.bounds.h + 1e-9));
    }

    /// A biggish pseudo-random graph.
    fn big(n: usize, kind: Kind) -> LayoutInput {
        let mut seed = 12345u64;
        let mut rnd = move |m: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % m
        };
        let nodes: Vec<LNode> = (0..n).map(|i| LNode { cluster: (i % 7 == 0).then(|| format!("c{}", i % 3)), ..node(&format!("n{i}"), 80.0 + rnd(60) as f64, 40.0 + rnd(30) as f64) }).collect();
        let mut edges = Vec::new();
        for i in 1..n {
            edges.push(edge(&format!("n{}", rnd(i)), &format!("n{i}")));
            if rnd(3) == 0 {
                edges.push(edge(&format!("n{i}"), &format!("n{}", rnd(n))));
            }
        }
        input(kind, nodes, edges)
    }

    #[test]
    fn big_graphs_hold_up() {
        for kind in [Kind::Flow, Kind::Tree, Kind::Mindmap, Kind::Grid, Kind::Radial] {
            let inp = big(300, kind);
            let t = std::time::Instant::now();
            let out = layout(&inp);
            let ms = t.elapsed().as_millis();
            assert_finite(&out);
            assert_no_overlap(&out);
            assert_clusters_exact(&inp, &out);
            // Generous for debug builds; release is far below.
            assert!(ms < 5000, "{kind:?} took {ms} ms");
            let again = layout(&inp);
            assert_eq!(again.nodes, out.nodes, "{kind:?} is not deterministic");
        }
    }

    fn svg(out: &LayoutOutput, inp: &LayoutInput) -> String {
        // Square canvas: thumbnailers fill a square and would crop anything else.
        let side = out.bounds.w.max(out.bounds.h) + 40.0;
        let mut s = format!("<svg xmlns='http://www.w3.org/2000/svg' width='{side}' height='{side}' viewBox='-20 -20 {side} {side}'><rect x='-20' y='-20' width='{side}' height='{side}' fill='white'/>");
        s += "<defs><marker id='a' viewBox='0 0 10 10' refX='9' refY='5' markerWidth='7' markerHeight='7' orient='auto'><path d='M0,0 L10,5 L0,10 z' fill='#333'/></marker></defs>";
        for (name, r) in &out.clusters {
            s += &format!("<rect x='{}' y='{}' width='{}' height='{}' rx='10' fill='#eef4ff' stroke='#89a' stroke-dasharray='6 4'/><text x='{}' y='{}' font-size='14' font-family='sans-serif' fill='#456'>{name}</text>", r.x, r.y, r.w, r.h, r.x + 10.0, r.y + 20.0);
        }
        for n in &inp.nodes {
            let Some(r) = out.nodes.get(&n.id) else { continue };
            let fill = match out.branch.get(&n.id).copied().flatten() {
                Some(b) => ["#ffd8a8", "#b2f2bb", "#a5d8ff", "#d0bfff", "#ffc9c9", "#fff3bf"][b % 6],
                None => "#f8f9fa",
            };
            let (cx, cy) = r.center();
            s += &match n.outline {
                Outline::Rect => format!("<rect x='{}' y='{}' width='{}' height='{}' rx='6' fill='{fill}' stroke='#333'/>", r.x, r.y, r.w, r.h),
                Outline::Ellipse => format!("<ellipse cx='{cx}' cy='{cy}' rx='{}' ry='{}' fill='{fill}' stroke='#333'/>", r.w / 2.0, r.h / 2.0),
                Outline::Diamond => format!("<polygon points='{},{} {},{} {},{} {},{}' fill='{fill}' stroke='#333'/>", cx, r.y, r.right(), cy, cx, r.bottom(), r.x, cy),
            };
            s += &format!("<text x='{cx}' y='{}' font-size='13' font-family='sans-serif' text-anchor='middle'>{}</text>", cy + 4.0, n.id);
        }
        for (e, r) in inp.edges.iter().zip(&out.edges) {
            if r.points.is_empty() {
                continue;
            }
            let pts = if r.style == EdgeStyle::Curved { smooth(&r.points, 12) } else { r.points.clone() };
            let d: Vec<String> = pts.iter().map(|p| format!("{:.1},{:.1}", p.0, p.1)).collect();
            let color = if r.reversed { "#c33" } else { "#333" };
            s += &format!("<polyline points='{}' fill='none' stroke='{color}' stroke-width='1.6' marker-end='url(#a)'/>", d.join(" "));
            if let Some((w, h)) = e.label {
                s += &format!("<rect x='{}' y='{}' width='{w}' height='{h}' fill='#fff8c4' stroke='#cc9'/>", r.label_at.0 - w / 2.0, r.label_at.1 - h / 2.0);
            }
        }
        s + "</svg>"
    }

    /// Writes SVGs of a few layouts to `$LAYOUT_SVG_DIR` (or the temp dir) to eyeball.
    #[test]
    #[ignore]
    fn dump_svgs() {
        let dir = std::env::var("LAYOUT_SVG_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
        let mut cases: Vec<(String, LayoutInput)> = Vec::new();
        for (dir, style) in [(Dir::Down, EdgeStyle::Elbow), (Dir::Right, EdgeStyle::Elbow), (Dir::Down, EdgeStyle::Curved), (Dir::Down, EdgeStyle::Straight)] {
            let mut i = architecture();
            i.dir = dir;
            i.edge_style = style;
            cases.push((format!("arch-{dir:?}-{style:?}"), i));
        }
        for style in [EdgeStyle::Elbow, EdgeStyle::Curved] {
            let mut i = org_chart();
            i.edge_style = style;
            cases.push((format!("org-{style:?}"), i.clone()));
            i.dir = Dir::Right;
            cases.push((format!("org-right-{style:?}"), i));
        }
        cases.push(("mindmap".into(), mind_map()));
        let mut r = mind_map();
        r.kind = Kind::Radial;
        cases.push(("radial".into(), r));
        let nodes = vec![node("start", 100.0, 40.0), shaped("ok?", 120.0, 70.0, Outline::Diamond), node("retry", 100.0, 40.0), node("done", 100.0, 40.0), shaped("end", 90.0, 50.0, Outline::Ellipse)];
        let edges = vec![edge("start", "ok?"), labelled("ok?", "done", 30.0, 20.0), labelled("ok?", "retry", 24.0, 20.0), edge("retry", "ok?"), edge("done", "end")];
        cases.push(("decision".into(), input(Kind::Flow, nodes, edges)));
        let mut gi = big(300, Kind::Flow);
        gi.nodes.truncate(40);
        gi.edges.retain(|e| e.from[1..].parse::<usize>().unwrap() < 40 && e.to[1..].parse::<usize>().unwrap() < 40);
        cases.push(("big40".into(), gi));
        let mut gr = input(Kind::Grid, (0..10).map(|i| LNode { cluster: Some(["A", "B", "C"][i % 3].into()), ..node(&format!("card{i}"), 120.0, 60.0) }).collect(), vec![edge("card0", "card4"), edge("card1", "card2"), edge("card3", "card9")]);
        gr.edge_style = EdgeStyle::Elbow;
        cases.push(("grid".into(), gr));
        for (name, inp) in cases {
            let out = layout(&inp);
            std::fs::write(dir.join(format!("layout-{name}.svg")), svg(&out, &inp)).unwrap();
        }
    }
}

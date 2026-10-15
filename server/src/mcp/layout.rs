//! Automatic diagram layouts (temporary stand-in).
use super::geometry::{Outline, Pt, Rect, Side};
use std::collections::HashMap;
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Kind { Flow, Tree, Mindmap, Grid, Radial }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Dir { Down, Right, Up, Left }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum EdgeStyle { Straight, Elbow, Curved }
pub struct LNode { pub id: String, pub w: f64, pub h: f64, pub outline: Outline, pub cluster: Option<String> }
pub struct LEdge { pub from: String, pub to: String, pub label: Option<(f64, f64)> }
pub struct LayoutInput { pub kind: Kind, pub dir: Dir, pub edge_style: EdgeStyle, pub nodes: Vec<LNode>, pub edges: Vec<LEdge>, pub node_gap: f64, pub rank_gap: f64, pub columns: Option<usize>, pub cluster_padding: f64, pub cluster_title: f64, pub edge_gap: f64 }
pub struct EdgeRoute { pub points: Vec<Pt>, pub start_side: Option<Side>, pub end_side: Option<Side>, pub style: EdgeStyle, pub label_at: Pt, pub reversed: bool }
pub struct LayoutOutput { pub nodes: HashMap<String, Rect>, pub edges: Vec<EdgeRoute>, pub clusters: Vec<(String, Rect)>, pub depth: HashMap<String, usize>, pub branch: HashMap<String, Option<usize>>, pub bounds: Rect }
pub fn layout(input: &LayoutInput) -> LayoutOutput {
    let mut nodes = HashMap::new(); let mut x = 0.0;
    for n in &input.nodes { nodes.insert(n.id.clone(), Rect::new(x, 0.0, n.w, n.h)); x += n.w + input.node_gap; }
    LayoutOutput { nodes, edges: input.edges.iter().map(|_| EdgeRoute { points: vec![], start_side: None, end_side: None, style: input.edge_style, label_at: (0.0, 0.0), reversed: false }).collect(), clusters: vec![], depth: HashMap::new(), branch: HashMap::new(), bounds: Rect::new(0.0, 0.0, x, 100.0) }
}
pub struct SeqMessage { pub from: String, pub to: String, pub label: Option<(f64, f64)> }
pub struct SeqInput { pub participants: Vec<LNode>, pub messages: Vec<SeqMessage>, pub participant_gap: f64, pub message_gap: f64, pub edge_gap: f64 }
pub struct SeqOutput { pub participants: HashMap<String, Rect>, pub footers: HashMap<String, Rect>, pub lifelines: HashMap<String, (Pt, Pt)>, pub messages: Vec<EdgeRoute>, pub bounds: Rect }
pub fn sequence(_i: &SeqInput) -> SeqOutput { SeqOutput { participants: HashMap::new(), footers: HashMap::new(), lifelines: HashMap::new(), messages: vec![], bounds: Rect::new(0.0, 0.0, 0.0, 0.0) } }

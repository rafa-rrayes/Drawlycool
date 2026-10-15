//! Scene descriptions and layout checks (temporary stand-in).
use super::geometry::{Pt, Rect};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
pub struct PeerView { pub name: String, pub pointer: Option<Pt>, pub selected: Vec<String>, pub is_you: bool }
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum Detail { Summary, Full }
pub struct DescribeOptions { pub region: Option<Rect>, pub ids: Option<Vec<String>>, pub detail: Detail, pub max_elements: usize }
impl Default for DescribeOptions { fn default() -> Self { Self { region: None, ids: None, detail: Detail::Summary, max_elements: 250 } } }
pub fn describe(name: &str, e: &[&Value], _b: Option<&str>, _p: &[PeerView], _o: &DescribeOptions) -> String { format!("{name}: {} elements", e.len()) }
pub fn element_line(el: &Value, _by: &HashMap<&str, &Value>) -> String { format!("{} {}", super::model::id(el), super::model::kind(el)) }
pub fn lint(_e: &[&Value], _t: &HashSet<String>) -> Vec<String> { vec![] }
pub fn free_spot(_e: &[&Value], _w: f64, _h: f64, _m: f64, near: Option<Pt>) -> Pt { near.unwrap_or((0.0, 0.0)) }

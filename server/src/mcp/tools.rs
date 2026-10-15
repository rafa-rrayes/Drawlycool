//! The tools an assistant can call, their schemas and handlers.

use super::args::Args;
use super::describe::{self, Detail, DescribeOptions, PeerView};
use super::draft::Draft;
use super::draw::{self, Cursor, Outcome};
use super::geometry::{self as geo, Pt, Rect};
use super::render::{self, RenderOptions};
use super::{diagram, model, Session};
use crate::{lock, Hub};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

type ToolResult = Result<Vec<Value>, String>;

fn text(s: impl Into<String>) -> Value {
    json!({ "type": "text", "text": s.into() })
}

pub fn definitions() -> Value {
    let region = json!({
        "type": "object",
        "description": "Area in canvas coordinates.",
        "properties": { "x": { "type": "number" }, "y": { "type": "number" }, "width": { "type": "number" }, "height": { "type": "number" } },
        "required": ["x", "y", "width", "height"]
    });
    let ids = json!({ "type": "array", "items": { "type": "string" } });
    json!([
        {
            "name": "list_canvases",
            "title": "List canvases",
            "description": "Lists the canvases on this Drawlycool server, with who is in each one right now.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "open_canvas",
            "title": "Open a canvas",
            "description": "Joins a canvas as a live participant (people there see your name and cursor) and returns a description of what's on it. Pass `canvas` (id or name) to open an existing one, or `create` with a name to make a new one. Everything else works on the canvas you opened.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "canvas": { "type": "string", "description": "Canvas id or (part of its) name." },
                    "create": { "type": "string", "description": "Name for a new canvas to create and open." }
                }
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "describe_canvas",
            "title": "Describe the canvas",
            "description": "Text inventory of the open canvas: every element with id, type, position (x,y = top-left), size, label and style; arrows with what they connect; frames; who is here, where their cursor is and what they have selected (when someone says \"this\" they usually mean their selection); and a free spot for new content. Use `region` or `ids` to focus, `detail: \"full\"` for raw properties.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "region": region,
                    "ids": { "type": "array", "items": { "type": "string" }, "description": "Only these elements (and what's attached to them)." },
                    "detail": { "type": "string", "enum": ["summary", "full"] }
                }
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "view_canvas",
            "title": "Look at the canvas",
            "description": "A PNG picture of the canvas (or part of it) as people see it, with a faint coordinate grid. Use it after drawing to check spacing, alignment, overlaps and overall look, then fix what's off. By default shows everything; zoom in with `ids` or `region`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ids": { "type": "array", "items": { "type": "string" }, "description": "Frame the view around these elements." },
                    "region": region,
                    "grid": { "type": "boolean", "description": "Coordinate grid overlay (default true)." },
                    "max_size": { "type": "integer", "description": "Longest side in pixels (default 1400, max 2400)." }
                }
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "draw",
            "title": "Draw elements",
            "description": DRAW_DESCRIPTION,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "elements": {
                        "type": "array",
                        "description": "Elements to create, or to update when `id` matches an existing element (only the properties you pass change). Processed in order, so later items can refer to earlier ones.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Readable id you choose (e.g. \"api\"). Reuse it to update, connect or place things relative to it. Auto-generated from the label if omitted." },
                                "type": { "type": "string", "enum": ["rectangle", "ellipse", "circle", "diamond", "text", "arrow", "line", "frame", "note", "freedraw"] },
                                "label": { "type": "string", "description": "Text inside a shape or on an arrow; the shape auto-sizes to fit. Use \\n for line breaks." },
                                "text": { "type": "string", "description": "Content of a text element." },
                                "x": { "type": "number" }, "y": { "type": "number" },
                                "width": { "type": "number" }, "height": { "type": "number" },
                                "center": { "type": "array", "items": { "type": "number" }, "description": "[x, y] centre instead of top-left x/y." },
                                "below": { "type": "string", "description": "Place below this element id (also: above, right_of, left_of)." },
                                "above": { "type": "string" }, "right_of": { "type": "string" }, "left_of": { "type": "string" },
                                "gap": { "type": "number", "description": "Distance for relative placement (default 60)." },
                                "align": { "type": "string", "enum": ["start", "center", "end"], "description": "Cross-axis alignment for relative placement (default center)." },
                                "color": { "type": "string", "description": "One hue for the whole element: dark stroke + soft fill of that hue (e.g. \"blue\"). Palette: gray red pink grape violet blue cyan teal green yellow orange bronze, plus black/white/transparent; prefix light- or dark-, or use hue-0…hue-4, or #hex." },
                                "stroke": { "type": "string" }, "fill": { "type": "string" }, "text_color": { "type": "string" },
                                "fill_style": { "type": "string", "enum": ["solid", "hachure", "cross-hatch", "zigzag"] },
                                "stroke_width": { "description": "thin | regular | bold | number" },
                                "stroke_style": { "type": "string", "enum": ["solid", "dashed", "dotted"] },
                                "roughness": { "description": "architect (clean) | artist (default) | cartoonist | 0-2" },
                                "opacity": { "type": "number", "description": "0-100" },
                                "rounded": { "type": "boolean", "description": "Rounded corners (default true for rectangles)." },
                                "font": { "type": "string", "enum": ["hand", "sans", "code", "heading"] },
                                "font_size": { "description": "s=16 | m=20 (default) | l=28 | xl=36 | xxl=48 | number" },
                                "text_align": { "type": "string", "enum": ["left", "center", "right"] },
                                "vertical_align": { "type": "string", "enum": ["top", "middle", "bottom"] },
                                "from": { "description": "Arrow/line start: element id (binds, follows the shape) or [x, y]." },
                                "to": { "description": "Arrow/line end: element id or [x, y]." },
                                "route": { "type": "string", "enum": ["straight", "elbow", "curved"], "description": "straight (default), elbow (right angles, re-routes itself when shapes move) or curved." },
                                "from_side": { "type": "string", "enum": ["top", "right", "bottom", "left"] },
                                "to_side": { "type": "string", "enum": ["top", "right", "bottom", "left"] },
                                "points": { "type": "array", "items": { "type": "array", "items": { "type": "number" } }, "description": "Absolute [x, y] points for lines/freedraw, or waypoints for an arrow between from/to." },
                                "closed": { "type": "boolean", "description": "Line: close it into a polygon (fill applies)." },
                                "arrowheads": { "type": "string", "enum": ["end", "start", "both", "none"] },
                                "start_arrowhead": { "type": "string" }, "end_arrowhead": { "type": "string", "description": "arrow | triangle | triangle_outline | circle | circle_outline | diamond | diamond_outline | bar | crowfoot_one | crowfoot_many | crowfoot_one_or_many | none" },
                                "contains": { "type": "array", "items": { "type": "string" }, "description": "Rectangle/ellipse: a background zone sized around these ids (label goes top-left). Frame: these become its children." },
                                "padding": { "type": "number", "description": "Space around `contains` (default 30, frames 40)." },
                                "name": { "type": "string", "description": "Frame title." },
                                "frame": { "type": "string", "description": "Put the element in this frame." },
                                "group": { "description": "Group name(s); elements in a group select and move together." },
                                "z": { "type": "string", "enum": ["front", "back"] },
                                "angle": { "type": "number", "description": "Rotation in degrees." },
                                "link": { "type": "string" },
                                "locked": { "type": "boolean" }
                            }
                        }
                    },
                    "animate": { "type": "boolean", "description": "Draw piece by piece with your cursor (default true). Set false for big batches you want to appear at once." }
                },
                "required": ["elements"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        },
        {
            "name": "draw_diagram",
            "title": "Draw a laid-out diagram",
            "description": DIAGRAM_DESCRIPTION,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "layout": { "type": "string", "enum": ["flow", "tree", "mindmap", "radial", "grid", "sequence"], "description": "flow: layered graph (flowcharts, pipelines, architectures, state machines). tree: hierarchy/org chart. mindmap: central idea with branches left and right. radial: rings around a centre. grid: cards in rows. sequence: participants and messages over time." },
                    "direction": { "type": "string", "enum": ["down", "right", "up", "left"], "description": "flow/tree direction (default down for flow & tree)." },
                    "nodes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "label": { "type": "string" },
                                "shape": { "type": "string", "enum": ["rectangle", "ellipse", "circle", "diamond", "note", "text"] },
                                "color": { "type": "string" },
                                "cluster": { "type": "string", "description": "Cluster id; nodes in a cluster are drawn together inside a labelled zone." },
                                "parent": { "type": "string", "description": "tree/mindmap: parent id (alternative to edges)." },
                                "width": { "type": "number" }, "height": { "type": "number" },
                                "font_size": {}, "fill": { "type": "string" }, "stroke": { "type": "string" }, "fill_style": { "type": "string" }, "stroke_style": { "type": "string" }
                            },
                            "required": ["id", "label"]
                        }
                    },
                    "edges": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "from": { "type": "string" }, "to": { "type": "string" }, "label": { "type": "string" },
                                "stroke_style": { "type": "string", "enum": ["solid", "dashed", "dotted"] },
                                "color": { "type": "string" },
                                "arrowheads": { "type": "string", "enum": ["end", "start", "both", "none"] }
                            },
                            "required": ["from", "to"]
                        }
                    },
                    "clusters": {
                        "type": "array",
                        "items": { "type": "object", "properties": { "id": { "type": "string" }, "label": { "type": "string" }, "color": { "type": "string" } }, "required": ["id"] }
                    },
                    "title": { "type": "string", "description": "Heading drawn above the diagram." },
                    "edge_style": { "type": "string", "enum": ["elbow", "straight", "curved"], "description": "Connector routing (default: elbow for flow/tree, curved for mindmap, straight for radial)." },
                    "spacing": { "description": "compact | normal (default) | roomy, or a number (gap between nodes)." },
                    "theme": { "type": "string", "enum": ["colorful", "mono"], "description": "colorful (default): nodes coloured by cluster, branch or role; mono: black outlines." },
                    "uniform": { "type": "boolean", "description": "Same size for nodes of the same shape (default true except mindmap)." },
                    "columns": { "type": "integer", "description": "grid: number of columns." },
                    "at": { "type": "array", "items": { "type": "number" }, "description": "[x, y] top-left. Default: free space next to existing content." },
                    "right_of": { "type": "string" }, "below": { "type": "string" },
                    "frame": { "type": "string", "description": "Wrap the whole diagram in a new frame with this name." },
                    "animate": { "type": "boolean" }
                },
                "required": ["nodes"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        },
        {
            "name": "arrange",
            "title": "Move, align and organise",
            "description": "Tidies existing elements. Each operation: {\"op\": …, \"ids\": [...], …}. Ops: move (dx/dy, or to:[x,y] for the selection's top-left), align (edge: left|center|right|top|middle|bottom), distribute (axis: horizontal|vertical; optional gap for exact spacing), row / column / grid (lay out in order with gap, optional at:[x,y], columns for grid), front / back (z-order), group / ungroup (name optional), lock / unlock, frame (frame: id → put ids in it), unframe. Labels and connected arrows follow automatically.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "operations": { "type": "array", "items": { "type": "object", "properties": {
                        "op": { "type": "string", "enum": ["move", "align", "distribute", "row", "column", "grid", "front", "back", "group", "ungroup", "lock", "unlock", "frame", "unframe"] },
                        "ids": ids,
                        "dx": { "type": "number" }, "dy": { "type": "number" },
                        "to": { "type": "array", "items": { "type": "number" } },
                        "at": { "type": "array", "items": { "type": "number" } },
                        "edge": { "type": "string" }, "axis": { "type": "string" },
                        "gap": { "type": "number" }, "columns": { "type": "integer" },
                        "name": { "type": "string" }, "frame": { "type": "string" }
                    }, "required": ["op", "ids"] } },
                    "animate": { "type": "boolean" }
                },
                "required": ["operations"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        },
        {
            "name": "delete_elements",
            "title": "Delete elements",
            "description": "Deletes elements by id, with their labels, a frame's contents, and arrows attached to them (unless keep_arrows). People can undo it in the editor. `all: true` clears the whole canvas — only when asked to.",
            "inputSchema": {
                "type": "object",
                "properties": { "ids": ids, "keep_arrows": { "type": "boolean" }, "all": { "type": "boolean" } }
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "point_at",
            "title": "Point at something",
            "description": "Moves your cursor to elements or a spot so people can see what you're talking about; with laser: true it circles them with the laser pointer. Anyone following you gets the view too. Use it when explaining a drawing or asking about a part of it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ids": ids,
                    "at": { "type": "array", "items": { "type": "number" } },
                    "region": region,
                    "laser": { "type": "boolean", "description": "Circle it with the laser pointer (default true)." },
                    "select": { "type": "boolean", "description": "Show the elements as selected by you (default true)." }
                }
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "update_canvas",
            "title": "Rename or recolour the canvas",
            "description": "Renames the open canvas and/or sets its background colour (e.g. white, light-gray, light-blue, #fdf8f6).",
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string" }, "background": { "type": "string" } }
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }
    ])
}

const DRAW_DESCRIPTION: &str = "Creates or updates elements on the open canvas; people watch your cursor draw them. \
Shapes auto-size to their label, labels are centred and wrapped like the editor does, and arrows given `from`/`to` ids are bound to those shapes so they stay connected when anyone moves them. \
Place things with x/y (top-left), `center`, or relative to another element (`below`/`above`/`right_of`/`left_of` + `gap`, `align`); with no position, an element goes right of the previous one in the same call, or into free space. \
Use one call per logical section (e.g. all boxes of a row plus their arrows). \
Returns the ids and boxes of what changed and a layout check that names overlaps, overflowing labels and arrows crossing shapes — fix those. \
Examples: {\"id\":\"api\",\"type\":\"rectangle\",\"label\":\"API\",\"color\":\"blue\",\"x\":0,\"y\":0} · {\"id\":\"db\",\"type\":\"ellipse\",\"label\":\"Postgres\",\"below\":\"api\"} · {\"type\":\"arrow\",\"from\":\"api\",\"to\":\"db\",\"label\":\"SQL\"} · {\"type\":\"text\",\"text\":\"Architecture\",\"font_size\":\"xl\",\"above\":\"api\",\"align\":\"start\"} · {\"type\":\"rectangle\",\"label\":\"Backend\",\"contains\":[\"api\",\"db\"],\"color\":\"light-gray\",\"stroke_style\":\"dashed\"} · update: {\"id\":\"api\",\"color\":\"green\",\"right_of\":\"db\"}.";

const DIAGRAM_DESCRIPTION: &str = "Draws a whole diagram from nodes and edges with automatic, professional layout: no coordinates needed, no overlaps, edges routed around nodes, clusters as labelled zones, consistent sizes and colours. \
Best for flowcharts, architectures, pipelines, state machines, org charts, trees, mind maps, sequence diagrams and card grids. Nodes become regular elements whose ids you choose, so you can refine them afterwards with draw/arrange. \
Flowchart convention when shapes aren't given and theme is colorful: diamond for decisions, ellipse for start/end. \
Placed in free space next to existing content unless `at`/`right_of`/`below` is given. Returns node positions and a layout check.";

// ------------------------------------------------------------------ dispatch

pub async fn call(hub: &Arc<Hub>, session: &Arc<Session>, name: &str, args: &Value) -> Value {
    let result = match name {
        "list_canvases" => list_canvases(hub, session).await,
        "open_canvas" => open_canvas(hub, session, args).await,
        "describe_canvas" => describe_canvas(hub, session, args).await,
        "view_canvas" => view_canvas(hub, session, args).await,
        "draw" => draw_tool(hub, session, args).await,
        "draw_diagram" => diagram_tool(hub, session, args).await,
        "arrange" => arrange(hub, session, args).await,
        "delete_elements" => delete_elements(hub, session, args).await,
        "point_at" => point_at(hub, session, args).await,
        "update_canvas" => update_canvas(hub, session, args).await,
        _ => Err(format!("Unknown tool “{name}”.")),
    };
    match result {
        Ok(content) => json!({ "content": content, "isError": false }),
        Err(e) => json!({ "content": [text(e)], "isError": true }),
    }
}

fn canvas_list(hub: &Hub) -> Vec<(String, String, Vec<String>)> {
    let metas = hub.storage().list();
    let rooms = lock(&hub.rooms);
    metas
        .into_iter()
        .map(|m| {
            let peers = rooms.get(&m.id).map(|r| lock(&r.state).peers.values().map(|p| p.name.clone()).collect()).unwrap_or_default();
            (m.id, m.name, peers)
        })
        .collect()
}

fn canvas_lines(list: &[(String, String, Vec<String>)]) -> String {
    if list.is_empty() {
        return "No canvases yet. Create one with open_canvas {\"create\": \"Name\"}.".into();
    }
    list.iter()
        .map(|(id, name, peers)| {
            let who = if peers.is_empty() { String::new() } else { format!(" — here now: {}", peers.join(", ")) };
            format!("- “{name}” (id {id}){who}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Makes sure the assistant is in a canvas: the one it opened, the one from
/// the connection URL, or the only one there is.
async fn ensure_canvas(hub: &Arc<Hub>, session: &Arc<Session>) -> Result<(), String> {
    let mut agent = session.agent.lock().await;
    if agent.presence().is_ok() {
        return Ok(());
    }
    if let Some(c) = &session.start_canvas {
        if hub.storage().exists(c) {
            return agent.join(hub, c);
        }
    }
    let list = canvas_list(hub);
    if list.len() == 1 {
        return agent.join(hub, &list[0].0);
    }
    Err(format!("Open a canvas first with open_canvas. Canvases on this server:\n{}", canvas_lines(&list)))
}

fn canvas_name(hub: &Hub, id: &str) -> String {
    hub.storage().meta(id).map(|(m, _)| m.name).unwrap_or_else(|_| "Untitled".into())
}

async fn list_canvases(hub: &Arc<Hub>, session: &Arc<Session>) -> ToolResult {
    let list = canvas_list(hub);
    let current = session.agent.lock().await.canvas_id().map(str::to_owned);
    let mut s = format!("Canvases on “{}”:\n{}", hub.name, canvas_lines(&list));
    if let Some(c) = current {
        s.push_str(&format!("\n\nYou're in “{}”.", canvas_name(hub, &c)));
    }
    Ok(vec![text(s)])
}

fn find_canvas(hub: &Hub, query: &str) -> Result<String, String> {
    let q = query.trim();
    let list = canvas_list(hub);
    if let Some((id, ..)) = list.iter().find(|(id, name, _)| id == q || name.eq_ignore_ascii_case(q)) {
        return Ok(id.clone());
    }
    let lower = q.to_lowercase();
    let matches: Vec<&(String, String, Vec<String>)> = list.iter().filter(|(_, name, _)| name.to_lowercase().contains(&lower)).collect();
    match matches.as_slice() {
        [one] => Ok(one.0.clone()),
        [] => Err(format!("No canvas called “{q}”. Canvases:\n{}", canvas_lines(&list))),
        many => Err(format!(
            "“{q}” matches several canvases; pass the id:\n{}",
            many.iter().map(|(id, name, _)| format!("- “{name}” (id {id})")).collect::<Vec<_>>().join("\n")
        )),
    }
}

async fn open_canvas(hub: &Arc<Hub>, session: &Arc<Session>, args: &Value) -> ToolResult {
    let a = Args::of(args);
    let id = if let Some(name) = a.str("create").or_else(|| a.str("new")).or_else(|| a.str("name")) {
        hub.storage().create(name, None)?.id
    } else if let Some(q) = a.str("canvas").or_else(|| a.str("id")) {
        find_canvas(hub, q)?
    } else if let Some(c) = session.start_canvas.clone().filter(|c| hub.storage().exists(c)) {
        c
    } else {
        let list = canvas_list(hub);
        match list.as_slice() {
            [one] => one.0.clone(),
            _ => return Err(format!("Which canvas? Pass `canvas` (id or name) or `create`.\n{}", canvas_lines(&list))),
        }
    };
    let mut agent = session.agent.lock().await;
    agent.join(hub, &id)?;
    let others = agent.presence().map(|p| lock(&p.room.state).peers.len().saturating_sub(1)).unwrap_or(0);
    let opts = DescribeOptions { max_elements: 150, ..DescribeOptions::default() };
    let desc = describe_now(hub, &mut agent, &opts)?;
    let who = match others {
        0 => "Nobody else is here right now.".to_owned(),
        1 => "1 other person is here and can see your cursor.".to_owned(),
        n => format!("{n} other people are here and can see your cursor."),
    };
    Ok(vec![text(format!("Joined “{}” as {}. {who}\n\n{desc}", canvas_name(hub, &id), agent.name))])
}

fn peers_of(agent: &mut super::agent::Agent) -> Result<Vec<PeerView>, String> {
    let p = agent.presence()?;
    let me = p.peer_id.clone();
    let st = lock(&p.room.state);
    Ok(st
        .peers
        .iter()
        .map(|(id, peer)| PeerView { name: peer.name.clone(), pointer: peer.pointer, selected: peer.selected.clone(), is_you: *id == me })
        .collect())
}

fn describe_now(hub: &Arc<Hub>, agent: &mut super::agent::Agent, opts: &DescribeOptions) -> Result<String, String> {
    let peers = peers_of(agent)?;
    let canvas = agent.canvas_id().unwrap_or_default().to_owned();
    let draft = agent.draft()?;
    let alive = draft.alive();
    Ok(describe::describe(&canvas_name(hub, &canvas), &alive, draft.background.as_deref(), &peers, opts))
}

fn region_of(a: &Args) -> Option<Rect> {
    let r = a.obj("region")?;
    Some(Rect::new(r.num("x")?, r.num("y")?, r.num("width").or(r.num("w"))?, r.num("height").or(r.num("h"))?))
}

async fn describe_canvas(hub: &Arc<Hub>, session: &Arc<Session>, args: &Value) -> ToolResult {
    ensure_canvas(hub, session).await?;
    let a = Args::of(args);
    let opts = DescribeOptions {
        region: region_of(&a),
        ids: a.strings("ids"),
        detail: if a.str("detail").is_some_and(|d| d.eq_ignore_ascii_case("full")) { Detail::Full } else { Detail::Summary },
        ..DescribeOptions::default()
    };
    let mut agent = session.agent.lock().await;
    Ok(vec![text(describe_now(hub, &mut agent, &opts)?)])
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A PNG of `region` (or of `ids`, or everything) as MCP content.
fn snapshot(draft: &Draft, region: Option<Rect>, ids: Option<&[String]>, grid: bool, max_size: u32) -> Result<(Value, String), String> {
    let alive = draft.alive();
    let region = region.or_else(|| {
        let ids = ids?;
        let set: HashSet<&str> = ids.iter().map(String::as_str).collect();
        geo::union_all(alive.iter().filter(|e| set.contains(model::id(e))).map(|e| model::bounds(e))).map(|r| r.inflate(60.0))
    });
    let opts = RenderOptions { region, max_size, grid, ..RenderOptions::default() };
    let bg = draft.background.clone().unwrap_or_else(|| "#ffffff".into());
    let shot = render::render(&alive, &draft.files, &bg, &opts)?;
    let r = shot.region;
    let caption = format!(
        "View of x {:.0}…{:.0}, y {:.0}…{:.0} ({}×{} px, {:.2} px per canvas unit).",
        r.x, r.right(), r.y, r.bottom(), shot.width, shot.height, shot.scale
    );
    Ok((json!({ "type": "image", "data": base64(&shot.png), "mimeType": "image/png" }), caption))
}

async fn view_canvas(hub: &Arc<Hub>, session: &Arc<Session>, args: &Value) -> ToolResult {
    ensure_canvas(hub, session).await?;
    let a = Args::of(args);
    let mut agent = session.agent.lock().await;
    let draft = agent.draft()?;
    let ids = a.strings("ids");
    let max = a.num("max_size").map(|n| n.clamp(200.0, 2400.0) as u32).unwrap_or(1400);
    let (image, caption) = snapshot(&draft, region_of(&a), ids.as_deref(), a.bool("grid").unwrap_or(true), max)?;
    Ok(vec![image, text(caption)])
}

/// The reply after a change: what changed, where, and what to fix.
fn report(draft: &Draft, out: &Outcome, verb: &str) -> String {
    let by_id = draft.by_id();
    let mut s = String::new();
    let listed: Vec<&String> = out.touched.iter().filter(|id| draft.exists(id)).collect();
    s.push_str(&format!("{verb} {} element(s).", listed.len()));
    if let Some(b) = geo::union_all(listed.iter().filter_map(|id| draft.get(id)).map(model::bounds)) {
        s.push_str(&format!(" Area: x {:.0}…{:.0}, y {:.0}…{:.0}.", b.x, b.right(), b.y, b.bottom()));
    }
    s.push('\n');
    const MAX_LINES: usize = 60;
    for id in listed.iter().take(MAX_LINES) {
        if let Some(el) = draft.get(id) {
            s.push_str(&format!("  {}\n", describe::element_line(el, &by_id)));
        }
    }
    if listed.len() > MAX_LINES {
        s.push_str(&format!("  … and {} more\n", listed.len() - MAX_LINES));
    }
    for n in &out.notes {
        s.push_str(&format!("Note: {n}\n"));
    }
    let touched: HashSet<String> = draft.changed().iter().cloned().collect();
    let issues = describe::lint_on(&draft.alive(), &touched, draft.background.as_deref());
    if issues.is_empty() {
        s.push_str("Layout check: no overlaps or crossings found.");
    } else {
        s.push_str("Layout check — please fix:\n");
        for i in issues {
            s.push_str(&format!("- {i}\n"));
        }
    }
    s
}

/// Point near the person who's here, for auto-placement.
fn near_people(agent: &mut super::agent::Agent) -> Option<Pt> {
    let peers = peers_of(agent).ok()?;
    peers.iter().filter(|p| !p.is_you).find_map(|p| p.pointer)
}

async fn draw_tool(hub: &Arc<Hub>, session: &Arc<Session>, args: &Value) -> ToolResult {
    ensure_canvas(hub, session).await?;
    let a = Args::of(args);
    let items: Vec<Value> = match a.get("elements").or_else(|| a.get("items")) {
        Some(Value::Array(list)) => list.clone(),
        Some(v @ Value::Object(_)) => vec![v.clone()],
        Some(Value::String(s)) => serde_json::from_str::<Vec<Value>>(s).map_err(|_| "`elements` must be an array of element objects.")?,
        _ if args.get("type").is_some() => vec![args.clone()],
        _ => return Err("Pass `elements`: an array like [{\"type\": \"rectangle\", \"label\": \"Hello\"}].".into()),
    };
    if items.is_empty() {
        return Err("`elements` is empty.".into());
    }
    let mut agent = session.agent.lock().await;
    let mut draft = agent.draft()?;
    let near = near_people(&mut agent);
    let mut out = Outcome::default();
    let mut cursor = Cursor { last: None };
    let mut errors = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if let Err(e) = draw::draw_one(&mut draft, item, &mut cursor, &mut out, near) {
            let id = Args::of(item).text("id").map(|id| format!(" (“{id}”)")).unwrap_or_default();
            errors.push(format!("element {}{id}: {e}", i + 1));
        }
    }
    if out.touched.is_empty() {
        return Err(format!("Nothing was drawn.\n{}", errors.join("\n")));
    }
    agent.commit(hub, &draft, a.bool("animate").unwrap_or(true)).await?;
    let mut s = report(&draft, &out, if out.updated > 0 && out.created == 0 { "Updated" } else { "Drew" });
    if !errors.is_empty() {
        s.push_str(&format!("\nSkipped (fix and retry these):\n{}", errors.join("\n")));
    }
    Ok(vec![text(s)])
}

async fn diagram_tool(hub: &Arc<Hub>, session: &Arc<Session>, args: &Value) -> ToolResult {
    ensure_canvas(hub, session).await?;
    let a = Args::of(args);
    let mut agent = session.agent.lock().await;
    let mut draft = agent.draft()?;
    let near = near_people(&mut agent);
    let out = diagram::draw(&mut draft, &a, near)?;
    agent.commit(hub, &draft, a.bool("animate").unwrap_or(true)).await?;
    Ok(vec![text(report(&draft, &out, "Drew a diagram with"))])
}

fn ids_arg(a: &Args, draft: &Draft) -> Result<Vec<String>, String> {
    let ids = a.strings("ids").or_else(|| a.strings("id")).ok_or("Pass `ids`.")?;
    let missing: Vec<&String> = ids.iter().filter(|i| !draft.exists(i)).collect();
    if !missing.is_empty() {
        return Err(format!("No element(s) {}.", missing.iter().map(|m| format!("“{m}”")).collect::<Vec<_>>().join(", ")));
    }
    // Labels move with their containers; use the container instead.
    Ok(ids
        .into_iter()
        .map(|id| draft.get(&id).and_then(model::container_id).filter(|c| draft.exists(c)).map(str::to_owned).unwrap_or(id))
        .collect())
}

fn arrange_one(draft: &mut Draft, op: &Value, out: &mut Outcome) -> Result<(), String> {
    let a = Args::of(op);
    let name = a.str("op").or_else(|| a.str("action")).ok_or("Each operation needs \"op\".")?.to_ascii_lowercase();
    let ids = ids_arg(&a, draft)?;
    let rects: Vec<(String, Rect)> = ids.iter().filter_map(|id| draft.get(id).map(|e| (id.clone(), model::bounds(e)))).collect();
    let all = geo::union_all(rects.iter().map(|(_, r)| *r)).ok_or("No elements to arrange.")?;
    let gap = a.num("gap");
    match name.as_str() {
        "move" | "translate" => {
            let (dx, dy) = match a.point("to").or_else(|| a.point("at")) {
                Some((x, y)) => (x - all.x, y - all.y),
                None => (a.num("dx").unwrap_or(0.0), a.num("dy").unwrap_or(0.0)),
            };
            draw::move_by(draft, &ids, dx, dy);
        }
        "align" => {
            let edge = a.str("edge").or_else(|| a.str("to")).unwrap_or("center").to_ascii_lowercase();
            for (id, r) in &rects {
                let (dx, dy) = match edge.as_str() {
                    "left" | "start" => (all.x - r.x, 0.0),
                    "right" | "end" => (all.right() - r.right(), 0.0),
                    "center" | "centre" | "center-x" | "vertical" => (all.center().0 - r.center().0, 0.0),
                    "top" => (0.0, all.y - r.y),
                    "bottom" => (0.0, all.bottom() - r.bottom()),
                    "middle" | "center-y" | "horizontal" => (0.0, all.center().1 - r.center().1),
                    other => return Err(format!("Unknown edge “{other}”: use left, center, right, top, middle or bottom.")),
                };
                draw::move_by(draft, std::slice::from_ref(id), dx, dy);
            }
        }
        "distribute" | "space" => {
            let horizontal = !a.str("axis").is_some_and(|x| x.to_ascii_lowercase().starts_with('v'));
            let mut sorted = rects.clone();
            sorted.sort_by(|p, q| if horizontal { p.1.x.total_cmp(&q.1.x) } else { p.1.y.total_cmp(&q.1.y) });
            let total: f64 = sorted.iter().map(|(_, r)| if horizontal { r.w } else { r.h }).sum();
            let span = if horizontal { all.w } else { all.h };
            let g = gap.unwrap_or_else(|| ((span - total) / (sorted.len().max(2) - 1) as f64).max(0.0));
            let mut pos = if horizontal { all.x } else { all.y };
            for (id, r) in &sorted {
                let (dx, dy) = if horizontal { (pos - r.x, 0.0) } else { (0.0, pos - r.y) };
                draw::move_by(draft, std::slice::from_ref(id), dx, dy);
                pos += if horizontal { r.w } else { r.h } + g;
            }
        }
        "row" | "column" | "col" | "grid" | "stack" => {
            let g = gap.unwrap_or(40.0);
            let (ox, oy) = a.point("at").or_else(|| a.point("to")).unwrap_or((all.x, all.y));
            let cols = match name.as_str() {
                "row" => rects.len(),
                "column" | "col" | "stack" => 1,
                _ => a.num("columns").map(|c| c.max(1.0) as usize).unwrap_or_else(|| (rects.len() as f64).sqrt().ceil() as usize),
            }
            .max(1);
            let rows = rects.len().div_ceil(cols);
            let col_w: Vec<f64> = (0..cols).map(|c| rects.iter().skip(c).step_by(cols).map(|(_, r)| r.w).fold(0.0, f64::max)).collect();
            let row_h: Vec<f64> = (0..rows).map(|r| rects.iter().skip(r * cols).take(cols).map(|(_, x)| x.h).fold(0.0, f64::max)).collect();
            for (i, (id, r)) in rects.iter().enumerate() {
                let (c, rw) = (i % cols, i / cols);
                let cx = ox + col_w[..c].iter().sum::<f64>() + g * c as f64 + col_w[c] / 2.0;
                let cy = oy + row_h[..rw].iter().sum::<f64>() + g * rw as f64 + row_h[rw] / 2.0;
                let (dx, dy) = (cx - r.center().0, cy - r.center().1);
                draw::move_by(draft, std::slice::from_ref(id), dx.round(), dy.round());
            }
        }
        "front" | "bring_to_front" | "back" | "send_to_back" => {
            let z = if name.contains("back") { super::draft::Z::Back } else { super::draft::Z::Front };
            let order: Vec<String> = if z == super::draft::Z::Back { ids.iter().rev().cloned().collect() } else { ids.clone() };
            for id in order {
                draft.restack(&id, z);
                // Labels stay right above their container.
                if let Some(t) = draft.get(&id).and_then(model::bound_text_id).filter(|t| draft.exists(t)) {
                    draft.restack_above(&[t], &id);
                }
            }
        }
        "group" => {
            let g = a.str("name").map(super::args::slug).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("g{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
            for id in with_labels(draft, &ids) {
                draft.update(&id, |e| {
                    let mut list = model::group_ids(e);
                    if !list.contains(&g) {
                        list.push(g.clone());
                    }
                    e["groupIds"] = json!(list);
                });
            }
        }
        "ungroup" => {
            for id in with_labels(draft, &ids) {
                draft.update(&id, |e| {
                    let mut list = model::group_ids(e);
                    list.pop();
                    e["groupIds"] = json!(list);
                });
            }
        }
        "lock" | "unlock" => {
            for id in with_labels(draft, &ids) {
                draft.update(&id, |e| e["locked"] = (name == "lock").into());
            }
        }
        "frame" => {
            let f = a.str("frame").ok_or("`frame`: the frame id to put the elements in.")?;
            if !draft.get(f).is_some_and(model::is_frame) {
                return Err(format!("“{f}” isn’t a frame."));
            }
            draw::adopt(draft, f, &ids);
        }
        "unframe" => {
            for id in with_labels(draft, &ids) {
                draft.update(&id, |e| e["frameId"] = Value::Null);
            }
        }
        other => return Err(format!("Unknown op “{other}”.")),
    }
    for id in &ids {
        out.touch(id);
    }
    Ok(())
}

fn with_labels(draft: &Draft, ids: &[String]) -> Vec<String> {
    let mut all = ids.to_vec();
    all.extend(ids.iter().filter_map(|id| draft.get(id).and_then(model::bound_text_id)).filter(|t| draft.exists(t)));
    all
}

async fn arrange(hub: &Arc<Hub>, session: &Arc<Session>, args: &Value) -> ToolResult {
    ensure_canvas(hub, session).await?;
    let a = Args::of(args);
    let ops: Vec<Value> = match a.get("operations").or_else(|| a.get("ops")) {
        Some(Value::Array(list)) => list.clone(),
        Some(v @ Value::Object(_)) => vec![v.clone()],
        _ if a.has("op") => vec![args.clone()],
        _ => return Err("Pass `operations`: [{\"op\": \"align\", \"ids\": [...], \"edge\": \"top\"}].".into()),
    };
    let mut agent = session.agent.lock().await;
    let mut draft = agent.draft()?;
    let mut out = Outcome::default();
    for (i, op) in ops.iter().enumerate() {
        arrange_one(&mut draft, op, &mut out).map_err(|e| format!("operation {}: {e} Nothing was changed.", i + 1))?;
    }
    for id in out.touched.clone() {
        if let Some(r) = draft.get(&id).map(model::bounds) {
            draft.step(r.center(), Some(&id));
            draft.update(&id, |_| {});
        }
    }
    agent.commit(hub, &draft, a.bool("animate").unwrap_or(true)).await?;
    Ok(vec![text(report(&draft, &out, "Arranged"))])
}

async fn delete_elements(hub: &Arc<Hub>, session: &Arc<Session>, args: &Value) -> ToolResult {
    ensure_canvas(hub, session).await?;
    let a = Args::of(args);
    let mut agent = session.agent.lock().await;
    let mut draft = agent.draft()?;
    let ids: Vec<String> = if a.bool("all") == Some(true) {
        draft.alive().iter().map(|e| model::id(e).to_owned()).collect()
    } else {
        let ids = a.strings("ids").ok_or("Pass `ids` (or `all: true` to clear the canvas).")?;
        let missing: Vec<&String> = ids.iter().filter(|i| !draft.exists(i)).collect();
        if missing.len() == ids.len() {
            return Err(format!("None of those ids exist: {}.", ids.join(", ")));
        }
        ids
    };
    let gone = draw::delete_ids(&mut draft, &ids, a.bool("keep_arrows").unwrap_or(false));
    agent.commit(hub, &draft, false).await?;
    let extra = gone.len().saturating_sub(ids.len());
    let mut s = format!("Deleted {} element(s)", ids.len());
    if extra > 0 {
        s.push_str(&format!(" plus {extra} attached (labels, arrows, frame contents)"));
    }
    s.push_str(". People can undo this in the editor.");
    Ok(vec![text(s)])
}

async fn point_at(hub: &Arc<Hub>, session: &Arc<Session>, args: &Value) -> ToolResult {
    ensure_canvas(hub, session).await?;
    let a = Args::of(args);
    let mut agent = session.agent.lock().await;
    let draft = agent.draft()?;
    let ids = a.strings("ids").unwrap_or_default();
    let area = if !ids.is_empty() {
        geo::union_all(ids.iter().filter_map(|id| draft.get(id)).map(model::bounds)).ok_or("None of those ids exist.")?
    } else if let Some(r) = region_of(&a) {
        r
    } else if let Some((x, y)) = a.point("at") {
        Rect::new(x - 20.0, y - 20.0, 40.0, 40.0)
    } else {
        return Err("Pass `ids`, `at` or `region`.".into());
    };
    let selected = if a.bool("select").unwrap_or(true) { ids.clone() } else { Vec::new() };
    agent.point_at(hub, area, a.bool("laser").unwrap_or(true), &selected).await?;
    Ok(vec![text(format!("Pointing at x {:.0}…{:.0}, y {:.0}…{:.0}.", area.x, area.right(), area.y, area.bottom()))])
}

async fn update_canvas(hub: &Arc<Hub>, session: &Arc<Session>, args: &Value) -> ToolResult {
    ensure_canvas(hub, session).await?;
    let a = Args::of(args);
    let mut agent = session.agent.lock().await;
    let canvas = agent.canvas_id().unwrap_or_default().to_owned();
    let mut done = Vec::new();
    if let Some(name) = a.str("name") {
        let meta = hub.storage().rename(&canvas, name)?;
        if let Ok(p) = agent.presence() {
            lock(&p.room.state).broadcast(None, crate::text(json!({ "type": "meta", "meta": meta })));
        }
        done.push(format!("renamed it “{}”", meta.name));
    }
    if let Some(bg) = a.str("background") {
        let lower = bg.to_ascii_lowercase();
        // Bare hues make very pale backgrounds.
        let color = if super::colors::PALETTE.iter().any(|(h, _)| *h == lower) { format!("{lower}-0") } else { lower };
        let c = super::colors::resolve(&color, super::colors::Slot::Fill).ok_or_else(|| format!("Unknown colour “{bg}”."))?;
        agent.set_background(hub, &c)?;
        done.push(format!("set the background to {c}"));
    }
    if done.is_empty() {
        return Err("Pass `name` and/or `background`.".into());
    }
    Ok(vec![text(format!("Done: {}.", done.join(" and ")))])
}

//! What the assistant is told when it connects: how the canvas works and
//! how to make drawings that look good.

pub fn instructions(has_start_canvas: bool) -> String {
    let start = if has_start_canvas {
        "You were invited into a specific canvas; it opens automatically on your first tool call (or call open_canvas to see it)."
    } else {
        "Start with list_canvases / open_canvas (or open_canvas {\"create\": \"Name\"} for a new one)."
    };
    format!(
        r#"Drawlycool is a shared, hand-drawn whiteboard (the Excalidraw editor). Through these tools you join a canvas as a live participant: people there see your name, your cursor moving, and your drawing appear piece by piece, and they can edit alongside you. {start}

WORKFLOW
1. open_canvas, then read the description: what's already there, who's here, what they've selected, and where free space is. Never draw over people's work; when someone says "this"/"here", use their selection or cursor.
2. Plan before drawing: the sections, their order, a grid, sizes and colours. Sketch the coordinates of each section in your head first.
3. Graphs (flowcharts, architectures, org charts, mind maps, sequence diagrams, card grids) → draw_diagram: automatic layout, no overlaps, tidy connectors. Anything custom → draw, one call per section, using relative placement (below / right_of + gap) instead of computing coordinates.
4. Check: read the layout check in every reply and fix what it reports. For anything non-trivial, call view_canvas and actually look: alignment, even spacing, balance, legibility. Fix with draw (same id = update) or arrange (align / distribute / row / grid / move).
5. When explaining, point_at what you mean. Ask before deleting other people's work.

DESIGN RULES (they make the difference between messy and beautiful)
- Layout: align to a 20 px grid; equal gaps between siblings (40–80 px; 100+ between sections); the same size for elements of the same kind (pass width/height or use draw_diagram's uniform sizing); flow left→right or top→bottom; leave whitespace.
- Hierarchy: one title (font_size xl or xxl) top-left above the content; section headings l; body text m (20); notes s (16). Never below 14.
- Colour with meaning: 2–4 hues per drawing, one hue per category, consistent everywhere. Use `color: "<hue>"` (dark stroke + soft fill of the same hue). Leave most of the canvas neutral; use light-<hue> fills for big background zones and strong colours only for emphasis. Palette: blue, green, orange, violet, red, teal, yellow, grape, cyan, pink, gray, bronze. Text and labels without a colour stay readable on their own: on a dark panel they turn light.
- Labels: short (1–5 words) inside shapes; details go in a smaller text below or a note. Shapes auto-size to labels (and widen rather than split a word), so don't set tiny sizes. Emoji and symbols (★ ✓ ⚠ →) are fine.
- Connectors: always connect with from/to ids (they stay attached when people move things). Use route "elbow" for structured diagrams, "curved" for organic ones; avoid crossings by ordering and placing things well; label arrows only when it adds meaning.
- Grouping: wrap related elements in a zone ({{"type":"rectangle","contains":[ids],"label":"Backend","color":"light-gray","stroke_style":"dashed"}}) or a frame; keep 30+ px padding.
- Shapes: rectangles for components/steps, ellipses for start/end/actors, diamonds for decisions, notes for comments, text for headings and annotations.
- Style: the default hand-drawn look (roughness artist, Excalifont "hand") suits most drawings; use roughness "architect" and font "sans" for crisp technical diagrams; font "code" for code/identifiers.

COORDINATES: x grows right, y grows down, units are pixels at 100% zoom. x/y is an element's top-left corner. Ids you pass become the elements' ids; reuse them to update, connect and place relative to them. After delete_elements, the same ids are free again for a redraw."#
    )
}

use egui::vec2;
use egui::{Pos2, Vec2};

use crate::gui::shape::{ComponentLabel, ComponentShape, PinAnchor, ShapeCmd};
use crate::sim::circuit::TunnelRole;
use crate::sim::component::{FanDirection, GateOp};

pub const GRID_SIZE: f32 = 10.0;
pub const COMP_MIN_WIDTH: f32 = 20.0; // Input/Output body width (1 cell)
pub const COMP_WIDTH: f32 = 30.0; // standard body width (2 cells)
                                  // Floor height for a single-pin edge: 2 grid cells, so the lone pin centres on
                                  // grid row 1. Also the box height for Input/Output/Tunnel.
pub const COMP_MIN_HEIGHT: f32 = 20.0;
pub const LABEL_FONT_SIZE: f32 = 8.0;

// Body widths expressed in whole grid cells - used to place right-edge pins
// (col == width in cells) so they land on the grid.
const COMP_W_CELLS: f32 = COMP_WIDTH / GRID_SIZE; // 2
const SPLITTER_W_CELLS: f32 = SPLITTER_WIDTH / GRID_SIZE; // 1
                                                          // Grid column of a centred top/bottom-edge pin (selector, carry, enable).
const MID_COL: f32 = COMP_W_CELLS / 2.0; // 1

// Splitter doesn't compute anything - it just re-routes bits - so it's drawn
// much narrower than other components to read as a connector rather than a
// processing block. See splitter_shape() for the comb-shaped body this pairs
// with.
const SPLITTER_WIDTH: f32 = 20.0;
// Normalized x-band (relative to SPLITTER_WIDTH) of the splitter's thin
// "spine" rectangle; the comb's trunk/teeth strokes extend from here out to
// x=0.0/x=1.0 to reach the pins. Kept narrow (a thin rod, not a block) so
// most of SPLITTER_WIDTH is free for trunk/tooth length - each side needs to
// clear the ~3px pin dot radius drawn at its far end, plus some margin, or
// the teeth end up fully hidden under the pin dots.
const SPLITTER_BODY_X: (f32, f32) = (0.25, 0.60);

pub fn snap_to_grid(pos: Pos2, pan: Vec2) -> [i32; 2] {
    [
        ((pos.x - pan.x) / GRID_SIZE).round() as i32,
        ((pos.y - pan.y) / GRID_SIZE).round() as i32,
    ]
}

// Grid row (in cells from the top edge) of pin `i` in a 2-cell-pitch stack:
// 1, 3, 5, ... A stack of `k` such pins spans rows 1..2k-1 with a 1-cell
// margin above/below, so its centre sits on integer grid row `k`.
fn pin_row(i: usize) -> f32 {
    2.0 * i as f32 + 1.0
}

// Pixel height of an edge carrying `k` stacked pins (k>=1): 2k cells (see
// pin_row) - the single source of truth for the height formulas below.
const fn stack_height_px(k: usize) -> f32 {
    let k = if k == 0 { 1 } else { k };
    2.0 * k as f32 * GRID_SIZE
}

pub fn rect_outline() -> Vec<ShapeCmd> {
    vec![
        ShapeCmd::MoveTo(vec2(0.0, 0.0)),
        ShapeCmd::LineTo(vec2(1.0, 0.0)),
        ShapeCmd::LineTo(vec2(1.0, 1.0)),
        ShapeCmd::LineTo(vec2(0.0, 1.0)),
    ]
}

fn and_outline() -> Vec<ShapeCmd> {
    vec![
        ShapeCmd::MoveTo(vec2(0.0, 0.0)),
        ShapeCmd::LineTo(vec2(0.5, 0.0)),
        ShapeCmd::CubicTo(vec2(0.776, 0.0), vec2(1.0, 0.224), vec2(1.0, 0.5)),
        ShapeCmd::CubicTo(vec2(1.0, 0.776), vec2(0.776, 1.0), vec2(0.5, 1.0)),
        ShapeCmd::LineTo(vec2(0.0, 1.0)),
    ]
}

fn or_outline() -> Vec<ShapeCmd> {
    vec![
        ShapeCmd::MoveTo(vec2(0.0, 0.0)),
        ShapeCmd::CubicTo(vec2(0.5, 0.0), vec2(0.9, 0.15), vec2(1.0, 0.5)),
        ShapeCmd::CubicTo(vec2(0.9, 0.85), vec2(0.5, 1.0), vec2(0.0, 1.0)),
        ShapeCmd::CubicTo(vec2(0.15, 0.75), vec2(0.15, 0.25), vec2(0.0, 0.0)),
    ]
}

// Convex-only outline for the OR gate fill (no concave left curve).
// epaint's fill tessellator uses a triangle fan + per-vertex feathering normals,
// which both assume convexity. The concave left side causes fill to bleed outside
// the boundary. We fill with this simpler convex shape and stroke with or_outline().
fn or_fill_outline() -> Vec<ShapeCmd> {
    vec![
        ShapeCmd::MoveTo(vec2(0.0, 0.0)),
        ShapeCmd::CubicTo(vec2(0.5, 0.0), vec2(0.9, 0.15), vec2(1.0, 0.5)),
        ShapeCmd::CubicTo(vec2(0.9, 0.85), vec2(0.5, 1.0), vec2(0.0, 1.0)),
        // PathShape closes with a straight line from (0,1) back to (0,0)
    ]
}

fn not_outline() -> Vec<ShapeCmd> {
    vec![
        ShapeCmd::MoveTo(vec2(0.0, 0.0)),
        ShapeCmd::LineTo(vec2(0.0, 1.0)),
        ShapeCmd::LineTo(vec2(1.0, 0.5)),
    ]
}

fn xor_extra_arc() -> Vec<ShapeCmd> {
    // Concave arc drawn just left of the OR body; negative x is outside the bounding box
    vec![
        ShapeCmd::MoveTo(vec2(-0.15, 0.05)),
        ShapeCmd::CubicTo(vec2(0.0, 0.25), vec2(0.0, 0.75), vec2(-0.15, 0.95)),
    ]
}

// Zero-allocation size queries, kept as the single source of truth for the
// height formulas below - the corresponding *_shape() functions call these
// rather than recomputing the formula, so callers that only need a
// bounding box (e.g. component_bounding_rect) don't have to build and
// immediately discard a full ComponentShape (outline/anchors/bubbles Vecs).
pub const fn gate_size(op: GateOp, n_inputs: usize) -> Vec2 {
    let n = if matches!(op, GateOp::Not) {
        1
    } else {
        n_inputs
    };
    vec2(COMP_WIDTH, stack_height_px(n))
}

pub const fn mux_size(sel_width: u8) -> Vec2 {
    let branches = 1usize << sel_width;
    vec2(COMP_WIDTH, stack_height_px(branches))
}

pub const fn demux_size(sel_width: u8) -> Vec2 {
    mux_size(sel_width) // same branches stack
}

pub const fn splitter_size(arms: u8) -> Vec2 {
    vec2(SPLITTER_WIDTH, stack_height_px(arms as usize))
}

pub const fn reg_size() -> Vec2 {
    // D + WE as a 2-pin stack (height 4 cells); the output centres on grid row 2.
    vec2(COMP_WIDTH, stack_height_px(2))
}

// Height accounts only for the two addend pins on the left edge, same formula as a
// 2-input gate - the carry-in/carry-out pins sit at the bottom/top edges (like
// encoder's enable_in/enable_out) and don't consume extra vertical space of their own.
pub const fn adder_size() -> Vec2 {
    vec2(COMP_WIDTH, stack_height_px(2))
}

// Same layout/formula as adder_size(): minuend/subtrahend on the left edge,
// borrow-in/borrow-out at the bottom/top edges.
pub const fn subtractor_size() -> Vec2 {
    adder_size()
}

// Same layout/formula as adder_size(): multiplicand/multiplier on the left edge,
// carry-in/carry-out at the bottom/top edges.
pub const fn multiplier_size() -> Vec2 {
    adder_size()
}

// Same layout/formula as adder_size(): dividend/divisor on the left edge,
// carry-in/remainder at the bottom/top edges.
pub const fn divider_size() -> Vec2 {
    adder_size()
}

// Height scales off the busier side - the 3 comparison outputs on the right,
// same formula as a 3-input gate - rather than the 2 operand inputs on the left.
pub const fn comparator_size() -> Vec2 {
    vec2(COMP_WIDTH, stack_height_px(3))
}

// Height scales with the arm count on the left edge, but never below 4 cells so
// the three right-side pins (enable_out at top, selector + group as a centred
// pair) always have room - the bottom/top pins (enable_in/enable_out) sit at the
// edges and don't consume extra vertical space of their own.
pub const fn encoder_size(sel_width: u8) -> Vec2 {
    let arms = 1usize << sel_width;
    let k = if arms < 2 { 2 } else { arms };
    vec2(COMP_WIDTH, stack_height_px(k))
}

pub fn gate_shape(op: GateOp, n_inputs: usize) -> ComponentShape {
    let n = if matches!(op, GateOp::Not) {
        1
    } else {
        n_inputs
    };
    let h = gate_size(op, n_inputs).y;
    let h_cells = h / GRID_SIZE; // 2n
    let bubble = matches!(op, GateOp::Nand | GateOp::Nor | GateOp::Xnor | GateOp::Not);

    let (outline, fill_outline, extra_strokes) = match op {
        GateOp::And | GateOp::Nand => (and_outline(), None, vec![]),
        GateOp::Or | GateOp::Nor => (or_outline(), Some(or_fill_outline()), vec![]),
        GateOp::Xor | GateOp::Xnor => {
            (or_outline(), Some(or_fill_outline()), vec![xor_extra_arc()])
        }
        GateOp::Not => (not_outline(), None, vec![]),
    };

    // Output centres on the input stack: grid row n (= h_cells / 2).
    let center_row = h_cells / 2.0;
    let out_anchor = if bubble {
        PinAnchor::right_bubble(COMP_W_CELLS, center_row)
    } else {
        PinAnchor::right(COMP_W_CELLS, center_row)
    };
    let input_anchors = (0..n).map(|i| PinAnchor::left(pin_row(i))).collect();

    ComponentShape {
        size: vec2(COMP_WIDTH, h),
        outline,
        fill_outline,
        input_anchors,
        output_anchors: vec![out_anchor],
        extra_strokes,
        output_bubbles: vec![bubble],
        labels: vec![],
        dynamic_label_pos: Vec2::ZERO,
    }
}

pub fn mux_shape(sel_width: u8) -> ComponentShape {
    let branches = 1usize << sel_width;
    let h = mux_size(sel_width).y;
    let h_cells = h / GRID_SIZE; // 2 * branches
    const T: f32 = 0.2;

    let outline = vec![
        ShapeCmd::MoveTo(vec2(0.0, 0.0)),
        ShapeCmd::LineTo(vec2(1.0, T)),
        ShapeCmd::LineTo(vec2(1.0, 1.0 - T)),
        ShapeCmd::LineTo(vec2(0.0, 1.0)),
    ];

    // input[0] = selector → bottom-center of shape; input[1..] = data → left edge
    let sel_anchor = PinAnchor::bottom(MID_COL, h_cells);
    let data_anchors = (0..branches).map(|i| PinAnchor::left(pin_row(i)));
    let input_anchors = std::iter::once(sel_anchor).chain(data_anchors).collect();

    // The selector pin sits on the bottom grid row, but the trapezoid's bottom
    // edge tapers up to y = 1 - T/2 at the centre; draw a short stub down to the
    // pin (like the splitter's teeth) so the wire visibly meets the body.
    let sel_stub = vec![
        ShapeCmd::MoveTo(vec2(0.5, 1.0 - T / 2.0)),
        ShapeCmd::LineTo(vec2(0.5, 1.0)),
    ];

    ComponentShape {
        size: vec2(COMP_WIDTH, h),
        outline,
        fill_outline: None,
        input_anchors,
        output_anchors: vec![PinAnchor::right(COMP_W_CELLS, h_cells / 2.0)],
        extra_strokes: vec![sel_stub],
        output_bubbles: vec![false],
        labels: vec![],
        dynamic_label_pos: Vec2::ZERO,
    }
}

pub fn reg_shape() -> ComponentShape {
    let h = reg_size().y;
    let h_cells = h / GRID_SIZE; // 4

    // input[0] = data (row 1), input[1] = write_enable (row 3), both on the left
    // edge; output[0] centres on the right (row 2).
    let input_anchors = vec![PinAnchor::left(pin_row(0)), PinAnchor::left(pin_row(1))];

    // "D"/"WE" sit level with their pins (same y as the anchors above), offset
    // right of the left-edge pin dot with room to spare in the 40px-wide box.
    let labels = vec![
        ComponentLabel {
            text: "D",
            pos: vec2(0.28, pin_row(0) / h_cells),
            ..Default::default()
        },
        ComponentLabel {
            text: "WE",
            pos: vec2(0.28, pin_row(1) / h_cells),
            ..Default::default()
        },
    ];

    ComponentShape {
        size: vec2(COMP_WIDTH, h),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors,
        output_anchors: vec![PinAnchor::right(COMP_W_CELLS, h_cells / 2.0)],
        extra_strokes: vec![],
        output_bubbles: vec![false],
        labels,
        dynamic_label_pos: Vec2::ZERO,
    }
}

pub fn demux_shape(sel_width: u8) -> ComponentShape {
    let branches = 1usize << sel_width;
    let h = demux_size(sel_width).y;
    let h_cells = h / GRID_SIZE; // 2 * branches
    const T: f32 = 0.2;

    let outline = vec![
        ShapeCmd::MoveTo(vec2(0.0, T)),
        ShapeCmd::LineTo(vec2(1.0, 0.0)),
        ShapeCmd::LineTo(vec2(1.0, 1.0)),
        ShapeCmd::LineTo(vec2(0.0, 1.0 - T)),
    ];

    // input[0] = data → left center (aligned with the output stack's centre);
    // input[1] = selector → bottom center.
    let data_anchor = PinAnchor::left(h_cells / 2.0);
    let sel_anchor = PinAnchor::bottom(MID_COL, h_cells);
    let output_anchors = (0..branches)
        .map(|i| PinAnchor::right(COMP_W_CELLS, pin_row(i)))
        .collect();

    // Stub from the tapered bottom edge down to the on-grid selector pin.
    let sel_stub = vec![
        ShapeCmd::MoveTo(vec2(0.5, 1.0 - T / 2.0)),
        ShapeCmd::LineTo(vec2(0.5, 1.0)),
    ];

    ComponentShape {
        size: vec2(COMP_WIDTH, h),
        outline,
        fill_outline: None,
        input_anchors: vec![data_anchor, sel_anchor],
        output_anchors,
        extra_strokes: vec![sel_stub],
        output_bubbles: vec![false; branches],
        labels: vec![],
        dynamic_label_pos: Vec2::ZERO,
    }
}

// Pin layout matches Component::priority_encoder's fixed order: input[0] = enable_in
// (bottom edge), input[1..] = arms (left edge, evenly spaced); output[0] = selector and
// output[2] = group_out (right edge, a centred pair), output[1] = enable_out (top edge).
pub fn encoder_shape(sel_width: u8) -> ComponentShape {
    let arms = 1usize << sel_width;
    let h = encoder_size(sel_width).y;
    let h_cells = h / GRID_SIZE; // max(2 * arms, 4)
    let center_row = h_cells / 2.0;

    let enable_in_anchor = PinAnchor::bottom(MID_COL, h_cells);
    let arm_anchors = (0..arms).map(|i| PinAnchor::left(pin_row(i)));
    let input_anchors = std::iter::once(enable_in_anchor)
        .chain(arm_anchors)
        .collect();

    let enable_out_anchor = PinAnchor::top(MID_COL);
    // selector/group_out sit as a centred pair, one grid row either side of centre.
    let sel_row = center_row - 1.0;
    let grp_row = center_row + 1.0;
    let output_anchors = vec![
        PinAnchor::right(COMP_W_CELLS, sel_row),
        enable_out_anchor,
        PinAnchor::right(COMP_W_CELLS, grp_row),
    ];

    // EN sits just above the bottom edge by a fixed pixel distance rather than a fixed
    // fraction of height - height grows with sel_width (more arms), but the label should
    // stay close to the pin it names instead of drifting toward the middle of a tall body.
    const BOTTOM_LABEL_INSET_PX: f32 = 6.0;
    let en_y = 1.0 - BOTTOM_LABEL_INSET_PX / h;

    let labels = vec![
        ComponentLabel {
            text: "EN",
            pos: vec2(0.5, en_y),
            ..Default::default()
        },
        ComponentLabel {
            text: "S",
            pos: vec2(0.78, sel_row / h_cells),
            ..Default::default()
        },
        ComponentLabel {
            text: "G",
            pos: vec2(0.78, grp_row / h_cells),
            ..Default::default()
        },
    ];

    ComponentShape {
        size: vec2(COMP_WIDTH, h),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors,
        output_anchors,
        extra_strokes: vec![],
        output_bubbles: vec![false, false, false],
        labels,
        dynamic_label_pos: Vec2::ZERO,
    }
}

// Pin layout matches Component::adder's fixed order: input[0]/[1] = addends (left
// edge), input[2] = carry-in (bottom edge); output[0] = sum (right edge, centred),
// output[1] = carry-out (top edge) - carry-in/out mirror encoder's enable_in/enable_out
// corner placement so they read as "flow-through" pins distinct from the data pins.
pub fn adder_shape() -> ComponentShape {
    op2_shape("+", 12.0, "CIN", "CO")
}

// Same layout as adder_shape(); minuend/subtrahend in, borrow-in/borrow-out flow-through.
pub fn subtractor_shape() -> ComponentShape {
    op2_shape("-", 12.0, "BIN", "BO")
}

// Same layout as adder_shape(); multiplicand/multiplier in, carry-in/carry-out flow-through.
pub fn multiplier_shape() -> ComponentShape {
    op2_shape("X", 12.0, "CIN", "CO")
}

// Same layout as adder_shape(); dividend/divisor in, carry-in (upper dividend half) /
// remainder flow-through.
pub fn divider_shape() -> ComponentShape {
    op2_shape("÷", 12.0, "UP", "REM")
}

// Shared body for the two-operand arithmetic units (adder/subtractor/multiplier/
// divider): two data inputs on the left, a centred result output on the right, and
// carry/borrow-style flow-through pins on the bottom (in) and top (out) edges.
fn op2_shape(
    op_label: &'static str,
    op_font: f32,
    bottom_label: &'static str,
    top_label: &'static str,
) -> ComponentShape {
    let h = adder_size().y;
    let h_cells = h / GRID_SIZE; // 4

    let carry_in_anchor = PinAnchor::bottom(MID_COL, h_cells);
    let input_anchors = vec![
        PinAnchor::left(pin_row(0)),
        PinAnchor::left(pin_row(1)),
        carry_in_anchor,
    ];

    let carry_out_anchor = PinAnchor::top(MID_COL);
    let output_anchors = vec![
        PinAnchor::right(COMP_W_CELLS, h_cells / 2.0),
        carry_out_anchor,
    ];

    // Flow-through labels sit a fixed pixel distance in from the bottom/top edges,
    // next to their pins; the op symbol sits just inside the right edge.
    const EDGE_LABEL_INSET_PX: f32 = 6.0;
    let bottom_y = 1.0 - EDGE_LABEL_INSET_PX / h;
    let top_y = EDGE_LABEL_INSET_PX / h;

    let labels = vec![
        ComponentLabel {
            text: op_label,
            pos: vec2(0.72, 0.5),
            font_size: op_font,
        },
        ComponentLabel {
            text: bottom_label,
            pos: vec2(0.5, bottom_y),
            ..Default::default()
        },
        ComponentLabel {
            text: top_label,
            pos: vec2(0.5, top_y),
            ..Default::default()
        },
    ];

    ComponentShape {
        size: vec2(COMP_WIDTH, h),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors,
        output_anchors,
        extra_strokes: vec![],
        output_bubbles: vec![false, false],
        labels,
        dynamic_label_pos: Vec2::ZERO,
    }
}

// Pin layout matches Component::comparator's fixed order: input[0]/[1] = the two
// compared operands (left edge, centred on the output stack); output[0] = greater-than,
// output[1] = equal, output[2] = less-than (right edge, evenly spaced, each labeled).
pub fn comparator_shape() -> ComponentShape {
    let h = comparator_size().y;
    let h_cells = h / GRID_SIZE; // 6
    let center_row = h_cells / 2.0; // 3

    // Two inputs centred on the 3-output stack: one grid row either side of centre.
    let input_anchors = vec![
        PinAnchor::left(center_row - 1.0),
        PinAnchor::left(center_row + 1.0),
    ];

    let output_anchors = (0..3)
        .map(|i| PinAnchor::right(COMP_W_CELLS, pin_row(i)))
        .collect();

    let labels = vec![
        ComponentLabel {
            text: ">",
            pos: vec2(0.72, pin_row(0) / h_cells),
            ..Default::default()
        },
        ComponentLabel {
            text: "=",
            pos: vec2(0.72, pin_row(1) / h_cells),
            ..Default::default()
        },
        ComponentLabel {
            text: "<",
            pos: vec2(0.72, pin_row(2) / h_cells),
            ..Default::default()
        },
    ];

    ComponentShape {
        size: vec2(COMP_WIDTH, h),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors,
        output_anchors,
        extra_strokes: vec![],
        output_bubbles: vec![false, false, false],
        labels,
        dynamic_label_pos: Vec2::ZERO,
    }
}

// direction == Right draws the classic splitter: a single trunk pin on the
// left (input), teeth fanning out to arm pins on the right (outputs). Left
// mirrors the whole shape horizontally (x -> 1-x) via `mx` and swaps which
// anchor list holds the trunk vs. the arms, turning it into a combiner: arm
// pins on the left (inputs), single trunk pin on the right (output) - this
// must match Component::splitter's Left-mode pin order (arm index ==
// input pin index, ascending).
pub fn splitter_shape(arms: u8, direction: FanDirection) -> ComponentShape {
    let n = arms as usize;
    let h = splitter_size(arms).y;
    let h_cells = h / GRID_SIZE; // 2n
    let flip = matches!(direction, FanDirection::Left);
    let mx = |x: f32| if flip { 1.0 - x } else { x };

    let (x0, x1) = SPLITTER_BODY_X;
    // Mirroring reverses x0 < x1 into mx(x1) < mx(x0), so re-sort into
    // (lo, hi) for a well-formed spine rect either way.
    let (bx0, bx1) = if flip { (mx(x1), mx(x0)) } else { (x0, x1) };

    // Thin rectangular "spine" - kept convex so it needs no separate
    // fill_outline, unlike the comb shape a full concave outline would need.
    let outline = vec![
        ShapeCmd::MoveTo(vec2(bx0, 0.0)),
        ShapeCmd::LineTo(vec2(bx1, 0.0)),
        ShapeCmd::LineTo(vec2(bx1, 1.0)),
        ShapeCmd::LineTo(vec2(bx0, 1.0)),
    ];

    // arm 0's tooth sits at the smallest y (grid row 1), i.e. the top. The data
    // pin lines up with it rather than sitting at mid-height, so the shape itself
    // communicates "arm 0 is the near/top one, arm N-1 is the far/bottom one".
    // Normalized y of a grid row = row / h_cells.
    let row_y = |row: f32| row / h_cells;
    let data_y = row_y(pin_row(0));

    // One trunk line from the data pin into the spine, then one tooth line
    // per arm fanning out from the spine to that arm's pin - drawn past the
    // spine's own edges to form the comb, rather than baking the fan into
    // the (concave) outline itself.
    let trunk = vec![
        ShapeCmd::MoveTo(vec2(mx(0.0), data_y)),
        ShapeCmd::LineTo(vec2(mx(x0), data_y)),
    ];
    let teeth = (0..n).map(|i| {
        let y = row_y(pin_row(i));
        vec![
            ShapeCmd::MoveTo(vec2(mx(x1), y)),
            ShapeCmd::LineTo(vec2(mx(1.0), y)),
        ]
    });
    let extra_strokes = std::iter::once(trunk).chain(teeth).collect();

    let arm_anchor = |i: usize| {
        if flip {
            PinAnchor::left(pin_row(i))
        } else {
            PinAnchor::right(SPLITTER_W_CELLS, pin_row(i))
        }
    };
    let trunk_anchor = if flip {
        PinAnchor::right(SPLITTER_W_CELLS, pin_row(0))
    } else {
        PinAnchor::left(pin_row(0))
    };

    let (input_anchors, output_anchors): (Vec<PinAnchor>, Vec<PinAnchor>) = if flip {
        ((0..n).map(arm_anchor).collect(), vec![trunk_anchor])
    } else {
        (vec![trunk_anchor], (0..n).map(arm_anchor).collect())
    };
    // Sized off output_anchors.len(), not `n` - they only coincide in Right mode.
    let output_bubbles = vec![false; output_anchors.len()];

    ComponentShape {
        size: vec2(SPLITTER_WIDTH, h),
        outline,
        fill_outline: None,
        input_anchors,
        output_anchors,
        extra_strokes,
        output_bubbles,
        labels: vec![],
        dynamic_label_pos: Vec2::ZERO,
    }
}

pub fn tunnel_shape(role: TunnelRole) -> ComponentShape {
    let outline = match role {
        TunnelRole::Feed => vec![
            ShapeCmd::MoveTo(vec2(0.0, 0.0)),
            ShapeCmd::LineTo(vec2(0.7, 0.0)),
            ShapeCmd::LineTo(vec2(1.0, 0.5)),
            ShapeCmd::LineTo(vec2(0.7, 1.0)),
            ShapeCmd::LineTo(vec2(0.0, 1.0)),
        ],
        TunnelRole::Pull => vec![
            ShapeCmd::MoveTo(vec2(0.0, 0.5)),
            ShapeCmd::LineTo(vec2(0.3, 0.0)),
            ShapeCmd::LineTo(vec2(1.0, 0.0)),
            ShapeCmd::LineTo(vec2(1.0, 1.0)),
            ShapeCmd::LineTo(vec2(0.3, 1.0)),
        ],
    };

    // 40 x 40 box (2 cells each way); the single pin centres on grid row 1.
    let center_row = COMP_MIN_HEIGHT / GRID_SIZE / 2.0; // 1
    let input_anchors = match role {
        TunnelRole::Feed => vec![],
        TunnelRole::Pull => vec![PinAnchor::left(center_row)],
    };
    let output_anchors = match role {
        TunnelRole::Feed => vec![PinAnchor::right(COMP_W_CELLS, center_row)],
        TunnelRole::Pull => vec![],
    };

    ComponentShape {
        size: vec2(COMP_WIDTH, COMP_MIN_HEIGHT),
        outline,
        fill_outline: None,
        input_anchors,
        output_anchors,
        extra_strokes: vec![],
        output_bubbles: vec![false],
        labels: vec![],
        dynamic_label_pos: vec2(0.45, 0.45),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The whole point of the grid-native anchor rework: every pin must sit on a
    // grid intersection. Since a pin's screen position is `top_left (grid-aligned)
    // + anchor.cell * GRID_SIZE`, that holds iff every anchor cell is an integer
    // number of cells - so assert exactly that for every shape/parameter combo.
    fn assert_shape_on_grid(name: &str, shape: &ComponentShape) {
        // The bounding box must itself be a whole number of grid cells, or a pin
        // on a far edge (col == width in cells) wouldn't be integer either.
        let w_cells = shape.size.x / GRID_SIZE;
        let h_cells = shape.size.y / GRID_SIZE;
        assert_eq!(
            w_cells.fract(),
            0.0,
            "{name}: width {} not whole cells",
            shape.size.x
        );
        assert_eq!(
            h_cells.fract(),
            0.0,
            "{name}: height {} not whole cells",
            shape.size.y
        );

        for (kind, anchors) in [
            ("input", &shape.input_anchors),
            ("output", &shape.output_anchors),
        ] {
            for (i, a) in anchors.iter().enumerate() {
                assert_eq!(
                    a.cell.x.fract(),
                    0.0,
                    "{name}: {kind} pin {i} col {} is off-grid",
                    a.cell.x
                );
                assert_eq!(
                    a.cell.y.fract(),
                    0.0,
                    "{name}: {kind} pin {i} row {} is off-grid",
                    a.cell.y
                );
            }
        }
    }

    #[test]
    fn all_component_pins_land_on_grid() {
        for op in [
            GateOp::And,
            GateOp::Or,
            GateOp::Xor,
            GateOp::Nand,
            GateOp::Nor,
            GateOp::Xnor,
            GateOp::Not,
        ] {
            for n in 1..=5usize {
                assert_shape_on_grid(&format!("gate {op:?} n={n}"), &gate_shape(op, n));
            }
        }

        for sel in 0..=3u8 {
            assert_shape_on_grid(&format!("mux sel={sel}"), &mux_shape(sel));
            assert_shape_on_grid(&format!("demux sel={sel}"), &demux_shape(sel));
            assert_shape_on_grid(&format!("encoder sel={sel}"), &encoder_shape(sel));
        }

        assert_shape_on_grid("reg", &reg_shape());
        assert_shape_on_grid("adder", &adder_shape());
        assert_shape_on_grid("subtractor", &subtractor_shape());
        assert_shape_on_grid("multiplier", &multiplier_shape());
        assert_shape_on_grid("divider", &divider_shape());
        assert_shape_on_grid("comparator", &comparator_shape());

        for arms in 1..=6u8 {
            assert_shape_on_grid(
                &format!("splitter R arms={arms}"),
                &splitter_shape(arms, FanDirection::Right),
            );
            assert_shape_on_grid(
                &format!("splitter L arms={arms}"),
                &splitter_shape(arms, FanDirection::Left),
            );
        }

        assert_shape_on_grid("tunnel feed", &tunnel_shape(TunnelRole::Feed));
        assert_shape_on_grid("tunnel pull", &tunnel_shape(TunnelRole::Pull));
    }

    // Bubble output pins sit one cell beyond the right edge (col == width + 1) so
    // the inversion bubble drawn in the gap doesn't push them off-grid.
    #[test]
    fn bubble_output_pin_is_one_cell_past_the_edge() {
        let shape = gate_shape(GateOp::Not, 1);
        let w_cells = shape.size.x / GRID_SIZE;
        assert_eq!(shape.output_anchors[0].cell.x, w_cells + 1.0);
    }
}

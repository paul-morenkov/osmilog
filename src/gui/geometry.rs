use egui::vec2;
use egui::{Pos2, Vec2};

use crate::gui::shape::{ComponentLabel, ComponentShape, PinAnchor, ShapeCmd};
use crate::sim::circuit::TunnelRole;
use crate::sim::component::{FanDirection, GateOp};

pub const GRID_SIZE: f32 = 20.0;
pub const COMP_MIN_WIDTH: f32 = 20.0;
pub const COMP_WIDTH: f32 = 40.0;
pub const COMP_MIN_HEIGHT: f32 = 20.0;
pub const LABEL_FONT_SIZE: f32 = 8.0;
const COMP_HEIGHT_PER_PIN: f32 = 10.0;
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

fn spaced(i: usize, n: usize) -> f32 {
    (i as f32 + 1.0) / (n as f32 + 1.0)
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
    vec2(
        COMP_WIDTH,
        ((n + 1) as f32 * COMP_HEIGHT_PER_PIN).max(COMP_MIN_HEIGHT),
    )
}

pub const fn mux_size(sel_width: u8) -> Vec2 {
    let branches = 1usize << sel_width;
    vec2(
        COMP_WIDTH,
        ((branches + 1) as f32 * COMP_HEIGHT_PER_PIN).max(COMP_MIN_HEIGHT),
    )
}

pub const fn demux_size(sel_width: u8) -> Vec2 {
    mux_size(sel_width) // same branches+1 formula
}

pub const fn splitter_size(arms: u8) -> Vec2 {
    vec2(
        SPLITTER_WIDTH,
        ((arms as usize + 1) as f32 * COMP_HEIGHT_PER_PIN).max(COMP_MIN_HEIGHT),
    )
}

pub const fn reg_size() -> Vec2 {
    vec2(
        COMP_WIDTH,
        ((2 + 3) as f32 * COMP_HEIGHT_PER_PIN).max(COMP_MIN_HEIGHT),
    )
}

// Height accounts only for the two addend pins on the left edge, same formula as a
// 2-input gate - the carry-in/carry-out pins sit at the bottom/top edges (like
// encoder's enable_in/enable_out) and don't consume extra vertical space of their own.
pub const fn adder_size() -> Vec2 {
    vec2(
        COMP_WIDTH,
        ((2 + 1) as f32 * COMP_HEIGHT_PER_PIN).max(COMP_MIN_HEIGHT),
    )
}

// Same layout/formula as adder_size(): minuend/subtrahend on the left edge,
// borrow-in/borrow-out at the bottom/top edges.
pub const fn subtractor_size() -> Vec2 {
    adder_size()
}

// Height scales with the arm count on the left edge, same formula as mux/demux's
// branches - the bottom/top pins (enable_in/enable_out) sit at the y=0/y=1 corners
// and don't consume extra vertical space of their own.
pub const fn encoder_size(sel_width: u8) -> Vec2 {
    let arms = 1usize << sel_width;
    vec2(
        COMP_WIDTH,
        ((arms + 1) as f32 * COMP_HEIGHT_PER_PIN).max(COMP_MIN_HEIGHT),
    )
}

pub fn gate_shape(op: GateOp, n_inputs: usize) -> ComponentShape {
    let n = if matches!(op, GateOp::Not) {
        1
    } else {
        n_inputs
    };
    let h = gate_size(op, n_inputs).y;
    let bubble = matches!(op, GateOp::Nand | GateOp::Nor | GateOp::Xnor | GateOp::Not);

    let (outline, fill_outline, extra_strokes) = match op {
        GateOp::And | GateOp::Nand => (and_outline(), None, vec![]),
        GateOp::Or | GateOp::Nor => (or_outline(), Some(or_fill_outline()), vec![]),
        GateOp::Xor | GateOp::Xnor => {
            (or_outline(), Some(or_fill_outline()), vec![xor_extra_arc()])
        }
        GateOp::Not => (not_outline(), None, vec![]),
    };

    let out_anchor = if bubble {
        PinAnchor::right_bubble(0.5)
    } else {
        PinAnchor::right(0.5)
    };
    let input_anchors = (0..n).map(|i| PinAnchor::left(spaced(i, n))).collect();

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
    const T: f32 = 0.2;

    let outline = vec![
        ShapeCmd::MoveTo(vec2(0.0, 0.0)),
        ShapeCmd::LineTo(vec2(1.0, T)),
        ShapeCmd::LineTo(vec2(1.0, 1.0 - T)),
        ShapeCmd::LineTo(vec2(0.0, 1.0)),
    ];

    // input[0] = selector → bottom-center of shape; input[1..] = data → left edge
    let sel_anchor = PinAnchor::bottom_mid(0.5, 1.0 - T / 2.0);
    let data_anchors = (0..branches).map(|i| PinAnchor::left(spaced(i, branches)));
    let input_anchors = std::iter::once(sel_anchor).chain(data_anchors).collect();

    ComponentShape {
        size: vec2(COMP_WIDTH, h),
        outline,
        fill_outline: None,
        input_anchors,
        output_anchors: vec![PinAnchor::right(0.5)],
        extra_strokes: vec![],
        output_bubbles: vec![false],
        labels: vec![],
        dynamic_label_pos: Vec2::ZERO,
    }
}

pub fn reg_shape() -> ComponentShape {
    let h = reg_size().y;

    // input[0] = data, input[1] = write_enable, both on the left edge; output[0] on the right
    let input_anchors = vec![PinAnchor::left(spaced(0, 3)), PinAnchor::left(spaced(2, 3))];

    // "D"/"WE" sit level with their pins (same y as the anchors above), offset
    // right of the left-edge pin dot with room to spare in the 40px-wide box.
    let labels = vec![
        ComponentLabel {
            text: "D",
            pos: vec2(0.28, spaced(0, 3)),
            ..Default::default()
        },
        ComponentLabel {
            text: "WE",
            pos: vec2(0.28, spaced(2, 3)),
            ..Default::default()
        },
    ];

    ComponentShape {
        size: vec2(COMP_WIDTH, h),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors,
        output_anchors: vec![PinAnchor::right(0.5)],
        extra_strokes: vec![],
        output_bubbles: vec![false],
        labels,
        dynamic_label_pos: Vec2::ZERO,
    }
}

pub fn demux_shape(sel_width: u8) -> ComponentShape {
    let branches = 1usize << sel_width;
    let h = demux_size(sel_width).y;
    const T: f32 = 0.2;

    let outline = vec![
        ShapeCmd::MoveTo(vec2(0.0, T)),
        ShapeCmd::LineTo(vec2(1.0, 0.0)),
        ShapeCmd::LineTo(vec2(1.0, 1.0)),
        ShapeCmd::LineTo(vec2(0.0, 1.0 - T)),
    ];

    // input[0] = data → left center; input[1] = selector → bottom center
    let data_anchor = PinAnchor {
        norm_pos: vec2(0.0, 0.5),
        wire_dir: vec2(-1.0, 0.0),
        pixel_offset: 0.0,
    };
    let sel_anchor = PinAnchor::bottom_mid(0.5, 1.0 - T / 2.0);
    let output_anchors = (0..branches)
        .map(|i| PinAnchor::right(spaced(i, branches)))
        .collect();

    ComponentShape {
        size: vec2(COMP_WIDTH, h),
        outline,
        fill_outline: None,
        input_anchors: vec![data_anchor, sel_anchor],
        output_anchors,
        extra_strokes: vec![],
        output_bubbles: vec![false; branches],
        labels: vec![],
        dynamic_label_pos: Vec2::ZERO,
    }
}

// Pin layout matches Component::priority_encoder's fixed order: input[0] = enable_in
// (bottom edge), input[1..] = arms (left edge, evenly spaced); output[0] = selector and
// output[2] = group_out (right edge, top/bottom of the pair), output[1] = enable_out
// (top edge).
pub fn encoder_shape(sel_width: u8) -> ComponentShape {
    let arms = 1usize << sel_width;
    let h = encoder_size(sel_width).y;

    let enable_in_anchor = PinAnchor {
        norm_pos: vec2(0.5, 1.0),
        wire_dir: vec2(0.0, 1.0),
        pixel_offset: 0.0,
    };
    let arm_anchors = (0..arms).map(|i| PinAnchor::left(spaced(i, arms)));
    let input_anchors = std::iter::once(enable_in_anchor)
        .chain(arm_anchors)
        .collect();

    let enable_out_anchor = PinAnchor {
        norm_pos: vec2(0.5, 0.0),
        wire_dir: vec2(0.0, -1.0),
        pixel_offset: 0.0,
    };
    let sel_y = spaced(0, 2);
    let grp_y = spaced(1, 2);
    let output_anchors = vec![
        PinAnchor::right(sel_y),
        enable_out_anchor,
        PinAnchor::right(grp_y),
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
            pos: vec2(0.78, sel_y),
            ..Default::default()
        },
        ComponentLabel {
            text: "G",
            pos: vec2(0.78, grp_y),
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
// edge), input[2] = carry-in (bottom edge); output[0] = sum (right edge), output[1]
// = carry-out (top edge) - carry-in/out mirror encoder's enable_in/enable_out corner
// placement so they read as "flow-through" pins distinct from the data pins.
pub fn adder_shape() -> ComponentShape {
    let h = adder_size().y;

    let carry_in_anchor = PinAnchor::bottom_mid(0.5, 1.0);
    let input_anchors = vec![
        PinAnchor::left(spaced(0, 2)),
        PinAnchor::left(spaced(1, 2)),
        carry_in_anchor,
    ];

    let carry_out_anchor = PinAnchor {
        norm_pos: vec2(0.5, 0.0),
        wire_dir: vec2(0.0, -1.0),
        pixel_offset: 0.0,
    };
    let output_anchors = vec![PinAnchor::right(0.5), carry_out_anchor];

    // CIN/CO sit a fixed pixel distance in from the bottom/top edges, next to
    // their pins; "+" sits just inside the right edge, next to the sum pin.
    const EDGE_LABEL_INSET_PX: f32 = 6.0;
    let cin_y = 1.0 - EDGE_LABEL_INSET_PX / h;
    let co_y = EDGE_LABEL_INSET_PX / h;

    let labels = vec![
        ComponentLabel {
            text: "+",
            pos: vec2(0.72, 0.5),
            font_size: 12.0,
        },
        ComponentLabel {
            text: "CIN",
            pos: vec2(0.5, cin_y),
            ..Default::default()
        },
        ComponentLabel {
            text: "CO",
            pos: vec2(0.5, co_y),
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

// Pin layout matches Component::subtractor's fixed order: input[0]/[1] = minuend/
// subtrahend (left edge), input[2] = borrow-in (bottom edge); output[0] = difference
// (right edge), output[1] = borrow-out (top edge). Same corner placement rationale
// as adder_shape()'s carry-in/carry-out.
pub fn subtractor_shape() -> ComponentShape {
    let h = subtractor_size().y;

    let borrow_in_anchor = PinAnchor::bottom_mid(0.5, 1.0);
    let input_anchors = vec![
        PinAnchor::left(spaced(0, 2)),
        PinAnchor::left(spaced(1, 2)),
        borrow_in_anchor,
    ];

    let borrow_out_anchor = PinAnchor {
        norm_pos: vec2(0.5, 0.0),
        wire_dir: vec2(0.0, -1.0),
        pixel_offset: 0.0,
    };
    let output_anchors = vec![PinAnchor::right(0.5), borrow_out_anchor];

    const EDGE_LABEL_INSET_PX: f32 = 6.0;
    let bin_y = 1.0 - EDGE_LABEL_INSET_PX / h;
    let bo_y = EDGE_LABEL_INSET_PX / h;

    let labels = vec![
        ComponentLabel {
            text: "-",
            pos: vec2(0.72, 0.5),
            font_size: 12.0,
        },
        ComponentLabel {
            text: "BIN",
            pos: vec2(0.5, bin_y),
            ..Default::default()
        },
        ComponentLabel {
            text: "BO",
            pos: vec2(0.5, bo_y),
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

    // arm 0's tooth sits at the smallest y (spaced() grows with i), i.e. the
    // top. The data pin lines up with it rather than sitting at mid-height,
    // so the shape itself communicates "arm 0 is the near/top one, arm N-1
    // is the far/bottom one" instead of leaving that ambiguous.
    let data_y = spaced(0, n.max(1));

    // One trunk line from the data pin into the spine, then one tooth line
    // per arm fanning out from the spine to that arm's pin - drawn past the
    // spine's own edges to form the comb, rather than baking the fan into
    // the (concave) outline itself.
    let trunk = vec![
        ShapeCmd::MoveTo(vec2(mx(0.0), data_y)),
        ShapeCmd::LineTo(vec2(mx(x0), data_y)),
    ];
    let teeth = (0..n).map(|i| {
        let y = spaced(i, n);
        vec![
            ShapeCmd::MoveTo(vec2(mx(x1), y)),
            ShapeCmd::LineTo(vec2(mx(1.0), y)),
        ]
    });
    let extra_strokes = std::iter::once(trunk).chain(teeth).collect();

    let arm_anchor = |i: usize| {
        if flip {
            PinAnchor::left(spaced(i, n))
        } else {
            PinAnchor::right(spaced(i, n))
        }
    };
    let trunk_anchor = if flip {
        PinAnchor::right(data_y)
    } else {
        PinAnchor::left(data_y)
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

    let input_anchors = match role {
        TunnelRole::Feed => vec![],
        TunnelRole::Pull => vec![PinAnchor::left(0.5)],
    };
    let output_anchors = match role {
        TunnelRole::Feed => vec![PinAnchor::right(0.5)],
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

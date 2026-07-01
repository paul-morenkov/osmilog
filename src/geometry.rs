use egui::vec2;
use egui::{Pos2, Vec2};

use crate::component::GateOp;
use crate::shape::{ComponentShape, PinAnchor, ShapeCmd};

pub const GRID_SIZE: f32 = 20.0;
pub const COMP_WIDTH: f32 = 40.0;
pub const COMP_MIN_HEIGHT: f32 = 30.0;
const COMP_HEIGHT_PER_PIN: f32 = 10.0;

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

pub fn gate_shape(op: GateOp, n_inputs: usize) -> ComponentShape {
    let n = if matches!(op, GateOp::Not) {
        1
    } else {
        n_inputs
    };
    let h = ((n + 1) as f32 * COMP_HEIGHT_PER_PIN).max(COMP_MIN_HEIGHT);
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
    let label_norm = if matches!(op, GateOp::Not) {
        vec2(0.32, 0.5)
    } else {
        vec2(0.38, 0.5)
    };

    ComponentShape {
        size: vec2(COMP_WIDTH, h),
        outline,
        fill_outline,
        input_anchors,
        output_anchors: vec![out_anchor],
        extra_strokes,
        output_bubbles: vec![bubble],
        label_norm,
    }
}

pub fn mux_shape(sel_width: u8) -> ComponentShape {
    let branches = 1usize << sel_width;
    let h = ((branches + 1) as f32 * COMP_HEIGHT_PER_PIN).max(COMP_MIN_HEIGHT);
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
        label_norm: vec2(0.45, 0.45),
    }
}

pub fn reg_shape() -> ComponentShape {
    let h = ((2 + 1) as f32 * COMP_HEIGHT_PER_PIN).max(COMP_MIN_HEIGHT);

    // input[0] = data, input[1] = write_enable, both on the left edge; output[0] on the right
    let input_anchors = vec![PinAnchor::left(spaced(0, 2)), PinAnchor::left(spaced(1, 2))];

    ComponentShape {
        size: vec2(COMP_WIDTH, h),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors,
        output_anchors: vec![PinAnchor::right(0.5)],
        extra_strokes: vec![],
        output_bubbles: vec![false],
        label_norm: vec2(0.5, 0.5),
    }
}

pub fn demux_shape(sel_width: u8) -> ComponentShape {
    let branches = 1usize << sel_width;
    let h = ((branches + 1) as f32 * COMP_HEIGHT_PER_PIN).max(COMP_MIN_HEIGHT);
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
        label_norm: vec2(0.55, 0.45),
    }
}

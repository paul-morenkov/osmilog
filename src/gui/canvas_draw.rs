//! Canvas rendering: the pure painter functions that draw the grid, placed
//! components/tunnels, their ghosts, and pin/wire colors. These take a `Painter`
//! plus read-only state (a `PlacedComponent`/`PlacedTunnel`, the `Circuit` for
//! live values, `Camera`/`Theme`); they own no app state. The orchestrating
//! `OsmilogApp::draw_canvas` calls them.
//!
//! Shared pixel constants and the pin-position / bounding-rect geometry helpers
//! live on `gui::app` (used by hit-testing too) and are imported here.

use egui::epaint::{PathShape, PathStroke};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke};

use crate::gui::app::{
    comp_pin_pos, component_bounding_rect, tunnel_bounding_rect, tunnel_pin_pos, PlacedTunnel,
    COMP_STROKE, PIN_RADIUS, WIRE_THICKNESS_THICK, WIRE_THICKNESS_THIN,
};
use crate::gui::geometry::{tunnel_shape, Camera, GridPos, LABEL_FONT_SIZE};
use crate::gui::placed_component::PlacedComponent;
use crate::gui::shape::{tessellate_path, BUBBLE_R};
use crate::gui::theme::Theme;
use crate::sim::circuit::{Circuit, TunnelRole};
use crate::sim::component::{ComponentSpec, Constant, PinId};
use crate::sim::value::Value;

/// Minimum on-screen spacing (px) kept between drawn grid dots; `draw_grid`
/// thins to a coarser stride of grid cells rather than let dots crowd closer
/// than this as the view zooms out.
const GRID_DOT_MIN_SPACING_PX: f32 = 16.0;

/// The stroke (color + weight) a wire/pin at value `val` is drawn with.
pub(crate) fn value_stroke(theme: Theme, val: Value) -> Stroke {
    let (color, weight) = match val {
        Value::Floating => (theme.value_floating, WIRE_THICKNESS_THIN),
        Value::Invalid => (theme.value_invalid, WIRE_THICKNESS_THICK),
        Value::Fixed { bits, width } => (
            if bits == 0 {
                theme.value_low
            } else {
                theme.value_high
            },
            if width == 1 {
                WIRE_THICKNESS_THIN
            } else {
                WIRE_THICKNESS_THICK
            },
        ),
    };
    Stroke::new(weight, color)
}

// egui's line segments use butt caps with no joins, so two segments meeting
// at a grid-node corner leave a visible notch (the stroke doesn't reach past
// the shared center point in the perpendicular direction). Extending each
// end by half the stroke width fills that gap, at the cost of slightly
// overshooting unjoined endpoints (wire tips, pins) by the same amount.
pub(crate) fn extend_segment(p0: Pos2, p1: Pos2, extend: f32) -> (Pos2, Pos2) {
    let delta = p1 - p0;
    let len = delta.length();
    if len < f32::EPSILON {
        return (p0, p1);
    }
    let dir = delta / len;
    (p0 - dir * extend, p1 + dir * extend)
}

pub(crate) fn draw_grid(painter: &Painter, clip_rect: Rect, camera: Camera, theme: Theme) {
    let step = camera.grid_scale();
    // As the view zooms out, thin the drawn dots to every `stride`-th grid
    // cell so on-screen spacing never crowds below GRID_DOT_MIN_SPACING_PX -
    // otherwise a wide-out view would paint thousands of near-touching dots.
    let stride = grid_dot_stride(step);
    let cell_x1 = ((clip_rect.right() - camera.pan.x) / step).ceil() as i32;
    let cell_y1 = ((clip_rect.bottom() - camera.pan.y) / step).ceil() as i32;
    let x0 = (((clip_rect.left() - camera.pan.x) / step).floor() as i32).div_euclid(stride) * stride;
    let y0 = (((clip_rect.top() - camera.pan.y) / step).floor() as i32).div_euclid(stride) * stride;

    let mut gx = x0;
    while gx <= cell_x1 {
        let mut gy = y0;
        while gy <= cell_y1 {
            painter.circle_filled(
                camera.grid_to_screen(GridPos::new(gx, gy)),
                1.0,
                theme.grid_dot,
            );
            gy += stride;
        }
        gx += stride;
    }
}

/// Smallest stride (in grid cells, one of 1/2/5 times a power of ten) that
/// keeps on-screen dot spacing at least `GRID_DOT_MIN_SPACING_PX` given
/// `cell_px` pixels per grid cell - the standard "nice numbers" tick-spacing
/// pick, applied to grid dots instead of axis labels.
fn grid_dot_stride(cell_px: f32) -> i32 {
    if cell_px >= GRID_DOT_MIN_SPACING_PX {
        return 1;
    }
    let target = GRID_DOT_MIN_SPACING_PX / cell_px;
    let mut magnitude = 1i32;
    loop {
        for base in [1, 2, 5] {
            let candidate = base * magnitude;
            if candidate as f32 >= target {
                return candidate;
            }
        }
        magnitude *= 10;
    }
}

// A small crosshair marking where a branch would tap an existing wire.
pub(crate) fn draw_reticle(painter: &Painter, pos: Pos2, camera: Camera, theme: Theme) {
    let r = camera.scale(PIN_RADIUS + 1.0);
    let stroke = Stroke::new(camera.scale(1.0), theme.wire_drag_preview);
    painter.line_segment([pos - egui::vec2(r, 0.0), pos + egui::vec2(r, 0.0)], stroke);
    painter.line_segment([pos - egui::vec2(0.0, r), pos + egui::vec2(0.0, r)], stroke);
}

pub(crate) fn draw_component(
    painter: &Painter,
    pc: &PlacedComponent,
    camera: Camera,
    circuit: &Circuit,
    is_selected: bool,
    theme: Theme,
) {
    puffin::profile_function!();
    let shape = &pc.shape;
    let rect = component_bounding_rect(pc, camera);
    let fill = theme.component_fill;
    let (stroke_w, stroke_col) = if is_selected {
        (camera.scale(COMP_STROKE + 1.0), theme.outline_selected)
    } else {
        (camera.scale(COMP_STROKE), theme.outline_default)
    };
    let outline_stroke = Stroke::new(stroke_w, stroke_col);

    // Fill: use the convex fill_outline if provided (avoids epaint's concave polygon artifact),
    // otherwise fall back to the regular outline.
    let fill_pts = tessellate_path(
        shape.fill_outline.as_deref().unwrap_or(&shape.outline),
        rect,
    );
    painter.add(egui::Shape::Path(PathShape {
        points: fill_pts,
        closed: true,
        fill,
        stroke: Stroke::NONE.into(),
    }));

    // Stroke: always use the full outline (may include concave curves).
    let stroke_pts = tessellate_path(&shape.outline, rect);
    painter.add(egui::Shape::Path(PathShape {
        points: stroke_pts,
        closed: true,
        fill: Color32::TRANSPARENT,
        stroke: PathStroke::new(stroke_w, stroke_col),
    }));

    for stroke_cmds in &shape.extra_strokes {
        let stroke_pts = tessellate_path(stroke_cmds, rect);
        painter.add(egui::Shape::line(stroke_pts, outline_stroke));
    }

    let bubble_r = camera.scale(BUBBLE_R);
    for (i, &has_bubble) in shape.output_bubbles.iter().enumerate() {
        if has_bubble {
            let anchor = &shape.output_anchors[i];
            // The pin sits one cell beyond the body edge; the bubble is drawn in
            // the gap, just outside the edge (one cell back from the pin).
            let pin = comp_pin_pos(shape, pc.grid_pos, camera, PinId::output(i as u8));
            let edge = pin - anchor.wire_dir * camera.grid_scale();
            let center = edge + anchor.wire_dir * bubble_r;
            painter.circle_filled(center, bubble_r, fill);
            painter.circle_stroke(center, bubble_r, outline_stroke);
        }
    }

    for label in &shape.labels {
        let label_pos = egui::pos2(
            rect.left() + label.pos.x * rect.width(),
            rect.top() + label.pos.y * rect.height(),
        );
        painter.text(
            label_pos,
            Align2::CENTER_CENTER,
            label.text,
            FontId::monospace(camera.scale(label.font_size)),
            theme.label_text,
        );
    }

    // A subcircuit's on-canvas label is the referenced document's name, drawn
    // dynamically (like a tunnel label) since it isn't a &'static str.
    if let ComponentSpec::Subcircuit { name, .. } = &pc.spec {
        let label_pos = egui::pos2(
            rect.left() + shape.dynamic_label_pos.x * rect.width(),
            rect.top() + shape.dynamic_label_pos.y * rect.height(),
        );
        painter.text(
            label_pos,
            Align2::CENTER_CENTER,
            name,
            FontId::monospace(camera.scale(LABEL_FONT_SIZE)),
            theme.label_text,
        );
    }

    // A Constant's on-canvas label is its current value, drawn dynamically
    // for the same reason a Subcircuit's name is - distinguishing it visually
    // from an Input at a glance, without a boundary pin to inspect.
    if let ComponentSpec::Constant(Constant { bits, .. }) = &pc.spec {
        let label_pos = egui::pos2(
            rect.left() + shape.dynamic_label_pos.x * rect.width(),
            rect.top() + shape.dynamic_label_pos.y * rect.height(),
        );
        painter.text(
            label_pos,
            Align2::CENTER_CENTER,
            format!("0x{:X}", bits),
            FontId::monospace(camera.scale(LABEL_FONT_SIZE)),
            theme.label_text,
        );
    }

    let pin_r = camera.scale(PIN_RADIUS);
    for i in 0..pc.spec.n_inputs() {
        let pos = comp_pin_pos(shape, pc.grid_pos, camera, PinId::input(i as u8));
        let val = circuit.components[pc.key].pins.inputs[i]
            .map(|nk| circuit.nets[nk].value)
            .unwrap_or(Value::Floating);
        painter.circle_filled(pos, pin_r, value_stroke(theme, val).color);
    }
    for i in 0..pc.spec.n_outputs() {
        let pos = comp_pin_pos(shape, pc.grid_pos, camera, PinId::output(i as u8));
        let val = circuit.components[pc.key].pins.out_cache[i];
        painter.circle_filled(pos, pin_r, value_stroke(theme, val).color);
    }
}

pub(crate) fn draw_tunnel(
    painter: &Painter,
    pt: &PlacedTunnel,
    camera: Camera,
    circuit: &Circuit,
    is_selected: bool,
    theme: Theme,
) {
    puffin::profile_function!();
    let shape = tunnel_shape(pt.role);
    let rect = tunnel_bounding_rect(pt, camera);
    // Distinct fill from components (theme's "open" widget tone), to visually
    // distinguish tunnels.
    let fill = theme.tunnel_fill;
    let (stroke_w, stroke_col) = if is_selected {
        (camera.scale(COMP_STROKE + 1.0), theme.outline_selected)
    } else {
        (camera.scale(COMP_STROKE), theme.outline_default)
    };

    let fill_pts = tessellate_path(&shape.outline, rect);
    painter.add(egui::Shape::Path(PathShape {
        points: fill_pts,
        closed: true,
        fill,
        stroke: Stroke::NONE.into(),
    }));

    let stroke_pts = tessellate_path(&shape.outline, rect);
    painter.add(egui::Shape::Path(PathShape {
        points: stroke_pts,
        closed: true,
        fill: Color32::TRANSPARENT,
        stroke: PathStroke::new(stroke_w, stroke_col),
    }));

    let label_pos = egui::pos2(
        rect.left() + shape.dynamic_label_pos.x * rect.width(),
        rect.top() + shape.dynamic_label_pos.y * rect.height(),
    );
    painter.text(
        label_pos,
        Align2::CENTER_CENTER,
        &pt.label,
        FontId::monospace(camera.scale(LABEL_FONT_SIZE)),
        theme.label_text,
    );

    let val = circuit
        .tunnels
        .get(pt.key)
        .and_then(|t| t.net)
        .map(|nk| circuit.nets[nk].value)
        .unwrap_or(Value::Floating);
    painter.circle_filled(
        tunnel_pin_pos(pt, camera),
        camera.scale(PIN_RADIUS),
        value_stroke(theme, val).color,
    );
}

pub(crate) fn draw_ghost(
    painter: &Painter,
    spec: &ComponentSpec,
    grid_pos: GridPos,
    camera: Camera,
    theme: Theme,
) {
    let shape = spec.shape();
    let rect = Rect::from_min_size(camera.grid_to_screen(grid_pos), shape.size * camera.zoom);
    let ghost_col = theme.ghost_preview;
    let stroke_w = camera.scale(COMP_STROKE);

    let pts = tessellate_path(&shape.outline, rect);
    painter.add(egui::Shape::Path(PathShape {
        points: pts,
        closed: true,
        fill: Color32::TRANSPARENT,
        stroke: PathStroke::new(stroke_w, ghost_col),
    }));

    for stroke_cmds in &shape.extra_strokes {
        let stroke_pts = tessellate_path(stroke_cmds, rect);
        painter.add(egui::Shape::line(
            stroke_pts,
            Stroke::new(stroke_w, ghost_col),
        ));
    }

    for label in &shape.labels {
        let label_pos = egui::pos2(
            rect.left() + label.pos.x * rect.width(),
            rect.top() + label.pos.y * rect.height(),
        );
        painter.text(
            label_pos,
            Align2::CENTER_CENTER,
            label.text,
            FontId::monospace(camera.scale(LABEL_FONT_SIZE)),
            ghost_col,
        );
    }

    if let ComponentSpec::Subcircuit { name, .. } = spec {
        let label_pos = egui::pos2(
            rect.left() + shape.dynamic_label_pos.x * rect.width(),
            rect.top() + shape.dynamic_label_pos.y * rect.height(),
        );
        painter.text(
            label_pos,
            Align2::CENTER_CENTER,
            name,
            FontId::monospace(camera.scale(LABEL_FONT_SIZE)),
            ghost_col,
        );
    }
}

pub(crate) fn draw_tunnel_ghost(
    painter: &Painter,
    role: TunnelRole,
    grid_pos: GridPos,
    camera: Camera,
    theme: Theme,
) {
    let shape = tunnel_shape(role);
    let rect = Rect::from_min_size(camera.grid_to_screen(grid_pos), shape.size * camera.zoom);
    let ghost_col = theme.ghost_preview;

    let pts = tessellate_path(&shape.outline, rect);
    painter.add(egui::Shape::Path(PathShape {
        points: pts,
        closed: true,
        fill: Color32::TRANSPARENT,
        stroke: PathStroke::new(camera.scale(COMP_STROKE), ghost_col),
    }));

    let label_pos = egui::pos2(
        rect.left() + shape.dynamic_label_pos.x * rect.width(),
        rect.top() + shape.dynamic_label_pos.y * rect.height(),
    );
    let label = match role {
        TunnelRole::Feed => "TUN(F)",
        TunnelRole::Pull => "TUN(P)",
    };
    painter.text(
        label_pos,
        Align2::CENTER_CENTER,
        label,
        FontId::monospace(camera.scale(LABEL_FONT_SIZE)),
        ghost_col,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_dot_stride_is_1_above_min_spacing() {
        assert_eq!(grid_dot_stride(GRID_DOT_MIN_SPACING_PX), 1);
        assert_eq!(grid_dot_stride(GRID_DOT_MIN_SPACING_PX * 2.0), 1);
    }

    #[test]
    fn grid_dot_stride_picks_smallest_nice_stride_that_fits() {
        // At half the min spacing, a stride of 2 already gets there.
        assert_eq!(grid_dot_stride(GRID_DOT_MIN_SPACING_PX / 2.0), 2);
        // At a fifth, stride 5 is the smallest nice number that clears it.
        assert_eq!(grid_dot_stride(GRID_DOT_MIN_SPACING_PX / 4.5), 5);
        // At a tenth, stride 10.
        assert_eq!(grid_dot_stride(GRID_DOT_MIN_SPACING_PX / 10.0), 10);
    }

    #[test]
    fn grid_dot_stride_result_always_clears_the_minimum_spacing() {
        for cell_px in [0.5, 1.0, 2.0, 2.5, 3.0, 7.0, 15.9, 16.0, 50.0] {
            let stride = grid_dot_stride(cell_px);
            assert!(
                cell_px * stride as f32 >= GRID_DOT_MIN_SPACING_PX,
                "cell_px={cell_px} stride={stride} leaves dots too close"
            );
        }
    }
}

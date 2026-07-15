use egui::vec2;
use egui::{Pos2, Vec2};
use serde::{Deserialize, Serialize};

use crate::gui::shape::{ComponentLabel, ComponentShape, PinAnchor, ShapeCmd};
use crate::sim::circuit::TunnelRole;
use crate::sim::component::{FanDirection, GateOp};

// ── Grid unit ───────────────────────────────────────────────────────────────
//
// Everything below is declared in whole grid CELLS (u32). Pixels enter the
// picture only through `px()`

/// Pixels per grid cell at zoom 1.0 - the sole cell-pixel conversion factor.
pub const GRID_SIZE: f32 = 10.0;
pub const LABEL_FONT_SIZE: f32 = 8.0;

/// Zoom limits for the canvas camera.
pub const ZOOM_MIN: f32 = 0.25;
pub const ZOOM_MAX: f32 = 4.0;

/// Log-zoom change per pixel of ctrl(+cmd)+scroll, fed directly into egui's own
/// `Options::input_options.scroll_zoom_speed` (see `handle_camera_input`) - egui
/// computes its ctrl-scroll zoom factor as `exp(scroll_px * scroll_zoom_speed)`,
/// so setting this constant *is* the sensitivity knob rather than a second scaling
/// stacked on top of egui's own default. Pinch-to-zoom (multi-touch) is a direct
/// physical finger-distance ratio and is unaffected by this constant.
pub const ZOOM_SCROLL_SPEED: f32 = 1.0 / 400.0;

/// A whole-cell count converted to pixels (at zoom 1.0).
pub const fn px(cells: u32) -> f32 {
    cells as f32 * GRID_SIZE
}

/// Width of components whose pins sit only on the left/right edges
const EDGE_BODY_W: u32 = 2;

/// Half-width of components that also carry a centered top/bottom-edge pin
/// (mux/demux selector, arithmetic carry, encoder enable). Kept even so
/// `MUX_CENTER_COL` lands on a whole cell.
const MUX_HALF_W: u32 = 1;
const MUX_W: u32 = 2 * MUX_HALF_W;
/// Center column of a `MUX_W`-wide body.
const MUX_CENTER_COL: u32 = MUX_HALF_W;

/// Similar strategy as `MUX_HALF_W` for arithmetic components, but wider.
const ARITH_HALF_W: u32 = 2;
const ARITH_W: u32 = 2 * ARITH_HALF_W;
const ARITH_CENTER_COL: u32 = ARITH_HALF_W;

/// Input / Output box width.
const IO_W: u32 = 2;

// Splitter/combine doesn't compute anything - it just re-routes bits - so it's
// drawn narrow to read as a connector rather than a processing block; only
// left/right-edge pins, so any whole-cell width is on-grid. See splitter_shape()
// for the comb-shaped body this pairs with.
const SPLITTER_W: u32 = 2;
// Normalized x-band of the thin "spine" rectangle; trunk/teeth strokes
// extend from here out to x=0.0/x=1.0 to reach the pins. Kept narrow so each
// side clears the ~3px pin dot radius, or teeth end up hidden under it.
const SPLITTER_BODY_X: (f32, f32) = (0.25, 0.60);

// Tunnels have their own width to account for a potentially long label.
const TUNNEL_W: u32 = 4;

// Constant carries its current value as a dynamic on-canvas label (like a
// Tunnel's), so it gets the same wider footprint rather than Input/Output's
// bare IO_W box.
const CONST_W: u32 = 4;

const REG_W: u32 = 3;

// Same width as Reg (fits "D"/"LD"/"SH"/"0" labels); ShiftReg's height instead
// scales with num_stages, so it stays a dedicated constant.
const SHIFT_REG_W: u32 = 3;

const ROM_W: u32 = 7;

// Same width as Rom - the spec calls for a RAM box "similarly sized to the
// ROM component" - taller instead of wider to fit the 4 stacked left-edge
// pins (see ram_size).
const RAM_W: u32 = ROM_W;

// A subcircuit is a plain box carrying the referenced document's name; wide
// enough for a short name and for pins to read as belonging to distinct sides.
const SUBCIRCUIT_W: u32 = 6;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(from = "[i32; 2]", into = "[i32; 2]")]
pub struct GridPos {
    pub x: i32,
    pub y: i32,
}

impl GridPos {
    pub const ZERO: Self = Self { x: 0, y: 0 };

    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

impl From<[i32; 2]> for GridPos {
    fn from(p: [i32; 2]) -> Self {
        Self { x: p[0], y: p[1] }
    }
}

impl From<GridPos> for [i32; 2] {
    fn from(p: GridPos) -> Self {
        [p.x, p.y]
    }
}

// ── Camera ────────────────────────────────────────────────────────────────────
//
// The view transform for the canvas: a screen-pixel `pan` offset plus a `zoom`
// scale factor. The screen<->grid transform is
//   screen = grid * (GRID_SIZE * zoom) + pan
// and everything drawn/hit-tested funnels through `grid_to_screen`/
// `screen_to_grid`. Fixed pixel sizes (radii, strokes, fonts) scale via `scale`
// so the whole canvas zooms as one, not just the spacing between things.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// Screen-pixel offset of the grid origin.
    pub pan: Vec2,
    /// Scale factor; 1.0 is the default (GRID_SIZE px per cell).
    pub zoom: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            pan: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

impl Camera {
    /// Pixels per grid cell at the current zoom.
    pub fn grid_scale(&self) -> f32 {
        GRID_SIZE * self.zoom
    }

    pub fn grid_to_screen(&self, gp: GridPos) -> Pos2 {
        let s = self.grid_scale();
        Pos2::new(gp.x as f32 * s + self.pan.x, gp.y as f32 * s + self.pan.y)
    }

    pub fn screen_to_grid(&self, pos: Pos2) -> GridPos {
        let s = self.grid_scale();
        GridPos::new(
            ((pos.x - self.pan.x) / s).round() as i32,
            ((pos.y - self.pan.y) / s).round() as i32,
        )
    }

    /// Scale a fixed pixel measurement (radius / stroke width / font size) by zoom.
    pub fn scale(&self, px: f32) -> f32 {
        px * self.zoom
    }

    /// Applies one frame of camera input over the canvas `response`: middle-drag
    /// pan and Ctrl(+Cmd)/pinch cursor-anchored zoom. Called before drawing so
    /// the view transform is current for the frame.
    pub fn handle_input(&mut self, response: &egui::Response, ctx: &egui::Context) {
        puffin::profile_function!();
        // Pan: middle-button drag. Use the raw pointer delta - `drag_delta`
        // tracks only the primary button.
        if response.dragged_by(egui::PointerButton::Middle) {
            self.pan += ctx.input(|i| i.pointer.delta());
        }

        // Zoom: Ctrl(+Cmd)+scroll, only while hovering the canvas. egui folds a
        // ctrl-scroll (its default `zoom_modifier`) - and any trackpad pinch -
        // into `zoom_delta()`, a multiplicative factor (1.0 = no change). egui's
        // own scroll-zoom sensitivity is set directly at startup (see
        // ZOOM_SCROLL_SPEED, set in `OsmilogApp::new`) rather than rescaled here
        // - `zoom_delta()` is already exactly the factor we want to apply.
        if response.hovered() {
            let zoom_delta = ctx.input(|i| i.zoom_delta());
            if zoom_delta != 1.0 {
                if let Some(cursor) = ctx.pointer_hover_pos() {
                    let old = self.zoom;
                    let new = (old * zoom_delta).clamp(ZOOM_MIN, ZOOM_MAX);
                    if new != old {
                        // Keep the grid point under the cursor fixed:
                        // pan' = cursor - (cursor - pan) * (new / old).
                        let c = cursor.to_vec2();
                        self.pan = c - (c - self.pan) * (new / old);
                        self.zoom = new;
                    }
                }
            }
        }
    }
}

// ── Stack geometry (in cells) ─────────────────────────────────────────────────

/// How a stack of pins is distributed along a component edge. Both layouts
/// keep the stack's centre row whole (height is always even), so an
/// opposite centred pin always has a definite row to line up with.
#[derive(Clone, Copy)]
enum Pitch {
    /// 2 cells per pin: rows 1, 3, 5, … A roomy, Logisim-style stack.
    Spread,
    /// 1 cell per pin. An even pin count leaves the centre row empty (a
    /// 2-cell gap) so it stays whole; an odd count's middle pin sits there.
    Tight,
}

impl Pitch {
    /// Grid row of pin `i` (0-based) in a stack of `k` pins.
    const fn row(self, i: usize, k: usize) -> u32 {
        match self {
            Pitch::Spread => 2 * (i as u32) + 1,
            Pitch::Tight => {
                // Pack up from row 1; once past the lower half, bump down by the
                // gap size - 1 cell for an even stack (leaving the centre row
                // empty), 0 for an odd one (its middle pin sits on the centre).
                let bump: u32 = if i >= k / 2 { 1 - (k as u32 % 2) } else { 0 };
                i as u32 + 1 + bump
            }
        }
    }

    /// Height (in cells) of an edge with `k` pins (k>=1). Always even, so
    /// `height / 2` is a whole centre row.
    const fn height(self, k: usize) -> u32 {
        let k = if k == 0 { 1 } else { k };
        match self {
            Pitch::Spread => 2 * (k as u32),
            // k+1 when odd (contiguous rows), k+2 when even (one extra for the
            // gap). `(k+1) % 2` is 0 for odd k and 1 for even k.
            Pitch::Tight => k as u32 + 1 + (k as u32 + 1) % 2,
        }
    }
}

/// Gates pack inputs tightly once there are enough that a roomy spread would
/// make the body needlessly tall.
const fn gate_pitch(n_inputs: usize) -> Pitch {
    if n_inputs > 3 {
        Pitch::Tight
    } else {
        Pitch::Spread
    }
}

/// Mux/demux/encoder branches pack tightly once there are enough of them
/// (sel_width >= 2, i.e. >= 4 branches) that a roomy spread would make the
/// body needlessly tall.
const fn sel_pitch(sel_width: u8) -> Pitch {
    if sel_width >= 2 {
        Pitch::Tight
    } else {
        Pitch::Spread
    }
}

/// A subcircuit's boundary pins (variable count, derived from the referenced
/// document's Input/Output components) pack tightly once a roomy spread would
/// make the body needlessly tall - same threshold as gates.
const fn sub_pitch(k: usize) -> Pitch {
    if k > 3 {
        Pitch::Tight
    } else {
        Pitch::Spread
    }
}

// Spread is the default layout; these terse wrappers keep the many spread call
// sites readable (Spread ignores the pin count, so the `0` below is a placeholder).
fn pin_row(i: usize) -> u32 {
    Pitch::Spread.row(i, 0)
}

const fn stack_h(k: usize) -> u32 {
    Pitch::Spread.height(k)
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

// ── Bounding-box sizes ────────────────────────────────────────────────────────
//
// Zero-allocation size queries, the single source of truth for the
// width/height formulas - the *_shape() functions call the same cell
// helpers, so callers like component_bounding_rect needn't build a full
// ComponentShape just to read its size.

pub const fn gate_size(op: GateOp, n_inputs: usize) -> Vec2 {
    let n = if matches!(op, GateOp::Not) {
        1
    } else {
        n_inputs
    };
    vec2(px(EDGE_BODY_W), px(gate_pitch(n).height(n)))
}

pub const fn mux_size(sel_width: u8) -> Vec2 {
    let branches = 1usize << sel_width;
    vec2(px(MUX_W), px(sel_pitch(sel_width).height(branches)))
}

pub const fn demux_size(sel_width: u8) -> Vec2 {
    mux_size(sel_width) // same branches stack
}

pub const fn splitter_size(arms: u8) -> Vec2 {
    // Arms pack tightly (1 cell each) so a wide fan stays a compact connector.
    vec2(px(SPLITTER_W), px(Pitch::Tight.height(arms as usize)))
}

pub const fn reg_size() -> Vec2 {
    // D + WE as a 2-pin stack (height 4 cells); the output centers on grid row 2.
    vec2(px(REG_W), px(stack_h(2)))
}

pub const fn counter_size() -> Vec2 {
    // 3 inputs (load/data/count) stacked on the left, sizing the body; the 2
    // outputs (Q/carry) center on the right within that same height, same
    // technique as comparator_size's busier-side-drives-height.
    vec2(px(REG_W), px(stack_h(3)))
}

// Height packs one row per preamble pin (D, optionally L, SH) plus one row
// per stage, contiguously (no Pitch stack - there's no symmetric centre row
// to preserve here since the bottom-edge reset pin isn't centered on it),
// plus one extra row so the reset pin has the same 1-cell gap off the last
// stage row that reg_size/flip_flop_size leave before their bottom pins.
pub const fn shift_reg_size(num_stages: usize, parallel_load: bool) -> Vec2 {
    let preamble = if parallel_load { 3 } else { 2 };
    let total_rows = preamble + num_stages as u32;
    vec2(px(SHIFT_REG_W), px(total_rows + 1))
}

// Square body, same proportions as op2_size (ARITH_W == stack_h(2)) even though
// a flip-flop only has one data-side input - the write-enable pin lives on the
// bottom edge instead of stacking with it on the left.
pub const fn flip_flop_size() -> Vec2 {
    vec2(px(ARITH_W), px(stack_h(2)))
}

// Height accounts only for the two addend pins on the left edge, same formula as a
// 2-input gate - the carry-in/carry-out pins sit at the bottom/top edges (like
// encoder's enable_in/enable_out) and don't consume extra vertical space of their own.
pub const fn op2_size() -> Vec2 {
    vec2(px(ARITH_W), px(stack_h(2)))
}

// Height scales off the busier side - the 3 comparison outputs on the right,
// packed tightly (1 cell each) - rather than the 2 operand inputs on the left.
pub const fn comparator_size() -> Vec2 {
    vec2(px(EDGE_BODY_W), px(Pitch::Tight.height(3)))
}

// Height scales with the arm count on the left edge, but never below 4 cells so
// the three right-side pins (enable_out at top, selector + group as a centered
// pair) always have room - the bottom/top pins (enable_in/enable_out) sit at the
// edges and don't consume extra vertical space of their own.
pub const fn encoder_size(sel_width: u8) -> Vec2 {
    let arms = 1usize << sel_width;
    let k = if arms < 2 { 2 } else { arms };
    vec2(px(MUX_W), px(sel_pitch(sel_width).height(k)))
}

// A plain box, same footprint as reg_size (REG_W wide, 4 cells tall) so the
// single A input on the left and D output on the right both center on grid row 2.
pub const fn rom_size() -> Vec2 {
    vec2(px(ROM_W), px(stack_h(2)))
}

// Same width as Rom (RAM_W == ROM_W); height scales for the 4 stacked
// left-edge pins (A/WE/LE/DI), one more stack level than Reg's D/WE pair.
pub const fn ram_size() -> Vec2 {
    vec2(px(RAM_W), px(stack_h(4)))
}

pub const fn io_size() -> Vec2 {
    // 1-pin edge → 2 cells tall, so the single side-pin centers on grid row 1.
    vec2(px(IO_W), px(stack_h(1)))
}

pub const fn constant_size() -> Vec2 {
    vec2(px(CONST_W), px(stack_h(1)))
}

// Height scales off whichever boundary side (inputs on the left, outputs on the
// right) has more pins; each side packs top-down from row 1 with its own pitch.
// Both pitch heights are even, so their max is too.
pub fn subcircuit_size(n_in: usize, n_out: usize) -> Vec2 {
    let h_cells = sub_pitch(n_in)
        .height(n_in)
        .max(sub_pitch(n_out).height(n_out));
    vec2(px(SUBCIRCUIT_W), px(h_cells))
}

// ── Shape builders ────────────────────────────────────────────────────────────

pub fn input_shape() -> ComponentShape {
    io_shape(true)
}

pub fn output_shape() -> ComponentShape {
    io_shape(false)
}

// Input (source, pin on the right) and Output (sink, pin on the left) share a
// plain box; the single pin centers on the middle grid row.
fn io_shape(is_input: bool) -> ComponentShape {
    let center_row = stack_h(1) / 2; // 1
    let (input_anchors, output_anchors, output_bubbles) = if is_input {
        (
            vec![],
            vec![PinAnchor::right(IO_W, center_row)],
            vec![false],
        )
    } else {
        (vec![PinAnchor::left(center_row)], vec![], vec![])
    };

    ComponentShape {
        size: io_size(),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors,
        output_anchors,
        extra_strokes: vec![],
        output_bubbles,
        labels: vec![],
        dynamic_label_pos: Vec2::ZERO,
    }
}

// A single-output source, like Input, but its current value is drawn as a
// dynamic label (see draw_component) rather than left blank - so it gets its
// own wider box (CONST_W) to fit that text instead of reusing io_shape.
pub fn constant_shape() -> ComponentShape {
    let center_row = stack_h(1) / 2; // 1
    ComponentShape {
        size: constant_size(),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors: vec![],
        output_anchors: vec![PinAnchor::right(CONST_W, center_row)],
        extra_strokes: vec![],
        output_bubbles: vec![false],
        labels: vec![],
        dynamic_label_pos: vec2(0.5, 0.5),
    }
}

pub fn gate_shape(op: GateOp, n_inputs: usize) -> ComponentShape {
    let n = if matches!(op, GateOp::Not) {
        1
    } else {
        n_inputs
    };
    let pitch = gate_pitch(n); // tight once n > 3, else spread
    let h_cells = pitch.height(n);
    let bubble = matches!(op, GateOp::Nand | GateOp::Nor | GateOp::Xnor | GateOp::Not);

    let (outline, fill_outline, extra_strokes) = match op {
        GateOp::And | GateOp::Nand => (and_outline(), None, vec![]),
        GateOp::Or | GateOp::Nor => (or_outline(), Some(or_fill_outline()), vec![]),
        GateOp::Xor | GateOp::Xnor => {
            (or_outline(), Some(or_fill_outline()), vec![xor_extra_arc()])
        }
        GateOp::Not => (not_outline(), None, vec![]),
    };

    // Output centers on the input stack: the (whole) centre row.
    let center_row = h_cells / 2;
    let out_anchor = if bubble {
        PinAnchor::right_bubble(EDGE_BODY_W, center_row)
    } else {
        PinAnchor::right(EDGE_BODY_W, center_row)
    };
    let input_anchors = (0..n).map(|i| PinAnchor::left(pitch.row(i, n))).collect();

    ComponentShape {
        size: vec2(px(EDGE_BODY_W), px(h_cells)),
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
    let pitch = sel_pitch(sel_width); // tight once sel_width >= 2
    let h_cells = pitch.height(branches);
    const T: f32 = 0.2;

    let outline = vec![
        ShapeCmd::MoveTo(vec2(0.0, 0.0)),
        ShapeCmd::LineTo(vec2(1.0, T)),
        ShapeCmd::LineTo(vec2(1.0, 1.0 - T)),
        ShapeCmd::LineTo(vec2(0.0, 1.0)),
    ];

    // input[0] = selector → bottom-center of shape; input[1..] = data → left edge
    let sel_anchor = PinAnchor::bottom(MUX_CENTER_COL, h_cells);
    let data_anchors = (0..branches).map(|i| PinAnchor::left(pitch.row(i, branches)));
    let input_anchors = std::iter::once(sel_anchor).chain(data_anchors).collect();

    // The selector pin sits on the bottom grid row, but the trapezoid's bottom
    // edge tapers up to y = 1 - T/2 at the center; draw a short stub down to the
    // pin (like the splitter's teeth) so the wire visibly meets the body.
    let sel_stub = vec![
        ShapeCmd::MoveTo(vec2(0.5, 1.0 - T / 2.0)),
        ShapeCmd::LineTo(vec2(0.5, 1.0)),
    ];

    ComponentShape {
        size: mux_size(sel_width),
        outline,
        fill_outline: None,
        input_anchors,
        output_anchors: vec![PinAnchor::right(MUX_W, h_cells / 2)],
        extra_strokes: vec![sel_stub],
        output_bubbles: vec![false],
        labels: vec![],
        dynamic_label_pos: Vec2::ZERO,
    }
}

pub fn reg_shape() -> ComponentShape {
    let h_cells = stack_h(2); // 4

    // input[0] = data (row 1), input[1] = write_enable (row 3), both on the
    // left edge; input[2] = async reset on the bottom edge, toward the right
    // (one cell in from the corner); output[0] centers on the right (row 2).
    let input_anchors = vec![
        PinAnchor::left(pin_row(0)),
        PinAnchor::left(pin_row(1)),
        PinAnchor::bottom(REG_W - 1, h_cells),
    ];

    // "D"/"WE" sit level with their pins (same y as the anchors above), offset
    // right of the left-edge pin dot with room to spare in the box. The reset
    // "0" sits just above its bottom-edge pin, a fixed pixel inset up (like the
    // flip-flops' bottom-edge labels).
    let row_y = |i: usize| pin_row(i) as f32 / h_cells as f32;
    const EDGE_LABEL_INSET_PX: f32 = 6.0;
    let reset_y = 1.0 - EDGE_LABEL_INSET_PX / px(h_cells);
    let labels = vec![
        ComponentLabel {
            text: "D",
            pos: vec2(0.28, row_y(0)),
            ..Default::default()
        },
        ComponentLabel {
            text: "WE",
            pos: vec2(0.28, row_y(1)),
            ..Default::default()
        },
        ComponentLabel {
            text: "0",
            pos: vec2((REG_W - 1) as f32 / REG_W as f32, reset_y),
            ..Default::default()
        },
    ];

    ComponentShape {
        size: reg_size(),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors,
        output_anchors: vec![PinAnchor::right(REG_W, h_cells / 2)],
        extra_strokes: vec![],
        output_bubbles: vec![false],
        labels,
        dynamic_label_pos: Vec2::ZERO,
    }
}

// Pin layout matches CounterConf's fixed pin order: input[0] = data, input[1]
// = load, input[2] = count (left edge), but the requested visual order
// top-to-bottom is load/data/count, so pin index and grid row diverge here -
// unlike reg_shape/comparator_shape, where index order and row order agree.
// output[0] = Q, output[1] = carry, centered as a pair on the right within
// the busier (3-pin) left stack's height - same "fewer side centers on the
// busier side" technique as comparator_shape.
pub fn counter_shape() -> ComponentShape {
    let h_cells = stack_h(3); // 6
    let center_row = h_cells / 2; // 3

    let input_anchors = vec![
        PinAnchor::left(pin_row(1)), // data -> middle row
        PinAnchor::left(pin_row(0)), // load -> top row
        PinAnchor::left(pin_row(2)), // count -> bottom row
    ];
    let output_anchors = vec![
        PinAnchor::right(REG_W, center_row - 1), // Q
        PinAnchor::right(REG_W, center_row + 1), // carry
    ];

    let row_y = |r: u32| r as f32 / h_cells as f32;
    let labels = vec![
        ComponentLabel {
            text: "L",
            pos: vec2(0.28, row_y(pin_row(0))),
            ..Default::default()
        },
        ComponentLabel {
            text: "D",
            pos: vec2(0.28, row_y(pin_row(1))),
            ..Default::default()
        },
        ComponentLabel {
            text: "CT",
            pos: vec2(0.22, row_y(pin_row(2))),
            ..Default::default()
        },
        ComponentLabel {
            text: "Q",
            pos: vec2(0.68, row_y(center_row - 1)),
            ..Default::default()
        },
        ComponentLabel {
            text: "CO",
            pos: vec2(0.6, row_y(center_row + 1)),
            ..Default::default()
        },
    ];

    ComponentShape {
        size: counter_size(),
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

// A subcircuit: a plain box with `n_in` inputs on the left edge and `n_out`
// outputs on the right, each side packed top-down from row 1 in the pin order
// the component exposes (the GUI derives that from the inner Input/Output
// component positions). The referenced document's name is drawn at
// `dynamic_label_pos` (like a tunnel label) since it isn't known at 'static
// time; `labels` therefore stays empty.
pub fn subcircuit_shape(n_in: usize, n_out: usize) -> ComponentShape {
    let in_pitch = sub_pitch(n_in);
    let out_pitch = sub_pitch(n_out);
    let h_cells = in_pitch.height(n_in).max(out_pitch.height(n_out));

    let input_anchors = (0..n_in)
        .map(|i| PinAnchor::left(in_pitch.row(i, n_in)))
        .collect();
    let output_anchors = (0..n_out)
        .map(|i| PinAnchor::right(SUBCIRCUIT_W, out_pitch.row(i, n_out)))
        .collect();

    ComponentShape {
        size: vec2(px(SUBCIRCUIT_W), px(h_cells)),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors,
        output_anchors,
        extra_strokes: vec![],
        output_bubbles: vec![false; n_out],
        labels: vec![],
        dynamic_label_pos: vec2(0.5, 0.5),
    }
}

// Pin layout matches ShiftRegConf's pin order: input[0] = serial data (row 0),
// input[1] = load (row 1, parallel_load only), input[next] = shift, then one
// stage input per row (parallel_load only), then the async reset (bottom
// edge, toward the right, like reg_shape). Rows are simply contiguous
// top-to-bottom - unlike the Pitch stacks elsewhere, there's no symmetric
// centre row to preserve since the bottom-edge reset pin isn't centered.
// Serial mode has a single output (the last stage, right edge, aligned with
// its row); parallel_load mode has one output per stage, each aligned with
// its own input row.
pub fn shift_reg_shape(num_stages: usize, parallel_load: bool) -> ComponentShape {
    let num_stages = num_stages.max(1);
    let preamble = if parallel_load { 3 } else { 2 };
    let total_rows = preamble + num_stages;
    let h_cells = total_rows as u32 + 1;
    let row = |i: usize| (i + 1) as u32;
    let row_y = |r: u32| r as f32 / h_cells as f32;

    let mut input_anchors = vec![PinAnchor::left(row(0))]; // data
    let mut labels = vec![ComponentLabel {
        text: "D",
        pos: vec2(0.28, row_y(row(0))),
        ..Default::default()
    }];

    let mut next = 1;
    if parallel_load {
        input_anchors.push(PinAnchor::left(row(next))); // load
        labels.push(ComponentLabel {
            text: "L",
            pos: vec2(0.28, row_y(row(next))),
            ..Default::default()
        });
        next += 1;
    }
    input_anchors.push(PinAnchor::left(row(next))); // shift
    labels.push(ComponentLabel {
        text: "SH",
        pos: vec2(0.22, row_y(row(next))),
        ..Default::default()
    });
    next += 1;

    let stage_rows: Vec<u32> = (0..num_stages).map(|i| row(next + i)).collect();
    let output_anchors: Vec<PinAnchor> = if parallel_load {
        for &r in &stage_rows {
            input_anchors.push(PinAnchor::left(r));
        }
        stage_rows
            .iter()
            .map(|&r| PinAnchor::right(SHIFT_REG_W, r))
            .collect()
    } else {
        vec![PinAnchor::right(
            SHIFT_REG_W,
            *stage_rows.last().expect("num_stages >= 1"),
        )]
    };

    // Async reset: bottom edge, toward the right (one cell in from the
    // corner), same placement as reg_shape/flip_flop_shape's reset pin.
    input_anchors.push(PinAnchor::bottom(SHIFT_REG_W - 1, h_cells));
    const EDGE_LABEL_INSET_PX: f32 = 6.0;
    let reset_y = 1.0 - EDGE_LABEL_INSET_PX / px(h_cells);
    labels.push(ComponentLabel {
        text: "0",
        pos: vec2((SHIFT_REG_W - 1) as f32 / SHIFT_REG_W as f32, reset_y),
        ..Default::default()
    });

    let output_bubbles = vec![false; output_anchors.len()];
    ComponentShape {
        size: vec2(px(SHIFT_REG_W), px(h_cells)),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors,
        output_anchors,
        extra_strokes: vec![],
        output_bubbles,
        labels,
        dynamic_label_pos: Vec2::ZERO,
    }
}

// D flip-flop: data in on the left edge, write-enable in on the bottom edge
// (like op2_shape's carry-in), Q out on the right edge - all centered on the
// square body.
pub fn d_flip_flop_shape() -> ComponentShape {
    flip_flop_shape("D")
}

// Same layout as d_flip_flop_shape(); the toggle input replaces the data input.
pub fn t_flip_flop_shape() -> ComponentShape {
    flip_flop_shape("T")
}

// JK/SR flip-flop: both control inputs stack on the left edge (like reg's
// D/WE), write-enable in on the bottom edge (like op2's carry-in), Q out on
// the right edge - all centered on the square body.
pub fn jk_flip_flop_shape() -> ComponentShape {
    two_input_flip_flop_shape("J", "K")
}

// Same layout as jk_flip_flop_shape(); set/reset replace jump/kill.
pub fn sr_flip_flop_shape() -> ComponentShape {
    two_input_flip_flop_shape("S", "R")
}

// Pin layout matches JKFlipFlop/SRFlipFlop's fixed order: input[0]/[1] = the
// two control inputs (left edge, stacked like reg's D/WE), input[2] =
// write-enable (bottom edge, centered); output[0] = Q (right edge, centered).
fn two_input_flip_flop_shape(
    top_label: &'static str,
    bottom_label: &'static str,
) -> ComponentShape {
    let h_cells = stack_h(2); // 4, matching op2's square proportions
    let center_row = h_cells / 2;

    // input[0]/[1] = control inputs (left), input[2] = write-enable (bottom
    // center), input[3] = async reset (bottom edge, toward the right).
    let input_anchors = vec![
        PinAnchor::left(pin_row(0)),
        PinAnchor::left(pin_row(1)),
        PinAnchor::bottom(ARITH_CENTER_COL, h_cells),
        PinAnchor::bottom(ARITH_W - 1, h_cells),
    ];

    const EDGE_LABEL_INSET_PX: f32 = 6.0;
    let h = px(h_cells);
    let we_y = 1.0 - EDGE_LABEL_INSET_PX / h;
    let row_y = |i: usize| pin_row(i) as f32 / h_cells as f32;

    let labels = vec![
        ComponentLabel {
            text: top_label,
            pos: vec2(0.28, row_y(0)),
            ..Default::default()
        },
        ComponentLabel {
            text: bottom_label,
            pos: vec2(0.28, row_y(1)),
            ..Default::default()
        },
        ComponentLabel {
            text: "WE",
            pos: vec2(0.5, we_y),
            ..Default::default()
        },
        ComponentLabel {
            text: "0",
            pos: vec2((ARITH_W - 1) as f32 / ARITH_W as f32, we_y),
            ..Default::default()
        },
    ];

    ComponentShape {
        size: flip_flop_size(),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors,
        output_anchors: vec![PinAnchor::right(ARITH_W, center_row)],
        extra_strokes: vec![],
        output_bubbles: vec![false],
        labels,
        dynamic_label_pos: Vec2::ZERO,
    }
}

// Pin layout matches DFlipFlop/TFlipFlop's fixed order: input[0] = data/toggle
// (left edge, centered), input[1] = write-enable (bottom edge, centered);
// output[0] = Q (right edge, centered).
fn flip_flop_shape(data_label: &'static str) -> ComponentShape {
    let h_cells = stack_h(2); // 4, matching op2's square proportions
    let center_row = h_cells / 2;

    // input[0] = data/toggle (left center), input[1] = write-enable (bottom
    // center), input[2] = async reset (bottom edge, toward the right).
    let input_anchors = vec![
        PinAnchor::left(center_row),
        PinAnchor::bottom(ARITH_CENTER_COL, h_cells),
        PinAnchor::bottom(ARITH_W - 1, h_cells),
    ];

    const EDGE_LABEL_INSET_PX: f32 = 6.0;
    let h = px(h_cells);
    let bottom_y = 1.0 - EDGE_LABEL_INSET_PX / h;
    let row_y = center_row as f32 / h_cells as f32;

    let labels = vec![
        ComponentLabel {
            text: data_label,
            pos: vec2(0.28, row_y),
            ..Default::default()
        },
        ComponentLabel {
            text: "WE",
            pos: vec2(0.5, bottom_y),
            ..Default::default()
        },
        ComponentLabel {
            text: "0",
            pos: vec2((ARITH_W - 1) as f32 / ARITH_W as f32, bottom_y),
            ..Default::default()
        },
    ];

    ComponentShape {
        size: flip_flop_size(),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors,
        output_anchors: vec![PinAnchor::right(ARITH_W, center_row)],
        extra_strokes: vec![],
        output_bubbles: vec![false],
        labels,
        dynamic_label_pos: Vec2::ZERO,
    }
}

pub fn demux_shape(sel_width: u8) -> ComponentShape {
    let branches = 1usize << sel_width;
    let pitch = sel_pitch(sel_width); // tight once sel_width >= 2
    let h_cells = pitch.height(branches);
    const T: f32 = 0.2;

    let outline = vec![
        ShapeCmd::MoveTo(vec2(0.0, T)),
        ShapeCmd::LineTo(vec2(1.0, 0.0)),
        ShapeCmd::LineTo(vec2(1.0, 1.0)),
        ShapeCmd::LineTo(vec2(0.0, 1.0 - T)),
    ];

    // input[0] = data → left center (aligned with the output stack's center);
    // input[1] = selector → bottom center.
    let data_anchor = PinAnchor::left(h_cells / 2);
    let sel_anchor = PinAnchor::bottom(MUX_CENTER_COL, h_cells);
    let output_anchors = (0..branches)
        .map(|i| PinAnchor::right(MUX_W, pitch.row(i, branches)))
        .collect();

    // Stub from the tapered bottom edge down to the on-grid selector pin.
    let sel_stub = vec![
        ShapeCmd::MoveTo(vec2(0.5, 1.0 - T / 2.0)),
        ShapeCmd::LineTo(vec2(0.5, 1.0)),
    ];

    ComponentShape {
        size: demux_size(sel_width),
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
// output[2] = group_out (right edge, a centered pair), output[1] = enable_out (top edge).
pub fn encoder_shape(sel_width: u8) -> ComponentShape {
    let arms = 1usize << sel_width;
    // Never below 4 cells, so the three right-side pins always have room (mirrors
    // encoder_size). height() keeps it even, so the center row stays whole. Once
    // tight kicks in (sel_width >= 2) arms >= 4, so k == arms.
    let k = if arms < 2 { 2 } else { arms };
    let pitch = sel_pitch(sel_width); // tight once sel_width >= 2
    let h_cells = pitch.height(k);
    let h = px(h_cells);
    let center_row = h_cells / 2;

    let enable_in_anchor = PinAnchor::bottom(MUX_CENTER_COL, h_cells);
    let arm_anchors = (0..arms).map(move |i| PinAnchor::left(pitch.row(i, k)));
    let input_anchors = std::iter::once(enable_in_anchor)
        .chain(arm_anchors)
        .collect();

    let enable_out_anchor = PinAnchor::top(MUX_CENTER_COL);
    // selector/group_out sit as a centered pair, one grid row either side of center.
    let sel_row = center_row - 1;
    let grp_row = center_row + 1;
    let output_anchors = vec![
        PinAnchor::right(MUX_W, sel_row),
        enable_out_anchor,
        PinAnchor::right(MUX_W, grp_row),
    ];

    // EN sits just above the bottom edge by a fixed pixel distance rather than a fixed
    // fraction of height - height grows with sel_width (more arms), but the label should
    // stay close to the pin it names instead of drifting toward the middle of a tall body.
    const BOTTOM_LABEL_INSET_PX: f32 = 6.0;
    let en_y = 1.0 - BOTTOM_LABEL_INSET_PX / h;
    let row_y = |row: u32| row as f32 / h_cells as f32;

    let labels = vec![
        ComponentLabel {
            text: "EN",
            pos: vec2(0.5, en_y),
            ..Default::default()
        },
        ComponentLabel {
            text: "S",
            pos: vec2(0.78, row_y(sel_row)),
            ..Default::default()
        },
        ComponentLabel {
            text: "G",
            pos: vec2(0.78, row_y(grp_row)),
            ..Default::default()
        },
    ];

    ComponentShape {
        size: encoder_size(sel_width),
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
// edge), input[2] = carry-in (bottom edge); output[0] = sum (right edge, centered),
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
// divider): two data inputs on the left, a centered result output on the right, and
// carry/borrow-style flow-through pins on the bottom (in) and top (out) edges.
fn op2_shape(
    op_label: &'static str,
    op_font: f32,
    bottom_label: &'static str,
    top_label: &'static str,
) -> ComponentShape {
    let h_cells = stack_h(2); // 4
    let h = px(h_cells);

    let carry_in_anchor = PinAnchor::bottom(ARITH_CENTER_COL, h_cells);
    let input_anchors = vec![
        PinAnchor::left(pin_row(0)),
        PinAnchor::left(pin_row(1)),
        carry_in_anchor,
    ];

    let carry_out_anchor = PinAnchor::top(ARITH_CENTER_COL);
    let output_anchors = vec![PinAnchor::right(ARITH_W, h_cells / 2), carry_out_anchor];

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
        size: vec2(px(ARITH_W), h),
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

// A memory block: single address input "A" on the left edge, single data output
// "D" on the right edge, both centered vertically, with a "ROM" label in the body.
pub fn rom_shape() -> ComponentShape {
    let h_cells = stack_h(2); // 4
    let center_row = h_cells / 2; // 2
    let row_y = center_row as f32 / h_cells as f32;

    let labels = vec![
        ComponentLabel {
            text: "ROM",
            pos: vec2(0.5, 0.5),
            ..Default::default()
        },
        ComponentLabel {
            text: "A",
            pos: vec2(0.2, row_y),
            ..Default::default()
        },
        ComponentLabel {
            text: "D",
            pos: vec2(0.8, row_y),
            ..Default::default()
        },
    ];

    ComponentShape {
        size: rom_size(),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors: vec![PinAnchor::left(center_row)],
        output_anchors: vec![PinAnchor::right(ROM_W, center_row)],
        extra_strokes: vec![],
        output_bubbles: vec![false],
        labels,
        dynamic_label_pos: Vec2::ZERO,
    }
}

// A read/write memory block: address "A", write_enable "WE", load_enable
// "LE" and data_in "DI" stack on the left edge (input order matches Ram's
// fixed pin order); the registered data_out "DO" centers on the right, with
// a "RAM" label in the body - same layout idea as rom_shape, just with 3
// more left-edge control pins.
pub fn ram_shape() -> ComponentShape {
    let h_cells = stack_h(4); // 8
    let center_row = h_cells / 2; // 4

    let input_anchors = vec![
        PinAnchor::left(pin_row(0)),
        PinAnchor::left(pin_row(1)),
        PinAnchor::left(pin_row(2)),
        PinAnchor::left(pin_row(3)),
    ];

    let row_y = |r: u32| r as f32 / h_cells as f32;
    let labels = vec![
        ComponentLabel {
            text: "RAM",
            pos: vec2(0.5, 0.5),
            ..Default::default()
        },
        ComponentLabel {
            text: "A",
            pos: vec2(0.2, row_y(pin_row(0))),
            ..Default::default()
        },
        ComponentLabel {
            text: "WE",
            pos: vec2(0.24, row_y(pin_row(1))),
            ..Default::default()
        },
        ComponentLabel {
            text: "LE",
            pos: vec2(0.24, row_y(pin_row(2))),
            ..Default::default()
        },
        ComponentLabel {
            text: "DI",
            pos: vec2(0.24, row_y(pin_row(3))),
            ..Default::default()
        },
        ComponentLabel {
            text: "DO",
            pos: vec2(0.78, row_y(center_row)),
            ..Default::default()
        },
    ];

    ComponentShape {
        size: ram_size(),
        outline: rect_outline(),
        fill_outline: None,
        input_anchors,
        output_anchors: vec![PinAnchor::right(RAM_W, center_row)],
        extra_strokes: vec![],
        output_bubbles: vec![false],
        labels,
        dynamic_label_pos: Vec2::ZERO,
    }
}

// Pin layout matches Component::comparator's fixed order: input[0]/[1] = the two
// compared operands (left edge, centered on the output stack); output[0] = greater-than,
// output[1] = equal, output[2] = less-than (right edge, evenly spaced, each labeled).
pub fn comparator_shape() -> ComponentShape {
    let pitch = Pitch::Tight; // the 3 outputs pack tightly
    let h_cells = pitch.height(3); // 4
    let center_row = h_cells / 2; // 2

    // Two inputs centered on the 3-output stack: one grid row either side of center.
    let input_anchors = vec![
        PinAnchor::left(center_row - 1),
        PinAnchor::left(center_row + 1),
    ];

    let output_anchors = (0..3)
        .map(|i| PinAnchor::right(EDGE_BODY_W, pitch.row(i, 3)))
        .collect();

    let row_y = |i: usize| pitch.row(i, 3) as f32 / h_cells as f32;
    let labels = vec![
        ComponentLabel {
            text: ">",
            pos: vec2(0.72, row_y(0)),
            ..Default::default()
        },
        ComponentLabel {
            text: "=",
            pos: vec2(0.72, row_y(1)),
            ..Default::default()
        },
        ComponentLabel {
            text: "<",
            pos: vec2(0.72, row_y(2)),
            ..Default::default()
        },
    ];

    ComponentShape {
        size: vec2(px(EDGE_BODY_W), px(h_cells)),
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

// Right draws the classic splitter (trunk in on the left, arms fanning out
// on the right); Left mirrors it horizontally via `mx` and swaps trunk/arm
// anchor lists into a combiner - must match Component::splitter's Left-mode
// pin order (arm index == input pin index, ascending).
pub fn splitter_shape(arms: u8, direction: FanDirection) -> ComponentShape {
    let n = arms as usize;
    let pitch = Pitch::Tight; // arms pack tightly
    let h_cells = pitch.height(n);
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
    let row_y = |row: u32| row as f32 / h_cells as f32;
    let data_y = row_y(pitch.row(0, n));

    // One trunk line from the data pin into the spine, then one tooth line
    // per arm fanning out from the spine to that arm's pin - drawn past the
    // spine's own edges to form the comb, rather than baking the fan into
    // the (concave) outline itself.
    let trunk = vec![
        ShapeCmd::MoveTo(vec2(mx(0.0), data_y)),
        ShapeCmd::LineTo(vec2(mx(x0), data_y)),
    ];
    let teeth = (0..n).map(|i| {
        let y = row_y(pitch.row(i, n));
        vec![
            ShapeCmd::MoveTo(vec2(mx(x1), y)),
            ShapeCmd::LineTo(vec2(mx(1.0), y)),
        ]
    });
    let extra_strokes = std::iter::once(trunk).chain(teeth).collect();

    let arm_anchor = |i: usize| {
        if flip {
            PinAnchor::left(pitch.row(i, n))
        } else {
            PinAnchor::right(SPLITTER_W, pitch.row(i, n))
        }
    };
    let trunk_anchor = if flip {
        PinAnchor::right(SPLITTER_W, pitch.row(0, n))
    } else {
        PinAnchor::left(pitch.row(0, n))
    };

    let (input_anchors, output_anchors): (Vec<PinAnchor>, Vec<PinAnchor>) = if flip {
        ((0..n).map(arm_anchor).collect(), vec![trunk_anchor])
    } else {
        (vec![trunk_anchor], (0..n).map(arm_anchor).collect())
    };
    // Sized off output_anchors.len(), not `n` - they only coincide in Right mode.
    let output_bubbles = vec![false; output_anchors.len()];

    ComponentShape {
        size: vec2(px(SPLITTER_W), px(h_cells)),
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

    // Same 2x2-cell box as Input/Output; the single pin centers on grid row 1.
    let center_row = stack_h(1) / 2; // 1
    let input_anchors = match role {
        TunnelRole::Feed => vec![],
        TunnelRole::Pull => vec![PinAnchor::left(center_row)],
    };
    let output_anchors = match role {
        TunnelRole::Feed => vec![PinAnchor::right(TUNNEL_W, center_row)],
        TunnelRole::Pull => vec![],
    };

    ComponentShape {
        size: vec2(px(TUNNEL_W), px(stack_h(1))),
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

    #[test]
    fn camera_default_is_identity_transform() {
        let cam = Camera::default();
        // At zoom 1 / no pan, a grid cell maps to GRID_SIZE px and the origin
        // sits at the screen origin - the old bare-GRID_SIZE behavior.
        assert_eq!(cam.grid_to_screen(GridPos::new(0, 0)), Pos2::new(0.0, 0.0));
        assert_eq!(
            cam.grid_to_screen(GridPos::new(3, -2)),
            Pos2::new(3.0 * GRID_SIZE, -2.0 * GRID_SIZE)
        );
    }

    #[test]
    fn camera_round_trips_grid_at_nonunit_zoom() {
        let cam = Camera {
            pan: Vec2::new(37.0, -14.0),
            zoom: 2.5,
        };
        for gp in [
            GridPos::new(0, 0),
            GridPos::new(5, 9),
            GridPos::new(-8, 3),
            GridPos::new(120, -77),
        ] {
            // Screen point of a cell centre round-trips back to the same cell.
            assert_eq!(cam.screen_to_grid(cam.grid_to_screen(gp)), gp);
        }
    }

    #[test]
    fn camera_cursor_anchored_zoom_keeps_point_fixed() {
        // Mirrors handle_camera_input: pan' = cursor - (cursor - pan) * (new/old)
        // must keep the grid point under the cursor fixed on screen.
        let mut cam = Camera {
            pan: Vec2::new(10.0, 20.0),
            zoom: 1.0,
        };
        let cursor = Pos2::new(123.0, 45.0);
        let grid_under_cursor = cam.screen_to_grid(cursor);

        let old = cam.zoom;
        let new = 2.0;
        let c = cursor.to_vec2();
        cam.pan = c - (c - cam.pan) * (new / old);
        cam.zoom = new;

        // The same grid cell is still the one under the cursor.
        assert_eq!(cam.screen_to_grid(cursor), grid_under_cursor);
    }

    #[test]
    fn camera_scale_multiplies_by_zoom() {
        let cam = Camera {
            pan: Vec2::ZERO,
            zoom: 3.0,
        };
        assert_eq!(cam.scale(2.0), 6.0);
        assert_eq!(cam.grid_scale(), GRID_SIZE * 3.0);
    }

    // The whole point of the grid-native anchor rework: every pin must sit on a
    // grid intersection. Since a pin's screen position is `top_left (grid-aligned)
    // + anchor.cell * GRID_SIZE`, that holds iff every anchor cell is an integer
    // number of cells - so assert exactly that for every shape/parameter combo.
    // (The `u32` PinAnchor API already makes this true by construction; this test
    // additionally guards the bounding boxes and documents the invariant.)
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
        assert_shape_on_grid("input", &input_shape());
        assert_shape_on_grid("output", &output_shape());

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
        for num_stages in 1..=4usize {
            for parallel_load in [false, true] {
                assert_shape_on_grid(
                    &format!("shift_reg stages={num_stages} pl={parallel_load}"),
                    &shift_reg_shape(num_stages, parallel_load),
                );
            }
        }
        assert_shape_on_grid("d_flip_flop", &d_flip_flop_shape());
        assert_shape_on_grid("t_flip_flop", &t_flip_flop_shape());
        assert_shape_on_grid("jk_flip_flop", &jk_flip_flop_shape());
        assert_shape_on_grid("sr_flip_flop", &sr_flip_flop_shape());
        assert_shape_on_grid("counter", &counter_shape());
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

        // Subcircuits have a variable, independent pin count on each edge
        // (derived from the referenced circuit's Input/Output components).
        for n_in in 0..=6usize {
            for n_out in 0..=6usize {
                let shape = subcircuit_shape(n_in, n_out);
                assert_eq!(
                    shape.input_anchors.len(),
                    n_in,
                    "subcircuit {n_in}x{n_out}: input anchor count",
                );
                assert_eq!(
                    shape.output_anchors.len(),
                    n_out,
                    "subcircuit {n_in}x{n_out}: output anchor count",
                );
                assert_shape_on_grid(&format!("subcircuit {n_in}x{n_out}"), &shape);
            }
        }
    }

    // Bubble output pins sit one cell beyond the right edge (col == width + 1) so
    // the inversion bubble drawn in the gap doesn't push them off-grid.
    #[test]
    fn bubble_output_pin_is_one_cell_past_the_edge() {
        let shape = gate_shape(GateOp::Not, 1);
        let w_cells = shape.size.x / GRID_SIZE;
        assert_eq!(shape.output_anchors[0].cell.x, w_cells + 1.0);
    }

    // centered top/bottom-edge pins (mux selector here) must sit on a whole column,
    // which holds only because CENTERED_BODY_W is even by construction.
    #[test]
    fn centered_body_widths_are_even() {
        assert_eq!(MUX_W % 2, 0);
        assert_eq!(ARITH_W % 2, 0);
        assert_eq!(MUX_CENTER_COL, MUX_W / 2);
        assert_eq!(ARITH_CENTER_COL, ARITH_W / 2);
    }

    // Tight layout: 1 cell per pin, with a 2-cell gap straddling the centre for an
    // even count so there's always a definite (whole) centre row. Both layouts keep
    // the height even, so `height / 2` is the centre row either way.
    #[test]
    fn tight_pitch_gaps_the_centre_for_even_stacks() {
        // Even k=4: rows 1,2,4,5 — centre row 3 is empty (the gap).
        let rows: Vec<u32> = (0..4).map(|i| Pitch::Tight.row(i, 4)).collect();
        assert_eq!(rows, vec![1, 2, 4, 5]);
        assert_eq!(Pitch::Tight.height(4), 6);
        assert!(!rows.contains(&(Pitch::Tight.height(4) / 2))); // centre (3) has no pin

        // Odd k=3: contiguous rows 1,2,3 — the middle pin sits on centre row 2.
        let rows: Vec<u32> = (0..3).map(|i| Pitch::Tight.row(i, 3)).collect();
        assert_eq!(rows, vec![1, 2, 3]);
        assert_eq!(Pitch::Tight.height(3), 4);
        assert!(rows.contains(&(Pitch::Tight.height(3) / 2))); // centre (2) holds a pin

        // Spread stays 2-cell pitch regardless of count.
        assert_eq!(
            (0..3).map(|i| Pitch::Spread.row(i, 3)).collect::<Vec<_>>(),
            vec![1, 3, 5]
        );
        assert_eq!(Pitch::Spread.height(3), 6);
    }
}

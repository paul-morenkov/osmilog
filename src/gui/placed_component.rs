use egui::Vec2;

use crate::gui::geometry::*;
use crate::gui::shape::{ComponentShape, PinAnchor};
use crate::sim::component::{
    Adder, CombLogic, CompKey, Component, Demux, Divider, Encoder, FanDirection, Gate, GateOp,
    Input, Multiplier, Mux, Reg, Subtractor,
};

// ── PlacedComponent ───────────────────────────────────────────────────────────

pub struct PlacedComponent {
    pub key: CompKey,
    pub def: ComponentDef,
    pub grid_pos: [i32; 2],
}

// ── ComponentDef ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ComponentDef {
    Input(Input),
    Output,
    Gate(Gate),
    Mux(Mux),
    Demux(Demux),
    Reg(Reg),
    Encoder(Encoder),
    Adder(Adder),
    Subtractor(Subtractor),
    Multiplier(Multiplier),
    Divider(Divider),
    // Kept as its own lightweight, GUI-only shape rather than wrapping the sim's Splitter
    // struct (method 2 elsewhere in this enum): the sim struct bundles raw params together
    // with a precomputed routing table cached for evaluate() performance, which the GUI has
    // no use for and which would go stale whenever the user edits arm_bits here.
    Splitter {
        width: u8,
        arm_bits: Vec<Vec<u8>>,
        direction: FanDirection,
    },
}

impl ComponentDef {
    pub fn n_inputs(&self) -> usize {
        match self {
            Self::Input(_) => 0,
            Self::Output => 1,
            Self::Gate(g) => g.n_inputs(),
            Self::Mux(m) => m.n_inputs(),
            Self::Demux(d) => d.n_inputs(),
            Self::Reg(r) => r.n_inputs(),
            Self::Encoder(e) => e.n_inputs(),
            Self::Adder(a) => a.n_inputs(),
            Self::Subtractor(s) => s.n_inputs(),
            Self::Multiplier(m) => m.n_inputs(),
            Self::Divider(d) => d.n_inputs(),
            Self::Splitter {
                arm_bits,
                direction,
                ..
            } => match direction {
                FanDirection::Right => 1,
                FanDirection::Left => arm_bits.len(),
            },
        }
    }

    pub fn n_outputs(&self) -> usize {
        match self {
            Self::Input(_) => 1,
            Self::Output => 0,
            Self::Gate(g) => g.n_outputs(),
            Self::Mux(m) => m.n_outputs(),
            Self::Demux(d) => d.n_outputs(),
            Self::Reg(r) => r.n_outputs(),
            Self::Encoder(e) => e.n_outputs(),
            Self::Adder(a) => a.n_outputs(),
            Self::Subtractor(s) => s.n_outputs(),
            Self::Multiplier(m) => m.n_outputs(),
            Self::Divider(d) => d.n_outputs(),
            Self::Splitter {
                arm_bits,
                direction,
                ..
            } => match direction {
                FanDirection::Right => arm_bits.len(),
                FanDirection::Left => 1,
            },
        }
    }

    // Zero-allocation bounding-box size, matching shape().size but without
    // building the full ComponentShape (outline/anchors/bubbles Vecs) just
    // to read one field - used by component_bounding_rect, which is called
    // every frame for hit-testing/selection.
    pub fn size(&self) -> Vec2 {
        match self {
            Self::Input(_) | Self::Output => egui::vec2(COMP_MIN_WIDTH, COMP_MIN_HEIGHT),
            Self::Gate(g) => gate_size(g.op, g.n_inputs),
            Self::Mux(m) => mux_size(m.sel_width),
            Self::Demux(d) => demux_size(d.sel_width),
            Self::Reg(_) => reg_size(),
            Self::Encoder(e) => encoder_size(e.sel_width),
            Self::Adder(_) => adder_size(),
            Self::Subtractor(_) => subtractor_size(),
            Self::Multiplier(_) => multiplier_size(),
            Self::Divider(_) => divider_size(),
            Self::Splitter { arm_bits, .. } => splitter_size(arm_bits.len() as u8),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Input(_) => "IN",
            Self::Output => "OUT",
            Self::Gate(g) => match g.op {
                GateOp::And => "AND",
                GateOp::Or => "OR",
                GateOp::Xor => "XOR",
                GateOp::Nand => "NAND",
                GateOp::Nor => "NOR",
                GateOp::Xnor => "XNOR",
                GateOp::Not => "NOT",
            },
            Self::Mux(_) => "MUX",
            Self::Demux(_) => "DEMUX",
            Self::Reg(_) => "REG",
            Self::Encoder(_) => "ENC",
            Self::Adder(_) => "ADD",
            Self::Subtractor(_) => "SUB",
            Self::Multiplier(_) => "MUL",
            Self::Divider(_) => "DIV",
            Self::Splitter { direction, .. } => match direction {
                FanDirection::Right => "SPLIT",
                FanDirection::Left => "COMBINE",
            },
        }
    }

    pub fn make_component(&self) -> Component {
        match self {
            Self::Input(p) => Component::input(p.bits, p.width),
            Self::Output => Component::output(),
            Self::Gate(g) => Component::gate(g.op, g.n_inputs, g.width),
            Self::Mux(m) => Component::mux(m.data_width, m.sel_width),
            Self::Demux(d) => Component::demux(d.data_width, d.sel_width),
            Self::Reg(r) => Component::reg(r.data_width),
            Self::Encoder(e) => Component::priority_encoder(e.sel_width),
            Self::Adder(a) => Component::adder(a.data_width),
            Self::Subtractor(s) => Component::subtractor(s.data_width),
            Self::Multiplier(m) => Component::multiplier(m.data_width),
            Self::Divider(d) => Component::divider(d.data_width),
            Self::Splitter {
                arm_bits,
                direction,
                ..
            } => Component::splitter(arm_bits.clone(), *direction),
        }
    }

    pub fn shape(&self) -> ComponentShape {
        match self {
            Self::Input(_) => {
                let h = COMP_MIN_HEIGHT;
                ComponentShape {
                    size: egui::vec2(COMP_MIN_WIDTH, h),
                    outline: rect_outline(),
                    fill_outline: None,
                    input_anchors: vec![],
                    output_anchors: vec![PinAnchor::right(0.5)],
                    extra_strokes: vec![],
                    output_bubbles: vec![false],
                    labels: vec![],
                    dynamic_label_pos: Vec2::ZERO,
                }
            }
            Self::Output => {
                let h = COMP_MIN_HEIGHT;
                ComponentShape {
                    size: egui::vec2(COMP_MIN_WIDTH, h),
                    outline: rect_outline(),
                    fill_outline: None,
                    input_anchors: vec![PinAnchor::left(0.5)],
                    output_anchors: vec![],
                    extra_strokes: vec![],
                    output_bubbles: vec![],
                    labels: vec![],
                    dynamic_label_pos: Vec2::ZERO,
                }
            }
            Self::Gate(g) => gate_shape(g.op, g.n_inputs),
            Self::Mux(m) => mux_shape(m.sel_width),
            Self::Demux(d) => demux_shape(d.sel_width),
            Self::Reg(_) => reg_shape(),
            Self::Encoder(e) => encoder_shape(e.sel_width),
            Self::Adder(_) => adder_shape(),
            Self::Subtractor(_) => subtractor_shape(),
            Self::Multiplier(_) => multiplier_shape(),
            Self::Divider(_) => divider_shape(),
            Self::Splitter {
                arm_bits,
                direction,
                ..
            } => splitter_shape(arm_bits.len() as u8, *direction),
        }
    }
}

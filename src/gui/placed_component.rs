use egui::Vec2;

use crate::gui::geometry::*;
use crate::gui::shape::{ComponentShape, PinAnchor};
use crate::sim::component::{CompKey, Component, FanDirection, GateOp};

// ── PlacedComponent ───────────────────────────────────────────────────────────

pub struct PlacedComponent {
    pub key: CompKey,
    pub def: ComponentDef,
    pub grid_pos: [i32; 2],
}

// ── ComponentDef ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ComponentDef {
    Input {
        bits: u32,
        width: u8,
    },
    Output,
    Gate {
        op: GateOp,
        n_inputs: usize,
        width: u8,
    },
    Mux {
        data_width: u8,
        sel_width: u8,
    },
    Demux {
        data_width: u8,
        sel_width: u8,
    },
    Reg {
        data_width: u8,
    },
    Splitter {
        width: u8,
        arm_bits: Vec<Vec<u8>>,
        direction: FanDirection,
    },
}

impl ComponentDef {
    pub fn n_inputs(&self) -> usize {
        match self {
            Self::Input { .. } => 0,
            Self::Output => 1,
            Self::Gate { n_inputs, .. } => *n_inputs,
            Self::Mux { sel_width, .. } => (1usize << sel_width) + 1,
            Self::Demux { .. } => 2,
            Self::Reg { .. } => 2,
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
            Self::Input { .. } => 1,
            Self::Output => 0,
            Self::Gate { .. } => 1,
            Self::Mux { .. } => 1,
            Self::Demux { sel_width, .. } => 1usize << sel_width,
            Self::Reg { .. } => 1,
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
            Self::Input { .. } | Self::Output => egui::vec2(COMP_WIDTH, COMP_MIN_HEIGHT),
            Self::Gate { op, n_inputs, .. } => gate_size(*op, *n_inputs),
            Self::Mux { sel_width, .. } => mux_size(*sel_width),
            Self::Demux { sel_width, .. } => demux_size(*sel_width),
            Self::Reg { .. } => reg_size(),
            Self::Splitter { arm_bits, .. } => splitter_size(arm_bits.len() as u8),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Input { .. } => "IN",
            Self::Output => "OUT",
            Self::Gate { op, .. } => match op {
                GateOp::And => "AND",
                GateOp::Or => "OR",
                GateOp::Xor => "XOR",
                GateOp::Nand => "NAND",
                GateOp::Nor => "NOR",
                GateOp::Xnor => "XNOR",
                GateOp::Not => "NOT",
            },
            Self::Mux { .. } => "MUX",
            Self::Demux { .. } => "DEMUX",
            Self::Reg { .. } => "REG",
            Self::Splitter { direction, .. } => match direction {
                FanDirection::Right => "SPLIT",
                FanDirection::Left => "COMBINE",
            },
        }
    }

    pub fn make_component(&self) -> Component {
        match self {
            Self::Input { bits, width } => Component::input(*bits, *width),
            Self::Output => Component::output(),
            Self::Gate {
                op,
                n_inputs,
                width,
            } => Component::gate(*op, *n_inputs, *width),
            Self::Mux {
                data_width,
                sel_width,
            } => Component::mux(*data_width, *sel_width),
            Self::Demux {
                data_width,
                sel_width,
            } => Component::demux(*data_width, *sel_width),
            Self::Reg { data_width } => Component::reg(*data_width),
            Self::Splitter {
                arm_bits,
                direction,
                ..
            } => Component::splitter(arm_bits.clone(), *direction),
        }
    }

    pub fn shape(&self) -> ComponentShape {
        match self {
            Self::Input { .. } => {
                let h = COMP_MIN_HEIGHT;
                ComponentShape {
                    size: egui::vec2(COMP_WIDTH, h),
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
                    size: egui::vec2(COMP_WIDTH, h),
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
            Self::Gate { op, n_inputs, .. } => gate_shape(*op, *n_inputs),
            Self::Mux { sel_width, .. } => mux_shape(*sel_width),
            Self::Demux { sel_width, .. } => demux_shape(*sel_width),
            Self::Reg { .. } => reg_shape(),
            Self::Splitter {
                arm_bits,
                direction,
                ..
            } => splitter_shape(arm_bits.len() as u8, *direction),
        }
    }
}

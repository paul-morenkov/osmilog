use egui::Vec2;

use crate::gui::geometry::*;
use crate::gui::shape::ComponentShape;
use crate::sim::component::{CompKey, ComponentSpec, FanDirection, GateOp};

// ── PlacedComponent ───────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct PlacedComponent {
    pub key: CompKey,
    pub spec: ComponentSpec,
    pub grid_pos: GridPos,
    // Cached `spec.shape()`: drawing/hit-testing reads it many times per frame.
    // Stays in lockstep with `spec` by only being built here, in `new`.
    pub shape: ComponentShape,
}

impl PlacedComponent {
    pub fn new(key: CompKey, spec: ComponentSpec, grid_pos: GridPos) -> Self {
        let shape = spec.shape();
        Self {
            key,
            spec,
            grid_pos,
            shape,
        }
    }
}

// ── GUI-only visual concerns for ComponentSpec ────────────────────────────────
// Depends on gui::geometry/gui::shape, which sim::component must not depend on.
impl ComponentSpec {
    // Matches shape().size without building the full ComponentShape; used every frame.
    pub fn size(&self) -> Vec2 {
        match self {
            Self::Input(_) | Self::Output => io_size(),
            Self::Constant(_) => constant_size(),
            Self::Probe(_) => probe_size(),
            Self::Gate(g) => gate_size(g.op, g.n_inputs),
            Self::Mux(m) => mux_size(m.sel_width),
            Self::Demux(d) => demux_size(d.sel_width),
            Self::Reg(_) => reg_size(),
            Self::ShiftReg(sr) => shift_reg_size(sr.num_stages, sr.parallel_load),
            Self::DFlipFlop(_) | Self::TFlipFlop(_) | Self::JKFlipFlop(_) | Self::SRFlipFlop(_) => {
                flip_flop_size()
            }
            Self::Counter(_) => counter_size(),
            Self::Encoder(e) => encoder_size(e.sel_width),
            Self::Adder(_) => op2_size(),
            Self::Subtractor(_) => op2_size(),
            Self::Multiplier(_) => op2_size(),
            Self::Divider(_) => op2_size(),
            Self::Comparator(_) => comparator_size(),
            Self::Rom(_) => rom_size(),
            Self::Ram(_) => ram_size(),
            Self::Splitter { arm_bits, .. } => splitter_size(arm_bits.len() as u8),
            Self::Subcircuit {
                input_widths,
                output_widths,
                ..
            } => subcircuit_size(input_widths.len(), output_widths.len()),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Input(_) => "IN",
            // Fallback only; the canvas draws the live value dynamically (see draw_component).
            Self::Constant(_) => "CONST",
            Self::Output => "OUT",
            // Fallback only; the canvas draws the probe's name dynamically.
            Self::Probe(_) => "PROBE",
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
            Self::ShiftReg(_) => "SHIFT",
            Self::DFlipFlop(_) => "D-FF",
            Self::TFlipFlop(_) => "T-FF",
            Self::JKFlipFlop(_) => "JK-FF",
            Self::SRFlipFlop(_) => "SR-FF",
            Self::Counter(_) => "CTR",
            Self::Encoder(_) => "ENC",
            Self::Adder(_) => "ADD",
            Self::Subtractor(_) => "SUB",
            Self::Multiplier(_) => "MUL",
            Self::Divider(_) => "DIV",
            Self::Comparator(_) => "CMP",
            Self::Rom(_) => "ROM",
            Self::Ram(_) => "RAM",
            Self::Splitter { direction, .. } => match direction {
                FanDirection::Right => "SPLIT",
                FanDirection::Left => "COMBINE",
            },
            // Fallback only; the canvas draws the referenced document's name dynamically.
            Self::Subcircuit { .. } => "SUB",
        }
    }

    pub fn shape(&self) -> ComponentShape {
        puffin::profile_function!();
        match self {
            Self::Input(_) => input_shape(),
            Self::Constant(_) => constant_shape(),
            Self::Output => output_shape(),
            Self::Probe(_) => probe_shape(),
            Self::Gate(g) => gate_shape(g.op, g.n_inputs),
            Self::Mux(m) => mux_shape(m.sel_width),
            Self::Demux(d) => demux_shape(d.sel_width),
            Self::Reg(_) => reg_shape(),
            Self::ShiftReg(sr) => shift_reg_shape(sr.num_stages, sr.parallel_load),
            Self::DFlipFlop(_) => d_flip_flop_shape(),
            Self::TFlipFlop(_) => t_flip_flop_shape(),
            Self::JKFlipFlop(_) => jk_flip_flop_shape(),
            Self::SRFlipFlop(_) => sr_flip_flop_shape(),
            Self::Counter(_) => counter_shape(),
            Self::Encoder(e) => encoder_shape(e.sel_width),
            Self::Adder(_) => adder_shape(),
            Self::Subtractor(_) => subtractor_shape(),
            Self::Multiplier(_) => multiplier_shape(),
            Self::Divider(_) => divider_shape(),
            Self::Comparator(_) => comparator_shape(),
            Self::Rom(_) => rom_shape(),
            Self::Ram(_) => ram_shape(),
            Self::Splitter {
                arm_bits,
                direction,
                ..
            } => splitter_shape(arm_bits.len() as u8, *direction),
            Self::Subcircuit {
                input_widths,
                output_widths,
                ..
            } => subcircuit_shape(input_widths.len(), output_widths.len()),
        }
    }
}

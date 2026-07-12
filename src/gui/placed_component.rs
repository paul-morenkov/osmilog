use egui::Vec2;

use crate::gui::geometry::*;
use crate::gui::shape::ComponentShape;
use crate::sim::component::{CompKey, ComponentSpec, FanDirection, GateOp};

// ── PlacedComponent ───────────────────────────────────────────────────────────

pub struct PlacedComponent {
    pub key: CompKey,
    pub spec: ComponentSpec,
    pub grid_pos: GridPos,
    // Cached `spec.shape()`. Building a ComponentShape allocates several Vecs,
    // and drawing/hit-testing reads it multiple times per component per frame,
    // so it's built once here (only `spec` determines it) instead of rebuilt on
    // every read. Kept in lockstep with `spec` by only constructing through
    // `PlacedComponent::new` - `reconfigure_component` rebuilds via that path.
    pub shape: ComponentShape,
    // Tombstone flag, mirroring sim::Component::active and wiring::WireNode::active:
    // a deleted PlacedComponent is flagged inactive rather than removed, so
    // its PlacedCompKey stays valid for Wiring/selection/drag state that
    // reference it. Reads iterate OsmilogApp::active_components.
    pub active: bool,
}

impl PlacedComponent {
    // Builds a placed component, caching its ComponentShape (see the `shape`
    // field). This is the only place a PlacedComponent is created, so `shape`
    // can never drift from `spec`.
    pub fn new(key: CompKey, spec: ComponentSpec, grid_pos: GridPos) -> Self {
        let shape = spec.shape();
        Self {
            key,
            spec,
            grid_pos,
            shape,
            active: true,
        }
    }
}

// ── GUI-only visual concerns for ComponentSpec ────────────────────────────────
//
// ComponentSpec itself (construction params per component type) lives in
// sim::component. This impl block adds display-only methods depending on
// gui::geometry/gui::shape types the sim layer must not depend on - a plain
// second inherent impl, no wrapper/newtype needed.
impl ComponentSpec {
    // Zero-allocation bounding-box size, matching shape().size without
    // building the full ComponentShape - used every frame for hit-testing.
    pub fn size(&self) -> Vec2 {
        match self {
            Self::Input(_) | Self::Output => io_size(),
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
            Self::Splitter { direction, .. } => match direction {
                FanDirection::Right => "SPLIT",
                FanDirection::Left => "COMBINE",
            },
        }
    }

    pub fn shape(&self) -> ComponentShape {
        puffin::profile_function!();
        match self {
            Self::Input(_) => input_shape(),
            Self::Output => output_shape(),
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
            Self::Splitter {
                arm_bits,
                direction,
                ..
            } => splitter_shape(arm_bits.len() as u8, *direction),
        }
    }
}

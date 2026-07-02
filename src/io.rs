// Save/load format for whole circuits, meant to be shared as a plain JSON
// file (readable, diffable in git, hand-editable). `CircuitFile` mirrors the
// GUI's visual state (PlacedComponent/PlacedTunnel/Wire/TunnelWire in
// src/gui/app.rs), not the sim `Circuit`'s internal SlotMaps directly -
// CompKey/TunnelKey/NetKey are ephemeral generational-arena identifiers
// assigned at runtime, not stable identity worth persisting. Every
// cross-reference here is a plain `usize` index into `components`/
// `tunnels` (position in the Vec, assigned at save time), not a slotmap
// key.
//
// This module is deliberately gui-light: it only depends on the plain-data
// types (ComponentDef, GateOp, FanDirection, TunnelRole) needed to describe
// a circuit's shape, not on OsmilogApp itself - the App<->CircuitFile
// conversion logic (OsmilogApp::to_circuit_file / load_circuit_file) lives
// in app.rs, which already owns the SlotMaps this format needs to walk.

use serde::{Deserialize, Serialize};

use crate::gui::placed_component::ComponentDef;
use crate::sim::circuit::TunnelRole;

// Bumped whenever CircuitFile's shape changes in a way that breaks
// compatibility with previously saved files. Checked by `validate()`.
pub const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitFile {
    pub version: u32,
    pub components: Vec<ComponentEntry>,
    pub tunnels: Vec<TunnelEntry>,
    pub wires: Vec<WireEntry>,
    pub tunnel_wires: Vec<TunnelWireEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentEntry {
    pub def: ComponentDef,
    pub grid_pos: [i32; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelEntry {
    pub label: String,
    pub role: TunnelRole,
    pub grid_pos: [i32; 2],
}

// `src`/`dst` are indices into CircuitFile::components (position in the
// Vec, assigned at save time) - not a CompKey.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WireEntry {
    pub src: usize,
    pub src_pin: u8,
    pub dst: usize,
    pub dst_pin: u8,
}

// `tunnel`/`comp` are indices into CircuitFile::tunnels/components
// respectively. `is_input` + `pin_index` spell out a PinId without this
// module needing to depend on sim::component::PinId.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TunnelWireEntry {
    pub tunnel: usize,
    pub comp: usize,
    pub is_input: bool,
    pub pin_index: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    UnsupportedVersion { found: u32, supported: u32 },
    ComponentIndexOutOfRange { index: usize, len: usize },
    TunnelIndexOutOfRange { index: usize, len: usize },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::UnsupportedVersion { found, supported } => write!(
                f,
                "circuit file version {found} is not supported by this build (supports version {supported})"
            ),
            LoadError::ComponentIndexOutOfRange { index, len } => write!(
                f,
                "component index {index} out of range (file has {len} components)"
            ),
            LoadError::TunnelIndexOutOfRange { index, len } => write!(
                f,
                "tunnel index {index} out of range (file has {len} tunnels)"
            ),
        }
    }
}

impl std::error::Error for LoadError {}

impl CircuitFile {
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }

    // Checks the version and every cross-reference's bounds without
    // touching any app state, so a caller (e.g. OsmilogApp::load_circuit_file)
    // can validate a file - possibly hand-edited - before committing to
    // replacing the current circuit. Does not check that a wire's pin index
    // is within the referenced component's actual pin count; the sim layer
    // doesn't validate that either yet (see Circuit::link's pin-index
    // TODOs), so an out-of-range pin index can still panic downstream.
    pub fn validate(&self) -> Result<(), LoadError> {
        if self.version != CURRENT_VERSION {
            return Err(LoadError::UnsupportedVersion {
                found: self.version,
                supported: CURRENT_VERSION,
            });
        }
        let n_components = self.components.len();
        let n_tunnels = self.tunnels.len();
        for w in &self.wires {
            if w.src >= n_components {
                return Err(LoadError::ComponentIndexOutOfRange {
                    index: w.src,
                    len: n_components,
                });
            }
            if w.dst >= n_components {
                return Err(LoadError::ComponentIndexOutOfRange {
                    index: w.dst,
                    len: n_components,
                });
            }
        }
        for tw in &self.tunnel_wires {
            if tw.tunnel >= n_tunnels {
                return Err(LoadError::TunnelIndexOutOfRange {
                    index: tw.tunnel,
                    len: n_tunnels,
                });
            }
            if tw.comp >= n_components {
                return Err(LoadError::ComponentIndexOutOfRange {
                    index: tw.comp,
                    len: n_components,
                });
            }
        }
        Ok(())
    }
}

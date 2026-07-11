// Save/load format for whole circuits, shared as a plain JSON file. Mirrors
// the GUI's visual state (PlacedComponent/PlacedTunnel + the wiring graph),
// not sim `Circuit`'s SlotMaps directly - slotmap keys are ephemeral, so
// every cross-reference here is a plain `usize` index into
// `components`/`tunnels`/`nodes` instead.
//
// Deliberately gui-light: depends only on plain-data types (ComponentSpec,
// GateOp, FanDirection, TunnelRole), not OsmilogApp itself - the
// App<->CircuitFile conversion lives in app.rs, which owns the SlotMaps.

use serde::{Deserialize, Serialize};

use crate::gui::geometry::GridPos;
use crate::sim::circuit::TunnelRole;
use crate::sim::component::ComponentSpec;

// Bumped whenever CircuitFile's shape changes in a way that breaks
// compatibility with previously saved files. Checked by `validate()`.
// v2: wires became a grid segment graph (`nodes` + `segments`), replacing the
// v1 pin-to-pin `wires`/`tunnel_wires` lists. v1 files are rejected.
pub const CURRENT_VERSION: u32 = 2;
pub const CIRCUIT_FILE_EXT: &str = "osm";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitFile {
    pub version: u32,
    pub components: Vec<ComponentEntry>,
    pub tunnels: Vec<TunnelEntry>,
    pub nodes: Vec<NodeEntry>,
    pub segments: Vec<SegEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentEntry {
    pub spec: ComponentSpec,
    pub grid_pos: GridPos,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelEntry {
    pub label: String,
    pub role: TunnelRole,
    pub grid_pos: GridPos,
}

// A wire graph node at a grid position, optionally bound to a component pin or
// a tunnel. `comp`/`tunnel` are indices into CircuitFile::components/tunnels;
// `is_input` + `pin_index` spell out a PinId without depending on
// sim::component::PinId.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct NodeEntry {
    pub pos: GridPos,
    pub attach: NodeAttachEntry,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum NodeAttachEntry {
    Free,
    Pin {
        comp: usize,
        is_input: bool,
        pin_index: u8,
    },
    Tunnel {
        tunnel: usize,
    },
}

// `a`/`b` are indices into CircuitFile::nodes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SegEntry {
    pub a: usize,
    pub b: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    UnsupportedVersion { found: u32, supported: u32 },
    ComponentIndexOutOfRange { index: usize, len: usize },
    TunnelIndexOutOfRange { index: usize, len: usize },
    NodeIndexOutOfRange { index: usize, len: usize },
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
            LoadError::NodeIndexOutOfRange { index, len } => write!(
                f,
                "wire node index {index} out of range (file has {len} nodes)"
            ),
        }
    }
}

impl std::error::Error for LoadError {}

impl CircuitFile {
    pub fn new(
        components: Vec<ComponentEntry>,
        tunnels: Vec<TunnelEntry>,
        nodes: Vec<NodeEntry>,
        segments: Vec<SegEntry>,
    ) -> Self {
        Self {
            version: CURRENT_VERSION,
            components,
            tunnels,
            nodes,
            segments,
        }
    }
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }

    // Checks the version and every cross-reference's bounds without
    // touching app state, so a caller can validate a possibly hand-edited
    // file before committing to it. Does NOT check a node's pin index
    // against the component's actual arity - out-of-range can still panic
    // downstream (see Circuit::link's pin-index TODOs).
    pub fn validate(&self) -> Result<(), LoadError> {
        if self.version != CURRENT_VERSION {
            return Err(LoadError::UnsupportedVersion {
                found: self.version,
                supported: CURRENT_VERSION,
            });
        }
        let n_components = self.components.len();
        let n_tunnels = self.tunnels.len();
        let n_nodes = self.nodes.len();
        for n in &self.nodes {
            match n.attach {
                NodeAttachEntry::Free => {}
                NodeAttachEntry::Pin { comp, .. } => {
                    if comp >= n_components {
                        return Err(LoadError::ComponentIndexOutOfRange {
                            index: comp,
                            len: n_components,
                        });
                    }
                }
                NodeAttachEntry::Tunnel { tunnel } => {
                    if tunnel >= n_tunnels {
                        return Err(LoadError::TunnelIndexOutOfRange {
                            index: tunnel,
                            len: n_tunnels,
                        });
                    }
                }
            }
        }
        for s in &self.segments {
            if s.a >= n_nodes {
                return Err(LoadError::NodeIndexOutOfRange {
                    index: s.a,
                    len: n_nodes,
                });
            }
            if s.b >= n_nodes {
                return Err(LoadError::NodeIndexOutOfRange {
                    index: s.b,
                    len: n_nodes,
                });
            }
        }
        Ok(())
    }
}

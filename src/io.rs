// Save/load format for whole circuits, shared as a plain JSON file. Mirrors
// the GUI's visual state (PlacedComponent/PlacedTunnel + the wiring graph),
// not sim `Circuit`'s SlotMaps directly - slotmap keys are ephemeral, so
// every cross-reference here is a plain `usize` index into
// `components`/`tunnels`/`nodes` instead.
//
// Deliberately gui-light: depends only on plain-data types (ComponentSpec,
// GateOp, FanDirection, TunnelRole), not OsmilogApp itself - the
// App<->records conversion lives in app.rs, which owns the SlotMaps.

use serde::{Deserialize, Serialize};

use crate::gui::geometry::GridPos;
use crate::sim::circuit::TunnelRole;
use crate::sim::component::ComponentSpec;

// Bumped whenever the on-disk shape changes in a way that breaks compatibility
// with previously saved files. Checked by `validate()`.
// v2: wires became a grid segment graph (`nodes` + `segments`), replacing the
// v1 pin-to-pin `wires`/`tunnel_wires` lists. v1 files are rejected.
// v3: the top-level file became a `ProjectFile` holding *several* named
// circuits (so subcircuits round-trip), rather than a single circuit. v2 files
// are still accepted and loaded as a one-circuit project (see
// `ProjectFile::from_json` / `LegacyV2File`).
pub const CURRENT_VERSION: u32 = 3;
// The single-circuit file version (`LegacyV2File`) that v3 still upgrades on
// load. Predates subcircuits, so such a file never carries cross-circuit refs.
pub const LEGACY_SINGLE_CIRCUIT_VERSION: u32 = 2;
pub const CIRCUIT_FILE_EXT: &str = "osm";

// One circuit's visual state as index-based records: placed components/tunnels
// plus the wiring graph (nodes + segments), every cross-reference a plain
// `usize` index into these vectors (slotmap keys are ephemeral and not worth
// persisting). NOT a file itself - it's the reusable payload shared by the
// clipboard snapshot (`gui::clipboard`), each project `CircuitEntry` (flattened
// in), and the legacy v2 file body (`LegacyV2File`). The App<->records
// conversion lives in `gui::app`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitSnapshot {
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
// a tunnel. `comp`/`tunnel` are indices into the snapshot's components/tunnels;
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

// `a`/`b` are indices into the snapshot's nodes.
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
    CircuitIndexOutOfRange { index: usize, len: usize },
    EmptyProject,
    Parse(String),
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
            LoadError::CircuitIndexOutOfRange { index, len } => write!(
                f,
                "circuit index {index} out of range (project has {len} circuits)"
            ),
            LoadError::EmptyProject => write!(f, "project file contains no circuits"),
            LoadError::Parse(msg) => write!(f, "malformed circuit file: {msg}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl CircuitSnapshot {
    // Checks every cross-reference's bounds (wire nodes -> components/tunnels,
    // segments -> nodes) without touching app state, so a caller can reject a
    // possibly hand-edited snapshot before committing to it. Does NOT check a
    // node's pin index against the component's actual arity - out-of-range can
    // still panic downstream (see Circuit::link's pin-index TODOs). Reused
    // per-circuit by `ProjectFile::validate`.
    pub fn validate(&self) -> Result<(), LoadError> {
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

// The v2 single-circuit file: a versioned `CircuitSnapshot`. v3 no longer writes
// this shape, but `ProjectFile::from_json` still reads it and upgrades it to a
// one-circuit project (v2 predates subcircuits, so there are never cross-circuit
// refs to carry). `#[serde(flatten)]` keeps the on-disk shape byte-identical to
// the old top-level file. Serialize is derived only so tests can fabricate an
// old file; production solely deserializes one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LegacyV2File {
    pub version: u32,
    #[serde(flatten)]
    pub snapshot: CircuitSnapshot,
}

// ── ProjectFile (v3): several circuits saved together ───────────────────────
// The top-level on-disk unit. A workspace of named circuits, so a subcircuit
// (one circuit placed as a component inside another) round-trips: its
// cross-circuit link is a plain `usize` index into `circuits`, exactly like
// every other cross-reference in this module. A legacy v2 file loads as a
// one-circuit project (see `from_json` / `LegacyV2File`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectFile {
    pub version: u32,
    // Index into `circuits` of the document that was active when saved; made
    // the active document again on load.
    pub active: usize,
    pub circuits: Vec<CircuitEntry>,
}

// One circuit within a project: its display name, its visual records (a
// `CircuitSnapshot`, flattened so the on-disk shape stays flat), plus its
// outgoing subcircuit references.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitEntry {
    pub name: String,
    #[serde(flatten)]
    pub snapshot: CircuitSnapshot,
    // One per placed Subcircuit component in `snapshot.components`.
    // `ComponentSpec::Subcircuit::doc` is a runtime-only slotmap key
    // (serde-skipped), so the cross-circuit link is carried here as indices and
    // re-bound to freshly-allocated DocIds on load.
    pub subcircuits: Vec<SubcircuitRef>,
}

// `component` indexes the owning `CircuitEntry::components`; `circuit` indexes
// the project's `circuits`. Together: "this placed subcircuit refers to that
// circuit".
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SubcircuitRef {
    pub component: usize,
    pub circuit: usize,
}

impl ProjectFile {
    pub fn new(active: usize, circuits: Vec<CircuitEntry>) -> Self {
        Self {
            version: CURRENT_VERSION,
            active,
            circuits,
        }
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    // Parses a project file, transparently upgrading a legacy v2 single-circuit
    // file into a one-circuit project. Bounds are not checked here - call
    // `validate()` before installing the result.
    pub fn from_json(s: &str) -> Result<Self, LoadError> {
        #[derive(Deserialize)]
        struct VersionProbe {
            version: u32,
        }
        let probe: VersionProbe =
            serde_json::from_str(s).map_err(|e| LoadError::Parse(e.to_string()))?;
        if probe.version == CURRENT_VERSION {
            serde_json::from_str(s).map_err(|e| LoadError::Parse(e.to_string()))
        } else if probe.version == LEGACY_SINGLE_CIRCUIT_VERSION {
            let legacy: LegacyV2File =
                serde_json::from_str(s).map_err(|e| LoadError::Parse(e.to_string()))?;
            Ok(Self::from_snapshot(legacy.snapshot))
        } else {
            Err(LoadError::UnsupportedVersion {
                found: probe.version,
                supported: CURRENT_VERSION,
            })
        }
    }

    // Wraps a single circuit's records as a one-circuit project: the sole
    // circuit becomes the first and active document, named "Main". Used to
    // upgrade a legacy v2 file, which predates subcircuits and so carries no
    // cross-circuit references.
    pub fn from_snapshot(snapshot: CircuitSnapshot) -> Self {
        Self::new(
            0,
            vec![CircuitEntry {
                name: "Main".to_string(),
                snapshot,
                subcircuits: Vec::new(),
            }],
        )
    }

    // Checks the version, that `active` is in range, and every per-circuit
    // cross-reference's bounds (wire nodes/segments and subcircuit refs), so a
    // caller can reject a malformed / hand-edited file before touching app
    // state. Like `CircuitSnapshot::validate`, does not check pin arity.
    pub fn validate(&self) -> Result<(), LoadError> {
        if self.version != CURRENT_VERSION {
            return Err(LoadError::UnsupportedVersion {
                found: self.version,
                supported: CURRENT_VERSION,
            });
        }
        let n_circuits = self.circuits.len();
        if n_circuits == 0 {
            return Err(LoadError::EmptyProject);
        }
        if self.active >= n_circuits {
            return Err(LoadError::CircuitIndexOutOfRange {
                index: self.active,
                len: n_circuits,
            });
        }
        for c in &self.circuits {
            c.snapshot.validate()?;
            for sub in &c.subcircuits {
                if sub.component >= c.snapshot.components.len() {
                    return Err(LoadError::ComponentIndexOutOfRange {
                        index: sub.component,
                        len: c.snapshot.components.len(),
                    });
                }
                if sub.circuit >= n_circuits {
                    return Err(LoadError::CircuitIndexOutOfRange {
                        index: sub.circuit,
                        len: n_circuits,
                    });
                }
            }
        }
        Ok(())
    }
}

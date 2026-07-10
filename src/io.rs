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

// ── File I/O ──────────────────────────────────────────────────────────────
//
// Native and WASM need genuinely different mechanics (blocking OS dialogs
// vs. Promise-based browser APIs), so each gets its own submodule instead of
// one function full of `#[cfg]`s. Both stay OsmilogApp-agnostic, taking/
// returning `CircuitFile`/`String` only.

#[cfg(not(target_arch = "wasm32"))]
pub mod native {

    use super::{CircuitFile, CIRCUIT_FILE_EXT};

    // Opens a native "Save As" dialog and writes `json` to the chosen path.
    // `None` means the user cancelled; `Some(Err(..))` means the dialog
    // completed but the write failed.
    pub fn save_dialog(json: &str) -> Option<Result<(), String>> {
        let path = rfd::FileDialog::new()
            .add_filter("osmilog circuit", &[CIRCUIT_FILE_EXT])
            .set_file_name(format!("circuit.{}", CIRCUIT_FILE_EXT))
            .save_file()?;
        Some(std::fs::write(path, json).map_err(|e| e.to_string()))
    }

    // Opens a native "Open" dialog and reads + parses + validates the
    // chosen file. `None` means the user cancelled.
    pub fn load_dialog() -> Option<Result<CircuitFile, String>> {
        let path = rfd::FileDialog::new()
            .add_filter("osmilog circuit", &[CIRCUIT_FILE_EXT])
            .pick_file()?;
        Some((|| {
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let file = CircuitFile::from_json(&text).map_err(|e| e.to_string())?;
            file.validate().map_err(|e| e.to_string())?;
            Ok(file)
        })())
    }
}

#[cfg(target_arch = "wasm32")]
pub mod wasm {
    use std::cell::RefCell;
    use std::rc::Rc;

    use wasm_bindgen::JsCast;
    use web_sys::{Blob, BlobPropertyBag, HtmlAnchorElement, Url};

    use super::{CircuitFile, CIRCUIT_FILE_EXT};

    // The browser has no synchronous file dialogs, so a spawned load task
    // delivers its outcome into this shared slot instead of returning it;
    // the host app polls it once per frame (see OsmilogApp::logic).
    pub type PendingLoad = Rc<RefCell<Option<Result<CircuitFile, String>>>>;

    pub fn new_pending_load() -> PendingLoad {
        Rc::new(RefCell::new(None))
    }

    // Triggers a browser download via Blob + object URL + synthetic
    // `<a download>` click, rather than the File System Access API's save
    // picker (which `rfd`'s wasm backend uses) - that API is Chromium-only.
    pub fn trigger_download(contents: &str) {
        let window = web_sys::window().expect("no window");
        let document = window.document().expect("no document");

        let parts = js_sys::Array::new();
        parts.push(&wasm_bindgen::JsValue::from_str(contents));
        let opts = BlobPropertyBag::new();
        opts.set_type("application/json");
        let blob =
            Blob::new_with_str_sequence_and_options(&parts, &opts).expect("failed to build blob");
        let url = Url::create_object_url_with_blob(&blob).expect("failed to create object url");
        let filename = format!("circuit.{}", CIRCUIT_FILE_EXT);

        let anchor: HtmlAnchorElement = document
            .create_element("a")
            .expect("failed to create anchor")
            .dyn_into()
            .expect("created element is not an anchor");
        anchor.set_href(&url);
        anchor.set_download(&filename);

        // Firefox requires the anchor be attached to the document for a
        // synthetic click to trigger a download; attach, click, detach.
        let body = document.body().expect("document has no body");
        body.append_child(&anchor).expect("failed to attach anchor");
        anchor.click();
        body.remove_child(&anchor).expect("failed to detach anchor");
        // Object URL intentionally left un-revoked: a one-off leaked blob
        // URL per Save click is harmless for an interactive session, and
        // revoking immediately risks racing the browser's download start.
    }

    // Opens the browser's file-upload dialog and, once the user picks a
    // file, reads + parses + validates it and delivers the outcome into
    // `slot`. Returns immediately; the caller polls `slot` on a later frame.
    pub fn spawn_load_dialog(slot: PendingLoad) {
        wasm_bindgen_futures::spawn_local(async move {
            let outcome = async {
                let handle = rfd::AsyncFileDialog::new()
                    .add_filter("osmilog circuit", &[CIRCUIT_FILE_EXT])
                    .pick_file()
                    .await
                    .ok_or_else(|| "no file selected".to_string())?;
                let bytes = handle.read().await;
                let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
                let file = CircuitFile::from_json(&text).map_err(|e| e.to_string())?;
                file.validate().map_err(|e| e.to_string())?;
                Ok(file)
            }
            .await;
            *slot.borrow_mut() = Some(outcome);
        });
    }
}

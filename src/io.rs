// Save/load format for whole circuits, meant to be shared as a plain JSON
// file (readable, diffable in git, hand-editable). `CircuitFile` mirrors the
// GUI's visual state (PlacedComponent/PlacedTunnel + the wiring graph in
// src/gui/app.rs / src/gui/wiring.rs), not the sim `Circuit`'s internal
// SlotMaps directly - CompKey/TunnelKey/NetKey are ephemeral generational-arena
// identifiers assigned at runtime, not stable identity worth persisting. Every
// cross-reference here is a plain `usize` index into `components`/`tunnels`/
// `nodes` (position in the Vec, assigned at save time), not a slotmap key.
//
// This module is deliberately gui-light: it only depends on the plain-data
// types (ComponentDef, GateOp, FanDirection, TunnelRole) needed to describe
// a circuit's shape, not on OsmilogApp itself - the App<->CircuitFile
// conversion logic (OsmilogApp::to_circuit_file / load_circuit_file) lives
// in app.rs, which already owns the SlotMaps this format needs to walk.

use serde::{Deserialize, Serialize};

use crate::gui::geometry::GridPos;
use crate::gui::placed_component::ComponentDef;
use crate::sim::circuit::TunnelRole;

// Bumped whenever CircuitFile's shape changes in a way that breaks
// compatibility with previously saved files. Checked by `validate()`.
// v2: wires became a grid segment graph (`nodes` + `segments`), replacing the
// v1 pin-to-pin `wires`/`tunnel_wires` lists. v1 files are rejected.
pub const CURRENT_VERSION: u32 = 2;

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
    pub def: ComponentDef,
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
    // touching any app state, so a caller (e.g. OsmilogApp::load_circuit_file)
    // can validate a file - possibly hand-edited - before committing to
    // replacing the current circuit. Does not check that a node's pin index
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
// Native and WASM need genuinely different mechanics here (blocking OS
// dialogs + a real filesystem vs. Promise-based browser APIs with no
// filesystem at all), so each gets its own submodule rather than one
// function with `#[cfg]`s sprinkled through its body. Both stay
// OsmilogApp-agnostic: they take/return `CircuitFile`/`String`, not app
// state, so the GUI's menu handlers (src/gui/app.rs) are thin callers.

#[cfg(not(target_arch = "wasm32"))]
pub mod native {
    use super::CircuitFile;

    // Opens a native "Save As" dialog and writes `json` to the chosen path.
    // `None` means the user cancelled; `Some(Err(..))` means the dialog
    // completed but the write failed.
    pub fn save_dialog(json: &str) -> Option<Result<(), String>> {
        let path = rfd::FileDialog::new()
            .add_filter("osmilog circuit", &["json"])
            .set_file_name("circuit.json")
            .save_file()?;
        Some(std::fs::write(path, json).map_err(|e| e.to_string()))
    }

    // Opens a native "Open" dialog and reads + parses + validates the
    // chosen file. `None` means the user cancelled.
    pub fn load_dialog() -> Option<Result<CircuitFile, String>> {
        let path = rfd::FileDialog::new()
            .add_filter("osmilog circuit", &["json"])
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

    use super::CircuitFile;

    // The browser has no synchronous file dialogs - picking and reading a
    // file are both Promise-based, so a spawned load task can't just
    // `return` its result to the button click handler that started it.
    // Instead it delivers the outcome into this shared slot, which the host
    // app polls once per frame (see OsmilogApp::logic).
    pub type PendingLoad = Rc<RefCell<Option<Result<CircuitFile, String>>>>;

    pub fn new_pending_load() -> PendingLoad {
        Rc::new(RefCell::new(None))
    }

    // Triggers a browser download of `contents` as `filename` via the
    // classic Blob + object URL + synthetic `<a download>` click. Chosen
    // over the File System Access API's save picker (which `rfd`'s wasm
    // backend uses) because that API is Chromium-only; this works in every
    // browser and matches "save downloads a file" rather than "save opens a
    // dialog", per the differing native/WASM save UX.
    pub fn trigger_download(filename: &str, contents: &str) {
        let window = web_sys::window().expect("no window");
        let document = window.document().expect("no document");

        let parts = js_sys::Array::new();
        parts.push(&wasm_bindgen::JsValue::from_str(contents));
        let opts = BlobPropertyBag::new();
        opts.set_type("application/json");
        let blob =
            Blob::new_with_str_sequence_and_options(&parts, &opts).expect("failed to build blob");
        let url = Url::create_object_url_with_blob(&blob).expect("failed to create object url");

        let anchor: HtmlAnchorElement = document
            .create_element("a")
            .expect("failed to create anchor")
            .dyn_into()
            .expect("created element is not an anchor");
        anchor.set_href(&url);
        anchor.set_download(filename);

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
                    .add_filter("osmilog circuit", &["json"])
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
